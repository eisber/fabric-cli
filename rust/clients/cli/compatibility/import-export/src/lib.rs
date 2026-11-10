// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatibility composition for the original `fabric-cli-transfer` API.
//!
//! Import and export algorithms live in their focused command owners.

mod parser;

pub use fabric_cli_import_export_contracts::{
    ImportCacheEffect, LocalTransferFiles, TransferCommand, TransferContext, TransferEvent,
    TransferFailure, TransferFiles, TransferFuture, TransferItem, TransferKind, TransferMetadata,
    TransferResult, TransferUi, process_tokens,
};
pub use parser::{command_from_matches, transfer_subcommands};

use fabric_cli_import_export_contracts::DefinitionCommandContext;
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, OutputStatus, Timestamp,
    text::{self, CharacterWidth, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_deploy::definitions::{DefinitionError, DefinitionErrorKind, DefinitionLimits};
use fabric_sdk::Client;
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use std::{
    fmt,
    future::Future,
    io::{self, Write},
};

/// Failure retaining earlier file and cache effects; no automatic replay is safe.
#[derive(Debug)]
pub struct TransferError {
    /// Original failure, including operation ID and dispatch uncertainty.
    pub failure: TransferFailure,
    /// Earlier completed files, plus current library progress when available.
    pub completed_paths: Vec<String>,
    /// Earlier workspace item exports that completed before this failure.
    pub exported_ids: Vec<String>,
    /// Earlier confirmed cache effects.
    pub effects: Vec<ImportCacheEffect>,
}

impl From<fabric_cli_import_export_contracts::TransferError> for TransferError {
    fn from(error: fabric_cli_import_export_contracts::TransferError) -> Self {
        Self {
            failure: error.failure,
            completed_paths: error.completed_paths,
            exported_ids: error.exported_ids,
            effects: error.effects,
        }
    }
}

impl From<CommandError> for TransferError {
    fn from(error: CommandError) -> Self {
        fabric_cli_import_export_contracts::TransferError::from(error).into()
    }
}

impl From<DefinitionError> for TransferError {
    fn from(error: DefinitionError) -> Self {
        fabric_cli_import_export_contracts::TransferError::from(error).into()
    }
}

impl TransferError {
    /// Shared source envelope, without discarding reconciliation evidence.
    pub fn envelope(&self, timestamp: Timestamp, command: Option<String>) -> OutputEnvelope {
        let mut output = match &self.failure {
            TransferFailure::Command(error) => return error.envelope(timestamp, command),
            TransferFailure::Definition(error) => {
                if let DefinitionErrorKind::Operation(operation) = &error.kind {
                    fabric_cli_mutations::operation_error_output(operation)
                } else {
                    let (code, message) = match &error.kind {
                        DefinitionErrorKind::InvalidInput => {
                            ("InvalidInput", "Invalid transfer input".into())
                        }
                        DefinitionErrorKind::InvalidDefinition => (
                            "InvalidDefinitionPayload",
                            "Invalid item definition payload".into(),
                        ),
                        DefinitionErrorKind::Limit => {
                            ("Limit", "Definition transfer limit reached".into())
                        }
                        DefinitionErrorKind::UnsafePath | DefinitionErrorKind::Collision => (
                            "InvalidPath",
                            "Unsafe or conflicting definition path".into(),
                        ),
                        DefinitionErrorKind::ConfirmationRequired => {
                            ("ConfirmationRequired", "Confirmation is required".into())
                        }
                        DefinitionErrorKind::DefinitionAccessDenied(_) => {
                            ("Unauthorized", "Item definition access denied".into())
                        }
                        DefinitionErrorKind::Unsupported(branch) => {
                            ("NotSupported", format!("Unported source branch: {branch}"))
                        }
                        DefinitionErrorKind::FileSystem(error) => (
                            if error.kind() == io::ErrorKind::Unsupported {
                                "NotSupported"
                            } else {
                                "InvalidPath"
                            },
                            error.to_string(),
                        ),
                        DefinitionErrorKind::Operation(_) => unreachable!(),
                    };
                    OutputInput {
                        status: OutputStatus::Failure,
                        error_code: Some(code.into()),
                        message: Some(message),
                        ..OutputInput::default()
                    }
                }
            }
        };
        output.command = command;
        OutputEnvelope::new(timestamp, output)
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transfer command failed; inspect original operation and file evidence")
    }
}

impl std::error::Error for TransferError {}

/// Runtime-neutral family host; every external capability is caller-injected.
pub struct TransferHost<'a, E: ?Sized> {
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

impl<E: OperationExecutor + ?Sized> TransferHost<'_, E> {
    /// Delegate the original family API without changing injected policy or effects.
    pub async fn execute(
        &mut self,
        command: &TransferCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TransferResult, TransferError> {
        let mut host = DefinitionCommandContext {
            client: self.client,
            executor: self.executor,
            metadata: self.metadata,
            files: self.files,
            ui: self.ui,
            definition_limits: self.definition_limits,
            operation_limits: self.operation_limits,
            command_line: self.command_line,
        };
        match command.kind {
            TransferKind::Import => fabric_cli_import::execute(&mut host, command, cancelled).await,
            TransferKind::Export | TransferKind::BulkExport | TransferKind::Deploy => {
                fabric_cli_export::execute(&mut host, command, cancelled).await
            }
        }
        .map_err(TransferError::from)
    }
}

/// Render actual outputs using the existing shared JSON/text implementation.
///
/// Immediate UI events have already been emitted. Cache effects remain the
/// caller's responsibility after successful delivery, as in the source handler.
/// A writer failure retains the result for evidence but does not replay mutation.
pub fn render_result(
    result: &TransferResult,
    command: &TransferCommand,
    timestamp: Timestamp,
    format: OutputFormat,
    presentation: Presentation,
    widths: &impl CharacterWidth,
) -> Result<TextPlan, TextError> {
    let format = selected_format(command, format)?;
    let mut plan = TextPlan {
        records: Vec::new(),
        exit_code: 0,
    };
    for output in &result.outputs {
        let mut output = output.clone();
        output.command = Some(command.kind.as_str().into());
        let rendered = text::render(
            &OutputEnvelope::new(timestamp.clone(), output),
            format,
            &TextOptions {
                key_value: result.key_value,
                ..TextOptions::default()
            },
            widths,
            presentation,
        )?;
        plan.records.extend(rendered.records);
        plan.exit_code = rendered.exit_code;
    }
    Ok(plan)
}

fn selected_format(
    command: &TransferCommand,
    config: OutputFormat,
) -> Result<OutputFormat, TextError> {
    OutputFormat::resolve(
        command.output_format.as_deref(),
        Some(match config {
            OutputFormat::Json => "json",
            OutputFormat::Text => "text",
        }),
    )
}

/// Render failures with the existing JSON/text stream and exit-code policy.
///
/// The returned plan does not consume original operation IDs, partial file paths,
/// cache effects or dispatch uncertainty retained by the error.
pub fn render_error(
    error: &TransferError,
    command: &TransferCommand,
    timestamp: Timestamp,
    format: OutputFormat,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    let envelope = error.envelope(timestamp, Some(command.kind.as_str().into()));
    match selected_format(command, format)? {
        OutputFormat::Json => {
            let record = envelope.render(fabric_cli_output::Indent::Four)?;
            Ok(TextPlan {
                exit_code: record.exit_code,
                records: vec![record],
            })
        }
        OutputFormat::Text => Ok(text::render_error(
            &CommandError::new(
                envelope.result().message().map(str::to_owned),
                envelope.result().error_code().map(str::to_owned),
            ),
            Some(command.kind.as_str()),
            presentation,
        )),
    }
}

/// Write a rendered result to caller-owned streams; partial writes propagate.
pub fn write_result(
    plan: &TextPlan,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    plan.write_to(stdout, stderr)?;
    Ok(plan.exit_code)
}
