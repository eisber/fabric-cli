// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-injected root Update host, with no-clobber preflight before dispatch.
//!
//! Hosts supply explicit approval, context, execution and total interruption.

pub use fabric_cli_mutations::v2_update::*;
use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile, ExactOutputReceipt};
use fabric_sdk::{
    RequestContext,
    items::update::{self as sdk, contract::ApprovalPolicy},
};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use std::{
    fs,
    future::Future,
    io::{self, Read, Write},
    path::Path,
};

/// Prepared local input/output and the exact SDK request, not remote execution.
#[derive(Debug)]
pub struct PreparedUpdateCommand {
    command: UpdateCommand,
    update: sdk::PreparedUpdate,
    output: Option<ExactOutputFile>,
}

/// Local/host failure preserving any prior remote receipt.
pub enum UpdateCommandError {
    /// Local file/stdin or writer error; no provider error text is synthesized.
    Io(io::Error),
    /// Native no-clobber preflight or commit error.
    File(ExactOutputError),
    /// Local argument/SDK validation failure.
    Invalid(String),
    /// The caller declined or failed immediate approval.
    Approval,
    /// Original typed SDK preparation/dispatch error.
    Sdk(sdk::UpdateError),
    /// Delivery failure after remote execution, retaining outcome and handles.
    Delivery {
        /// Original local writer/file error.
        source: Box<UpdateCommandError>,
        /// Original remote report, including unknown or partial effects.
        report: Box<sdk::UpdateReport>,
    },
}
impl std::fmt::Display for UpdateCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("metadata update failed; inspect retained local and remote evidence")
    }
}
impl std::error::Error for UpdateCommandError {}

impl std::fmt::Debug for UpdateCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => f
                .debug_tuple("UpdateCommandError::Io")
                .field(&error.kind())
                .finish(),
            Self::File(error) => f
                .debug_struct("UpdateCommandError::File")
                .field("kind", &error.kind)
                .field("stage", &error.stage)
                .field("path_created", &error.path_created)
                .field("bytes_written", &error.bytes_written)
                .finish_non_exhaustive(),
            Self::Invalid(_) => f.write_str("UpdateCommandError::Invalid"),
            Self::Approval => f.write_str("UpdateCommandError::Approval"),
            Self::Sdk(error) => f
                .debug_tuple("UpdateCommandError::Sdk")
                .field(error)
                .finish(),
            Self::Delivery { source, report } => f
                .debug_struct("UpdateCommandError::Delivery")
                .field("source", source)
                .field("report", report)
                .finish(),
        }
    }
}

