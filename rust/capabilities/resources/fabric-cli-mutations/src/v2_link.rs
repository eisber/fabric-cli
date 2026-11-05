// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive link composition for explicitly resolved native relationships.
//!
//! The standalone schema does not replace the existing `ln` command or install
//! a production host. Files, resolution, approval, credentials, cancellation and
//! bounded execution are supplied by the caller.

use crate::{CommandFuture, MutationError};
use clap::{Arg, ArgAction, Command};
use fabric_sdk_items::links::{
    NativeLinkKind, NativeLinkRequest, parse_link_json, validate_link_id,
};
use fabric_sdk_onelake::link::ShortcutLinkRequest;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{ffi::OsString, fmt, num::NonZeroUsize};

/// Stable operation identity shared by the common and artifact-facade entry points.
pub const OPERATION_ID: &str = "fabric.link";

/// An owner with a named link applicability mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkOwner {
    /// OneLake Files or an explicitly supported Tables shortcut location.
    Lakehouse,
    /// A native KQL table shortcut, not a generic OneLake file.
    KqlDatabase,
    /// A staging datasource reference.
    DataAgent,
    /// A semantic-model datasource connection reference.
    SemanticModel,
    /// A schema binding requiring a representation-specific native adapter.
    Ontology,
}

impl LinkOwner {
    /// Exact artifact facade spelling.
    pub const fn facade(self) -> &'static str {
        match self {
            Self::Lakehouse => "lakehouse",
            Self::KqlDatabase => "kql-database",
            Self::DataAgent => "data-agent",
            Self::SemanticModel => "semantic-model",
            Self::Ontology => "ontology",
        }
    }

    /// Exact owner type supplied by the typed resolver.
    pub const fn item_type(self) -> &'static str {
        match self {
            Self::Lakehouse => "Lakehouse",
            Self::KqlDatabase => "KQLDatabase",
            Self::DataAgent => "DataAgent",
            Self::SemanticModel => "SemanticModel",
            Self::Ontology => "Ontology",
        }
    }
}

/// A typed relationship child; ordinary file contents are a different target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkScope {
    /// An exact shortcut location and name, without a display-only suffix.
    LakehouseShortcut {
        /// Relative destination parent, including Files or Tables.
        path: String,
        /// Exact native shortcut name.
        name: String,
    },
    /// A native KQL shortcut name with no OneLake destination path field.
    KqlTableShortcut {
        /// Exact native table-shortcut name.
        name: String,
    },
    /// The agent's staging datasource collection, never its published state.
    DataAgentStagingDatasources,
    /// A native semantic-model connection reference, not a gateway or report.
    SemanticModelConnections,
    /// An Ontology schema for which no generic replacement is safe.
    OntologySchema,
}

/// Resolver-owned authoritative identity, captured before submission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLinkTarget {
    workspace: String,
    item: String,
    owner: LinkOwner,
    scope: LinkScope,
}

impl ResolvedLinkTarget {
    /// Validate the owner/child pairing without fetching metadata or credentials.
    ///
    /// # Errors
    /// Rejects incompatible child kinds and unsafe identifier route components.
    /// The native SDK additionally validates endpoint-specific identifier shapes.
    pub fn new(
        workspace: &str,
        item: &str,
        owner: LinkOwner,
        scope: LinkScope,
    ) -> Result<Self, MutationError> {
        let matches = matches!(
            (owner, &scope),
            (LinkOwner::Lakehouse, LinkScope::LakehouseShortcut { .. })
                | (LinkOwner::KqlDatabase, LinkScope::KqlTableShortcut { .. })
                | (LinkOwner::DataAgent, LinkScope::DataAgentStagingDatasources)
                | (
                    LinkOwner::SemanticModel,
                    LinkScope::SemanticModelConnections
                )
                | (LinkOwner::Ontology, LinkScope::OntologySchema)
        );
        if !matches {
            return Err(unsupported(
                "The relationship child does not belong to this owner",
            ));
        }
        ResourceOperation::new(
            fabric_sdk_workspaces::mutations::ResourceMethod::Get,
            &["workspaces", workspace, "items", item],
            None,
            CompletionPolicy::ReturnAccepted,
        )?;
        Ok(Self {
            workspace: workspace.to_owned(),
            item: item.to_owned(),
            owner,
            scope,
        })
    }

