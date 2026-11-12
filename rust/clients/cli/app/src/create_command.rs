// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in create host. Typed bindings supply native guarantees; the SDK planner
//! remains the only policy engine. No identity, approval or transport is inferred.

use crate::get_target::{GetRepresentation, GetTarget, is_id};
pub use fabric_cli_mutations::v2_create::{self, CreateCommand};
use fabric_cli_mutations::v2_create::{InputSource, OutputDestination};
use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile, ExactOutputReceipt};
use fabric_sdk::{
    Audience, Client, RequestContext, ServiceOrigin,
    items::{
        ItemExecutor, ItemListLimits, ItemListOptions,
        create::{
            self as sdk, AcceptedHandle, CreateAction, CreateCapabilities, CreateError,
            CreateInputValidator, CreatePlan, CreateTarget, ExecutionOutcome, LeafState,
            MutationEffect, NativeId, ObservationEvidence, Preflight, ReplacementEvidence,
            ResourceIdentity, ResourceResult, TargetScope,
        },
    },
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use futures_util::future::{Either, select};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    future::Future,
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::Path,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
};

/// Bound for raw input, each discovery/operation response, and final receipt.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ANTICIPATED_EFFECTS: usize = 16;
const MAX_EFFECT_BYTES: usize = 64 * 1024;

/// Additional provider-native effects, not observed outcomes or permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CreateEffectKind {
    /// The one native request may implicitly create a provider-owned child.
    ChildResourceCreation,
    /// The provider operation may incur service-metered consumption.
    MeteredConsumption,
}

/// Bounded provider-contract disclosure for immediate approval and receipts.
///
/// These descriptors contain no child IDs or success claims. The typed binding
/// owns the evidence for every declared effect; the host never infers it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnticipatedCreateEffect {
    kind: CreateEffectKind,
    resource_type: Option<String>,
    description: String,
    contract: String,
}

impl AnticipatedCreateEffect {
    /// Validate a non-secret effect description and its provider contract pin.
    ///
    /// # Errors
    /// Rejects unbounded/control-bearing labels and missing child owner types.
    pub fn new(
        kind: CreateEffectKind,
        resource_type: Option<&str>,
        description: &str,
        contract: &str,
    ) -> Result<Self, CreateError> {
        let valid = |value: &str, maximum| {
            !value.trim().is_empty()
                && value.len() <= maximum
                && !value.chars().any(char::is_control)
        };
        if !valid(description, 2048)
            || !valid(contract, 2048)
            || resource_type.is_some_and(|kind| !valid(kind, 256))
            || (kind == CreateEffectKind::ChildResourceCreation && resource_type.is_none())
        {
            return Err(CreateError::InputRejected);
        }
        Ok(Self {
            kind,
            resource_type: resource_type.map(str::to_owned),
            description: description.into(),
            contract: contract.into(),
        })
    }

    /// The anticipated effect category, never its execution status.
    pub const fn kind(&self) -> CreateEffectKind {
        self.kind
    }
    /// Provider-owned child/resource type when applicable.
    pub fn resource_type(&self) -> Option<&str> {
        self.resource_type.as_deref()
    }
    /// Exact bounded disclosure, including any provider-owned condition.
    pub fn description(&self) -> &str {
        &self.description
    }
    /// Typed binding's provider contract reference/pin, not fetched by this host.
    pub fn contract(&self) -> &str {
        &self.contract
    }
}

/// Header evidence observed before an incomplete HTTP body was abandoned.
pub struct CreateTransportEvidence {
    /// Actual status of the latest mutation/poll exchange, if observed.
    pub http_status: Option<u16>,
    /// Provider operation UUID from that exchange, never a resource ID.
    pub operation_id: Option<String>,
}

/// One operation executor and its command-local, read-only evidence accessor.
///
/// Observe only this execution, not discovery or a previous invocation. The host
/// uses header evidence only to retain reconciliation data, never prove success.
pub struct CreateExecution<'a, E> {
    /// Existing operation engine adapter; no separate HTTP implementation.
    pub executor: E,
    /// Snapshot headers even if streaming fails or execution is cancelled.
    pub observe: &'a (dyn Fn() -> CreateTransportEvidence + Send + Sync),
}

/// A positively selected native leaf, not a generic HTTP or plugin adapter.
pub trait CreateBinding: CreateInputValidator + Send + Sync {
    /// Exact supported owner type, used before input, discovery or credentials.
    fn item_type(&self) -> &'static str;
    /// Provider-backed capabilities for this exact schema.
    fn capabilities(&self) -> CreateCapabilities;
    /// Build the native request from the immutable, SDK-validated plan.
    ///
    /// Explicit raw bytes must equal the validated input and remain unchanged.
    /// Only the selected action may execute, with no upsert or policy fallback.
    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError>;

