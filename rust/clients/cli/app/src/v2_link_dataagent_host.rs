// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded DataAgent staging link composition over caller-owned native operations.

use clap::Command;
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
    io::{self, Read, Write},
    num::NonZeroUsize,
};

/// Maximum native link input size, enforced while reading files and stdin.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// Mount only the DataAgent facade and its same-owner root alias.
pub fn command() -> Command {
    Command::new("fab-cli-v2")
        .subcommand_required(true)
        .subcommand(v2_link::link_command())
        .subcommand(
            Command::new("artifact")
                .subcommand_required(true)
                .subcommand(
                    Command::new("data-agent")
                        .subcommand_required(true)
                        .subcommand(v2_link::link_command()),
                ),
        )
}

/// Whether the explicit v2 head should dispatch to this bounded host.
pub fn handles(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| arg == "link")
        || (args.first().is_some_and(|arg| arg == "artifact")
            && args.get(1).is_some_and(|arg| arg == "data-agent")
            && args.get(2).is_some_and(|arg| arg == "link"))
}

/// Parse the constrained surface using the existing shared link argument model.
///
/// # Errors
/// Returns Clap help or syntax errors without performing input or identity work.
pub fn parse(args: Vec<OsString>) -> Result<LinkCommand, clap::Error> {
    let args: Vec<_> = std::iter::once(OsString::from("fab-cli-v2"))
        .chain(args)
        .collect();
    command().try_get_matches_from(&args)?;
    v2_link::parse_args(args).map_err(|_| {
        clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "Invalid DataAgent link arguments",
        )
    })
}

struct Resolver;

impl LinkResolver for Resolver {
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        Box::pin(async move {
            let parts: Vec<_> = path.split('/').collect();
            let ["", workspace, agent, "settings", "staging", "datasources"] = parts.as_slice()
            else {
                return Err(invalid_target());
            };
            let workspace = workspace
                .strip_suffix(".Workspace")
                .filter(|id| crate::get_target::is_id(id))
                .ok_or_else(invalid_target)?;
            let agent = agent
                .strip_suffix(".DataAgent")
                .filter(|id| crate::get_target::is_id(id))
                .ok_or_else(invalid_target)?;
            ResolvedLinkTarget::new(
                workspace,
                agent,
                LinkOwner::DataAgent,
                LinkScope::DataAgentStagingDatasources,
            )
        })
    }
}

fn invalid_target() -> MutationError {
    MutationError::new(
        "InvalidTarget",
        "Use /<workspace-id>.Workspace/<agent-id>.DataAgent/settings/staging/datasources; only explicit IDs and staging are supported",
    )
}

/// Bounded local input adapter; structured stdin is never used for approval.
pub struct NativeFiles<'a> {
    stdin: &'a mut (dyn Read + Send),
}

impl<'a> NativeFiles<'a> {
    /// Use the caller's stdin reader, without ambient input or credential discovery.
    pub fn new(stdin: &'a mut (dyn Read + Send)) -> Self {
        Self { stdin }
    }
}

impl LinkFiles for NativeFiles<'_> {
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        max_bytes: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        Box::pin(async move {
            let failure = || {
                MutationError::new(
                    "InputFailed",
                    "Link input must be a readable regular file or stdin",
                )
            };
            let mut file;
            let reader: &mut dyn Read = if path == "-" {
                &mut self.stdin
            } else {
                if !std::fs::metadata(path).map_err(|_| failure())?.is_file() {
                    return Err(failure());
                }
                file = std::fs::File::open(path).map_err(|_| failure())?;
                if !file.metadata().map_err(|_| failure())?.is_file() {
                    return Err(failure());
                }
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
                    "Link input exceeds 1 MiB",
                ));
            }
            Ok(bytes)
        })
    }
}

/// Resolve and strictly validate before approval, credentials or service requests.
///
/// # Errors
/// Preserves strict UTF-8, duplicate/unknown JSON, schema, target and input failures.
pub async fn prepare(
    command: &LinkCommand,
    files: &mut dyn LinkFiles,
) -> Result<PreparedLink, MutationError> {
    v2_link::prepare_command(
        command,
        &mut Resolver,
        files,
        NonZeroUsize::new(MAX_INPUT_BYTES).expect("positive constant"),
    )
    .await
}

struct CheckedOperations<'a> {
    inner: &'a mut dyn ResourceOperations,
    prepared: &'a PreparedLink,
    used: bool,
}

impl ResourceOperations for CheckedOperations<'_> {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        Box::pin(async move {
            let expected = self.prepared.operation();
            if self.used
                || self.prepared.target().owner() != LinkOwner::DataAgent
                || !matches!(
                    self.prepared.target().scope(),
                    LinkScope::DataAgentStagingDatasources
                )
                || operation.method() != ResourceMethod::Post
                || operation.path() != expected.path()
                || operation.body() != expected.body()
                || operation.completion() != expected.completion()
            {
                return Err(ResourceError::input());
            }
            self.used = true;
            let target = self.prepared.target();
            let owner = metadata(self.inner, target.workspace(), target.item()).await?;
            if owner != "DataAgent" {
                return Err(unsupported_type());
            }
            let body = expected.body().ok_or_else(ResourceError::input)?;
            let lakehouse = body.get("type").and_then(Value::as_str) == Some("LakehouseTables");
            let reference = body
                .get(if lakehouse {
                    "lakehouseReference"
                } else {
                    "itemReference"
                })
                .ok_or_else(ResourceError::input)?;
            let source_workspace = reference["workspaceId"]
                .as_str()
                .ok_or_else(ResourceError::input)?;
            let source_item = reference["itemId"]
                .as_str()
                .ok_or_else(ResourceError::input)?;
            let source_type = metadata(self.inner, source_workspace, source_item).await?;
            let supported = if lakehouse {
                source_type == "Lakehouse"
            } else {
                matches!(
                    source_type.as_str(),
                    "Report"
                        | "SemanticModel"
                        | "Lakehouse"
                        | "KQLDatabase"
                        | "Warehouse"
                        | "MirroredDatabase"
                        | "MirroredAzureDatabricksCatalog"
                        | "GraphModel"
                        | "SQLDatabase"
                        | "Ontology"
                )
            };
            if !supported {
                return Err(unsupported_type());
            }
            self.inner.execute(operation).await
        })
    }
}