    /// Resolved destination workspace, not a name to resolve again while polling.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Resolved destination item.
    pub fn item(&self) -> &str {
        &self.item
    }

    /// Resolved native owner type.
    pub const fn owner(&self) -> LinkOwner {
        self.owner
    }

    /// Exact typed relationship child.
    pub fn scope(&self) -> &LinkScope {
        &self.scope
    }
}

/// Parsed arguments; the input path never contains inline credentials or JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkCommand {
    /// Explicit target path, passed unchanged to the caller's typed resolver.
    pub path: String,
    /// Local UTF-8 input file or an explicitly advertised stdin selector.
    pub input: String,
    /// Optional facade constraint, not another handler.
    pub facade: Option<LinkOwner>,
    /// Observe the submitted operation with the caller's existing bounded policy.
    pub wait: bool,
}

/// Build the same leaf schema for the root and all artifact facades.
pub fn link_command() -> Command {
    Command::new("link")
        .about("Create a supported native shortcut or relationship binding")
        .arg(Arg::new("path").required(true))
        .arg(
            Arg::new("input")
                .long("input")
                .required(true)
                .help("Typed native JSON input file; - uses caller-supplied stdin"),
        )
        .arg(
            Arg::new("wait")
                .long("wait")
                .action(ArgAction::SetTrue)
                .help("Observe only this submission; timeout does not cancel remote work"),
        )
}

/// Build a standalone additive command surface without replacing the v1 parser.
///
/// The central host can mount [`link_command`] in its reviewed v2 composition.
/// Registering this schema alone is not a configured production execution host.
pub fn command() -> Command {
    let mut artifacts = Command::new("artifact").subcommand_required(true);
    for owner in [
        LinkOwner::DataAgent,
        LinkOwner::KqlDatabase,
        LinkOwner::Lakehouse,
        LinkOwner::Ontology,
        LinkOwner::SemanticModel,
    ] {
        artifacts = artifacts.subcommand(
            Command::new(owner.facade())
                .subcommand_required(true)
                .subcommand(link_command()),
        );
    }
    Command::new("fab-cli")
        .subcommand_required(true)
        .subcommand(link_command())
        .subcommand(artifacts)
}

/// Parse the standalone schema without filesystem, service or authentication work.
///
/// # Errors
/// Rejects invalid argument shapes without echoing paths, input or parser text.
/// Hosts that need help text should render [`command`] directly.
pub fn parse_args<I, T>(args: I) -> Result<LinkCommand, MutationError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command()
        .try_get_matches_from(args)
        .map_err(|_| invalid("Invalid link arguments; use link --help"))?;
    let (facade, leaf) = match matches.subcommand() {
        Some(("link", leaf)) => (None, leaf),
        Some(("artifact", artifacts)) => {
            let (name, owner) = artifacts
                .subcommand()
                .ok_or_else(|| invalid("Missing artifact link facade"))?;
            let facade = [
                LinkOwner::DataAgent,
                LinkOwner::KqlDatabase,
                LinkOwner::Lakehouse,
                LinkOwner::Ontology,
                LinkOwner::SemanticModel,
            ]
            .into_iter()
            .find(|candidate| candidate.facade() == name)
            .ok_or_else(|| invalid("Unsupported artifact link facade"))?;
            let leaf = owner
                .subcommand_matches("link")
                .ok_or_else(|| invalid("Missing link operation"))?;
            (Some(facade), leaf)
        }
        _ => return Err(invalid("Missing link operation")),
    };
    let required = |name| {
        leaf.get_one::<String>(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| invalid("An explicit target and input are required"))
    };
    Ok(LinkCommand {
        path: required("path")?,
        input: required("input")?,
        facade,
        wait: leaf.get_flag("wait"),
    })
}

