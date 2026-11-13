// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Livy list leaf; identity, HTTP, deadline and delivery remain shared.

use super::{NativeSession, capture_fabric_selection};
use fabric_cli::runtime_v2::{
    ReadErrorKind, ReadOptions,
    host::{self, HostError},
    livy_list::{self, PreparedList},
};
use fabric_cli_output::OutputStream;
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use fabric_sdk_http::ExecutionOptions;
use serde_json::json;
use std::{
    ffi::OsString,
    future::Future,
    time::{Duration, Instant},
};

enum Failure {
    Host(HostError),
    Local(&'static str, &'static str, u8),
}

impl From<HostError> for Failure {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}

impl From<fabric_cli::runtime_v2::ReadError> for Failure {
    fn from(error: fabric_cli::runtime_v2::ReadError) -> Self {
        Self::Host(error.into())
    }
}

pub(super) fn run(args: Vec<OsString>, root_help: bool) -> u8 {
    let deadline = Instant::now() + ExecutionOptions::default().timeout;
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
    else {
        // No synchronous stream fallback: runtime failure cannot block the process.
        return 1;
    };
    let exit = runtime.block_on(async {
        match execute(args, root_help, deadline).await {
            Ok(()) => 0,
            Err(Failure::Host(error)) => {
                let exit = if matches!(
                    error.kind,
                    ReadErrorKind::InvalidInput
                        | ReadErrorKind::UnsupportedCapability
                        | ReadErrorKind::WrongOwner
                ) {
                    2
                } else {
                    1
                };
                let _ = host::diagnostic(&error, deadline).await;
                exit
            }
            Err(Failure::Local(code, message, exit)) => {
                let bytes = json!({"error":{"code":code,"message":message},
                    "remoteCancelled":false,"remoteReplayed":false})
                .to_string()
                .into_bytes();
                let _ = host::write_stream(OutputStream::Stderr, bytes, deadline).await;
                exit
            }
        }
    });
    runtime.shutdown_timeout(
        deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(1)),
    );
    exit
}

async fn execute(args: Vec<OsString>, root_help: bool, deadline: Instant) -> Result<(), Failure> {
    if root_help {
        let text = format!(
            "{}\n\nOpt-in runtime list: fab-cli-v2 list <GUID.Workspace/GUID.Lakehouse/sessions> [OPTIONS]\n\
             Run list --help for the experimental one-page Livy read contract.",
            super::ROOT_HELP
        );
        #[cfg(feature = "v2-create")]
        let text = format!("{text}\n\n{}", super::CREATE_HELP);
        #[cfg(feature = "v2-kql-database-create")]
        let text = format!("{text}\n\n{}", super::KQL_CREATE_HELP);
        return host::write_stream(OutputStream::Stdout, text.into_bytes(), deadline)
            .await
            .map_err(|_| Failure::Local("OutputFailed", "Help output failed.", 1));
    }
    if livy_list::help_requested(&args) {
        let mut bytes = Vec::new();
        livy_list::command()
            .write_long_help(&mut bytes)
            .map_err(|_| Failure::Local("OutputFailed", "Livy help rendering failed.", 1))?;
        return host::write_stream(OutputStream::Stdout, bytes, deadline)
            .await
            .map_err(|_| Failure::Local("OutputFailed", "Livy help output failed.", 1));
    }
    let options = ReadOptions::default();
    let prepared = PreparedList::parse(
        std::iter::once(OsString::from("fab-cli-v2")).chain(args),
        options,
    )?;
    if prepared.invocation().path.owner.kind == fabric_cli::runtime_v2::OwnerKind::LivyService {
        return Err(Failure::Local(
            "unsupported_capability",
            "Native Livy authorization is not configured; no Fabric profile fallback is used.",
            2,
        ));
    }
    let host = std::env::var("FAB_RUST_FABRIC_HOST").map_err(|_| {
        Failure::Local(
            "NotConfigured",
            "Livy list requires an explicit FAB_RUST_FABRIC_HOST and GUID-typed Lakehouse path; native Livy authorization is not configured.",
            1,
        )
    })?;
    let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, &host).map_err(|_| {
        Failure::Local(
            "InvalidConfiguration",
            "Invalid explicit Fabric endpoint.",
            2,
        )
    })?;
    // Validate typed GUID ownership before output preflight or identity setup.
    prepared.bind_fabric(&endpoint)?;
    let cwd = std::env::current_dir().map_err(|_| {
        Failure::Local(
            "InvalidWorkingDirectory",
            "The output directory is unavailable.",
            2,
        )
    })?;
    let cancelled = async {
        // A failed signal listener also stops observation, rather than losing cancellation.
        let _ = tokio::signal::ctrl_c().await;
    };
    tokio::pin!(cancelled);
    let destination = host::prepare_destination(
        prepared.invocation().output.clone(),
        cwd,
        deadline,
        &mut cancelled,
    )
    .await?;
    let session =
        setup_session(endpoint, deadline, &mut cancelled, capture_fabric_selection).await?;
    prepared
        .fabric(session.context, (*session.executor).clone())?
        .acquire(&destination, deadline, &mut cancelled)
        .await?
        .deliver(destination, deadline, &mut cancelled)
        .await?;
    Ok(())
}

