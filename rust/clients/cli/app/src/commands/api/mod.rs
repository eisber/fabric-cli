// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod args;
mod request;
mod response;

pub(crate) use args::command;

use super::selected;
use crate::{
    Host,
    progress::{ConsoleProgress, ProgressUpdate, progress_error},
};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    CancellationToken, CoreError, ErrorKind, HttpExecutor, OperationContext,
    diagnostics::DiagnosticStage,
};
use fabric_sdk_http::HttpClient;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub(super) async fn execute(
    api: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
) -> Result<Value, CommandError> {
    // Validate local inputs before identity, configuration, or transport effects.
    let started = observer.start(DiagnosticStage::Prepare).await?;
    let prepared = observer
        .finish(DiagnosticStage::Prepare, started, request::prepare(api))
        .await?;
    let started = observer.start(DiagnosticStage::Selection).await?;
    let binding = observer
        .finish(DiagnosticStage::Selection, started, selected(host))
        .await?;
    let clock = host.clock();
    observer
        .heartbeat_clock
        .set(clock.clone())
        .map_err(|_| CoreError::new(ErrorKind::Protocol, "Progress clock already set"))?;
    let started = observer.start(DiagnosticStage::Authentication).await?;
    let credential = observer
        .finish(
            DiagnosticStage::Authentication,
            started,
            host.credential(binding),
        )
        .await?;
    let mut client = HttpClient::new(credential, host.transport()?, clock.clone());
    if observer.timing.is_some() {
        client = client.with_diagnostics(observer.clone());
    }
    let context = OperationContext {
        deadline: Some(clock.now() + Duration::from_secs(240)),
        cancellation: cancellation.clone(),
    };
    let initial = client
        .execute(prepared.request, prepared.audience, &context)
        .await?;
    // Capture only safe evidence; raw/async must not reject invalid operation handles.
    let recovery =
        match fabric_core::operation_handle(prepared.audience, initial.status, &initial.headers) {
            Ok(handle) => handle.map(|handle| handle.recovery()),
            Err(error) => error.recovery,
        };
    observer
        .reporting
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .recovery = recovery.clone();
    let attach_recovery = |mut error: CommandError| {
        if error.recovery.is_none() {
            error.recovery = observer
                .reporting
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recovery
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok());
        }
        error
    };
    let response = if prepared.asynchronous {
        initial
    } else {
        response::validate_response_body(&initial.body).map_err(attach_recovery)?;
        let reconciliation = fabric_sdk_api::complete_observed(
            &client,
            clock.as_ref(),
            prepared.audience,
            initial,
            cancellation,
            prepared.lro_config,
            observer.as_ref(),
        );
        tokio::select! {
            biased;
            () = observer.failed.cancelled() => {
                return Err(attach_recovery(progress_error().into()));
            }
            result = reconciliation => result?,
        }
    };
    let started = observer.start(DiagnosticStage::Projection).await?;
    let result =
        response::response_data(response, prepared.show_headers, prepared.query.as_deref())
            .map_err(attach_recovery);
    let data = observer
        .finish(DiagnosticStage::Projection, started, result)
        .await?;
    if !prepared.asynchronous && recovery.is_some() {
        observer
            .send(ProgressUpdate::Finished)
            .await
            .map_err(|error| attach_recovery(error.into()))?;
    }
    Ok(data)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new("InvalidInput", message)
}

/// Describe a JSON failure by position only; content is never echoed.
fn json_failure(prefix: &str, bytes: &[u8]) -> String {
    match serde_json::from_slice::<Value>(bytes) {
        Err(error) if error.is_eof() => format!(
            "{prefix}: unexpected end of input at line {}, column {}",
            error.line(),
            error.column()
        ),
        Err(error) if error.is_syntax() => format!(
            "{prefix}: syntax error at line {}, column {}",
            error.line(),
            error.column()
        ),
        _ => format!(
            "{prefix}: nesting exceeds 128 levels or a number is outside the supported range"
        ),
    }
}