/// Declared effects used by host approval; linking is not an ACL grant or copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkEffects {
    /// A remote relationship reference is created or bound.
    pub remote_write: bool,
    /// Native KQL acceleration can incur cache, indexing and compute costs.
    pub query_acceleration: bool,
    /// The native OneDrive/SharePoint option can update the owner's sensitivity.
    pub update_item_sensitivity: bool,
}

/// Validated, destination-pinned request with no hidden I/O or identity selection.
#[derive(Clone, Debug)]
pub struct PreparedLink {
    target: ResolvedLinkTarget,
    effects: LinkEffects,
    operation: ResourceOperation,
}

impl PreparedLink {
    /// Stable identity for root and facade consumers.
    pub const fn operation_id(&self) -> &'static str {
        OPERATION_ID
    }

    /// Native request for inspection by an explicitly authorized host.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Destination identity captured before approval or submission.
    pub fn target(&self) -> &ResolvedLinkTarget {
        &self.target
    }

    /// Exact native effects used by the approval boundary.
    pub const fn effects(&self) -> LinkEffects {
        self.effects
    }
}

/// Validate native input against one resolved relationship target.
///
/// # Errors
/// Rejects wrong facade/owner/child combinations, body/path selector conflicts,
/// unsupported Ontology representations, and invalid native schemas before any
/// execution. There is no generic full-definition or gateway fallback.
pub fn prepare(
    target: ResolvedLinkTarget,
    facade: Option<LinkOwner>,
    input: &Value,
    wait: bool,
) -> Result<PreparedLink, MutationError> {
    if facade.is_some_and(|facade| facade != target.owner) {
        return Err(unsupported(
            "The artifact facade does not match the resolved owner",
        ));
    }
    if target.owner == LinkOwner::Ontology {
        return Err(unsupported(
            "Ontology linking requires a representation-specific binding and concurrency adapter",
        ));
    }
    match &target.scope {
        LinkScope::LakehouseShortcut { path, name } => {
            if input.get("path").and_then(Value::as_str) != Some(path)
                || input.get("name").and_then(Value::as_str) != Some(name)
            {
                return Err(invalid(
                    "Shortcut input conflicts with the resolved destination",
                ));
            }
        }
        LinkScope::KqlTableShortcut { name } => {
            if input.get("name").and_then(Value::as_str) != Some(name) {
                return Err(invalid(
                    "Shortcut input conflicts with the resolved destination",
                ));
            }
        }
        LinkScope::DataAgentStagingDatasources
        | LinkScope::SemanticModelConnections
        | LinkScope::OntologySchema => {}
    }
    let completion = if !wait {
        CompletionPolicy::ReturnAccepted
    } else if target.owner == LinkOwner::SemanticModel {
        CompletionPolicy::WaitForCompletion
    } else {
        CompletionPolicy::WaitForResult
    };
    let operation = match target.owner {
        LinkOwner::Lakehouse | LinkOwner::KqlDatabase => ShortcutLinkRequest::from_value(
            target.owner.item_type(),
            &target.workspace,
            &target.item,
            input,
        )?
        .operation(completion)?,
        LinkOwner::DataAgent | LinkOwner::SemanticModel => NativeLinkRequest::from_value(
            if target.owner == LinkOwner::DataAgent {
                NativeLinkKind::DataAgentDatasource
            } else {
                NativeLinkKind::SemanticModelConnection
            },
            &target.workspace,
            &target.item,
            input,
        )?
        .operation(completion)?,
        LinkOwner::Ontology => {
            return Err(unsupported(
                "Ontology linking has no generic definition replacement",
            ));
        }
    };
    let effects = LinkEffects {
        remote_write: true,
        query_acceleration: target.owner == LinkOwner::KqlDatabase
            && input
                .get("enableQueryAcceleration")
                .and_then(Value::as_bool)
                == Some(true),
        update_item_sensitivity: target.owner == LinkOwner::Lakehouse
            && input
                .pointer("/target/oneDriveSharePoint/updateFabricItemSensitivity")
                .and_then(Value::as_bool)
                == Some(true),
    };
    Ok(PreparedLink {
        target,
        effects,
        operation,
    })
}

