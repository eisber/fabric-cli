// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Environment staging publication over the shared operation executor.
//!
//! HTTP 200 can contain a running native publish, unlike a completed Fabric LRO.
//! This adapter never saves source, uploads libraries, cancels work, or replays a
//! mutation. Hosts own approval, credentials, deadlines and presentation.

use crate::{Audience, Client, RequestContext, ServiceOrigin, v2};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fmt,
    future::Future,
    pin::{Pin, pin},
    sync::Arc,
    time::Duration,
};
use v2::{
    ApprovalPolicy, ApprovedOperation, Availability, Binding, ContractError, Coverage, Effect,
    ExecutionEvidence, ExecutionReceipt, FailureInfo, Generation, OperationDescriptor,
    OperationHandle, Outcome, Precondition, PreparedOperation, Prerequisite, ReceiptError,
    Registry, ResolvedTarget, SourceEvidence, TargetIdentity, TargetKind, Verb,
};

/// Stable operation identity, shared by independent execution heads.
pub const OPERATION_ID: &str = "environment.staging.publish";

/// Explicit source intent; no implicit authoring or deployment step exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishSource {
    /// Publish the environment's already-saved staging state.
    ExistingStaging,
}

/// Caller-selected handling of an advertised generic Fabric LRO.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObservationPolicy {
    /// Retain acceptance without polling.
    ReturnAccepted,
    /// Reuse shared LRO polling without requesting a nonexistent resource result.
    ///
    /// A native HTTP 200 Running response remains incomplete. It has no generic
    /// operation handle; use [`observe_version`] with its actual target version.
    WaitForOperation,
}

/// Register only the documented Environment publication binding.
///
/// The host must verify write permission, supported identity/tenant policy and
/// approval. Registration neither checks those facts nor enables a CLI command.
///
/// # Errors
/// Rejects duplicate or conflicting operation entries through the shared registry.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    registry.register(OperationDescriptor::new(
        OPERATION_ID,
        Verb::Publish,
        json!({
            "type": "object",
            "required": ["source", "observation"],
            "additionalProperties": false,
            "properties": {
                "source": {"const": "existingStaging"},
                "observation": {"enum": ["returnAccepted", "waitForOperation"]}
            }
        }),
        json!({"type": "object", "description": "Environment publication receipt"}),
        vec![Effect::RemoteWrite, Effect::Publication],
    )?)?;
    registry.bind(Binding::new(
        "Environment",
        "staging",
        "environment",
        Generation::Any,
        OPERATION_ID,
        Availability::Conditional(vec![
            Prerequisite {
                id: "environment.write".into(),
                description: "Environment item write permission and delegated Item.ReadWrite.All or Environment.ReadWrite.All when applicable".into(),
            },
            Prerequisite {
                id: "environment.identity".into(),
                description: "Native supported identity and Fabric tenant policy; no credential or consent fallback".into(),
            },
        ]),
    )?)
}

fn validate_target(target: &ResolvedTarget) -> Result<(), ReceiptError> {
    let selection = target.selection();
    if selection.kind != TargetKind::Item
        || selection.owner_type != "Environment"
        || selection.facet != "staging"
        || selection.representation != "environment"
        || target.child().is_some()
    {
        return Err(ReceiptError::TargetMismatch);
    }
    Ok(())
}

