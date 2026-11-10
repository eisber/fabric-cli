// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ML endpoint delivery on the caller's existing native identity and HTTP host.

#[cfg(test)]
#[path = "native_ml_endpoint_tests.rs"]
mod tests;

use super::{CommandOutcome, Failure, get};
use fabric_cli::ml_endpoint::EndpointCommand;
use fabric_cli_output::{OutputStream, RenderPlan, text::TextPlan};
use fabric_sdk::Client;
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_operations::{OperationLimits, OperationSession};
use serde_json::json;
use std::{
    future::Future,
    sync::atomic::{AtomicU8, Ordering},
    time::Instant,
};

pub(super) async fn before_connect<T>(
    work: impl Future<Output = Result<T, Failure>>,
    deadline: Instant,
) -> Result<T, Failure> {
    if Instant::now() >= deadline {
        return Err(super::failure(
            "timed_out",
            "ML endpoint deadline elapsed before dispatch.",
            1,
        ));
    }
    tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => match signal {
            Ok(()) => Err(super::failure("cancelled", "ML endpoint command cancelled before dispatch.", 2)),
            Err(_) => Err(super::failure("signal_failed", "The cancellation handler could not start.", 1)),
        },
        _ = tokio::time::sleep_until(deadline.into()) =>
            Err(super::failure("timed_out", "ML endpoint deadline elapsed before dispatch.", 1)),
        result = work => result,
    }
}

pub(super) async fn dispatch(
    command: &EndpointCommand,
    sdk: &Client,
    executor: &HttpExecutor,
    deadline: Instant,
) -> Result<get::Delivery, Failure> {
    let interrupted = AtomicU8::new(0);
    let cancelled = async {
        let reason = tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => if signal.is_ok() { 1 } else { 3 },
            _ = tokio::time::sleep_until(deadline.into()) => 2,
        };
        interrupted.store(reason, Ordering::SeqCst);
    };
    let mut operations = OperationSession::with_context(
        sdk.context(),
        executor.clone(),
        OperationLimits::default(),
        cancelled,
    );
    deliver(command, &mut operations, &interrupted).await
}

async fn deliver(
    command: &EndpointCommand,
    operations: &mut dyn fabric_sdk_workspaces::mutations::ResourceOperations,
    interrupted: &AtomicU8,
) -> Result<get::Delivery, Failure> {
    let (value, exit, stream) = match command.execute(operations).await {
        Ok(value) => (value, 0, OutputStream::Stdout),
        Err(mut error) => {
            match interrupted.load(Ordering::SeqCst) {
                1 => {
                    error.code = "cancelled".into();
                    error.message = "ML endpoint waiting cancelled; remote effects may still complete. No rollback or replay is implied.".into();
                }
                2 => {
                    error.code = "timed_out".into();
                    error.message = "ML endpoint deadline elapsed; remote effects may still complete. No rollback or replay is implied.".into();
                }
                3 => {
                    error.code = "signal_failed".into();
                    error.message = "The cancellation handler could not start.".into();
                }
                _ => {}
            }
            (
                json!({"error": {
                    "code": error.code, "message": error.message,
                    "httpStatus": error.status, "operationId": error.operation_id,
                    "mutationMayHaveOccurred": error.mutation_may_have_occurred,
                    "serviceError": error.raw_service_error
                }}),
                1,
                OutputStream::Stderr,
            )
        }
    };
    let mut payload = serde_json::to_vec(&value)
        .map_err(|_| super::failure("render_failed", "ML endpoint output encoding failed.", 1))?;
    payload.push(b'\n');
    let plan = TextPlan {
        records: vec![RenderPlan {
            stream,
            exit_code: exit,
            payload,
        }],
        exit_code: exit,
    };
    let outcome = match interrupted.load(Ordering::SeqCst) {
        1 => CommandOutcome::Cancelled(exit),
        2 | 3 => CommandOutcome::Stop(exit),
        _ => CommandOutcome::Returned(exit),
    };
    Ok(get::Delivery::plan(plan, outcome))
}
