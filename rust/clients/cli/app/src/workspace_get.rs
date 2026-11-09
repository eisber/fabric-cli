// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ReadHost, errors, parser::ReadCommand, reads::required_id, session::ReadFailure};
use fabric_cli_context::Element;
use fabric_cli_output::{CommandError, OutputInput};
use fabric_sdk::workspaces::{
    WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceMetadata, WorkspaceMetadataErrorKind,
};
use fabric_sdk_operations::{
    FabricOperation, OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use serde_json::Value;
use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

struct BorrowedOperations<'a>(&'a dyn OperationExecutor);

impl OperationExecutor for BorrowedOperations<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.execute(request)
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.0.wait(delay)
    }
}

impl ReadHost<'_> {
    pub(crate) async fn workspace_get(
        &self,
        target: &Element,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<OutputInput>, CommandError> {
        match self
            .workspace_get_with_evidence(target, command, operations, cancelled)
            .await
        {
            Ok(output) => Ok(output),
            Err(failure) => {
                if let Some(mut output) = failure.operation_output {
                    output.command = Some(command.name.clone());
                    Ok(Some(*output))
                } else {
                    Err(failure.command)
                }
            }
        }
    }

    pub(crate) async fn workspace_get_with_evidence(
        &self,
        target: &Element,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<OutputInput>, ReadFailure> {
        let mut cancelled = pin!(cancelled);
        let workspace = required_id(target)?;
        let result = if let Some(operations) = operations {
            if workspace.contains(['?', '#']) {
                return Err(
                    errors::error("InvalidInput", "Invalid workspace request identifier").into(),
                );
            }
            let operation = ResourceOperation::encoded(
                ResourceMethod::Get,
                &["workspaces", workspace],
                None,
                CompletionPolicy::PythonSynchronous,
            )
            .map_err(|_| errors::error("InvalidInput", "Invalid workspace request identifier"))?;
            let operation = FabricOperation::from_resource(&operation)
                .and_then(FabricOperation::with_source_read_operation_ids)
                .map_err(|_| {
                    errors::error("InvalidInput", "Invalid workspace request identifier")
                })?;
            let mut session = OperationSession::with_context(
                self.sdk.context(),
                BorrowedOperations(operations),
                OperationLimits::default(),
                cancelled.as_mut(),
            );
            let response = session
                .execute(operation)
                .await
                .map_err(ReadFailure::from)?;
            // The source handler ignores accepted and other non-200 primary replies.
            if response.status != 200 {
                return Ok(None);
            }
            let metadata: WorkspaceMetadata =
                serde_json::from_slice(&response.body).map_err(|_| {
                    errors::error(
                        "InvalidResponse",
                        "Fabric returned invalid workspace metadata",
                    )
                })?;
            session
                .check_cancelled()
                .await
                .map_err(|operation| ReadFailure {
                    command: errors::error("LongRunningOperationCancelled", "Operation cancelled"),
                    operation: Some(Box::new(operation)),
                    operation_output: None,
                })?;
            drop(session);
            self.sdk
                .enrich_workspace_metadata(
                    self.workspaces,
                    workspace,
                    metadata,
                    WorkspaceListLimits::default(),
                    cancelled.as_mut(),
                )
                .await
        } else {
            self.sdk
                .get_workspace_metadata(
                    self.workspaces,
                    workspace,
                    WorkspaceListLimits::default(),
                    cancelled.as_mut(),
                )
                .await
        }
        .map_err(|error| match &error.kind {
            WorkspaceMetadataErrorKind::InvalidIdentifier => {
                errors::error("InvalidInput", "Invalid workspace request identifier")
            }
            WorkspaceMetadataErrorKind::Read(WorkspaceListErrorKind::InvalidResponse) => {
                errors::error(
                    "InvalidResponse",
                    "Fabric returned invalid workspace metadata",
                )
            }
            WorkspaceMetadataErrorKind::Read(
                kind @ (WorkspaceListErrorKind::Http { .. }
                | WorkspaceListErrorKind::Credential
                | WorkspaceListErrorKind::Cancelled
                | WorkspaceListErrorKind::Execution(
                    fabric_sdk::workspaces::WorkspaceExecutionError::Cancelled,
                )),
            ) => errors::workspace(kind),
            WorkspaceMetadataErrorKind::Read(_) => errors::error(
                "ReadFailed",
                "Workspace metadata operation did not complete",
            ),
        })?;
        let data = self.projection.project(
            result.metadata.into_value(),
            command.query.as_deref().filter(|query| !query.is_empty()),
            false,
        )?;
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return Err(
                errors::error("LongRunningOperationCancelled", "Operation cancelled").into(),
            );
        }
        let print = match &data {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_f64() != Some(0.0),
            Value::String(value) => !value.is_empty(),
            Value::Array(value) => !value.is_empty(),
            Value::Object(value) => !value.is_empty(),
        };
        Ok(print.then(|| OutputInput {
            command: Some(command.name.clone()),
            data: Some(data),
            ..OutputInput::default()
        }))
    }
}
