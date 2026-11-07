// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Projected GET file effects using the existing asynchronous file capability.

use super::{BorrowedOperations, GetErrorKind, GetEvidence, OutputClock, RuntimeGetHost};
use fabric_cli_transfer::{TransferEvent, TransferFuture};
use fabric_deploy::definitions::{
    DefinitionError, DefinitionErrorKind, DefinitionFile, ExportStorage,
    serialize_get_projected_result, validate_part_path,
};
use fabric_sdk::Client;
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use futures_util::future::FusedFuture;
use serde_json::Value;
use std::{
    future::{Future, poll_fn},
    io,
    path::PathBuf,
    pin::{Pin, pin},
    task::Poll,
};

/// Validated caller-owned local `%Y%m%d_%H%M%S`, never acquired from a clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTimestamp(String);

impl LocalTimestamp {
    /// Validate the exact source filename shape without granting path authority.
    ///
    /// # Errors
    /// Rejects separators, controls, missing/extra digits and malformed fields.
    pub fn parse(value: &str) -> Result<Self, fabric_cli_output::CommandError> {
        let valid = value.len() == 15
            && value.as_bytes()[8] == b'_'
            && value
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 8 || byte.is_ascii_digit());
        if !valid {
            return Err(crate::errors::error(
                "InvalidInput",
                "Invalid local GET timestamp",
            ));
        }
        let field = |start, end| value[start..end].parse::<u32>().unwrap_or(u32::MAX);
        let (year, month, day) = (field(0, 4), field(4, 6), field(6, 8));
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let max_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if year == 0
            || day == 0
            || day > max_day
            || field(9, 11) > 23
            || field(11, 13) > 59
            || field(13, 15) > 59
        {
            return Err(crate::errors::error(
                "InvalidInput",
                "Invalid local GET timestamp",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Exact timestamp selected by the caller.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Caller-owned asynchronous local clock, consulted only at the output boundary.
pub trait GetOutputClock: Send + Sync {
    /// Sample source local wall time without granting filesystem authority.
    fn local_timestamp(
        &self,
    ) -> TransferFuture<'_, Result<LocalTimestamp, fabric_cli_output::CommandError>>;
}

/// Explicit durable/possibly-partial file effects, never an export envelope.
#[derive(Clone, Debug, Default)]
pub struct GetFileReceipt {
    /// Root resolved by the caller's existing local/OneLake policy.
    pub root: Option<PathBuf>,
    /// Item directory whose create call completed.
    pub completed_directory: Option<String>,
    /// A create call began but did not complete.
    pub incomplete_directory: Option<String>,
    /// Fully written relative files, in source order.
    pub completed_paths: Vec<String>,
    /// A write began and may have left partial local or remote effects.
    pub incomplete_path: Option<String>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned output boundaries."
)]
pub(super) async fn write_projected(
    sdk: &Client,
    host: &mut RuntimeGetHost<'_>,
    destination: &str,
    source_name: &str,
    projected: &Value,
    evidence: &mut GetEvidence,
    cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<(), GetErrorKind> {
    let mut session = OperationSession::with_context(
        sdk.context(),
        BorrowedOperations(host.operations),
        host.operation_limits,
        cancelled,
    );
    let files = host.files.as_deref_mut().ok_or_else(|| {
        crate::errors::unsupported(
            "GET result-file output requires the caller's TransferFiles host",
        )
    })?;
    let root = phase(
        &mut session,
        files.resolve_export_directory_async(destination),
        || {},
    )
    .await
    .map_err(|(error, _)| error)?;
    evidence.files.root = Some(root.clone());
    let timestamp = match &host.clock {
        OutputClock::Fixed(timestamp) => (**timestamp).clone(),
        OutputClock::Deferred(clock) => {
            let mut work = pin!(clock.local_timestamp());
            poll_fn(|cx| {
                let mut checkpoint = pin!(session.check_cancelled());
                if let Poll::Ready(Err(error)) = checkpoint.as_mut().poll(cx) {
                    return Poll::Ready(Err(GetErrorKind::from(error)));
                }
                work.as_mut()
                    .poll(cx)
                    .map(|result| result.map_err(GetErrorKind::from))
            })
            .await?
        }
    };
    let storage = files.export_storage(&root);
    let encoded =
        serialize_get_projected_result(projected, storage, host.newline, host.definition_limits)?;
    let base = format!("{source_name}/get_{}", timestamp.as_str());
    // do_output's displayed suffix does not change the path passed to storage.
    let suffix = if encoded.is_json { ".json" } else { "" };
    let path = format!("{base}{suffix}");
    validate_part_path(&path, host.definition_limits).map_err(DefinitionError::from)?;
    let displayed = files.export_display_path(&root, destination);
    host.ui.emit(TransferEvent::Grey(format!(
        "Exporting result to '{displayed}/{path}'..."
    )))?;
    let mut prepared = phase(
        &mut session,
        files.preflight_async(
            &root,
            vec![DefinitionFile {
                path: path.clone(),
                bytes: encoded.bytes,
            }],
        ),
        || {},
    )
    .await
    .map_err(|(error, _)| error)?;
    if prepared.len() != 1 || prepared[0].path != path {
        return Err(
            crate::errors::error("InvalidInput", "GET file preflight changed its plan").into(),
        );
    }
    let directories: Vec<_> = if storage == ExportStorage::Local {
        source_name.split('/').collect()
    } else {
        vec![source_name]
    };
    let mut directory_root = root.clone();
    let mut directory_path = String::new();
    for component in directories {
        session.check_cancelled().await?;
        if !directory_path.is_empty() {
            directory_path.push('/');
        }
        directory_path.push_str(component);
        match phase(
            &mut session,
            files.create_directory_async(&directory_root, component),
            || evidence.files.incomplete_directory = Some(directory_path.clone()),
        )
        .await
        {
            Ok(()) => {
                evidence.files.incomplete_directory = None;
                evidence.files.completed_directory = Some(directory_path.clone());
            }
            Err((error, started)) => {
                evidence.files.incomplete_directory = started.then(|| directory_path.clone());
                return Err(error.into());
            }
        }
        if storage == ExportStorage::Local {
            directory_root.push(component);
        }
    }
    session.check_cancelled().await?;
    let file = prepared.pop().expect("checked one-file plan");
    match phase(&mut session, files.write_async(&root, file), || {
        evidence.files.incomplete_path = Some(path.clone());
    })
    .await
    {
        Ok(()) => {
            evidence.files.incomplete_path = None;
            evidence.files.completed_paths.push(path.clone());
        }
        Err((mut error, started)) => {
            evidence.files.incomplete_path = started.then(|| path.clone());
            error.incomplete_path = evidence.files.incomplete_path.clone();
            return Err(error.into());
        }
    }
    session.check_cancelled().await?;
    Ok(())
}

async fn phase<T, E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    work: impl Future<Output = io::Result<T>>,
    mut on_start: impl FnMut(),
) -> Result<T, (DefinitionError, bool)> {
    let mut work = pin!(work);
    let mut started = false;
    poll_fn(|cx| {
        let mut checkpoint = pin!(session.check_cancelled());
        if let Poll::Ready(Err(error)) = checkpoint.as_mut().poll(cx) {
            return Poll::Ready(Err((
                DefinitionErrorKind::Operation(Box::new(error)).into(),
                started,
            )));
        }
        if !started {
            on_start();
            started = true;
        }
        work.as_mut().poll(cx).map(|result| {
            result.map_err(|error| (DefinitionErrorKind::FileSystem(error).into(), true))
        })
    })
    .await
}