/// Prepare a single existing-staging publication, without I/O or approval.
///
/// The native method exposes no caller revision guard. Its server-side rejection
/// of another ongoing publish is not an optimistic concurrency guarantee.
///
/// # Errors
/// Rejects wrong-owner/subresource targets and invalid registry evidence.
pub fn prepare(
    client: &Client,
    target: ResolvedTarget,
    source: PublishSource,
    observation: ObservationPolicy,
) -> Result<PreparedPublish, ReceiptError> {
    validate_target(&target)?;
    if client.selection().audience() != Audience::Fabric
        || client.context().endpoint().origin() != ServiceOrigin::Fabric
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    let mut registry = Registry::default();
    register(&mut registry).map_err(ReceiptError::Contract)?;
    let resolution = registry
        .resolve(Verb::Publish, target.selection(), None)
        .map_err(ReceiptError::Contract)?;
    let prepared = PreparedOperation::new(
        resolution,
        target,
        json!({"source": source, "observation": observation}),
    )?
    .with_preconditions(vec![
        Precondition::ExistingOnly,
        Precondition::UnguardedConcurrency,
    ])?;
    Ok(PreparedPublish {
        prepared,
        context: client.context().clone(),
        observation,
    })
}

/// Immutable publication intent bound to the caller's actual request context.
///
/// Its private context retains endpoint, account/tenant/audience and original
/// credential provider provenance without token acquisition or serialization.
pub struct PreparedPublish {
    prepared: PreparedOperation,
    context: RequestContext,
    observation: ObservationPolicy,
}

impl PreparedPublish {
    /// Returns the neutral request/effects for host policy and presentation.
    pub fn operation(&self) -> &PreparedOperation {
        &self.prepared
    }

    /// Returns the immutable context for policy inspection; never log credentials.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Validate a neutral registry-prepared intent and bind its actual context.
    ///
    /// # Errors
    /// Rejects changed schema/input/effects/preconditions or provider requirements.
    /// Neutral approval alone cannot be converted into publication authority.
    pub fn from_operation(
        client: &Client,
        operation: PreparedOperation,
    ) -> Result<Self, ReceiptError> {
        let observation = match operation.input().get("observation").and_then(Value::as_str) {
            Some("returnAccepted") => ObservationPolicy::ReturnAccepted,
            Some("waitForOperation") => ObservationPolicy::WaitForOperation,
            _ => return Err(ReceiptError::InvalidEvidence),
        };
        let expected = prepare(
            client,
            operation.target().clone(),
            PublishSource::ExistingStaging,
            observation,
        )?;
        if operation.operation_id() != expected.operation().operation_id()
            || operation.input() != expected.operation().input()
            || operation.effects() != expected.operation().effects()
            || operation.preconditions() != expected.operation().preconditions()
            || operation.availability() != expected.operation().availability()
        {
            return Err(ReceiptError::InvalidEvidence);
        }
        Ok(Self {
            prepared: operation,
            ..expected
        })
    }

    /// Obtain immediate approval bound to this exact intent and context.
    ///
    /// # Errors
    /// Preserves the original host refusal; no credential or service work occurs.
    pub fn authorize<P: ApprovalPolicy>(
        &self,
        policy: &mut P,
    ) -> Result<ApprovedPublish<'_>, P::Error> {
        Ok(ApprovedPublish {
            approved: self.prepared.authorize(policy)?,
            context: &self.context,
            observation: self.observation,
        })
    }
}

impl fmt::Debug for PreparedPublish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedPublish(<bound intent>)")
    }
}

/// Opaque immediate approval, never convertible from an unbound neutral grant.
pub struct ApprovedPublish<'a> {
    approved: ApprovedOperation<'a>,
    context: &'a RequestContext,
    observation: ObservationPolicy,
}

impl fmt::Debug for ApprovedPublish<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApprovedPublish(<bound intent>)")
    }
}

fn validate_context(approved: &ApprovedPublish<'_>, client: &Client) -> Result<(), ReceiptError> {
    let context = client.context();
    if context.endpoint() != approved.context.endpoint()
        || context.selection() != approved.context.selection()
        || !Arc::ptr_eq(context.credential(), approved.context.credential())
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    Ok(())
}

struct BorrowedExecutor<'a, E>(&'a E);

impl<E: OperationExecutor> OperationExecutor for BorrowedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.execute(request)
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.0.wait(delay)
    }
}