    /// Disclose additional effects of the same single native create request.
    ///
    /// The default is only for bindings with no additional effects to disclose.
    /// A binding must reject unknown/unrepresentable required effects rather than
    /// silently omit them. No effect authorizes another HTTP mutation.
    ///
    /// # Errors
    /// Returns a typed local refusal when provider effect evidence is unavailable.
    fn anticipated_effects(
        &self,
        _plan: &CreatePlan,
    ) -> Result<Vec<AnticipatedCreateEffect>, CreateError> {
        Ok(Vec::new())
    }
}

/// Render the existing grammar with the selected binding's actual host subset.
///
/// This performs no input, credential, discovery or provider work. Help does not
/// enable capabilities that the shared execution host cannot implement.
pub fn command_for_binding(binding: &dyn CreateBinding) -> clap::Command {
    let capabilities = host_capabilities(binding);
    let mut policies = Vec::new();
    if capabilities.create == sdk::CreateSupport::RejectExisting {
        policies.push("error");
    }
    if capabilities.skip == sdk::SkipSupport::ExactExistingUnchanged {
        policies.push("skip");
    }
    let policies = if policies.is_empty() {
        "none".into()
    } else {
        policies.join(" and ")
    };
    v2_create::command().after_help(format!(
        "Selected native owner: {owner}. This mount requires \
         /<workspaceGUID>.Workspace/<literal-name>.{owner} and strict native \
         create JSON, not a local source file. Enabled policies: {policies}. \
         No overwrite, duplicate, collection, profile or artifact facade binding. \
         Explicit terminal approval is required; --force never bypasses it.",
        owner = binding.item_type(),
    ))
}

/// Immediate approval view borrowed from the exact executing plan and context.
pub struct CreateApprovalRequest<'a> {
    plan: &'a CreatePlan,
    context: &'a RequestContext,
    raw: Option<&'a [u8]>,
    command: &'a CreateCommand,
    operation: Option<&'a FabricOperation>,
    url: Option<&'a azure_core::http::Url>,
    anticipated_effects: &'a [AnticipatedCreateEffect],
}

impl CreateApprovalRequest<'_> {
    /// Exact resolved target, requested policy and chosen action.
    pub fn plan(&self) -> &CreatePlan {
        self.plan
    }
    /// Immutable endpoint/account/tenant/audience and injected credential.
    pub fn context(&self) -> &RequestContext {
        self.context
    }
    /// Original explicit input, never log it implicitly.
    pub fn raw_input(&self) -> Option<&[u8]> {
        self.raw
    }
    /// Exact wait, timeout, projection and output intent.
    pub fn command(&self) -> &CreateCommand {
        self.command
    }
    /// Whether this action may write remotely; skip asserts no content equality.
    pub fn remote_write(&self) -> bool {
        self.operation.is_some()
    }
    /// Borrows the exact already-built operation that will be consumed once.
    pub fn operation(&self) -> Option<&FabricOperation> {
        self.operation
    }
    /// The prepared route bound to the same immutable execution endpoint.
    pub fn url(&self) -> Option<&azure_core::http::Url> {
        self.url
    }
    /// Provider-declared anticipated effects, not proof they occurred.
    pub fn anticipated_effects(&self) -> &[AnticipatedCreateEffect] {
        self.anticipated_effects
    }
    /// Render immediate terminal approval with escaped exact wire bytes/context.
    ///
    /// The host explicitly invokes this sensitive-content view; Debug does not
    /// log it. Control characters/newlines are escaped, never terminal commands.
    pub fn approval_question(&self) -> String {
        let action = if self.remote_write() {
            "create"
        } else {
            "skip"
        };
        let request = match (self.operation, self.url) {
            (Some(operation), Some(url)) => {
                use fabric_sdk_workspaces::mutations::ResourceMethod;
                let method = match operation.method() {
                    ResourceMethod::Get => "GET",
                    ResourceMethod::Post => "POST",
                    ResourceMethod::Patch => "PATCH",
                    ResourceMethod::Delete => "DELETE",
                };
                format!(
                    "method={method}, url=\"{}\", body=\"{}\", completion={:?}",
                    url.as_str().escape_debug(),
                    std::str::from_utf8(operation.body())
                        .expect("prepared create body is validated UTF-8")
                        .escape_debug(),
                    operation.completion(),
                )
            }
            _ => "no native request (unchanged skip)".into(),
        };
        let effects = self
            .anticipated_effects
            .iter()
            .map(|effect| {
                format!(
                    "{{kind={:?}, resourceType={:?}, description=\"{}\", contract=\"{}\"}}",
                    effect.kind(),
                    effect.resource_type(),
                    effect.description().escape_debug(),
                    effect.contract().escape_debug(),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Approve {owner} {action} policy {policy} for \"{name}\" in workspace \"{workspace}\" \
             using account \"{account}\", tenant \"{tenant}\", audience={audience:?}, \
             endpoint=\"{endpoint}\", remoteWrite={write}, {request}, \
             anticipatedEffects=[{effects}], output={output:?}, project={project:?}, \
             wait={wait}, timeout={timeout:?}? \
             Anticipated effects are not observed outcomes. Acceptance may not be completion. \
             Local cancellation/output failure is not rollback; do not automatically replay.",
            owner = self.plan.target().item_type().escape_debug(),
            policy = self.plan.policy().as_str(),
            name = self.plan.target().name().escape_debug(),
            workspace = self.plan.target().parent_id().as_str().escape_debug(),
            account = self.context.selection().account().escape_debug(),
            tenant = self.context.selection().tenant().escape_debug(),
            audience = self.context.selection().audience(),
            endpoint = self.context.endpoint().base_url().as_str().escape_debug(),
            write = self.remote_write(),
            output = self.command.output,
            project = self.command.project,
            wait = self.command.wait,
            timeout = self.command.timeout,
        )
    }
}

/// Explicit host refusal; there is no force or default approval implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateApprovalError {
    /// The caller declined this precise request.
    Denied,
    /// The host cannot obtain immediate approval.
    Unavailable,
}

