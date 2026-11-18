// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw API arguments, validation, execution, and response projection.

mod args;
mod metadata;
mod request;
mod response;

pub use args::{command, parse_failure_message};
pub use request::parse_headers;

use fabric_cli_command_contracts::{
    CommandDiagnostics, CommandError, CommandOutput, Host, ValidatedInvocation, selected,
};
use fabric_core::{
    CancellationToken, Clock, CoreError, HttpExecutor, OperationContext, RecoveryInfo,
    diagnostics::{DiagnosticObserver, DiagnosticStage},
};
use fabric_sdk_api::PollObserver;
use fabric_sdk_http::HttpClient;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

/// Reporting effects needed by raw API execution. The caller owns the console.
pub trait ApiReporting: CommandDiagnostics + DiagnosticObserver + PollObserver {
    /// Set the clock that drives the console polling heartbeat.
    fn set_poll_clock(&self, clock: Arc<dyn Clock>) -> Result<(), CoreError>;
    /// Whether HTTP diagnostics should be enabled.
    fn diagnostics_enabled(&self) -> bool;
    /// Record the current safe recovery evidence, if any.
    fn set_recovery(&self, recovery: Option<RecoveryInfo>);
    /// Read the latest recovery evidence for a failed command.
    fn recovery(&self) -> Option<RecoveryInfo>;
    /// Wait for a progress output failure and return its error.
    fn progress_failure(&self) -> impl Future<Output = CoreError> + Send;
    /// Emit the final progress update.
    fn finish_progress(&self) -> impl Future<Output = Result<(), CoreError>> + Send;
}

/// Validate and prepare raw API inputs, then execute with injected effects.
pub async fn execute(
    invocation: &ValidatedInvocation,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<impl ApiReporting + 'static>,
) -> Result<CommandOutput, CommandError> {
    let api = invocation.command("api")?;
    // Validate local inputs and the endpoint before identity or transport effects.
    let started = observer.start(DiagnosticStage::Prepare).await?;
    let prepared = observer
        .finish(
            DiagnosticStage::Prepare,
            started,
            request::prepare(api, host),
        )
        .await?;
    let metadata =
        metadata::request_metadata(host, api.get_one::<String>("skill").map(String::as_str))?;
    let started = observer.start(DiagnosticStage::Selection).await?;
    let binding = observer
        .finish(DiagnosticStage::Selection, started, selected(host))
        .await?;
    let clock = host.clock();
    observer.set_poll_clock(clock.clone())?;
    let started = observer.start(DiagnosticStage::Authentication).await?;
    let credential = observer
        .finish(
            DiagnosticStage::Authentication,
            started,
            host.credential(binding),
        )
        .await?;
    let mut client = HttpClient::new(credential, host.transport()?, clock.clone())
        .with_request_metadata(metadata)
        .with_endpoint(prepared.endpoint.clone());
    if observer.diagnostics_enabled() {
        client = client.with_diagnostics(observer.clone());
    }
    let context = OperationContext {
        deadline: Some(clock.now() + Duration::from_secs(240)),
        cancellation: cancellation.clone(),
    };
    let initial = client
        .execute(prepared.request, prepared.endpoint.audience(), &context)
        .await?;
    // Capture only safe evidence; raw/async must not reject invalid operation handles.
    let recovery = match fabric_core::operation_handle_with_endpoint(
        &prepared.endpoint,
        initial.status,
        &initial.headers,
    ) {
        Ok(handle) => handle.map(|handle| handle.recovery()),
        Err(error) => error.recovery,
    };
    observer.set_recovery(recovery.clone());
    let attach_recovery = |mut error: CommandError| {
        if error.recovery.is_none() {
            error.recovery = observer
                .recovery()
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok())
                .map(Box::new);
        }
        error
    };
    let response = if prepared.asynchronous {
        initial
    } else {
        response::validate_response_body(&initial.body).map_err(attach_recovery)?;
        let reconciliation = fabric_sdk_api::complete_observed_with_endpoint(
            &client,
            clock.as_ref(),
            &prepared.endpoint,
            initial,
            cancellation,
            prepared.lro_config,
            observer.as_ref(),
        );
        tokio::select! {
            biased;
            error = observer.progress_failure() => {
                return Err(attach_recovery(error.into()));
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
            .finish_progress()
            .await
            .map_err(|error| attach_recovery(error.into()))?;
    }
    Ok(CommandOutput::Api(data))
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