fn unsupported_type() -> ResourceError {
    let mut error = ResourceError::input();
    error.code = "UnsupportedCapability".into();
    error.message = "Resolved owner or source type is not supported for this DataAgent link".into();
    error
}

async fn metadata(
    operations: &mut dyn ResourceOperations,
    workspace: &str,
    item: &str,
) -> Result<String, ResourceError> {
    let result = operations
        .execute(ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", workspace, "items", item],
            None,
            CompletionPolicy::ReturnAccepted,
        )?)
        .await?;
    if let ResourceOutcome::Completed {
        status: 200,
        data: Some(data),
    } = result
        && data
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.eq_ignore_ascii_case(item))
        && data.get("workspaceId").is_none_or(|id| {
            id.as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(workspace))
        })
        && let Some(kind) = data.get("type").and_then(Value::as_str)
    {
        return Ok(kind.to_owned());
    }
    let mut error = ResourceError::input();
    error.code = "InvalidResponse".into();
    error.message = "Link metadata did not establish the exact item identity and type".into();
    Err(error)
}

/// Original execution evidence and its nonsecret presentation projection.
#[derive(Debug)]
pub struct Dispatch {
    /// Retained native receipt or failure, including malformed completed responses.
    pub result: Result<v2_link::LinkReceipt, Box<v2_link::LinkFailure>>,
    /// Small output envelope, never native input or arbitrary returned source data.
    pub value: Value,
}

impl Dispatch {
    /// Acceptance is not completion; failures always return a nonzero process status.
    pub fn exit_code(&self) -> u8 {
        u8::from(self.result.is_err())
    }

    /// Deliver once, preserving the original dispatch even after a partial write.
    ///
    /// # Errors
    /// Retains bytes written and native evidence; callers must not replay the POST.
    pub fn write_to(self, writer: &mut dyn Write) -> Result<Self, Box<DeliveryFailure>> {
        let mut written = 0;
        let result = (|| {
            let mut bytes = serde_json::to_vec(&self.value).map_err(io::Error::other)?;
            bytes.push(b'\n');
            while written < bytes.len() {
                match writer.write(&bytes[written..]) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(count) => written += count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            writer.flush()
        })();
        match result {
            Ok(()) => Ok(self),
            Err(error) => Err(Box::new(DeliveryFailure {
                dispatch: self,
                written,
                error,
            })),
        }
    }
}

/// Output failure never discards provider evidence or implies rollback.
#[derive(Debug)]
pub struct DeliveryFailure {
    /// Original execution evidence, independent of presentation.
    pub dispatch: Dispatch,
    /// Confirmed bytes delivered before write/flush failure.
    pub written: usize,
    /// Actual local output error.
    pub error: io::Error,
}

impl DeliveryFailure {
    /// Best-effort stderr evidence, without replay or successful-looking fallback.
    pub fn evidence(&self) -> Value {
        json!({
            "execution": self.dispatch.value,
            "output": {"status":"partial", "bytesWritten":self.written},
            "remoteReplayed":false, "rollback":false
        })
    }
}

/// Approve before any service request, validate native owner/source metadata, then POST once.
///
/// The service remains the authority for DataAgent read/write and source access.
/// A metadata read is not an ACL grant. Operations and approval must be bound to
/// the same caller-owned credential selection and endpoint.
pub async fn dispatch(
    prepared: PreparedLink,
    operations: &mut dyn ResourceOperations,
    approval: Option<&mut dyn LinkApproval>,
) -> Dispatch {
    let mut checked = CheckedOperations {
        inner: operations,
        prepared: &prepared,
        used: false,
    };
    let result = v2_link::execute(prepared.clone(), &mut checked, approval).await;
    let mut value = match &result {
        Ok(receipt) => {
            let mut value = receipt.metadata();
            if let ResourceOutcome::Completed { data, .. } = &receipt.outcome {
                value["status"] = json!("completed");
                value["datasourceId"] = data
                    .as_ref()
                    .and_then(|data| data.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            value
        }
        Err(failure) => {
            let operation = failure.error.operation.as_deref();
            let uncertain = operation.is_some_and(|error| error.mutation_may_have_occurred);
            json!({
                "operation":v2_link::OPERATION_ID,
                "status":if failure.observed.is_some() { "partial" } else if uncertain { "unknown" } else { "failed" },
                "error":{"code":failure.error.code,"message":failure.error.message},
                "target":{"workspaceId":failure.target.workspace(),"itemId":failure.target.item(),"type":"DataAgent"},
                "effects":{
                    "remoteWrite":failure.effects.remote_write,
                    "queryAcceleration":failure.effects.query_acceleration,
                    "updateItemSensitivity":failure.effects.update_item_sensitivity
                },
                "httpStatus":operation.and_then(|error| error.status),
                "operationId":operation.and_then(|error| error.operation_id.as_deref()),
                "mutationMayHaveOccurred":uncertain,
                "observedCompleted":matches!(failure.observed,Some(ResourceOutcome::Completed { .. })),
            })
        }
    };
    value["remoteReplayed"] = json!(false);
    value["rollback"] = json!(false);
    Dispatch { result, value }
}