/// Caller-owned approval, cancellable under the original whole-command deadline.
pub trait CreateApproval {
    /// Authorize exactly this target/body/context/effect, not a reusable grant.
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>>;
}

/// Local/approval errors, preserving remote evidence after delivery failure.
#[derive(Debug)]
pub enum CreateCommandError {
    /// Input/output I/O failed.
    Io(io::Error),
    /// The exact no-clobber output could not be prepared or committed.
    File(ExactOutputError),
    /// Target, context, profile or bounded JSON was rejected.
    Invalid,
    /// Original SDK policy or typed binding error.
    Policy(CreateError),
    /// Bounded complete discovery did not establish the target.
    Discovery,
    /// Cancellation/deadline occurred before mutation.
    Interrupted,
    /// Immediate host approval was refused or unavailable.
    Approval(CreateApprovalError),
    /// Projection or delivery failed, retaining the original receipt.
    Delivery {
        /// Local failure, never proof of remote rollback.
        source: Box<CreateCommandError>,
        /// Unprojected receipt and typed remote error.
        report: Box<CreateReport>,
    },
}

impl std::fmt::Display for CreateCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("create failed; inspect retained local, policy and operation evidence")
    }
}
impl std::error::Error for CreateCommandError {}

/// Local immutable input/output, before any provider or credential work.
pub struct PreparedCreateCommand {
    command: CreateCommand,
    workspace: String,
    name: String,
    owner: &'static str,
    capabilities: CreateCapabilities,
    raw: Option<Vec<u8>>,
    input: Option<Value>,
    output: Option<ExactOutputFile>,
}

