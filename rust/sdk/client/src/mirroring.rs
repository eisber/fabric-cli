// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host-neutral, explicitly approved MirroredDatabase START.
//!
//! The Python state gate is pinned to `8ad4c703`: only Stopped and Initialized
//! permit START. Unlike that source's incomplete-probe path, this v2 adapter
//! fails closed. The status probe is advisory, not an atomic server precondition.
//! A documented HTTP 200 completes the START action, not replication or catch-up.
//! No polling, job submission, STOP, rollback, retry or identity fallback exists.

use crate::{Audience, Client, RequestContext, ServiceOrigin, v2};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::{
    FabricServiceError,
    mutations::{CompletionPolicy, ResourceMethod},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    time::Duration,
};
use v2::{
    ApprovalPolicy, ApprovedOperation, Availability, Binding, ContractError, Coverage, Effect,
    ExecutionEvidence, ExecutionReceipt, FailureInfo, Generation, ItemIdentity,
    OperationDescriptor, OperationHandle, Outcome, Precondition, PreparedOperation, Prerequisite,
    ReceiptError, Registry, ResolvedTarget, SourceEvidence, Target, TargetIdentity, TargetKind,
    Verb,
};

/// Stable identity for this single state-probed native START action.
pub const OPERATION_ID: &str = "mirroredDatabase.mirroring.start";

/// The probe reads remote state; the separately approved START writes remote state.
pub const EFFECTS: [Effect; 2] = [Effect::RemoteRead, Effect::RemoteWrite];

/// Register exactly the MirroredDatabase runtime/mirroring START binding.
///
/// Hosts verify item read/write permission, delegated scopes when applicable,
/// supported user/service-principal/managed identity and tenant policy. Force or
/// approval cannot bypass the SDK's state gate.
///
/// # Errors
/// Preserves duplicate/conflicting registration errors.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    registry.register(OperationDescriptor::new(
        OPERATION_ID,
        Verb::Start,
        json!({"type": "object", "additionalProperties": false, "properties": {}}),
        json!({
            "type": "object",
            "required": ["preflightState", "actionCompleted", "replicationState"],
            "additionalProperties": false,
            "properties": {
                "preflightState": {"enum": ["Stopped", "Initialized"]},
                "actionCompleted": {"const": true},
                "replicationState": {"type": "null"}
            }
        }),
        EFFECTS.to_vec(),
    )?)?;
    registry.bind(Binding::new(
        "MirroredDatabase",
        "runtime",
        "mirroring",
        Generation::Any,
        OPERATION_ID,
        Availability::Conditional(vec![
            Prerequisite {
                id: "mirroring.readWrite".into(),
                description: "MirroredDatabase read/write permission and delegated MirroredDatabase.ReadWrite.All or Item.ReadWrite.All when applicable".into(),
            },
            Prerequisite {
                id: "mirroring.identity".into(),
                description: "Supported user, service principal or managed identity and tenant policy; no credential or consent fallback".into(),
            },
        ]),
    )?)
}

/// Return local registration metadata without credentials, resolution or I/O.
///
/// # Errors
/// Preserves invalid contract metadata errors.
pub fn schemas() -> Result<Value, ContractError> {
    let mut registry = Registry::default();
    register(&mut registry)?;
    Ok(json!({
        "operations": registry.operations().collect::<Vec<_>>(),
        "bindings": registry.bindings()
    }))
}

