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
use fabric_sdk_operations::{
    OperationError, OperationErrorKind, OperationLimits, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceError;
use serde_json::{Value, json};
use std::{
    future::Future,
    io::{self, Write},
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
    let result = command.execute(&mut operations).await;
    let evidence = operations.evidence(OperationErrorKind::InvalidResponse);
    deliver(result, evidence, &interrupted)
}

fn deliver(
    result: Result<Value, ResourceError>,
    evidence: OperationError,
    interrupted: &AtomicU8,
) -> Result<get::Delivery, Failure> {
    let (value, exit, stream) = match result {
        Ok(mut value) => {
            if let Some(operation_id) = &evidence.operation_id {
                value["operationId"] = json!(operation_id);
            }
            value["mutationMayHaveOccurred"] = json!(evidence.mutation_may_have_occurred);
            (value, 0, OutputStream::Stdout)
        }
        Err(mut error) => {
            if error.operation_id.is_none() {
                error.operation_id = evidence.operation_id;
            }
            error.mutation_may_have_occurred |= evidence.mutation_may_have_occurred;
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

pub(super) async fn publish(
    delivery: get::Delivery,
    deadline: Instant,
) -> Result<get::Delivery, Failure> {
    let get::Delivery::Plan {
        mut plan,
        outcome,
        export: None,
    } = delivery
    else {
        return Err(super::failure(
            "output_failed",
            "Invalid ML endpoint delivery.",
            1,
        ));
    };
    let record = plan
        .records
        .pop()
        .filter(|_| plan.records.is_empty())
        .ok_or_else(|| super::failure("output_failed", "Invalid ML endpoint output records.", 1))?;
    let value: Value = serde_json::from_slice(&record.payload)
        .map_err(|_| super::failure("render_failed", "Invalid ML endpoint output envelope.", 1))?;
    // ASCII JSON is safe through an owned Windows console handle as well as pipes.
    let mut payload = fabric_cli_output::json_dumps(&value)
        .map_err(|_| super::failure("render_failed", "ML endpoint JSON encoding failed.", 1))?
        .into_bytes();
    payload.push(b'\n');
    let result = match super::output::owned_stream(record.stream, true) {
        Ok(writer) => {
            publish_to(writer, payload, outcome, deadline, async {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => {
                        super::failure("cancelled", "ML endpoint output waiting cancelled.", 2)
                    }
                    Err(_) => super::failure(
                        "signal_failed",
                        "The output cancellation handler failed.",
                        1,
                    ),
                }
            })
            .await
        }
        Err(_) => Err(super::failure(
            "output_failed",
            "The ML endpoint output handle could not be retained.",
            1,
        )),
    };
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(failure) => {
            let diagnostic = output_failure(&value, failure);
            // This is bounded error cleanup, not a new remote-operation deadline.
            if let Ok(writer) = super::output::owned_stream(OutputStream::Stderr, true) {
                let mut bytes = fabric_cli_output::json_dumps(&diagnostic)
                    .map_err(|_| {
                        super::failure("render_failed", "Output diagnostic encoding failed.", 1)
                    })?
                    .into_bytes();
                bytes.push(b'\n');
                let _cleanup_result = publish_to(
                    writer,
                    bytes,
                    CommandOutcome::Stop(1),
                    Instant::now() + std::time::Duration::from_secs(1),
                    std::future::pending(),
                )
                .await;
            }
            CommandOutcome::Stop(1)
        }
    };
    Ok(get::Delivery::plan(
        TextPlan {
            records: Vec::new(),
            exit_code: 0,
        },
        outcome,
    ))
}

async fn publish_to(
    writer: impl Write + Send + 'static,
    payload: Vec<u8>,
    outcome: CommandOutcome,
    deadline: Instant,
    cancelled: impl Future<Output = Failure>,
) -> Result<CommandOutcome, Failure> {
    use super::output::{OwnedWrite, write_owned};
    match write_owned(writer, deadline, cancelled, move |writer| {
        writer.write_all(&payload)?;
        writer.flush()?;
        Ok(outcome)
    })
    .await
    {
        OwnedWrite::Complete(Ok(outcome)) => Ok(outcome),
        OwnedWrite::Complete(Err(_)) => Err(super::failure(
            "output_failed",
            "Writing or flushing ML endpoint output failed.",
            1,
        )),
        OwnedWrite::Deadline => Err(super::failure(
            "timed_out",
            "The ML endpoint deadline elapsed during output.",
            1,
        )),
        OwnedWrite::Cancelled(failure) => Err(failure),
        OwnedWrite::WorkerFailed => Err(super::failure(
            "output_failed",
            "The ML endpoint output worker did not complete.",
            1,
        )),
    }
}

fn output_failure(response: &Value, (code, message, _): Failure) -> Value {
    json!({"error": {
        "code": code,
        "message": format!("{message} Output may be partial; an in-flight write may still complete. The host must stop. No remote replay or rollback is implied."),
        "operationId": response.get("operationId").or_else(|| response.pointer("/error/operationId")),
        "mutationMayHaveOccurred": response.get("mutationMayHaveOccurred").or_else(|| response.pointer("/error/mutationMayHaveOccurred")),
    }, "response": response})
}