impl PreparedCreateCommand {
    /// Read bounded strict JSON from an exclusively owned stream or regular file.
    ///
    /// Profiles are not implemented by this explicit head and fail rather than
    /// silently selecting ambient identity. Type checks precede input reads.
    /// Output preflight creates nothing; commit uses the existing no-clobber sink.
    pub fn preflight(
        command: CreateCommand,
        binding: &dyn CreateBinding,
        cwd: &Path,
        stdin: &mut dyn Read,
        stdin_owned_elsewhere: bool,
    ) -> Result<Self, CreateCommandError> {
        if command.profile.is_some() {
            return Err(CreateCommandError::Invalid);
        }
        command
            .validate_stdin(stdin_owned_elsewhere)
            .map_err(|_| CreateCommandError::Invalid)?;
        let path = command
            .destination
            .to_str()
            .ok_or(CreateCommandError::Invalid)?;
        let GetTarget::Item {
            workspace,
            item: name,
            item_type: Some(owner),
            representation: GetRepresentation::Metadata,
        } = GetTarget::parse(path).map_err(|_| CreateCommandError::Invalid)?
        else {
            return Err(CreateCommandError::Invalid);
        };
        if owner != binding.item_type() || !is_id(&workspace) {
            return Err(CreateCommandError::Invalid);
        }
        let capabilities = host_capabilities(binding);
        sdk::validate_policy_support(command.policy, &capabilities)
            .map_err(CreateCommandError::Policy)?;
        if let Some(project) = &command.project {
            fabric_cli_query::validate_projection(project)
                .map_err(|_| CreateCommandError::Invalid)?;
        }
        let raw = command
            .input
            .as_ref()
            .map(|source| {
                let mut bytes = Vec::new();
                match source {
                    InputSource::Stdin => stdin
                        .take((MAX_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(CreateCommandError::Io)?,
                    InputSource::File(path) => {
                        let path = cwd.join(path);
                        let metadata =
                            fs::symlink_metadata(&path).map_err(CreateCommandError::Io)?;
                        if !metadata.is_file() || metadata.file_type().is_symlink() {
                            return Err(CreateCommandError::Invalid);
                        }
                        fs::File::open(path)
                            .map_err(CreateCommandError::Io)?
                            .take((MAX_BYTES + 1) as u64)
                            .read_to_end(&mut bytes)
                            .map_err(CreateCommandError::Io)?
                    }
                };
                if bytes.len() > MAX_BYTES {
                    return Err(CreateCommandError::Invalid);
                }
                Ok(bytes)
            })
            .transpose()?;
        let input = raw
            .as_deref()
            .map(|bytes| {
                fabric_cli_output::parse_data(
                    std::str::from_utf8(bytes).map_err(|_| CreateCommandError::Invalid)?,
                )
                .map_err(|_| CreateCommandError::Invalid)?;
                fabric_cli_output::input::decode_json_input(
                    Some(bytes),
                    "application/json",
                    NonZeroUsize::new(MAX_BYTES).unwrap(),
                )
                .map_err(|_| CreateCommandError::Invalid)
            })
            .transpose()?;
        // Validate against the explicit parent ID before any discovery.
        sdk::validate_create_request(
            command.policy,
            target(&workspace, &name, &owner)?,
            &capabilities,
            input.clone(),
            binding,
        )
        .map_err(CreateCommandError::Policy)?;
        let output = match &command.output {
            OutputDestination::Stdout => None,
            OutputDestination::File(path) => Some(
                ExactOutputFile::preflight(&cwd.join(path), MAX_BYTES)
                    .map_err(CreateCommandError::File)?,
            ),
        };
        Ok(Self {
            command,
            workspace,
            name,
            owner: binding.item_type(),
            capabilities,
            raw,
            input,
            output,
        })
    }

    /// Resolve bounded metadata, revalidate the exact plan, immediately approve,
    /// and submit at most once through the existing operation engine.
    ///
    /// `interrupted` covers the original total deadline starting before local
    /// preflight. The immutable client context is the same one used for discovery,
    /// approval, mutation and polling. No fallback, replay or rollback is installed.
    pub async fn execute<E: OperationExecutor>(
        self,
        binding: &dyn CreateBinding,
        client: &Client,
        items: &dyn ItemExecutor,
        execution: CreateExecution<'_, E>,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        if binding.item_type() != self.owner
            || host_capabilities(binding) != self.capabilities
            || client.context().selection().audience() != Audience::Fabric
            || client.context().endpoint().origin() != ServiceOrigin::Fabric
        {
            return Err(CreateCommandError::Invalid);
        }
        let mut interrupted = pin!(interrupted);
        let workspace = self.workspace.clone();
        let listing = client
            .list_items(
                items,
                &workspace,
                ItemListOptions {
                    query: vec![("recursive".into(), "false".into())],
                    limits: ItemListLimits {
                        max_pages: NonZeroUsize::new(10).unwrap(),
                        max_items: NonZeroUsize::new(1_000).unwrap(),
                        max_response_bytes: NonZeroUsize::new(MAX_BYTES).unwrap(),
                    },
                },
                interrupted.as_mut(),
            )
            .await
            .map_err(|_| CreateCommandError::Discovery)?;
        let matches = listing
            .value
            .iter()
            .filter(|item| item.display_name.eq_ignore_ascii_case(&self.name))
            .collect::<Vec<_>>();
        let mut root_matches = Vec::new();
        for item in &matches {
            if !is_id(&item.id)
                || item.item_type().is_none_or(|kind| kind.trim().is_empty())
                || item.additional_fields.get("workspaceId").is_some_and(|id| {
                    !id.as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(&workspace))
                })
            {
                return Err(CreateCommandError::Invalid);
            }
            if is_workspace_root(item.additional_fields.get("folderId"))? {
                root_matches.push(*item);
            }
        }
        if !matches.is_empty() && root_matches.is_empty() {
            return Err(CreateCommandError::Invalid);
        }
        let mut typed = root_matches
            .iter()
            .filter(|item| item.item_type() == Some(self.owner));
        let mut selected_folder = None;
        let state = match (typed.next(), typed.next()) {
            (None, _) if matches.is_empty() => LeafState::Missing,
            (None, _) => LeafState::WrongKind,
            (Some(_), Some(_)) => LeafState::Ambiguous,
            (Some(item), None) => {
                if item.display_name != self.name {
                    return Err(CreateCommandError::Policy(CreateError::NameChanged));
                } else {
                    selected_folder = item.additional_fields.get("folderId").cloned();
                    LeafState::Existing(
                        ResourceIdentity::new(
                            NativeId::new(&item.id).map_err(CreateCommandError::Policy)?,
                            NativeId::new(&workspace).map_err(CreateCommandError::Policy)?,
                            self.owner,
                        )
                        .map_err(CreateCommandError::Policy)?,
                    )
                }
            }
        };
        let plan = self
            .command
            .plan(
                target(&workspace, &self.name, self.owner)?,
                Preflight::Leaf(state),
                &self.capabilities,
                self.input.clone(),
                binding,
            )
            .map_err(CreateCommandError::Policy)?;
        let operation = match plan.action() {
            CreateAction::Skip { .. } => None,
            CreateAction::Create { .. } => Some(
                binding
                    .operation(&plan, self.raw.as_deref(), self.command.wait)
                    .map_err(CreateCommandError::Policy)?,
            ),
            CreateAction::Replace { .. } | CreateAction::Duplicate { .. } => {
                return Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy));
            }
        };
        let anticipated_effects = if operation.is_some() {
            let effects = binding
                .anticipated_effects(&plan)
                .map_err(CreateCommandError::Policy)?;
            if effects.len() > MAX_ANTICIPATED_EFFECTS
                || serde_json::to_vec(&effects)
                    .map_err(|_| CreateCommandError::Invalid)?
                    .len()
                    > MAX_EFFECT_BYTES
            {
                return Err(CreateCommandError::Invalid);
            }
            effects
        } else {
            Vec::new()
        };
        let url = operation
            .as_ref()
            .map(|operation| {
                if operation.method() != fabric_sdk_workspaces::mutations::ResourceMethod::Post
                    || operation.body().len() > MAX_BYTES
                    || std::str::from_utf8(operation.body()).is_err()
                {
                    return Err(CreateCommandError::Invalid);
                }
                client
                    .context()
                    .endpoint()
                    .url(operation.path())
                    .map_err(|_| CreateCommandError::Invalid)
            })
            .transpose()?;
        {
            let authorize = pin!(approval.authorize(CreateApprovalRequest {
                plan: &plan,
                context: client.context(),
                raw: self.raw.as_deref(),
                command: &self.command,
                operation: operation.as_ref(),
                url: url.as_ref(),
                anticipated_effects: &anticipated_effects,
            }));
            match select(interrupted.as_mut(), authorize).await {
                Either::Left(_) => return Err(CreateCommandError::Interrupted),
                Either::Right((result, _)) => result.map_err(CreateCommandError::Approval)?,
            }
        }
        let remote_write = operation.is_some();
        let mut report = if let Some(operation) = operation {
            let submission = Arc::new(Mutex::new(None));
            let mut session = OperationSession::with_context(
                client.context(),
                ObservedCreateExecutor {
                    inner: execution.executor,
                    submission: submission.clone(),
                    observe: execution.observe,
                },
                OperationLimits::default(),
                interrupted.as_mut(),
            );
            let result = session.execute(operation).await;
            let submission = submission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let mut report = CreateReport::observe(&plan, result, &session, submission.as_ref());
            report.retain_transport_evidence((execution.observe)(), submission.as_ref());
            report
        } else {
            CreateReport::skipped(&plan)
        };
        report.value["meta"]["remoteWrite"] = json!(remote_write);
        report.value["meta"]["anticipatedEffects"] = json!(anticipated_effects);
        if !remote_write {
            report.value["meta"]["folderIdObserved"] = json!(selected_folder.is_some());
            report.value["meta"]["folderId"] = json!(selected_folder);
        }
        Ok(CreateDelivery {
            command: self.command,
            output: self.output,
            report,
        })
    }
}