/// Resolve one exact ID-qualified runtime/mirroring path without service I/O.
///
/// Only `/<workspace UUID>.Workspace/<item UUID>.MirroredDatabase/runtime/mirroring`
/// is accepted. Names, aliases, relative paths, traversal, queries and bulk
/// selectors are deliberately unsupported.
///
/// # Errors
/// Rejects every other path grammar and malformed UUIDs.
pub fn target(path: &str) -> Result<ResolvedTarget, ReceiptError> {
    if path.len() > 128 {
        return Err(ReceiptError::InvalidIdentity);
    }
    let segments: Vec<_> = path.split('/').collect();
    let ["", workspace, item, "runtime", "mirroring"] = segments.as_slice() else {
        return Err(ReceiptError::InvalidIdentity);
    };
    let workspace = workspace
        .strip_suffix(".Workspace")
        .ok_or(ReceiptError::InvalidIdentity)?;
    let item = item
        .strip_suffix(".MirroredDatabase")
        .ok_or(ReceiptError::InvalidIdentity)?;
    ResolvedTarget::new(
        path,
        ItemIdentity::new(workspace, item)?,
        Target {
            kind: TargetKind::Item,
            owner_type: "MirroredDatabase".into(),
            facet: "runtime".into(),
            representation: "mirroring".into(),
            generation: None,
        },
        None,
    )
}

/// Prepare immutable START intent bound to the client's actual request context.
///
/// The canonical path and resolver-provided identity must agree exactly. No
/// credential, status request or approval occurs during preparation.
///
/// # Errors
/// Rejects noncanonical/wrong-owner targets and non-Fabric request contexts.
pub fn prepare(client: &Client, resolved: ResolvedTarget) -> Result<PreparedStart, ReceiptError> {
    if target(resolved.canonical_path())? != resolved {
        return Err(ReceiptError::TargetMismatch);
    }
    if client.selection().audience() != Audience::Fabric
        || client.context().endpoint().origin() != ServiceOrigin::Fabric
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    let mut registry = Registry::default();
    register(&mut registry).map_err(ReceiptError::Contract)?;
    let resolution = registry
        .resolve(Verb::Start, resolved.selection(), None)
        .map_err(ReceiptError::Contract)?;
    let prepared =
        PreparedOperation::new(resolution, resolved, json!({}))?.with_preconditions(vec![
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency,
        ])?;
    Ok(PreparedStart {
        prepared,
        context: client.context().clone(),
    })
}

/// Immutable intent retaining endpoint, account, tenant and credential provenance.
pub struct PreparedStart {
    prepared: PreparedOperation,
    context: RequestContext,
}

impl PreparedStart {
    /// Return the exact neutral intent presented to host policy.
    pub fn operation(&self) -> &PreparedOperation {
        &self.prepared
    }

    /// Return the bound context without acquiring a token.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Obtain immediate host approval for this exact intent and context.
    ///
    /// # Errors
    /// Preserves the host's refusal without credentials or service dispatch.
    pub fn authorize<P: ApprovalPolicy>(
        &self,
        policy: &mut P,
    ) -> Result<ApprovedStart<'_>, P::Error> {
        Ok(ApprovedStart {
            approved: self.prepared.authorize(policy)?,
            context: &self.context,
        })
    }
}

impl fmt::Debug for PreparedStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedStart(<bound intent>)")
    }
}

/// Opaque, consumed approval; an unbound neutral grant cannot authorize START.
pub struct ApprovedStart<'a> {
    approved: ApprovedOperation<'a>,
    context: &'a RequestContext,
}

impl fmt::Debug for ApprovedStart<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApprovedStart(<bound intent>)")
    }
}

/// The only preflight states permitting START under the pinned Python contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum PreflightState {
    /// Mirroring has stopped.
    Stopped,
    /// Mirroring is initialized but has not started.
    Initialized,
}

/// Action completion only; no after-state is inferred or fabricated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartData {
    /// Advisory state read before dispatch, not an atomic concurrency guard.
    pub preflight_state: PreflightState,
    /// True only for the documented empty HTTP 200 action response.
    pub action_completed: bool,
    /// Always absent: this action does not establish Running or caught-up state.
    pub replication_state: Option<String>,
}

/// The native phase to which failure/uncertainty belongs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StartPhase {
    /// A semantic read, despite its native POST method.
    Preflight,
    /// The one possible mutating START dispatch.
    Start,
}