/// Native Environment state, not the generic operation status domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum PublishState {
    /// Service has queued publication.
    Waiting,
    /// Publication is running.
    Running,
    /// Cancellation is still in progress; not rollback.
    Cancelling,
    /// Service reports successful publication.
    Success,
    /// Service reports failed publication.
    Failed,
    /// Service reports cancelled publication; not rollback.
    Cancelled,
}

fn state(value: &Value) -> Option<PublishState> {
    match value.as_str()? {
        "Waiting" => Some(PublishState::Waiting),
        "Running" => Some(PublishState::Running),
        "Cancelling" => Some(PublishState::Cancelling),
        "Success" => Some(PublishState::Success),
        "Failed" => Some(PublishState::Failed),
        "Cancelled" => Some(PublishState::Cancelled),
        _ => None,
    }
}

/// Exact version returned by the native publication, never a fabricated handle.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PublishVersion(String);

impl PublishVersion {
    /// Validate a native version UUID; construction performs no service lookup.
    ///
    /// # Errors
    /// Rejects malformed or route-changing UUIDs.
    pub fn new(value: &str) -> Result<Self, ReceiptError> {
        if value.len() != 36
            || !value.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(ReceiptError::InvalidIdentity);
        }
        Ok(Self(value.into()))
    }

    /// Returns original source spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PublishVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublishVersion(<native>)")
    }
}

/// Bounded native state preserving the original unknown fields and absent values.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePublication {
    /// Recognized service-native state.
    pub state: PublishState,
    /// Version only when actually supplied.
    pub target_version: Option<PublishVersion>,
    /// Unmodified publishDetails, including component states and timestamps.
    pub details: Value,
}

impl fmt::Debug for NativePublication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativePublication")
            .field("state", &self.state)
            .field("details", &"<service data>")
            .finish()
    }
}

fn native(details: &Value) -> Result<NativePublication, OperationErrorKind> {
    if !details.is_object() {
        return Err(OperationErrorKind::InvalidResponse);
    }
    let state = details
        .get("state")
        .and_then(state)
        .ok_or(OperationErrorKind::InvalidResponse)?;
    let target_version = match details.get("targetVersion") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(|value| PublishVersion::new(value).ok())
                .ok_or(OperationErrorKind::InvalidResponse)?,
        ),
    };
    Ok(NativePublication {
        state,
        target_version,
        details: details.clone(),
    })
}

fn components_succeeded(publication: &NativePublication) -> bool {
    let Some(components) = publication.details.get("componentPublishInfo") else {
        return true;
    };
    let Some(components) = components.as_object() else {
        return false;
    };
    components
        .values()
        .all(|component| component.get("state").and_then(state) == Some(PublishState::Success))
}

/// Completion established by this invocation, not a later unrelated success.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "camelCase")]
pub enum PublishData {
    /// State directly returned by publication or a version-correlated read.
    Native(NativePublication),
    /// Shared LRO returned Succeeded for this invocation; no native version inferred.
    Operation(Value),
}

/// Publication evidence kept outside any host's projection or output delivery.
pub struct PublishExecution {
    /// Protected common receipt of the exact approved intent.
    pub receipt: ExecutionReceipt<PublishData>,
    /// Exact initial publication response, retained before any LRO observation.
    pub initial_response: Option<OperationResponse>,
    /// Exact final shared-executor response, if available.
    ///
    /// With LRO waiting this is the operation-state response, not the initial 202.
    pub response: Option<OperationResponse>,
    /// Original shared error; never flatten its service/uncertainty evidence.
    pub operation_error: Option<Box<OperationError>>,
}

impl fmt::Debug for PublishExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishExecution")
            .field("response", &"<service data>")
            .field("operation_error", &"<typed evidence>")
            .finish_non_exhaustive()
    }
}