fn host_capabilities(binding: &dyn CreateBinding) -> CreateCapabilities {
    let mut capabilities = binding.capabilities();
    capabilities.replacement = None;
    capabilities.duplicate = sdk::DuplicateSupport::Unsupported;
    capabilities
}

fn is_workspace_root(folder: Option<&Value>) -> Result<bool, CreateCommandError> {
    match folder {
        None | Some(Value::Null) => Ok(true),
        Some(Value::String(id)) if is_id(id) => Ok(false),
        _ => Err(CreateCommandError::Invalid),
    }
}

struct SubmissionObservation {
    http_status: u16,
    operation_id: Option<String>,
    accepted_body: Option<Vec<u8>>,
    folder_id: Option<Value>,
}

struct ObservedCreateExecutor<'a, E> {
    inner: E,
    submission: Arc<Mutex<Option<SubmissionObservation>>>,
    observe: &'a (dyn Fn() -> CreateTransportEvidence + Send + Sync),
}

impl<E: OperationExecutor> OperationExecutor for ObservedCreateExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let initial =
                request.method() == fabric_sdk_workspaces::mutations::ResourceMethod::Post;
            let result = self.inner.execute(request).await;
            if initial {
                let observed = (self.observe)();
                let status = result
                    .as_ref()
                    .map(|response| response.status)
                    .ok()
                    .or(observed.http_status);
                if let Some(http_status) = status {
                    *self
                        .submission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(SubmissionObservation {
                            http_status,
                            operation_id: observed.operation_id.filter(|id| is_id(id)),
                            accepted_body: result
                                .as_ref()
                                .ok()
                                .filter(|response| {
                                    response.status == 202 && response.body.len() <= MAX_BYTES
                                })
                                .map(|response| response.body.clone()),
                            folder_id: result
                                .as_ref()
                                .ok()
                                .filter(|response| response.body.len() <= MAX_BYTES)
                                .and_then(|response| {
                                    fabric_cli_output::input::strict_json_bounded(
                                        &response.body,
                                        MAX_BYTES,
                                    )
                                    .ok()
                                })
                                .and_then(|body| body.get("folderId").cloned()),
                        });
                }
            }
            result
        })
    }

    fn wait(
        &self,
        delay: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.inner.wait(delay)
    }
}