/// Caller-owned typed path resolution; errors must not become missing resources.
pub trait LinkResolver: Send {
    /// Resolve one destination, preserving ambiguity and authorization failures.
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>>;
}

/// Caller-owned bounded local input, including stdin only when explicitly selected.
pub trait LinkFiles: Send {
    /// Read at most `max_bytes`; enforce the limit during reading, not afterward.
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        max_bytes: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>>;
}

/// Host/API approval, separate from payload flags and service permissions.
pub trait LinkApproval: Send {
    /// Approve exactly this resolved target and effect set, or fail explicitly.
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>>;
}

/// Actual provider evidence with captured destination and declared effects.
#[derive(Clone, Debug)]
pub struct LinkReceipt {
    /// Immutable destination identity.
    pub target: ResolvedLinkTarget,
    /// Effects presented to host approval.
    pub effects: LinkEffects,
    /// Original provider completion or acceptance, never a synthetic resource.
    pub outcome: ResourceOutcome,
}

impl LinkReceipt {
    /// Build a small nonsecret receipt without echoing native input or service data.
    ///
    /// The original outcome remains available independently for callers that are
    /// authorized to inspect the native response. No output writer is installed.
    pub fn metadata(&self) -> Value {
        let (status, http_status, operation) = match &self.outcome {
            ResourceOutcome::Completed { status, .. } => ("succeeded", Some(*status), None),
            ResourceOutcome::Accepted { operation_id } => {
                ("accepted", None, operation_id.as_deref())
            }
        };
        json!({
            "operation": OPERATION_ID,
            "status": status,
            "target": {
                "workspaceId": self.target.workspace,
                "itemId": self.target.item,
                "type": self.target.owner.item_type()
            },
            "effects": {
                "remoteWrite": self.effects.remote_write,
                "queryAcceleration": self.effects.query_acceleration,
                "updateItemSensitivity": self.effects.update_item_sensitivity
            },
            "httpStatus": http_status,
            "operationId": operation
        })
    }
}

/// Failed execution retaining both the native error and any rejected observation.
#[derive(Debug)]
pub struct LinkFailure {
    /// Destination identity used for the single attempted operation.
    pub target: ResolvedLinkTarget,
    /// Declared effects, which do not imply that they completed.
    pub effects: LinkEffects,
    /// Original structured failure, including uncertainty and operation IDs.
    pub error: MutationError,
    /// Original completed response if its endpoint-specific shape was invalid.
    pub observed: Option<ResourceOutcome>,
}

impl fmt::Display for LinkFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Link failed or is incomplete; inspect its retained operation evidence")
    }
}

impl std::error::Error for LinkFailure {}

