// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! KQL-only executable link boundary over the shared native link contract.

use crate::get_target::{GetRepresentation, GetTarget};
use fabric_cli_mutations::{
    MutationError,
    v2_link::{
        self, LinkApproval, LinkCommand, LinkEffects, LinkFailure, LinkFiles, LinkOwner,
        LinkReceipt, LinkResolver, LinkScope, PreparedLink, ResolvedLinkTarget,
    },
};
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use serde_json::{Value, json};
use std::{ffi::OsString, io, io::Write, num::NonZeroUsize};

/// Finite input/discovery/response allowance for this experimental leaf.
pub const MAX_BYTES: usize = 1_048_576;

/// Explicit typed shortcut destination; no current-directory or owner fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KqlDestination {
    owner: GetTarget,
    name: String,
}

impl KqlDestination {
    /// Parse `/workspace.Workspace/database.KQLDatabase/Tables/name.Shortcut`.
    ///
    /// # Errors
    /// Rejects other owners, ordinary table/file paths and nonliteral selectors.
    pub fn parse(path: &str) -> Result<Self, MutationError> {
        let parts: Vec<_> = path.split('/').collect();
        let ["", workspace, database, "Tables", shortcut] = parts.as_slice() else {
            return Err(invalid_target());
        };
        let name = shortcut
            .strip_suffix(".Shortcut")
            .filter(|name| !name.is_empty() && !matches!(*name, "." | ".."))
            .ok_or_else(invalid_target)?;
        if path.len() > 4096
            || path.contains(['\\', '?', '#', '%', '[', ']'])
            || path.chars().any(char::is_control)
        {
            return Err(invalid_target());
        }
        let owner =
            GetTarget::parse(&format!("/{workspace}/{database}")).map_err(|_| invalid_target())?;
        if !matches!(
            &owner,
            GetTarget::Item {
                item_type: Some(kind),
                representation: GetRepresentation::Metadata,
                ..
            } if kind == "KQLDatabase"
        ) {
            return Err(invalid_target());
        }
        Ok(Self {
            owner,
            name: name.into(),
        })
    }

    /// The metadata-only owner prefix used by existing bounded name resolution.
    pub fn owner(&self) -> GetTarget {
        self.owner.clone()
    }

    /// Capture authoritative workspace/database IDs and the literal child once.
    ///
    /// # Errors
    /// Propagates invalid native identity components.
    pub fn resolved(
        &self,
        workspace: &str,
        database: &str,
    ) -> Result<ResolvedLinkTarget, MutationError> {
        ResolvedLinkTarget::new(
            workspace,
            database,
            LinkOwner::KqlDatabase,
            LinkScope::KqlTableShortcut {
                name: self.name.clone(),
            },
        )
    }
}

/// Parse the shared leaf schema, rejecting every non-KQL facade before I/O.
///
/// # Errors
/// Rejects invalid arguments, overwrite flags, and unsupported typed destinations.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<LinkCommand, MutationError> {
    let command = v2_link::parse_args(std::iter::once("fab-cli-v2".into()).chain(args))?;
    if command
        .facade
        .is_some_and(|owner| owner != LinkOwner::KqlDatabase)
    {
        return Err(invalid_target());
    }
    KqlDestination::parse(&command.path)?;
    Ok(command)
}

/// Resolve, read and validate through the existing link contract with a fixed bound.
///
/// # Errors
/// Preserves strict JSON, native target restrictions and body/path conflicts.
pub async fn prepare(
    command: &LinkCommand,
    resolver: &mut dyn LinkResolver,
    files: &mut dyn LinkFiles,
) -> Result<PreparedLink, MutationError> {
    KqlDestination::parse(&command.path)?;
    if command
        .facade
        .is_some_and(|owner| owner != LinkOwner::KqlDatabase)
    {
        return Err(invalid_target());
    }
    // Root link is constrained too: an injected resolver cannot change its family.
    let mut scoped = command.clone();
    scoped.facade = Some(LinkOwner::KqlDatabase);
    v2_link::prepare_command(
        &scoped,
        resolver,
        files,
        NonZeroUsize::new(MAX_BYTES).unwrap(),
    )
    .await
}