fn target(workspace: &str, name: &str, owner: &str) -> Result<CreateTarget, CreateCommandError> {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(workspace).map_err(CreateCommandError::Policy)?,
        name,
        owner,
    )
    .map_err(CreateCommandError::Policy)
}

/// Unprojected receipt, retaining typed operation failure independently of JSON.
#[derive(Debug)]
pub struct CreateReport {
    value: Value,
    exit: u8,
    error: Option<OperationError>,
}

impl CreateReport {
    /// Original data and protected identity/handle/effect metadata.
    pub fn value(&self) -> &Value {
        &self.value
    }
    /// Submission/skip success is distinct from confirmed completion in metadata.
    pub const fn exit_code(&self) -> u8 {
        self.exit
    }
    /// Original typed remote failure, including service reconciliation evidence.
    pub fn error(&self) -> Option<&OperationError> {
        self.error.as_ref()
    }

    fn retain_transport_evidence(
        &mut self,
        evidence: CreateTransportEvidence,
        submission: Option<&SubmissionObservation>,
    ) {
        let fallback;
        let submission = if let Some(submission) = submission {
            Some(submission)
        } else if self
            .error
            .as_ref()
            .is_some_and(|error| error.mutation_may_have_occurred)
            && let Some(http_status) = evidence.http_status
        {
            // A cancelled first response may have delivered headers before its
            // executor future returned. No poll can occur before a saved POST.
            fallback = SubmissionObservation {
                http_status,
                operation_id: evidence.operation_id.clone().filter(|id| is_id(id)),
                accepted_body: None,
                folder_id: None,
            };
            Some(&fallback)
        } else {
            None
        };
        if let Some(submission) = submission {
            if submission.http_status == 202
                && let Some(error) = &mut self.error
                && error.mutation_may_have_occurred
                && error.operation_id.is_none()
                && let Some(id) = &submission.operation_id
            {
                self.value["meta"]["operationHandle"] = json!(format!("/operations/{id}"));
                error.operation_id = Some(id.clone());
            }
            self.value["meta"]["submission"] = json!({
                "httpStatus":submission.http_status,
                "operationHandle":self.value["meta"]["operationHandle"],
                "folderId":submission.folder_id,
                "folderIdObserved":submission.folder_id.is_some(),
            });
            if let Some(body) = &submission.accepted_body {
                match fabric_cli_output::input::strict_json_bounded(body, MAX_BYTES) {
                    Ok(data) => {
                        if let Some(object) = data.as_object() {
                            self.value["meta"]["submission"]["folderId"] =
                                json!(object.get("folderId"));
                            self.value["meta"]["submission"]["folderIdObserved"] =
                                json!(object.contains_key("folderId"));
                            let resource = json!({
                                "id":object.get("id"),"workspaceId":object.get("workspaceId"),
                                "type":object.get("type"),"displayName":object.get("displayName"),
                            });
                            self.value["meta"]["submission"]["resource"] = resource.clone();
                            if self.error.is_some() && self.value["data"].is_null() {
                                self.value["data"] = data;
                                self.value["meta"]["resourceId"] = resource["id"].clone();
                                self.value["meta"]["observedResource"] = resource;
                                self.value["meta"]["folderId"] = self.value["data"]
                                    .get("folderId")
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                self.value["meta"]["folderIdObserved"] =
                                    json!(self.value["data"].get("folderId").is_some());
                            }
                        }
                    }
                    Err(_) if !body.is_empty() => {
                        use base64::Engine;
                        self.value["meta"]["unparsedAcceptanceBodyBase64"] =
                            json!(base64::engine::general_purpose::STANDARD.encode(body));
                    }
                    Err(_) => {}
                }
            }
        }
        if let Some(status) = evidence.http_status {
            self.value["meta"]["observation"] = json!({"httpStatus":status});
            if self.value["meta"]["httpStatus"].is_null() {
                self.value["meta"]["httpStatus"] = json!(status);
            }
        }
    }

