// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ReadHost, errors, parser::ReadCommand, reads::required_id};
use fabric_cli_context::Element;
use fabric_cli_output::CommandError;
use fabric_sdk::workspaces::{
    WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceMetadataErrorKind,
};
use serde_json::Value;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};

impl ReadHost<'_> {
    pub(crate) async fn workspace_get(
        &self,
        target: &Element,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<Value>, CommandError> {
        let mut cancelled = pin!(cancelled);
        let result = self
            .sdk
            .get_workspace_metadata(
                self.workspaces,
                required_id(target)?,
                WorkspaceListLimits::default(),
                cancelled.as_mut(),
            )
            .await
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
            return Err(errors::error(
                "LongRunningOperationCancelled",
                "Operation cancelled",
            ));
        }
        let print = match &data {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_f64() != Some(0.0),
            Value::String(value) => !value.is_empty(),
            Value::Array(value) => !value.is_empty(),
            Value::Object(value) => !value.is_empty(),
        };
        Ok(print.then_some(data))
    }
}