/// Invalid local approval or contradictory receipt construction.
///
/// Executed evidence is retained if the shared receipt rejects it.
pub enum PublishError {
    /// No service dispatch was attempted.
    Input(ReceiptError),
    /// Retains the original execution result rather than returning empty success.
    Receipt(Box<v2::ReceiptBuildError<PublishData>>),
    /// Retains the receipt and source evidence when shared validation rejects it.
    Source(Box<v2::SourceEvidenceError<PublishData>>),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Environment publish intent or receipt is invalid")
    }
}

impl std::error::Error for PublishError {}

impl fmt::Debug for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublishError(<retained evidence>)")
    }
}

fn failure(code: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: "Environment publication failed or remains incomplete; inspect retained evidence"
            .into(),
    }
}

fn base_path(target: &ResolvedTarget) -> Result<String, ReceiptError> {
    let TargetIdentity::Item { identity } = target.identity() else {
        return Err(ReceiptError::TargetMismatch);
    };
    Ok(format!(
        "/v1/workspaces/{}/environments/{}",
        identity.workspace_id(),
        identity.item_id()
    ))
}

struct Classified {
    outcome: Outcome<PublishData>,
    source_details: Option<Value>,
}

fn classify_response(
    response: &OperationResponse,
    operation_id: Option<&str>,
) -> Result<Classified, OperationErrorKind> {
    if response.status == 202 && response.body.is_empty() && operation_id.is_some() {
        return Ok(Classified {
            outcome: Outcome::Accepted,
            source_details: None,
        });
    }
    if response.status != 200 {
        return Err(OperationErrorKind::InvalidResponse);
    }
    let value: Value =
        serde_json::from_slice(&response.body).map_err(|_| OperationErrorKind::InvalidResponse)?;
    if operation_id.is_some() && value.get("status").and_then(Value::as_str) == Some("Succeeded") {
        return Ok(Classified {
            outcome: Outcome::Succeeded {
                data: PublishData::Operation(value),
            },
            source_details: None,
        });
    }
    let publication = native(&value["publishDetails"])?;
    let source_details = Some(json!({
        "nativeState": publication.state,
        "targetVersion": publication.target_version,
    }));
    Ok(Classified {
        outcome: native_outcome(publication),
        source_details,
    })
}

fn failed_outcome(error: &OperationError) -> Outcome<PublishData> {
    if matches!(error.kind, OperationErrorKind::OperationFailed { .. })
        || (matches!(
            error.kind,
            OperationErrorKind::Http {
                status: 400 | 401 | 403 | 404 | 409 | 429,
                ..
            }
        ) && error.operation_id.is_none())
    {
        Outcome::Failed {
            error: failure("publish_rejected_or_failed"),
        }
    } else {
        Outcome::Unknown {
            error: failure("publish_outcome_unknown"),
        }
    }
}

