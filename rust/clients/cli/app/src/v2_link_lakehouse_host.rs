// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded Lakehouse relationship host; legacy `ln` is a separate contract.

use crate::get_target::is_id;
use clap::Command;
pub use fabric_cli_mutations::v2_link::{LinkFailure, LinkReceipt};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{
        self, LinkApproval, LinkCommand, LinkFiles, LinkOwner, LinkResolver, LinkScope,
        PreparedLink, ResolvedLinkTarget,
    },
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::Path,
};

/// Local input allowance; enforced while reading files and explicit stdin.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// Root link and its Lakehouse-only facade, without mounting other link owners.
pub fn command() -> Command {
    Command::new("fab-cli-v2")
        .subcommand_required(true)
        .subcommand(v2_link::link_command())
        .subcommand(
            Command::new("artifact").subcommand_required(true).subcommand(
                Command::new("lakehouse")
                    .subcommand_required(true)
                    .subcommand(v2_link::link_command()),
            ),
        )
        .after_help("Relationship target: /<workspace-guid>.Workspace/<item-guid>.Lakehouse/Files/<parents>/<name>.Shortcut or Tables/<name>.Shortcut. Names are not discovered. No force, overwrite, copy or dereference.")
}

/// Parse only this bounded executable surface before any input or identity work.
///
/// # Errors
/// Returns Clap's structured syntax/help result; callers must not echo raw errors.
pub fn parse<I, T>(args: I) -> Result<LinkCommand, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    let (facade, leaf) = if let Some(leaf) = matches.subcommand_matches("link") {
        (None, leaf)
    } else {
        let leaf = matches
            .subcommand_matches("artifact")
            .and_then(|artifact| artifact.subcommand_matches("lakehouse"))
            .and_then(|owner| owner.subcommand_matches("link"))
            .expect("required command schema");
        (Some(LinkOwner::Lakehouse), leaf)
    };
    Ok(LinkCommand {
        path: leaf
            .get_one::<String>("path")
            .expect("required path")
            .clone(),
        input: leaf
            .get_one::<String>("input")
            .expect("required input")
            .clone(),
        wait: leaf.get_flag("wait"),
        facade,
    })
}

/// Explicit GUID-qualified relationship resolution, never a file-content target.
pub struct LakehouseResolver;

impl LinkResolver for LakehouseResolver {
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        Box::pin(std::future::ready(resolve(path)))
    }
}

fn resolve(path: &str) -> Result<ResolvedLinkTarget, MutationError> {
    let invalid = || {
        MutationError::new(
            "InvalidTarget",
            "An explicit Lakehouse shortcut target is required",
        )
    };
    if !path.starts_with('/')
        || path.contains(['\\', '%', '?', '#'])
        || path.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    if parts.len() < 4
        || parts
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
    {
        return Err(invalid());
    }
    let workspace = parts[0]
        .strip_suffix(".Workspace")
        .filter(|id| is_id(id))
        .ok_or_else(invalid)?;
    let item = parts[1]
        .strip_suffix(".Lakehouse")
        .filter(|id| is_id(id))
        .ok_or_else(invalid)?;
    if !matches!(parts[2], "Files" | "Tables") {
        return Err(invalid());
    }
    if parts[3..parts.len() - 1]
        .iter()
        .any(|part| part.ends_with(".Shortcut") || part.ends_with(".shortcut"))
    {
        return Err(invalid());
    }
    // Only the final relationship marker is removed, never a parent or substring.
    let name = parts
        .last()
        .and_then(|part| part.strip_suffix(".Shortcut"))
        .filter(|name| !name.is_empty())
        .ok_or_else(invalid)?;
    ResolvedLinkTarget::new(
        workspace,
        item,
        LinkOwner::Lakehouse,
        LinkScope::LakehouseShortcut {
            path: parts[2..parts.len() - 1].join("/"),
            name: name.into(),
        },
    )
}

/// Caller-owned blocking local input. Native hosts run preflight off the runtime.
pub struct LocalFiles<'a> {
    /// Explicit local working directory, never a Fabric current directory.
    pub cwd: &'a Path,
    /// Read only when the input selector is exactly `-`.
    pub stdin: &'a mut (dyn Read + Send),
}

impl LinkFiles for LocalFiles<'_> {
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        max_bytes: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        Box::pin(async move {
            let failure = || MutationError::new("InputFailure", "Link input could not be read");
            let mut file;
            let reader: &mut dyn Read = if path == "-" {
                &mut *self.stdin
            } else {
                let path = self.cwd.join(path);
                let metadata = fs::symlink_metadata(&path).map_err(|_| failure())?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(failure());
                }
                file = fs::File::open(path).map_err(|_| failure())?;
                &mut file
            };
            let mut bytes = Vec::new();
            reader
                .take(max_bytes.get().saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| failure())?;
            if bytes.len() > max_bytes.get() {
                return Err(MutationError::new(
                    "BudgetExceeded",
                    "Link input exceeds the byte limit",
                ));
            }
            Ok(bytes)
        })
    }
}