/// Bounded native evidence, never automatically serialized or displayed.
pub struct StartEvidence {
    /// The approved canonical target, retained even if receipt construction fails.
    pub target: ResolvedTarget,
    /// Exact preflight response when within the evidence byte bound.
    pub preflight_response: Option<OperationResponse>,
    /// Exact START response when within the evidence byte bound.
    pub response: Option<OperationResponse>,
    /// Typed error with semantic effect evidence for the identified phase.
    pub operation_error: Option<Box<OperationError>>,
    /// Original shared error for a POST probe, before correcting its method-based
    /// mutation flag and removing any probe-only generic operation ID.
    pub original_preflight_error: Option<Box<OperationError>>,
    /// Identifies the failed phase without conflating a POST read with START.
    pub error_phase: Option<StartPhase>,
}

impl fmt::Debug for StartEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartEvidence(<retained native evidence>)")
    }
}

/// Protected receipt plus original bounded evidence outside projected result data.
pub struct StartExecution {
    /// Common v2 envelope, safe for normal receipt serialization.
    pub receipt: ExecutionReceipt<StartData>,
    /// Typed native failure; preflight uncertainty is never a START mutation.
    pub operation_error: Option<Box<OperationError>>,
    /// Exact bounded preflight response, including otherwise unprojected errors.
    pub preflight_response: Option<OperationResponse>,
    /// Exact bounded START response, including undocumented acceptance headers.
    pub response: Option<OperationResponse>,
    /// Original shared POST-probe error, retained before semantic normalization.
    pub original_preflight_error: Option<Box<OperationError>>,
    /// Identifies the phase for the retained failure, when present.
    pub error_phase: Option<StartPhase>,
}

impl fmt::Debug for StartExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartExecution(<protected receipt and native evidence>)")
    }
}

/// Invalid local approval or contradictory receipt; native failures use receipts.
pub enum StartError {
    /// Rejected before credential acquisition or service dispatch.
    Input(ReceiptError),
    /// Shared receipt construction failed without discarding executed evidence.
    Receipt {
        /// Original result and receipt-validation failure.
        cause: Box<v2::ReceiptBuildError<StartData>>,
        /// Native responses and typed errors retained independently.
        evidence: Box<StartEvidence>,
    },
    /// Shared source validation failed without discarding executed evidence.
    Source {
        /// Original receipt and source observations.
        cause: Box<v2::SourceEvidenceError<StartData>>,
        /// Native responses and typed errors retained independently.
        evidence: Box<StartEvidence>,
    },
}

impl StartError {
    /// Project retained execution facts into a safe local-error envelope.
    ///
    /// Receipt/source failures retain the original outcome, canonical target,
    /// common effect/status/handle/coverage evidence and phase. Diagnostics are
    /// fixed SDK text; raw responses, provider messages and rejected source
    /// details are never projected. This is diagnostic evidence, not a validated
    /// execution receipt. Input failures explicitly have no executed evidence.
    pub fn evidence(&self) -> Value {
        let (kind, code, target, outcome, execution, native) = match self {
            Self::Input(_) => {
                return json!({
                    "kind": "input",
                    "operationId": OPERATION_ID,
                    "target": null,
                    "outcome": null,
                    "evidence": null,
                    "phase": null,
                    "diagnostic": {
                        "code": "mirroring_start_input_invalid",
                        "message": "Mirroring START approval or request context is invalid; no service dispatch was attempted"
                    }
                });
            }
            Self::Receipt { cause, evidence } => (
                "receipt",
                "mirroring_start_receipt_invalid",
                &evidence.target,
                &cause.outcome,
                &cause.evidence,
                evidence,
            ),
            Self::Source { cause, evidence } => (
                "source",
                "mirroring_start_source_invalid",
                cause.receipt.target(),
                cause.receipt.outcome(),
                cause.receipt.evidence(),
                evidence,
            ),
        };
        json!({
            "kind": kind,
            "operationId": OPERATION_ID,
            "target": target,
            "outcome": outcome,
            "evidence": execution,
            "phase": native.error_phase,
            "diagnostic": {
                "code": code,
                "message": "Mirroring START executed evidence could not be published as a validated receipt"
            }
        })
    }

