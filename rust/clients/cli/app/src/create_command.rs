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
    FabricOperation, OperationError, OperationErrorKind, OperationExecutor, OperationLimits,
    OperationSession,
};
use futures_util::future::{Either, select};
use serde_json::{Value, json};
use std::{
    fs,
    future::Future,
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::Path,
    pin::{Pin, pin},
};

/// Bound for raw input, each discovery/operation response, and final receipt.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A positively selected native leaf, not a generic HTTP or plugin adapter.
pub trait CreateBinding: CreateInputValidator {
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
}

/// Immediate approval view borrowed from the exact executing plan and context.
pub struct CreateApprovalRequest<'a> {
    plan: &'a CreatePlan,
    context: &'a RequestContext,
    raw: Option<&'a [u8]>,
    command: &'a CreateCommand,
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
        !matches!(self.plan.action(), CreateAction::Skip { .. })
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
        let capabilities = binding.capabilities();
        // Validate against the explicit parent ID before any discovery.
        command
            .plan(
                target(&workspace, &name, &owner)?,
                Preflight::Leaf(LeafState::Missing),
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
        executor: E,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        if binding.item_type() != self.owner
            || binding.capabilities() != self.capabilities
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
                    query: Vec::new(),
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
        let mut matches = listing
            .value
            .iter()
            .filter(|item| item.display_name.eq_ignore_ascii_case(&self.name));
        let state = match (matches.next(), matches.next()) {
            (None, _) => LeafState::Missing,
            (Some(_), Some(_)) => LeafState::Ambiguous,
            (Some(item), None) => {
                if item.item_type() != Some(self.owner) {
                    LeafState::WrongKind
                } else if !is_id(&item.id)
                    || item
                        .additional_fields
                        .get("workspaceId")
                        .is_some_and(|id| id.as_str() != Some(workspace.as_str()))
                {
                    return Err(CreateCommandError::Invalid);
                } else {
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
        let operation = if matches!(plan.action(), CreateAction::Skip { .. }) {
            None
        } else {
            Some(
                binding
                    .operation(&plan, self.raw.as_deref(), self.command.wait)
                    .map_err(CreateCommandError::Policy)?,
            )
        };
        {
            let authorize = pin!(approval.authorize(CreateApprovalRequest {
                plan: &plan,
                context: client.context(),
                raw: self.raw.as_deref(),
                command: &self.command,
            }));
            match select(interrupted.as_mut(), authorize).await {
                Either::Left(_) => return Err(CreateCommandError::Interrupted),
                Either::Right((result, _)) => result.map_err(CreateCommandError::Approval)?,
            }
        }
        let report = if let Some(operation) = operation {
            let mut session = OperationSession::with_context(
                client.context(),
                executor,
                OperationLimits::default(),
                interrupted.as_mut(),
            );
            let result = session.execute(operation).await;
            CreateReport::observe(&plan, result, &session)
        } else {
            CreateReport::skipped(&plan)
        };
        Ok(CreateDelivery {
            command: self.command,
            output: self.output,
            report,
        })
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
                            return Self::failed(
                                plan,
                                Value::Null,
                                session.evidence(OperationErrorKind::InvalidResponse),
                            );
                        }
                    }
                };
                let valid_status = response.status == 202
                    || (response.status == 201 && session.operation_id().is_none())
                    || (response.status == 200 && session.operation_id().is_some());
                if !valid_status {
                    return Self::failed(
                        plan,
                        data,
                        session.evidence(OperationErrorKind::InvalidResponse),
                    );
                }
                (data, response.status, None)
            }
            Err(error) => return Self::failed(plan, Value::Null, error),
        };
        let evidence = evidence(&data, session.operation_id(), true);
        let outcome: ExecutionOutcome<OperationError> = if status == 202 {
            ExecutionOutcome::Accepted(evidence)
        } else {
            ExecutionOutcome::Completed(evidence)
        };
        let verified = sdk::verify_outcome(plan, outcome).is_ok();
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
        let native_failure = matches!(
            error.kind,
            OperationErrorKind::Http { .. } | OperationErrorKind::OperationFailed { .. }
        );
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

fn evidence(data: &Value, operation: Option<&str>, mutated: bool) -> ObservationEvidence {
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
            parent_id: id("workspaceId"),
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