/// Resolve and validate native input before credentials, approval or requests.
///
/// # Errors
/// Preserves strict JSON, native schema, destination and local input errors.
pub async fn prepare(
    command: &LinkCommand,
    files: &mut dyn LinkFiles,
) -> Result<PreparedLink, MutationError> {
    v2_link::prepare_command(
        command,
        &mut LakehouseResolver,
        files,
        NonZeroUsize::new(MAX_INPUT_BYTES).expect("positive input limit"),
    )
    .await
}

struct CheckedOwner<'a> {
    operations: &'a mut dyn ResourceOperations,
    target: &'a ResolvedLinkTarget,
}

impl ResourceOperations for CheckedOwner<'_> {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        Box::pin(async move {
            let read = ResourceOperation::new(
                ResourceMethod::Get,
                &[
                    "workspaces",
                    self.target.workspace(),
                    "items",
                    self.target.item(),
                ],
                None,
                CompletionPolicy::ReturnAccepted,
            )?;
            let observation = self.operations.execute(read).await?;
            match &observation {
                ResourceOutcome::Completed {
                    status: 200,
                    data: Some(data),
                } if data
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.eq_ignore_ascii_case(self.target.item()))
                    && data.get("type").and_then(Value::as_str) == Some("Lakehouse") => {}
                _ => {
                    let mut error = ResourceError::input();
                    error.code = "InvalidOwnerResponse".into();
                    error.message =
                        "Destination metadata did not establish the selected Lakehouse owner"
                            .into();
                    match observation {
                        ResourceOutcome::Completed { status, .. } => error.status = Some(status),
                        ResourceOutcome::Accepted { operation_id } => {
                            error.operation_id = operation_id
                        }
                    }
                    return Err(error);
                }
            }
            self.operations.execute(operation).await
        })
    }
}

/// Approve, verify the actual Lakehouse owner, and submit at most one mutation.
///
/// Missing/declined approval makes no request. The captured GUIDs are never
/// resolved again, and Warehouse is permitted only as a source reference.
///
/// # Errors
/// Retains the shared native failure and original malformed completion evidence.
pub async fn execute(
    prepared: PreparedLink,
    operations: &mut dyn ResourceOperations,
    approval: Option<&mut dyn LinkApproval>,
) -> Result<LinkReceipt, Box<LinkFailure>> {
    if prepared.target().owner() != LinkOwner::Lakehouse {
        return Err(Box::new(LinkFailure {
            target: prepared.target().clone(),
            effects: prepared.effects(),
            error: MutationError::new(
                "UnsupportedCapability",
                "This host only creates Lakehouse shortcuts",
            ),
            observed: None,
        }));
    }
    let target = prepared.target().clone();
    let mut checked = CheckedOwner {
        operations,
        target: &target,
    };
    v2_link::execute(prepared, &mut checked, approval).await
}

/// Output failure retaining the original receipt; never a reason to resubmit.
#[derive(Debug)]
pub struct DeliveryFailure {
    /// Actual local writer failure.
    pub source: io::Error,
    /// Actual provider outcome, including acceptance rather than completion.
    pub receipt: LinkReceipt,
}

/// Publish only nonsecret receipt metadata, retaining remote evidence on failure.
///
/// # Errors
/// Preserves the receipt on serialization, partial output or flush failure.
pub fn deliver(receipt: LinkReceipt, writer: &mut dyn Write) -> Result<(), Box<DeliveryFailure>> {
    let result = (|| {
        serde_json::to_writer(&mut *writer, &receipt_metadata(&receipt))
            .map_err(io::Error::other)?;
        writer.write_all(b"\n")?;
        writer.flush()
    })();
    result.map_err(|source| Box::new(DeliveryFailure { source, receipt }))
}

/// Nonsecret outcome and exact relationship destination for reconciliation.
pub fn receipt_metadata(receipt: &LinkReceipt) -> Value {
    let mut metadata = receipt.metadata();
    metadata["relationship"] = relationship(&receipt.target);
    metadata
}

fn relationship(target: &ResolvedLinkTarget) -> Value {
    match target.scope() {
        LinkScope::LakehouseShortcut { path, name } => json!({"path": path, "name": name}),
        _ => Value::Null,
    }
}

/// Sanitized reconciliation projection; original structured evidence is retained.
pub fn failure_metadata(failure: &LinkFailure) -> Value {
    let native = failure.error.operation.as_deref();
    json!({
        "target": {"workspaceId": failure.target.workspace(), "itemId": failure.target.item(), "type": failure.target.owner().item_type()},
        "relationship": relationship(&failure.target),
        "effects": {"remoteWrite": failure.effects.remote_write, "queryAcceleration": failure.effects.query_acceleration, "updateItemSensitivity": failure.effects.update_item_sensitivity},
        "code": failure.error.code,
        "httpStatus": native.and_then(|error| error.status),
        "operationId": native.and_then(|error| error.operation_id.as_deref()),
        "serviceCode": native.and_then(|error| error.service_error.as_deref()).map(|error| &error.error_code),
        "mutationMayHaveOccurred": native.is_some_and(|error| error.mutation_may_have_occurred),
        "observedHttpStatus": failure.observed.as_ref().and_then(|outcome| match outcome { ResourceOutcome::Completed { status, .. } => Some(*status), ResourceOutcome::Accepted { .. } => None }),
        "remoteReplayed": false, "rollback": false
    })
}