    /// Return safe common execution evidence when receipt validation failed.
    ///
    /// Hosts can serialize this in a local error envelope without exposing raw
    /// provider messages or losing possible mutation effects and actual handles.
    /// Rejected evidence is diagnostic, not a successfully validated receipt.
    /// Input failures return `None` because no service dispatch was attempted.
    pub fn execution_evidence(&self) -> Option<&ExecutionEvidence> {
        match self {
            Self::Input(_) => None,
            Self::Receipt { cause, .. } => Some(&cause.evidence),
            Self::Source { cause, .. } => Some(cause.receipt.evidence()),
        }
    }
}

impl fmt::Debug for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartError(<retained evidence>)")
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mirroring START intent or receipt is invalid")
    }
}

impl std::error::Error for StartError {}

struct RecordingExecutor<'a, E> {
    inner: &'a E,
    response: &'a Mutex<Option<OperationResponse>>,
    max_bytes: usize,
}

impl<E: OperationExecutor> OperationExecutor for RecordingExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let response = self.inner.execute(request).await?;
            let bytes = response
                .headers
                .iter()
                .try_fold(response.body.len(), |bytes, (name, value)| {
                    bytes.checked_add(name.len())?.checked_add(value.len())
                });
            if bytes.is_some_and(|bytes| bytes <= self.max_bytes) {
                *self.response.lock().unwrap() = Some(response.clone());
            } else {
                return Err(OperationExecutionError::ResponseLimit);
            }
            Ok(response)
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.inner.wait(delay)
    }
}

struct PhaseExecution {
    result: Result<OperationResponse, OperationError>,
    response: Option<OperationResponse>,
    evidence: OperationError,
}

async fn request<E: OperationExecutor>(
    client: &Client,
    executor: &E,
    path: &str,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> PhaseExecution {
    let response = Mutex::new(None);
    let mut session = OperationSession::with_context(
        client.context(),
        RecordingExecutor {
            inner: executor,
            response: &response,
            max_bytes: limits.max_response_bytes.get(),
        },
        limits,
        cancelled,
    );
    let result = match FabricOperation::new(
        ResourceMethod::Post,
        path,
        Vec::new(),
        CompletionPolicy::ReturnAccepted,
    ) {
        Ok(operation) => session.execute(operation).await,
        Err(kind) => Err(session.evidence(kind)),
    };
    let evidence = session.evidence(OperationErrorKind::InvalidResponse);
    drop(session);
    PhaseExecution {
        result,
        response: response.into_inner().unwrap(),
        evidence,
    }
}

#[derive(Deserialize)]
struct StatusResponse {
    status: String,
    #[serde(default)]
    error: Option<Value>,
}

fn preflight(response: &OperationResponse) -> Result<PreflightState, OperationErrorKind> {
    if response.status != 200 {
        return Err(OperationErrorKind::InvalidResponse);
    }
    let status: StatusResponse =
        serde_json::from_slice(&response.body).map_err(|_| OperationErrorKind::InvalidResponse)?;
    if let Some(error) = status.error {
        let error: FabricServiceError =
            serde_json::from_value(error).map_err(|_| OperationErrorKind::InvalidResponse)?;
        return Err(OperationErrorKind::OperationFailed {
            status: "PreflightError".into(),
            error: Some(Box::new(error)),
        });
    }
    match status.status.as_str() {
        "Stopped" => Ok(PreflightState::Stopped),
        "Initialized" => Ok(PreflightState::Initialized),
        "Paused" | "Initializing" | "Starting" | "Running" | "Stopping" | "Failed" => {
            Err(OperationErrorKind::OperationFailed {
                status: "StatePrecondition".into(),
                error: None,
            })
        }
        _ => Err(OperationErrorKind::InvalidResponse),
    }
}

fn failure(code: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: "Mirroring START failed or remains unestablished; inspect retained evidence"
            .into(),
    }
}