async fn setup_session(
    expected_endpoint: EndpointBinding,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
    capture: impl FnOnce() -> Result<(super::headless::Selection, EndpointBinding), super::Failure>
    + Send
    + 'static,
) -> Result<NativeSession, Failure> {
    host::setup(deadline, cancelled, move || {
        let (selection, endpoint) = capture()?;
        if endpoint != expected_endpoint {
            return Err(super::failed(
                "InvalidConfiguration",
                "The explicit Fabric endpoint changed during setup.",
                2,
            ));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| super::failed("TimedOut", "The original Livy deadline elapsed.", 1))?;
        NativeSession::new(selection, endpoint, remaining)
    })
    .await
    .map_err(|error| match error {
        host::SetupError::Host(error) => Failure::Host(error),
        host::SetupError::Work((code, _, _, exit)) => {
            Failure::Local(code, "Explicit native session setup failed.", exit)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::pending, sync::mpsc};

    #[tokio::test]
    async fn stalled_identity_setup_obeys_original_deadline_and_latched_cancellation() {
        for cancel in [false, true] {
            let (release, stalled) = mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let deadline = Instant::now() + Duration::from_millis(100);
            let cancelled = async {
                if cancel {
                    ready.await.unwrap();
                } else {
                    pending::<()>().await;
                }
            };
            let result = setup_session(
                EndpointBinding::public(ServiceOrigin::Fabric),
                deadline,
                cancelled,
                move || {
                    let _ = started.send(());
                    let _ = stalled.recv_timeout(Duration::from_secs(2));
                    Err(super::super::failed("Private", "private-setup-error", 2))
                },
            )
            .await;
            release.send(()).unwrap();
            match result {
                Err(Failure::Host(error)) => assert_eq!(
                    error.kind,
                    if cancel {
                        ReadErrorKind::Cancelled
                    } else {
                        ReadErrorKind::Timeout
                    }
                ),
                _ => panic!("stalled identity setup escaped the shared interruption boundary"),
            }
        }
    }

    #[tokio::test]
    async fn setup_preserves_configuration_error_without_echoing_private_details() {
        let result = setup_session(
            EndpointBinding::public(ServiceOrigin::Fabric),
            Instant::now() + Duration::from_secs(1),
            pending(),
            || {
                Err(super::super::failed(
                    "InvalidConfiguration",
                    "private-config",
                    2,
                ))
            },
        )
        .await;
        match result {
            Err(Failure::Local(code, message, exit)) => {
                assert_eq!(code, "InvalidConfiguration");
                assert_eq!(exit, 2);
                assert!(!message.contains("private"));
            }
            _ => panic!("typed setup error was lost"),
        }
    }
}
