// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatibility host over capability-owned execution; no service logic lives here.

use super::{AccessCommand, AccessKind};
use crate::{
    CommandError, CommandValue, Confirmation, FamilyPresentation, render_family_error,
    render_family_output,
};
pub use fabric_cli_access::host::AccessTargets;
pub use fabric_cli_api::ApiClients;
use fabric_cli_output::{
    Timestamp,
    text::{CharacterWidth, OutputFormat, TextError, TextPlan},
};
use fabric_sdk_api::{Limits, RequestBounds};
use std::{future::Future, io::Write, path::Path};

/// Compatibility local execution; remote leaves do not construct any host.
pub fn execute_local(
    command: &AccessCommand,
    definitions: Option<&Path>,
) -> Option<Result<CommandValue, CommandError>> {
    fabric_cli_access::host::execute_local(&command.access()?, definitions)
}

/// Existing borrowed host layout, retained for downstream struct literals.
pub struct AccessHost<'a> {
    /// Explicit caller-bound SDK sessions.
    pub clients: ApiClients<'a>,
    /// Command-local resolver.
    pub targets: &'a dyn AccessTargets,
    /// Caller-selected local label definitions.
    pub label_definitions: Option<&'a Path>,
    /// Caller-owned confirmation; absence never means consent.
    pub confirmation: Option<&'a mut dyn Confirmation>,
    /// Explicit validated multipart boundary.
    pub multipart_boundary: &'a str,
    /// Existing collection and response bounds.
    pub limits: Limits,
    /// Existing request allocation bounds.
    pub request_bounds: RequestBounds,
}

impl AccessHost<'_> {
    /// Retains the source API contract, independent of output selection.
    pub async fn execute(
        &mut self,
        command: &AccessCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        self.execute_for_output(command, OutputFormat::Json, cancelled)
            .await
    }

    /// Routes to the owning capability, preserving the old host entry point.
    pub async fn execute_for_output(
        &mut self,
        command: &AccessCommand,
        format: OutputFormat,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        if command.skill.is_some() {
            return Err(CommandError::new(
                "NotSupported",
                "Skill headers require a configured shared host binding",
            ));
        }
        match &command.kind {
            AccessKind::Api(options) => {
                self.clients
                    .execute(
                        options,
                        self.multipart_boundary,
                        self.limits,
                        self.request_bounds,
                        cancelled,
                    )
                    .await
            }
            AccessKind::Find(options) => {
                fabric_cli_catalog::execute(self.clients.fabric, options, self.limits, cancelled)
                    .await
            }
            _ => {
                fabric_cli_access::host::AccessHost {
                    client: self.clients.fabric,
                    targets: self.targets,
                    label_definitions: self.label_definitions,
                    confirmation: self
                        .confirmation
                        .as_deref_mut()
                        .map(|value| &mut *value as &mut dyn Confirmation),
                    limits: self.limits,
                }
                .execute_for_output(
                    &command.access().expect("access variant"),
                    format,
                    cancelled,
                )
                .await
            }
        }
    }
}

/// Shared-renderer result and explicit logger observations.
pub struct AccessOutput {
    /// Existing ordered stdout/stderr records and source exit code.
    pub plan: TextPlan,
    /// Host logger warnings, not silently redirected into JSON.
    pub warnings: Vec<String>,
}

impl AccessOutput {
    /// Writes caller-owned streams; a failed write is never a successful exit.
    pub fn write_to(
        &self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> std::io::Result<u8> {
        self.plan.write_to(stdout, stderr)?;
        Ok(self.plan.exit_code)
    }
}

/// Preserves the existing renderer and command-specific presentation selection.
pub fn render(
    command: &AccessCommand,
    result: Result<CommandValue, CommandError>,
    timestamp: Timestamp,
    presentation: &FamilyPresentation<'_>,
    widths: &impl CharacterWidth,
) -> Result<AccessOutput, TextError> {
    let mut presentation = presentation.clone();
    presentation.surface = command.surface();
    if let Some(format) = command.output_format {
        presentation.format = format;
    }
    let (plan, warnings) = match result {
        Ok(value) => (
            render_family_output(
                &value,
                command.command_name(),
                timestamp,
                &presentation,
                widths,
            )?,
            value.warnings,
        ),
        Err(error) => (
            render_family_error(&error, command.command_name(), timestamp, &presentation)?,
            error.warnings,
        ),
    };
    Ok(AccessOutput { plan, warnings })
}