fn known_state(response: Option<&OperationResponse>) -> Option<&'static str> {
    let value: Value = serde_json::from_slice(&response?.body).ok()?;
    match value.get("status")?.as_str()? {
        "Stopped" => Some("Stopped"),
        "Initialized" => Some("Initialized"),
        "Paused" => Some("Paused"),
        "Initializing" => Some("Initializing"),
        "Starting" => Some("Starting"),
        "Running" => Some("Running"),
        "Stopping" => Some("Stopping"),
        "Failed" => Some("Failed"),
        _ => None,
    }
}

fn record_error(evidence: &mut StartEvidence, mut error: OperationError, phase: StartPhase) {
    if phase == StartPhase::Preflight {
        evidence.original_preflight_error = Some(Box::new(error.clone()));
        error.mutation_may_have_occurred = false;
        error.operation_id = None;
    }
    evidence.error_phase = Some(phase);
    evidence.operation_error = Some(Box::new(error));
}

fn unknown(error: &OperationError) -> bool {
    error.mutation_may_have_occurred
        && !matches!(
            error.kind,
            OperationErrorKind::Http {
                status: 400 | 401 | 403 | 404 | 409 | 429,
                ..
            }
        )
}

fn classified_failure(error: &OperationError, phase: Option<StartPhase>) -> FailureInfo {
    if unknown(error) {
        return failure("mirroring_start_outcome_unknown");
    }
    let code = match &error.kind {
        OperationErrorKind::Http { status: 400, .. } => "mirroring_bad_request",
        OperationErrorKind::Http { status: 401, .. } => "mirroring_unauthorized",
        OperationErrorKind::Http { status: 403, .. } => "mirroring_forbidden",
        OperationErrorKind::Http { status: 404, .. } => "mirroring_not_found",
        OperationErrorKind::Http { status: 409, .. } => "mirroring_conflict",
        OperationErrorKind::Http { status: 429, .. } => "mirroring_rate_limited",
        OperationErrorKind::OperationFailed { status, .. } if status == "StatePrecondition" => {
            "mirroring_state_prohibits_start"
        }
        OperationErrorKind::OperationFailed { .. } => "mirroring_preflight_service_error",
        OperationErrorKind::InvalidResponse => "mirroring_preflight_invalid_response",
        OperationErrorKind::Cancelled => "mirroring_cancelled_before_start",
        OperationErrorKind::Credential => "mirroring_credential_failed",
        OperationErrorKind::Limit => "mirroring_limit",
        _ if phase == Some(StartPhase::Preflight) => "mirroring_preflight_failed",
        _ => "mirroring_start_failed",
    };
    failure(code)
}

fn unique_header<'a>(
    response: &'a OperationResponse,
    name: &str,
) -> Result<Option<&'a str>, OperationErrorKind> {
    let mut values = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = values.next().map(|(_, value)| value.as_str());
    if values.next().is_some() {
        return Err(OperationErrorKind::InvalidResponse);
    }
    Ok(value)
}

fn start_handle(
    context: &RequestContext,
    response: Option<&OperationResponse>,
    error: Option<&OperationError>,
) -> Option<OperationHandle> {
    let response = response?;
    if response.status != 202 {
        return None;
    }
    // The shared session validates UUID syntax, but can infer an ID from a
    // Location before rejecting its origin/path. Require independent provenance.
    let id = error?.operation_id.as_ref()?;
    match unique_header(response, "x-ms-operation-id").ok()? {
        Some(header) if header == id.as_str() => {}
        Some(_) => return None,
        None => {
            let location = unique_header(response, "Location").ok()??;
            context
                .endpoint()
                .validate_link(location, &format!("/v1/operations/{id}"))
                .ok()?;
        }
    }
    Some(OperationHandle::FabricOperation { id: id.clone() })
}

