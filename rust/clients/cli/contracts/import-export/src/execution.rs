// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared injected inputs, cancellation and bounded metadata preconditions.

use crate::parser::{command_error, invalid};
use crate::{
    TransferCommand, TransferContext, TransferError, TransferFiles, TransferFuture, TransferKind,
    TransferMetadata, TransferUi,
};
use fabric_cli_context::{Element, NodeKind};
use fabric_cli_output::{CommandError, OutputInput, text::OutputFormat};
use fabric_deploy::definitions::{DefinitionError, DefinitionErrorKind, DefinitionLimits};
use fabric_sdk::Client;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use std::{
    future::{Future, poll_fn},
    io,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::Duration,
};

/// Borrow the caller's existing executor without installing transport or time.
pub struct BorrowedExecutor<'a, E: ?Sized>(
    /// Caller-owned executor.
    pub &'a E,
);

impl<E: OperationExecutor + ?Sized> OperationExecutor for BorrowedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> TransferFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        self.0.execute(request)
    }

    fn wait(&self, delay: Duration) -> TransferFuture<'_, Result<(), OperationExecutionError>> {
        self.0.wait(delay)
    }
}

/// Caller-owned cancellation future shared across command phases.
pub type Cancellation<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Poll cancellation before the next injected phase, retaining its typed failure.
pub async fn phase<T, E: Into<TransferError>>(
    work: impl Future<Output = Result<T, E>>,
    cancelled: &mut Cancellation<'_>,
) -> Result<T, TransferError> {
    let mut work = pin!(work);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(command_error(
                "LongRunningOperationCancelled",
                "Operation cancelled",
            )
            .into()));
        }
        work.as_mut()
            .poll(cx)
            .map(|result| result.map_err(Into::into))
    })
    .await
}

/// Borrowed inputs shared by the independent import and export command owners.
pub struct DefinitionCommandContext<'a, E: ?Sized> {
    /// Existing credential/selection facade; no identity discovery.
    pub client: &'a Client,
    /// Existing shared HTTP adapter and asynchronous time.
    pub executor: &'a E,
    /// Complete asynchronous metadata/context owner.
    pub metadata: &'a dyn TransferMetadata,
    /// Stable caller-owned file policy.
    pub files: &'a mut dyn TransferFiles,
    /// Source confirmation, selection, warning and progress policy.
    pub ui: &'a dyn TransferUi,
    /// Finite library file/path/byte bounds.
    pub definition_limits: DefinitionLimits,
    /// Existing bounded shared operation policy.
    pub operation_limits: OperationLimits,
    /// Source command-line mode rejects interactive workspace selection.
    pub command_line: bool,
}

impl<E: OperationExecutor + ?Sized> DefinitionCommandContext<'_, E> {
    /// Validate source preconditions and resolve only the initial bounded target.
    ///
    /// Collection discovery stays in its import/export-specific source position.
    pub async fn prepare(
        &mut self,
        command: &TransferCommand,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<TransferContext, TransferError> {
        OutputFormat::resolve(command.output_format.as_deref(), Some("text"))
            .map_err(|error| error.command_error())?;
        if command.kind == TransferKind::Deploy {
            return Err(command_error("NotSupported",
                "Config deployment requires the native deployment host, not the definition transfer host").into());
        }
        if command.kind == TransferKind::BulkExport && !command.recursive {
            return Err(command_error(
                "InvalidOperation",
                "The --recursive flag is required when bulk-exporting a Workspace or Folder",
            )
            .into());
        }
        phase(std::future::ready(Ok::<_, CommandError>(())), cancelled).await?;
        let context = phase(
            self.metadata.resolve_target(command.kind, &command.path),
            cancelled,
        )
        .await?;
        self.check_snapshot(&context)?;
        Ok(context)
    }

    /// Reject incomplete or oversized metadata before subsequent command work.
    pub fn check_snapshot(&self, context: &TransferContext) -> Result<(), TransferError> {
        if context.items.len() > self.definition_limits.max_files.get()
            || context.selection_order.len() > self.definition_limits.max_files.get()
        {
            return Err(DefinitionError::from(DefinitionErrorKind::Limit).into());
        }
        Ok(())
    }
}

/// Construct a source-silent-data success message.
pub fn message(text: impl Into<String>) -> OutputInput {
    OutputInput {
        message: Some(text.into()),
        ..OutputInput::default()
    }
}

/// Retain the original filesystem failure as definition evidence.
pub fn local_error(error: io::Error) -> TransferError {
    DefinitionError::from(DefinitionErrorKind::FileSystem(error)).into()
}

/// Require an existing resource ID without discovering metadata.
pub fn require_id(element: &Element) -> Result<&str, TransferError> {
    element
        .id()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("Existing resource ID is required").into())
}

/// Require a Fabric item and its caller-resolved workspace.
pub fn item_parts(item: &Arc<Element>) -> Result<(Arc<Element>, &'static str), TransferError> {
    let NodeKind::Item(item_type) = item.kind() else {
        return Err(command_error("NotSupported", "This transfer requires a Fabric item").into());
    };
    Ok((
        item.workspace()
            .ok_or_else(|| invalid("Workspace metadata is required"))?,
        item_type.as_str(),
    ))
}