/// Dispatch one approved bodyless release-version publication.
///
/// Consumes its opaque immediate approval; the same grant cannot be reused.
/// Endpoint, selection and provider provenance must match the prepared context.
/// All execution limits are caller supplied, including a delay ceiling compatible
/// with the service's documented Retry-After: 120 example. The shared session owns
/// LRO polling and cancellation; neither timeout nor output failure replays/cancels.
/// HTTP 200 with active native state yields Partial: the common Accepted receipt
/// requires a genuine handle, and a targetVersion is not a Fabric operation ID.
///
/// # Errors
/// Rejects substituted approval before credentials. Service failures and response
/// loss return an execution receipt retaining their original typed evidence.
///
/// A neutral unbound grant cannot authorize publication:
/// ```compile_fail
/// use fabric_sdk::{Client, environment_publish};
/// use fabric_sdk::v2::ApprovedOperation;
/// use fabric_sdk_operations::{OperationExecutor, OperationLimits};
/// async fn unbound<E: OperationExecutor>(
///     client: &Client, executor: E, grant: ApprovedOperation<'_>, limits: OperationLimits,
/// ) {
///     environment_publish::publish(client, executor, grant, limits, std::future::pending()).await;
/// }
/// ```
pub async fn publish<E: OperationExecutor>(
    client: &Client,
    executor: E,
    approved: ApprovedPublish<'_>,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<PublishExecution, PublishError> {
    validate_context(&approved, client).map_err(PublishError::Input)?;
    let path = base_path(approved.approved.prepared().target()).map_err(PublishError::Input)?;
    let mut cancelled = pin!(cancelled);
    let mut session = OperationSession::with_context(
        client.context(),
        BorrowedExecutor(&executor),
        limits,
        cancelled.as_mut(),
    );
    let operation = FabricOperation::new(
        ResourceMethod::Post,
        &format!("{path}/staging/publish?beta=false"),
        Vec::new(),
        CompletionPolicy::ReturnAccepted,
    );
    let result = match operation {
        Ok(operation) => session.execute(operation).await,
        Err(kind) => Err(session.evidence(kind)),
    };
    let initial_response = result.as_ref().ok().cloned();
    let (mut response, mut operation_error) = match result {
        Ok(response) => (Some(response), None),
        Err(error) => (None, Some(Box::new(error))),
    };
    if let Some(initial) = &initial_response
        && initial.status == 202
        && classify_response(initial, session.operation_id()).is_ok()
        && approved.observation == ObservationPolicy::WaitForOperation
    {
        drop(session);
        session = OperationSession::with_context(
            client.context(),
            BorrowedExecutor(&executor),
            limits,
            cancelled.as_mut(),
        );
        match session
            .reconcile_accepted(initial.clone(), CompletionPolicy::WaitForCompletion)
            .await
        {
            Ok(observed) => response = Some(observed),
            Err(error) => {
                response = None;
                operation_error = Some(Box::new(error));
            }
        }
    }
    let classified = match &response {
        Some(response) => match classify_response(response, session.operation_id()) {
            Ok(classified) => classified,
            Err(kind) => {
                operation_error = Some(Box::new(session.evidence(kind)));
                Classified {
                    outcome: Outcome::Unknown {
                        error: failure("invalid_publish_response"),
                    },
                    source_details: None,
                }
            }
        },
        None => Classified {
            outcome: failed_outcome(
                operation_error
                    .as_ref()
                    .expect("failed execution retains error"),
            ),
            source_details: None,
        },
    };
    let outcome = classified.outcome;
    let shared_evidence = session.evidence(OperationErrorKind::InvalidResponse);
    let http_status = response
        .as_ref()
        .map(|response| response.status)
        .or_else(|| {
            operation_error.as_ref().and_then(|error| match error.kind {
                OperationErrorKind::Http { status, .. } => Some(status),
                _ => None,
            })
        });
    let coverage = if matches!(outcome, Outcome::Succeeded { .. } | Outcome::Failed { .. }) {
        Coverage::Complete
    } else {
        Coverage::Incomplete {
            reasons: vec!["Publication completion has not been established".into()],
            continuation: None,
        }
    };
    let evidence = ExecutionEvidence {
        http_status,
        handles: session
            .operation_id()
            .map(|id| OperationHandle::FabricOperation { id: id.into() })
            .into_iter()
            .collect(),
        coverage,
        mutation_may_have_occurred: shared_evidence.mutation_may_have_occurred,
    };
    let source = SourceEvidence {
        source: OPERATION_ID.into(),
        http_status,
        coverage: evidence.coverage.clone(),
        error: match &outcome {
            Outcome::Partial { error, .. }
            | Outcome::Failed { error }
            | Outcome::Unknown { error } => Some(error.clone()),
            _ => None,
        },
        details: classified.source_details,
    };
    let receipt = ExecutionReceipt::new(&approved.approved, outcome, evidence)
        .map_err(PublishError::Receipt)?
        .with_source_evidence(vec![source])
        .map_err(PublishError::Source)?;
    Ok(PublishExecution {
        receipt,
        initial_response,
        response,
        operation_error,
    })
}