fn finish(
    approved: &ApprovedStart<'_>,
    state: Option<PreflightState>,
    evidence: StartEvidence,
) -> Result<StartExecution, StartError> {
    let error = evidence.operation_error.as_deref();
    let outcome = match error {
        Some(error) if unknown(error) => Outcome::Unknown {
            error: classified_failure(error, evidence.error_phase),
        },
        Some(error) => Outcome::Failed {
            error: classified_failure(error, evidence.error_phase),
        },
        None => Outcome::Succeeded {
            data: StartData {
                preflight_state: state.expect("successful START retains allowed preflight state"),
                action_completed: true,
                replication_state: None,
            },
        },
    };
    let safe_error = match &outcome {
        Outcome::Failed { error } | Outcome::Unknown { error } => Some(error.clone()),
        _ => None,
    };
    let coverage = if matches!(outcome, Outcome::Unknown { .. })
        || evidence.error_phase == Some(StartPhase::Preflight)
    {
        Coverage::Incomplete {
            reasons: vec![
                "A permitted preflight and completed START were not both established".into(),
            ],
            continuation: None,
        }
    } else {
        Coverage::Complete
    };
    let http_status = if evidence.error_phase == Some(StartPhase::Preflight) {
        evidence.preflight_response.as_ref()
    } else {
        evidence.response.as_ref()
    }
    .map(|response| response.status)
    .or_else(|| {
        error.and_then(|error| match error.kind {
            OperationErrorKind::Http { status, .. } => Some(status),
            _ => None,
        })
    });
    let mut sources = vec![SourceEvidence {
        source: "mirroredDatabase.mirroring.start.preflight".into(),
        http_status: evidence
            .preflight_response
            .as_ref()
            .map(|response| response.status),
        coverage: if state.is_some() {
            Coverage::Complete
        } else {
            Coverage::Incomplete {
                reasons: vec!["No permitted preflight state was established".into()],
                continuation: None,
            }
        },
        error: (evidence.error_phase == Some(StartPhase::Preflight))
            .then(|| safe_error.clone())
            .flatten(),
        details: Some(json!({
            "semanticEffect": "remoteRead",
            "state": known_state(evidence.preflight_response.as_ref())
        })),
    }];
    if state.is_some() {
        sources.push(SourceEvidence {
            source: OPERATION_ID.into(),
            http_status,
            coverage: coverage.clone(),
            error: safe_error,
            details: None,
        });
    }
    let receipt = match ExecutionReceipt::new(
        &approved.approved,
        outcome,
        ExecutionEvidence {
            http_status,
            // Actual START acceptance IDs remain evidence, never authority to
            // poll an endpoint that does not advertise an LRO contract.
            handles: if evidence.error_phase == Some(StartPhase::Start) {
                start_handle(approved.context, evidence.response.as_ref(), error)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            },
            coverage,
            mutation_may_have_occurred: evidence.error_phase != Some(StartPhase::Preflight)
                && error.is_none_or(|error| error.mutation_may_have_occurred),
        },
    ) {
        Ok(receipt) => receipt,
        Err(cause) => {
            return Err(StartError::Receipt {
                cause,
                evidence: Box::new(evidence),
            });
        }
    };
    let receipt = match receipt.with_source_evidence(sources) {
        Ok(receipt) => receipt,
        Err(cause) => {
            return Err(StartError::Source {
                cause,
                evidence: Box::new(evidence),
            });
        }
    };
    Ok(StartExecution {
        receipt,
        operation_error: evidence.operation_error,
        preflight_response: evidence.preflight_response,
        response: evidence.response,
        original_preflight_error: evidence.original_preflight_error,
        error_phase: evidence.error_phase,
    })
}