    fn skipped(plan: &CreatePlan) -> Self {
        let CreateAction::Skip { existing } = plan.action() else {
            unreachable!("only SDK skip plans are delivered unchanged");
        };
        let outcome: ExecutionOutcome<OperationError> = ExecutionOutcome::Skipped;
        let verified = sdk::verify_outcome(plan, outcome).is_ok();
        Self {
            value: json!({
                "data":null,
                "meta":{
                    "policy":plan.policy().as_str(),"action":"skip","status":"unchanged",
                    "completed":verified,"resourceId":existing.native_id().as_str(),
                    "workspaceId":existing.parent_id().as_str(),"type":existing.item_type(),
                    "operationHandle":null,"mutationMayHaveOccurred":false,
                    "remoteReplayed":false,"remoteCancelled":false,"contentEqualityClaimed":false
                }
            }),
            exit: u8::from(!verified),
            error: None,
        }
    }

    fn observe<E: OperationExecutor>(
        plan: &CreatePlan,
        result: Result<fabric_sdk_operations::OperationResponse, OperationError>,
        session: &OperationSession<'_, E>,
        submission: Option<&SubmissionObservation>,
    ) -> Self {
        let (data, status, mut error) = match result {
            Ok(response) => {
                let data = if response.body.is_empty() {
                    Value::Null
                } else {
                    match std::str::from_utf8(&response.body)
                        .ok()
                        .and_then(|text| fabric_cli_output::parse_data(text).ok())
                    {
                        Some(value) => value,
                        None => {
                            let mut report = Self::failed(
                                plan,
                                Value::Null,
                                session.evidence(OperationErrorKind::InvalidResponse),
                            );
                            use base64::Engine;
                            report.value["meta"]["httpStatus"] = json!(response.status);
                            report.value["meta"]["unparsedResponseBodyBase64"] = json!(
                                base64::engine::general_purpose::STANDARD.encode(&response.body)
                            );
                            return report;
                        }
                    }
                };
                let valid_status = response.status == 202
                    || (response.status == 201 && session.operation_id().is_none())
                    || (response.status == 200 && session.operation_id().is_some());
                if !valid_status {
                    let mut report = Self::failed(
                        plan,
                        data,
                        session.evidence(OperationErrorKind::InvalidResponse),
                    );
                    report.value["meta"]["httpStatus"] = json!(response.status);
                    return report;
                }
                (data, response.status, None)
            }
            Err(error) => {
                let accepted = submission
                    .and_then(|submission| submission.accepted_body.as_deref())
                    .filter(|bytes| !bytes.is_empty());
                let data = accepted
                    .and_then(|bytes| {
                        fabric_cli_output::input::strict_json_bounded(bytes, MAX_BYTES).ok()
                    })
                    .unwrap_or(Value::Null);
                let mut report = Self::failed(plan, data, error);
                if let Some(bytes) = accepted
                    && report.value["data"].is_null()
                {
                    use base64::Engine;
                    report.value["meta"]["unparsedAcceptanceBodyBase64"] =
                        json!(base64::engine::general_purpose::STANDARD.encode(bytes));
                }
                return report;
            }
        };
        let evidence = evidence(&data, session.operation_id(), true, plan);
        let outcome: ExecutionOutcome<OperationError> = if status == 202 {
            ExecutionOutcome::Accepted(evidence)
        } else {
            ExecutionOutcome::Completed(evidence)
        };
        let verified = sdk::verify_outcome(plan, outcome).is_ok()
            && (status == 202 || is_workspace_root(data.get("folderId")).is_ok_and(|root| root));
        if !verified {
            error = Some(session.evidence(OperationErrorKind::InvalidResponse));
        }
        let mut report = Self::base(
            plan,
            data,
            session.operation_id(),
            if !verified {
                "unknown"
            } else if status == 202 {
                "accepted"
            } else {
                "completed"
            },
            true,
        );
        report.value["meta"]["httpStatus"] = json!(status);
        if !verified {
            report.value["error"] = json!({"code":"InvalidResponse"});
        }
        report.exit = u8::from(!verified);
        report.error = error;
        report
    }