/// The complete effect-bound approval question, safe for terminal presentation.
pub fn approval_question(target: &ResolvedLinkTarget, effects: LinkEffects) -> String {
    let name = match target.scope() {
        LinkScope::KqlTableShortcut { name } => name.as_str(),
        _ => "",
    };
    format!(
        "Create one KQL table shortcut '{}' in workspace '{}' database '{}'? \
         Remote reference write only; no copy, data query, ACL grant or other database mutation. \
         Query acceleration: {}. {}",
        name.escape_debug(),
        target.workspace().escape_debug(),
        target.item().escape_debug(),
        if effects.query_acceleration {
            "enabled"
        } else {
            "disabled"
        },
        if effects.query_acceleration {
            "Acceleration may incur cache, indexing, storage and compute costs."
        } else {
            "No query acceleration is requested."
        },
    )
}

/// One attempted operation and its retained reconciliation evidence.
#[derive(Debug)]
pub struct Delivery {
    result: Result<LinkReceipt, Box<LinkFailure>>,
    operation_id: Option<String>,
}

impl Delivery {
    /// Execute exactly once using the caller's existing operation policy.
    pub async fn execute<E: OperationExecutor>(
        prepared: PreparedLink,
        operations: &mut OperationSession<'_, E>,
        approval: Option<&mut dyn LinkApproval>,
    ) -> Self {
        let result = v2_link::execute(prepared, operations, approval).await;
        Self {
            result,
            operation_id: operations.operation_id().map(str::to_owned),
        }
    }

    /// Original provider evidence remains available even after a writer fails.
    pub fn result(&self) -> &Result<LinkReceipt, Box<LinkFailure>> {
        &self.result
    }

    /// Nonsecret presentation; never echo source/connection/request bodies.
    pub fn metadata(&self) -> Value {
        let (target, effects, mut value) = match &self.result {
            Ok(receipt) => (&receipt.target, receipt.effects, receipt.metadata()),
            Err(failure) => {
                let operation = failure.error.operation.as_deref();
                let uncertain = operation.is_some_and(|error| error.mutation_may_have_occurred);
                let terminal = operation.is_some_and(|error| {
                    error.code == "LongRunningOperationFailed"
                        || (error.code == "LongRunningOperationCancelled"
                            && error.raw_service_error.is_some())
                });
                let code =
                    operation.map_or(failure.error.code.as_str(), |error| error.code.as_str());
                (
                    &failure.target,
                    failure.effects,
                    json!({
                        "operation": v2_link::OPERATION_ID,
                        "status": if uncertain && !terminal { "unknown" } else { "failed" },
                        "error": {"code":code,"message":failure.to_string()},
                        "httpStatus":operation.and_then(|error| error.status),
                        "operationId":operation.and_then(|error| error.operation_id.as_deref()),
                        "serviceErrorCode":operation.and_then(|error| error.service_error.as_deref()).map(|error| &error.error_code),
                        "mutationMayHaveOccurred":uncertain,
                        "observationRetained":failure.observed.is_some(),
                    }),
                )
            }
        };
        value["target"] = json!({
            "workspaceId":target.workspace(), "itemId":target.item(),
            "type":target.owner().item_type(),
            "name":match target.scope() { LinkScope::KqlTableShortcut { name } => Some(name), _ => None },
        });
        value["effects"] = json!({
            "remoteWrite":effects.remote_write,
            "queryAcceleration":effects.query_acceleration,
            "updateItemSensitivity":effects.update_item_sensitivity,
        });
        if self.result.is_ok() {
            value["mutationMayHaveOccurred"] = json!(true);
        }
        if value["operationId"].is_null() {
            value["operationId"] = json!(self.operation_id);
        }
        value["remoteCancelled"] = json!(false);
        value["remoteReplayed"] = json!(false);
        value
    }

    /// Deliver once; output failure reports retained evidence without reexecution.
    ///
    /// The original receipt/failure is still owned by `self` for reconciliation.
    pub fn write_to(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        let value = self.metadata();
        let written = if self.result.is_ok() {
            write_json(stdout, &value)
        } else {
            write_json(stderr, &value)
        };
        if written.is_err() {
            let _ = write_json(
                stderr,
                &json!({
                    "error":{"code":"OutputFailed","message":"Link output failed; do not replay the mutation."},
                    "evidence":value,
                }),
            );
            return 1;
        }
        u8::from(self.result.is_err())
    }
}

fn write_json(writer: &mut dyn Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writeln!(writer)?;
    writer.flush()
}

fn invalid_target() -> MutationError {
    MutationError::new(
        "UnsupportedCapability",
        "Link requires an absolute /workspace.Workspace/database.KQLDatabase/Tables/name.Shortcut destination",
    )
}