impl PreparedUpdateCommand {
    /// Read bounded input and preflight one exact output in a blocking host.
    ///
    /// The supplied stdin is read only for `--input -`. Files must be regular,
    /// non-symlink entries. The host owns a trusted parent namespace; portable
    /// safe calls cannot pin ancestors against a hostile process replacing them.
    ///
    /// # Errors
    /// Rejects malformed/oversize input and existing output before any credential
    /// or provider work. No directories or output files are created by preflight.
    pub fn preflight(
        command: UpdateCommand,
        cwd: &Path,
        stdin: &mut dyn Read,
    ) -> Result<Self, UpdateCommandError> {
        let mut bytes = Vec::new();
        match &command.input {
            UpdateInput::Stdin => {
                stdin
                    .take((sdk::MAX_INPUT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(UpdateCommandError::Io)?;
            }
            UpdateInput::File(path) => {
                let path = cwd.join(path);
                let metadata = fs::symlink_metadata(&path).map_err(UpdateCommandError::Io)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(UpdateCommandError::Invalid(
                        "Input requires a regular non-symlink file".into(),
                    ));
                }
                fs::File::open(path)
                    .map_err(UpdateCommandError::Io)?
                    .take((sdk::MAX_INPUT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(UpdateCommandError::Io)?;
            }
        }
        let update = command
            .prepare(&bytes, cwd)
            .map_err(|error| UpdateCommandError::Invalid(error.to_string()))?;
        let output = command
            .output
            .as_ref()
            .map(|path| ExactOutputFile::preflight(&cwd.join(path), sdk::MAX_INPUT_BYTES))
            .transpose()
            .map_err(UpdateCommandError::File)?;
        Ok(Self {
            command,
            update,
            output,
        })
    }

    /// Access the exact pure plan for discussion or a policy decision.
    pub fn update(&self) -> &sdk::PreparedUpdate {
        &self.update
    }

    /// Deliver a plan locally with no credentials, approval, or remote request.
    ///
    /// # Errors
    /// Rejects execution intent and preserves no-clobber/write errors.
    pub fn deliver_plan(
        self,
        stdout: &mut dyn Write,
    ) -> Result<Option<ExactOutputReceipt>, UpdateCommandError> {
        if !self.command.plan {
            return Err(UpdateCommandError::Invalid(
                "Plan delivery requires explicit plan intent".into(),
            ));
        }
        let mut bytes = serde_json::to_vec(&self.update.plan_document())
            .map_err(io::Error::other)
            .map_err(UpdateCommandError::Io)?;
        bytes.push(b'\n');
        deliver(self.output, &bytes, stdout)
    }

    /// Bind context and obtain exact host approval, then submit once.
    ///
    /// The caller's interruption must cover preparation time and the remaining
    /// command deadline; this method installs no timer, runtime or policy.
    /// No cache success/rename is applied by this library.
    ///
    /// # Errors
    /// A plan, context failure or declined approval dispatches nothing. Typed
    /// remote failure/partial/unknown outcomes are returned as receipts.
    pub async fn execute<E: OperationExecutor, P: ApprovalPolicy>(
        self,
        context: RequestContext,
        executor: E,
        limits: OperationLimits,
        policy: &mut P,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<UpdateDelivery, UpdateCommandError> {
        self.with_context(context, limits)?
            .execute_bound(executor, policy, interrupted)
            .await
    }

    /// Bind the exact context before an asynchronous host approval interaction.
    ///
    /// # Errors
    /// Rejects plan intent, invalid context and rebinding without dispatch.
    pub fn with_context(
        mut self,
        context: RequestContext,
        limits: OperationLimits,
    ) -> Result<Self, UpdateCommandError> {
        if self.command.plan {
            return Err(UpdateCommandError::Invalid(
                "Plan intent cannot execute".into(),
            ));
        }
        self.update = self
            .update
            .with_context(context, limits)
            .map_err(UpdateCommandError::Sdk)?;
        Ok(self)
    }

    /// Submit the context-bound request after exact caller approval.
    ///
    /// The host may inspect [`Self::update`] before calling this method. Its
    /// policy must authorize that same immutable request, not an earlier plan.
    ///
    /// # Errors
    /// Missing context, plan intent or denied approval sends nothing.
    pub async fn execute_bound<E: OperationExecutor, P: ApprovalPolicy>(
        self,
        executor: E,
        policy: &mut P,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<UpdateDelivery, UpdateCommandError> {
        if self.command.plan {
            return Err(UpdateCommandError::Invalid(
                "Plan intent cannot execute".into(),
            ));
        }
        let approval = self
            .update
            .prepared()
            .authorize(policy)
            .map_err(|_| UpdateCommandError::Approval)?;
        let report = sdk::execute(&self.update, &approval, executor, interrupted)
            .await
            .map_err(UpdateCommandError::Sdk)?;
        Ok(UpdateDelivery {
            command: self.command,
            output: self.output,
            report,
        })
    }
}

/// A remote report awaiting explicit blocking delivery, not rollback custody.
pub struct UpdateDelivery {
    command: UpdateCommand,
    output: Option<ExactOutputFile>,
    report: sdk::UpdateReport,
}

impl std::fmt::Debug for UpdateDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateDelivery")
            .field("report", &self.report)
            .field("file_output", &self.output.is_some())
            .finish_non_exhaustive()
    }
}

impl UpdateDelivery {
    /// Retain protected unprojected receipt before any local delivery.
    pub fn report(&self) -> &sdk::UpdateReport {
        &self.report
    }

    /// Write projected data and intact meta to stdout or the exact no-clobber file.
    ///
    /// # Errors
    /// Writer/projection/commit failures retain the original remote report.
    pub fn deliver(
        self,
        stdout: &mut dyn Write,
    ) -> Result<Option<ExactOutputReceipt>, UpdateCommandError> {
        let mut bytes = Vec::new();
        let result = self
            .command
            .write_report(&self.report, &mut bytes)
            .map_err(UpdateCommandError::Io)
            .and_then(|()| deliver(self.output, &bytes, stdout));
        result.map_err(|source| UpdateCommandError::Delivery {
            source: Box::new(source),
            report: Box::new(self.report),
        })
    }
}

fn deliver(
    output: Option<ExactOutputFile>,
    bytes: &[u8],
    stdout: &mut dyn Write,
) -> Result<Option<ExactOutputReceipt>, UpdateCommandError> {
    if bytes.len() > sdk::MAX_INPUT_BYTES {
        return Err(UpdateCommandError::Invalid(
            "Update receipt exceeded its byte bound".into(),
        ));
    }
    match output {
        Some(output) => output
            .commit(bytes)
            .map(Some)
            .map_err(UpdateCommandError::File),
        None => {
            stdout.write_all(bytes).map_err(UpdateCommandError::Io)?;
            stdout.flush().map_err(UpdateCommandError::Io)?;
            Ok(None)
        }
    }
}