fn native_outcome(publication: NativePublication) -> Outcome<PublishData> {
    match publication.state {
        PublishState::Success if components_succeeded(&publication) => Outcome::Succeeded {
            data: PublishData::Native(publication),
        },
        PublishState::Failed | PublishState::Cancelled => Outcome::Failed {
            error: failure("native_publish_failed_or_cancelled"),
        },
        _ => Outcome::Partial {
            data: PublishData::Native(publication),
            error: failure("native_publish_incomplete"),
        },
    }
}

/// One exact version-correlated native read; never an implicit poll loop.
pub struct NativeObservation {
    /// Original recognized state and native version.
    pub publication: NativePublication,
    /// True only when Success and every supplied component agrees.
    pub succeeded: bool,
    /// Exact bounded HTTP response with original metadata.
    pub response: OperationResponse,
}

/// Failed observation retains raw native state, including a concurrent new version.
pub struct ObservationError {
    /// Original shared classification and effect evidence (read-only).
    pub cause: Box<OperationError>,
    /// Actual response when parsing, owner validation or version correlation fails.
    pub response: Option<OperationResponse>,
}

impl fmt::Display for ObservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Environment publication observation failed or version changed")
    }
}

impl std::error::Error for ObservationError {}

impl fmt::Debug for NativeObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeObservation")
            .field("publication", &self.publication)
            .field("succeeded", &self.succeeded)
            .field("response", &"<service data>")
            .finish()
    }
}

impl fmt::Debug for ObservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObservationError(<retained evidence>)")
    }
}

/// Read exactly one native state for an explicitly retained publish version.
///
/// Caller scheduling uses the existing timer/deadline policy. There is no newest
/// version fallback, remote cancellation, or inferred generic operation handle.
///
/// # Errors
/// Preserves denied, malformed, wrong-owner and concurrent-version responses.
pub async fn observe_version<E: OperationExecutor>(
    client: &Client,
    executor: E,
    target: &ResolvedTarget,
    version: &PublishVersion,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<NativeObservation, ObservationError> {
    let mut session = OperationSession::with_context(client.context(), executor, limits, cancelled);
    let operation = {
        let local_error = |kind| ObservationError {
            cause: Box::new(session.evidence(kind)),
            response: None,
        };
        validate_target(target).map_err(|_| local_error(OperationErrorKind::InvalidInput))?;
        let path = base_path(target).map_err(|_| local_error(OperationErrorKind::InvalidInput))?;
        FabricOperation::new(
            ResourceMethod::Get,
            &path,
            Vec::new(),
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(local_error)?
    };
    let response = session
        .execute(operation)
        .await
        .map_err(|cause| ObservationError {
            cause: Box::new(cause),
            response: None,
        })?;
    let parsed = (|| {
        if response.status != 200 {
            return Err(OperationErrorKind::InvalidResponse);
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| OperationErrorKind::InvalidResponse)?;
        let TargetIdentity::Item { identity } = target.identity() else {
            return Err(OperationErrorKind::InvalidInput);
        };
        if value.get("type").and_then(Value::as_str) != Some("Environment")
            || !value
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(identity.item_id()))
            || !value
                .get("workspaceId")
                .and_then(Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(identity.workspace_id()))
        {
            return Err(OperationErrorKind::InvalidResponse);
        }
        let publication = native(&value["properties"]["publishDetails"])?;
        if !publication
            .target_version
            .as_ref()
            .is_some_and(|observed| observed.as_str().eq_ignore_ascii_case(version.as_str()))
        {
            return Err(OperationErrorKind::InvalidResponse);
        }
        Ok(publication)
    })();
    match parsed {
        Ok(publication) => Ok(NativeObservation {
            succeeded: publication.state == PublishState::Success
                && components_succeeded(&publication),
            publication,
            response,
        }),
        Err(kind) => Err(ObservationError {
            cause: Box::new(session.evidence(kind)),
            response: Some(response),
        }),
    }
}