/// Probe once and dispatch at most one approved, bodyless START.
///
/// The bound client cannot be replaced with another endpoint, credential provider
/// or account/tenant selection. One latched cancellation signal spans both phases;
/// the probe and action share the caller's total request budget. A failed,
/// malformed, non-200 or incomplete probe blocks START. Concurrent native failures
/// are retained, never converted to success based on the earlier advisory status.
/// Undocumented 202 responses remain Unknown, without polling or invented handles.
/// Actual operation-ID headers and Location-only IDs whose links match the
/// selected endpoint's exact operation route are retained as evidence, not LRO
/// authority. An ID inferred from a rejected Location is not a valid handle.
///
/// # Errors
/// Only invalid local approval or contradictory receipt evidence returns
/// [`StartError`]. Native failures/cancellation return [`StartExecution`] with
/// Failed or Unknown receipts and typed errors outside normal serialization.
///
/// Neutral approval cannot be substituted for context-bound START approval:
/// ```compile_fail
/// use fabric_sdk::{Client, mirroring, v2::ApprovedOperation};
/// use fabric_sdk_operations::{OperationExecutor, OperationLimits};
/// async fn unbound<E: OperationExecutor>(
///     client: &Client, executor: &E, grant: ApprovedOperation<'_>, limits: OperationLimits,
/// ) {
///     mirroring::execute(client, grant, executor, limits, std::future::pending()).await;
/// }
/// ```
pub async fn execute<E: OperationExecutor>(
    client: &Client,
    approved: ApprovedStart<'_>,
    executor: &E,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<StartExecution, StartError> {
    let context = client.context();
    if context.endpoint() != approved.context.endpoint()
        || context.selection() != approved.context.selection()
        || !Arc::ptr_eq(context.credential(), approved.context.credential())
    {
        return Err(StartError::Input(ReceiptError::InvalidEvidence));
    }
    let TargetIdentity::Item { identity } = approved.approved.prepared().target().identity() else {
        return Err(StartError::Input(ReceiptError::TargetMismatch));
    };
    let base = format!(
        "/v1/workspaces/{}/mirroredDatabases/{}",
        identity.workspace_id(),
        identity.item_id()
    );
    let mut cancelled = pin!(cancelled);
    let mut observed = false;
    let mut cancelled = pin!(poll_fn(|cx| {
        if observed {
            return std::task::Poll::Ready(());
        }
        let result = cancelled.as_mut().poll(cx);
        observed = result.is_ready();
        result
    }));
    let probe = request(
        client,
        executor,
        &format!("{base}/getMirroringStatus"),
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            ..limits
        },
        cancelled.as_mut(),
    )
    .await;
    let mut evidence = StartEvidence {
        target: approved.approved.prepared().target().clone(),
        preflight_response: probe.response,
        response: None,
        operation_error: None,
        original_preflight_error: None,
        error_phase: None,
    };
    let state = match probe.result {
        Ok(response) => match preflight(&response) {
            Ok(state) => state,
            Err(kind) => {
                let mut error = OperationError {
                    kind,
                    ..probe.evidence
                };
                error.raw_service_error = serde_json::from_slice::<Value>(&response.body)
                    .ok()
                    .and_then(|value| value.get("error").cloned())
                    .map(Box::new);
                record_error(&mut evidence, error, StartPhase::Preflight);
                return finish(&approved, None, evidence);
            }
        },
        Err(error) => {
            record_error(&mut evidence, error, StartPhase::Preflight);
            return finish(&approved, None, evidence);
        }
    };
    let Some(remaining) = NonZeroUsize::new(limits.max_requests.get() - 1) else {
        record_error(
            &mut evidence,
            OperationError {
                kind: OperationErrorKind::Limit,
                operation_id: None,
                mutation_may_have_occurred: false,
                raw_service_error: None,
            },
            StartPhase::Start,
        );
        return finish(&approved, Some(state), evidence);
    };
    let action = request(
        client,
        executor,
        &format!("{base}/startMirroring"),
        OperationLimits {
            max_requests: remaining,
            ..limits
        },
        cancelled.as_mut(),
    )
    .await;
    evidence.response = action.response;
    match action.result {
        Ok(response) if response.status == 200 && response.body.is_empty() => {}
        Ok(_) => record_error(&mut evidence, action.evidence, StartPhase::Start),
        Err(error) => record_error(&mut evidence, error, StartPhase::Start),
    }
    finish(&approved, Some(state), evidence)
}