/// Execute a prevalidated request once, with explicit host approval.
///
/// # Errors
/// A missing/failed/declined approval sends no request. Service, cancellation,
/// deadline and malformed-response failures preserve reconciliation evidence.
/// No branch replays a mutation, changes identity or rolls back a created link.
pub async fn execute(
    prepared: PreparedLink,
    operations: &mut dyn ResourceOperations,
    approval: Option<&mut dyn LinkApproval>,
) -> Result<LinkReceipt, LinkFailure> {
    let result = async {
        let approval = approval
            .ok_or_else(|| MutationError::new("ApprovalRequired", "Link requires host approval"))?;
        if !approval.approve(&prepared.target, prepared.effects).await? {
            return Err(MutationError::new(
                "ApprovalRequired",
                "Link was not approved",
            ));
        }
        operations
            .execute(prepared.operation.clone())
            .await
            .map_err(MutationError::from)
    }
    .await;
    let outcome = result.map_err(|error| LinkFailure {
        target: prepared.target.clone(),
        effects: prepared.effects,
        error,
        observed: None,
    })?;
    if !valid_completion(&prepared.target, &outcome) {
        let mut error = ResourceError::input();
        error.code = "InvalidResponse".into();
        error.message = "Link returned an invalid completed response".into();
        if let ResourceOutcome::Completed { status, .. } = &outcome {
            error.status = Some(*status);
        }
        error.mutation_may_have_occurred = true;
        return Err(LinkFailure {
            target: prepared.target,
            effects: prepared.effects,
            error: error.into(),
            observed: Some(outcome),
        });
    }
    Ok(LinkReceipt {
        target: prepared.target,
        effects: prepared.effects,
        outcome,
    })
}

fn valid_completion(target: &ResolvedLinkTarget, outcome: &ResourceOutcome) -> bool {
    let owner = target.owner;
    match outcome {
        ResourceOutcome::Accepted { .. } => true,
        ResourceOutcome::Completed { status, data } => {
            if owner == LinkOwner::SemanticModel {
                return *status == 200;
            }
            let expected_status = match owner {
                LinkOwner::Lakehouse => matches!(status, 200 | 201),
                LinkOwner::KqlDatabase | LinkOwner::DataAgent => *status == 201 || *status == 200,
                LinkOwner::SemanticModel | LinkOwner::Ontology => false,
            };
            let Some(data) = data.as_ref().filter(|value| value.is_object()) else {
                return false;
            };
            let identity_matches = match &target.scope {
                LinkScope::LakehouseShortcut { path, name } => {
                    data.get("path").and_then(Value::as_str) == Some(path.as_str())
                        && data.get("name").and_then(Value::as_str) == Some(name.as_str())
                }
                LinkScope::KqlTableShortcut { name } => {
                    data.get("name").and_then(Value::as_str) == Some(name.as_str())
                }
                LinkScope::DataAgentStagingDatasources => data
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| validate_link_id(id).is_ok()),
                LinkScope::SemanticModelConnections | LinkScope::OntologySchema => false,
            };
            expected_status && identity_matches
        }
    }
}

/// Resolve, read and validate one command before asking approval or sending a write.
///
/// # Errors
/// Enforces an explicit positive input bound, strict UTF-8/JSON and native schema.
/// The caller configures deadlines and cancellation on the shared executor.
pub async fn prepare_command(
    command: &LinkCommand,
    resolver: &mut dyn LinkResolver,
    files: &mut dyn LinkFiles,
    max_bytes: NonZeroUsize,
) -> Result<PreparedLink, MutationError> {
    let target = resolver.resolve(&command.path).await?;
    if command.facade.is_some_and(|facade| facade != target.owner) {
        return Err(unsupported(
            "The artifact facade does not match the resolved owner",
        ));
    }
    if target.owner == LinkOwner::Ontology {
        return Err(unsupported(
            "Ontology linking requires a representation-specific binding and concurrency adapter",
        ));
    }
    let bytes = files.read(&command.input, max_bytes).await?;
    if bytes.len() > max_bytes.get() {
        return Err(MutationError::new(
            "BudgetExceeded",
            "Link input exceeds the caller's byte limit",
        ));
    }
    let input =
        std::str::from_utf8(&bytes).map_err(|_| invalid("Link input must be strict UTF-8 JSON"))?;
    let input = parse_link_json(input)?;
    prepare(target, command.facade, &input, command.wait)
}

fn invalid(message: &str) -> MutationError {
    MutationError::new("InvalidInput", message)
}

fn unsupported(message: &str) -> MutationError {
    MutationError::new("UnsupportedCapability", message)
}
