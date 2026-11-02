// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{TransferError, TransferEvent, TransferFailure, TransferUi, command_error};
use fabric_deploy::definitions::{DefinitionError, DefinitionErrorKind, PreparedImport};
use fabric_sdk_operations::{OperationErrorKind, OperationExecutor, OperationSession};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::Duration,
};

pub(crate) fn source_definition_error(error: TransferError) -> TransferError {
    if matches!(
        &error.failure,
        TransferFailure::Definition(error)
            if matches!(error.kind, DefinitionErrorKind::Unsupported(
                "Report byPath; switch to byConnection"
            ))
    ) {
        return command_error(
            "InvalidDefinitionPayload",
            "Definition includes byPath; switch to byConnection before importing",
        )
        .into();
    }
    error
}

fn operation_error<E: OperationExecutor>(
    session: &OperationSession<'_, E>,
    kind: OperationErrorKind,
) -> TransferError {
    DefinitionError::from(DefinitionErrorKind::Operation(Box::new(
        session.evidence(kind),
    )))
    .into()
}

pub(crate) async fn ready<E: OperationExecutor, W: OperationExecutor + ?Sized>(
    session: &mut OperationSession<'_, E>,
    executor: &W,
    workspace: &str,
    item: &str,
    ui: &dyn TransferUi,
    max_delay: Duration,
) -> Result<(), TransferError> {
    wait_state(session, executor, workspace, item, true, ui, max_delay).await
}

pub(crate) async fn publish<E: OperationExecutor, W: OperationExecutor + ?Sized>(
    session: &mut OperationSession<'_, E>,
    executor: &W,
    workspace: &str,
    item: &str,
    ui: &dyn TransferUi,
    max_delay: Duration,
) -> Result<(), TransferError> {
    PreparedImport::trigger_environment_publish(session, workspace, item).await?;
    ui.emit(TransferEvent::Grey("Environment publish triggered".into()))?;
    wait_state(session, executor, workspace, item, false, ui, max_delay).await?;
    ui.emit(TransferEvent::Grey("Environment publish completed".into()))?;
    Ok(())
}

async fn wait_state<E: OperationExecutor, W: OperationExecutor + ?Sized>(
    session: &mut OperationSession<'_, E>,
    executor: &W,
    workspace: &str,
    item: &str,
    readiness: bool,
    ui: &dyn TransferUi,
    max_delay: Duration,
) -> Result<(), TransferError> {
    for attempt in 1..=60 {
        let state = PreparedImport::environment_publish_state(session, workspace, item).await?;
        if state == "success" || readiness && matches!(state.as_str(), "failed" | "cancelled") {
            return Ok(());
        }
        if !readiness && matches!(state.as_str(), "failed" | "cancelled") {
            return Err(operation_error(
                session,
                OperationErrorKind::OperationFailed {
                    status: state,
                    error: None,
                },
            ));
        }
        if attempt == 60 {
            return Err(operation_error(session, OperationErrorKind::Limit));
        }
        let delay = 60.min(5 * 2_u64.pow(attempt.min(4)));
        if Duration::from_secs(delay) > max_delay {
            return Err(operation_error(session, OperationErrorKind::Limit));
        }
        let prefix = if readiness {
            "Existing Environment publish is in progress"
        } else {
            "Environment publish in progress"
        };
        ui.emit(TransferEvent::Grey(format!(
            "{prefix} Checking again in {delay} seconds (Attempt {attempt}/60)..."
        )))?;
        wait(session, executor, Duration::from_secs(delay)).await?;
    }
    unreachable!()
}

async fn wait<E: OperationExecutor, W: OperationExecutor + ?Sized>(
    session: &mut OperationSession<'_, E>,
    executor: &W,
    delay: Duration,
) -> Result<(), TransferError> {
    let mut waiting = pin!(executor.wait(delay));
    poll_fn(|cx| {
        {
            let cancellation = session.check_cancelled();
            let mut cancellation = pin!(cancellation);
            match cancellation.as_mut().poll(cx) {
                Poll::Ready(Err(error)) => {
                    return Poll::Ready(Err(DefinitionError::from(
                        DefinitionErrorKind::Operation(Box::new(error)),
                    )
                    .into()));
                }
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(())) => {}
            }
        }
        waiting.as_mut().poll(cx).map(|result| {
            result.map_err(|error| operation_error(session, OperationErrorKind::Execution(error)))
        })
    })
    .await
}