    fn failed(plan: &CreatePlan, data: Value, error: OperationError) -> Self {
        let native_failure = match &error.kind {
            OperationErrorKind::Http { status, .. } => !(200..300).contains(status),
            OperationErrorKind::OperationFailed { .. } => true,
            _ => false,
        };
        let mut report = Self::base(
            plan,
            data,
            error.operation_id.as_deref(),
            if native_failure { "failed" } else { "unknown" },
            error.mutation_may_have_occurred,
        );
        let code = match &error.kind {
            OperationErrorKind::Cancelled => "Cancelled",
            OperationErrorKind::Execution(
                fabric_sdk_operations::OperationExecutionError::TimedOut,
            ) => "TimedOut",
            OperationErrorKind::Execution(_) => "ExecutionFailed",
            OperationErrorKind::Http { status, .. } if (200..300).contains(status) => {
                "InvalidResponse"
            }
            OperationErrorKind::Http { .. } => "ServiceRejected",
            OperationErrorKind::OperationFailed { .. } => "OperationFailed",
            OperationErrorKind::Credential => "CredentialFailed",
            OperationErrorKind::Limit => "LimitExceeded",
            _ => "InvalidResponse",
        };
        report.value["error"] = json!({"code":code});
        match &error.kind {
            OperationErrorKind::Http { status, code, .. } => {
                report.value["meta"]["httpStatus"] = json!(status);
                report.value["meta"]["serviceCode"] = json!(code);
            }
            OperationErrorKind::OperationFailed { status, .. } => {
                report.value["meta"]["operationStatus"] = json!(status);
            }
            _ => {}
        }
        report.value["meta"]["serviceError"] = json!(error.raw_service_error);
        report.exit = 1;
        report.error = Some(error);
        report
    }

    fn base(
        plan: &CreatePlan,
        data: Value,
        operation: Option<&str>,
        status: &str,
        may_have_occurred: bool,
    ) -> Self {
        Self {
            value: json!({
                "meta":{
                    "policy":plan.policy().as_str(),"action":"create","status":status,
                    "completed":status=="completed",
                    "resourceId":data.get("id"),"workspaceId":plan.target().parent_id().as_str(),
                    "type":plan.target().item_type(),
                    "folderId":data.get("folderId"),
                    "folderIdObserved":data.get("folderId").is_some(),
                    "observedResource":data.as_object().map(|_| json!({
                        "id":data.get("id"),"workspaceId":data.get("workspaceId"),
                        "type":data.get("type"),"displayName":data.get("displayName")
                    })),
                    "operationHandle":operation.map(|id|format!("/operations/{id}")),
                    "mutationMayHaveOccurred":may_have_occurred,
                    "remoteReplayed":false,"remoteCancelled":false
                },
                "data":data
            }),
            exit: 0,
            error: None,
        }
    }
}

fn evidence(
    data: &Value,
    operation: Option<&str>,
    mutated: bool,
    plan: &CreatePlan,
) -> ObservationEvidence {
    let id = |field| {
        data.get(field)
            .and_then(Value::as_str)
            .filter(|value| is_id(value))
            .and_then(|value| NativeId::new(value).ok())
    };
    ObservationEvidence {
        operation_handle: operation
            .and_then(|id| AcceptedHandle::new(format!("/operations/{id}")).ok()),
        resource: data.as_object().map(|_| ResourceResult {
            native_id: id("id"),
            parent_id: id("workspaceId").map(|id| {
                if id
                    .as_str()
                    .eq_ignore_ascii_case(plan.target().parent_id().as_str())
                {
                    plan.target().parent_id().clone()
                } else {
                    id
                }
            }),
            item_type: data.get("type").and_then(Value::as_str).map(str::to_owned),
            name: data
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            replacement: ReplacementEvidence::Unverified,
        }),
        effect: if mutated {
            MutationEffect::MayHaveOccurred
        } else {
            MutationEffect::NotOccurred
        },
    }
}

/// Consumed once by local delivery; a writer failure retains the original report.
pub struct CreateDelivery {
    command: CreateCommand,
    output: Option<ExactOutputFile>,
    report: CreateReport,
}

impl CreateDelivery {
    /// Retain the report before attempting projection or file/stdout delivery.
    pub fn report(&self) -> &CreateReport {
        &self.report
    }
    /// Project only data. Never replay a mutation on projection/write/flush error.
    pub fn deliver(
        self,
        stdout: &mut dyn Write,
    ) -> Result<Option<ExactOutputReceipt>, CreateCommandError> {
        let mut value = self.report.value.clone();
        let result = (|| {
            if let Some(project) = &self.command.project {
                value["data"] = fabric_cli_query::project_value(&value["data"], project)
                    .map_err(|_| CreateCommandError::Invalid)?;
            }
            let mut bytes = serde_json::to_vec(&value)
                .map_err(io::Error::other)
                .map_err(CreateCommandError::Io)?;
            bytes.push(b'\n');
            if bytes.len() > MAX_BYTES {
                return Err(CreateCommandError::Invalid);
            }
            match self.output {
                Some(output) => output
                    .commit(&bytes)
                    .map(Some)
                    .map_err(CreateCommandError::File),
                None => {
                    stdout.write_all(&bytes).map_err(CreateCommandError::Io)?;
                    stdout.flush().map_err(CreateCommandError::Io)?;
                    Ok(None)
                }
            }
        })();
        result.map_err(|source| CreateCommandError::Delivery {
            source: Box::new(source),
            report: Box::new(self.report),
        })
    }
}
