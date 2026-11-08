// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! GA-only native pipeline-stage deployment, not repository publication.
//!
//! The caller supplies credentials, execution, cancellation/deadlines and immediate
//! approval. Fabric owns dependency ordering and pairing. No mutation is replayed,
//! no workspace is implicitly created, and stopping observation is not rollback.

use fabric_sdk::RequestContext;
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Explicit deployment scope; an empty selection is never interpreted as all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionIntent {
    /// Omit native `items` and deploy all supported source-stage items.
    AllSupported,
    /// Deploy only the nonempty explicit native `items` list.
    Selected,
}

/// Sanitized admission/execution failure, with original evidence retained separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentErrorKind {
    /// Invalid UUID, JSON, selection, native metadata or response.
    InvalidInput,
    /// A route/body/stage identity was substituted.
    TargetMismatch,
    /// Beta plans, workspace creation, options, backward/nonadjacent stages or types.
    UnsupportedCapability,
    /// Caller approval was refused.
    ApprovalDenied,
    /// The native API explicitly denied permission.
    PermissionDenied,
    /// Caller cancellation stopped local work, not remote deployment.
    Cancelled,
    /// The injected executor's deadline elapsed.
    TimedOut,
    /// Native response did not establish a supported outcome.
    InvalidResponse,
    /// Shared identity, permission, transport, cancellation or operation failure.
    Operation,
    /// The native deployment reported failure, possibly after some steps completed.
    DeploymentFailed,
}

/// Failed execution retains observed acceptance and partial/uncertain effects.
pub struct DeploymentError {
    /// Stable classification without raw tenant/service text.
    pub kind: DeploymentErrorKind,
    /// Original shared-engine error; never converted to an empty success result.
    pub operation: Option<Box<OperationError>>,
    /// Actual deployment receipt when any remote execution was attempted.
    pub receipt: Option<Box<DeploymentReceipt>>,
}

impl fmt::Debug for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeploymentError")
            .field("kind", &self.kind)
            .field("has_operation_evidence", &self.operation.is_some())
            .field("receipt", &self.receipt)
            .finish()
    }
}
impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native stage deployment failed or is incomplete; retain operation evidence")
    }
}
impl std::error::Error for DeploymentError {}

fn error(kind: DeploymentErrorKind) -> DeploymentError {
    DeploymentError {
        kind,
        operation: None,
        receipt: None,
    }
}

fn operation_kind(cause: &OperationError) -> DeploymentErrorKind {
    match &cause.kind {
        OperationErrorKind::Http { status: 403, .. } => DeploymentErrorKind::PermissionDenied,
        OperationErrorKind::Cancelled
        | OperationErrorKind::Execution(OperationExecutionError::Cancelled) => {
            DeploymentErrorKind::Cancelled
        }
        OperationErrorKind::Execution(OperationExecutionError::TimedOut) => {
            DeploymentErrorKind::TimedOut
        }
        OperationErrorKind::InvalidResponse | OperationErrorKind::InvalidLink => {
            DeploymentErrorKind::InvalidResponse
        }
        _ => DeploymentErrorKind::Operation,
    }
}

fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn same_id(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Immutable native payload and target, independent of CLI spelling.
pub struct DeploymentRequest {
    pipeline_id: String,
    source_stage_id: String,
    target_stage_id: String,
    intent: SelectionIntent,
    body: Value,
}

impl fmt::Debug for DeploymentRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeploymentRequest")
            .field("intent", &self.intent)
            .field("body", &"<validated native input>")
            .finish_non_exhaustive()
    }
}

impl DeploymentRequest {
    /// Parse a bounded native JSON request and bind its target to the resolved path.
    ///
    /// Only Notebook, Report and SemanticModel explicit selections are advertised
    /// in this initial slice. AllSupported is the service's native scope, not a
    /// claim that every item/identity/tenant combination is supported. Native
    /// permissions, pairing and dependencies remain enforced by Fabric.
    ///
    /// # Errors
    /// Rejects unknown fields/options, empty/null/duplicate selections, more than
    /// 300 items, malformed IDs and source/target substitution before any I/O.
    pub fn from_json(
        pipeline_id: &str,
        target_stage_id: &str,
        bytes: &[u8],
        max_input_bytes: usize,
        intent: SelectionIntent,
    ) -> Result<Self, DeploymentError> {
        if !uuid(pipeline_id)
            || !uuid(target_stage_id)
            || max_input_bytes == 0
            || bytes.len() > max_input_bytes
        {
            return Err(error(DeploymentErrorKind::InvalidInput));
        }
        let body: Value =
            serde_json::from_slice(bytes).map_err(|_| error(DeploymentErrorKind::InvalidInput))?;
        let fields = body
            .as_object()
            .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
        if fields
            .keys()
            .any(|key| !["sourceStageId", "targetStageId", "items", "note"].contains(&key.as_str()))
        {
            return Err(error(DeploymentErrorKind::UnsupportedCapability));
        }
        let source = fields
            .get("sourceStageId")
            .and_then(Value::as_str)
            .filter(|id| uuid(id))
            .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
        let target = fields
            .get("targetStageId")
            .and_then(Value::as_str)
            .filter(|id| uuid(id))
            .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
        if !same_id(target, target_stage_id) || same_id(source, target) {
            return Err(error(DeploymentErrorKind::TargetMismatch));
        }
        if let Some(note) = fields.get("note")
            && !note
                .as_str()
                .is_some_and(|note| note.chars().count() <= 1024)
        {
            return Err(error(DeploymentErrorKind::InvalidInput));
        }
        match intent {
            SelectionIntent::AllSupported if fields.contains_key("items") => {
                return Err(error(DeploymentErrorKind::InvalidInput));
            }
            SelectionIntent::Selected => {
                let items = fields
                    .get("items")
                    .and_then(Value::as_array)
                    .filter(|items| !items.is_empty() && items.len() <= 300)
                    .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
                let mut seen = HashSet::new();
                for item in items {
                    let item = item
                        .as_object()
                        .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
                    if item.len() != 2 {
                        return Err(error(DeploymentErrorKind::UnsupportedCapability));
                    }
                    let id = item
                        .get("sourceItemId")
                        .and_then(Value::as_str)
                        .filter(|id| uuid(id))
                        .ok_or_else(|| error(DeploymentErrorKind::InvalidInput))?;
                    if !seen.insert(id.to_ascii_lowercase()) {
                        return Err(error(DeploymentErrorKind::InvalidInput));
                    }
                    if !item
                        .get("itemType")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| ["Notebook", "Report", "SemanticModel"].contains(&kind))
                    {
                        return Err(error(DeploymentErrorKind::UnsupportedCapability));
                    }
                }
            }
            SelectionIntent::AllSupported => {}
        }
        Ok(Self {
            pipeline_id: pipeline_id.into(),
            source_stage_id: source.into(),
            target_stage_id: target.into(),
            intent,
            body,
        })
    }

    /// Exact pipeline UUID; never inferred from a workspace.
    pub fn pipeline_id(&self) -> &str {
        &self.pipeline_id
    }
    /// Required source-stage UUID.
    pub fn source_stage_id(&self) -> &str {
        &self.source_stage_id
    }
    /// Required target-stage UUID, validated against the addressed target.
    pub fn target_stage_id(&self) -> &str {
        &self.target_stage_id
    }
    /// Explicit all-supported versus selected-items intent.
    pub const fn intent(&self) -> SelectionIntent {
        self.intent
    }
    /// Exact validated native body for host approval.
    pub fn body(&self) -> &Value {
        &self.body
    }
}

/// Native existing stage metadata acquired through the scoped stage-read API.
pub struct StageMetadata {
    pipeline_id: String,
    stage_id: String,
    workspace_id: String,
    order: u64,
    context: RequestContext,
}

/// Borrowed native stage reader over the existing shared request policy.
pub struct StageReader<'a, E> {
    /// Exact endpoint/credential binding retained in the returned metadata.
    pub context: &'a RequestContext,
    /// Caller-bound shared execution, without credentials or audience fallback.
    pub executor: &'a E,
    /// Bounds for this preflight read; the outer host debits its overall budget.
    pub limits: OperationLimits,
}

impl<E: OperationExecutor> StageReader<'_, E> {
    /// Read one exact stage; no ambient lookup or caller-fabricated stage snapshot.
    ///
    /// # Errors
    /// Preserves permission/identity errors and rejects missing workspaces or
    /// mismatched returned IDs. Workspace creation is outside this slice.
    pub async fn read_stage(
        &self,
        pipeline_id: &str,
        stage_id: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<StageMetadata, DeploymentError> {
        if !uuid(pipeline_id) || !uuid(stage_id) {
            return Err(error(DeploymentErrorKind::InvalidInput));
        }
        let operation = FabricOperation::new(
            ResourceMethod::Get,
            &format!("/v1/deploymentPipelines/{pipeline_id}/stages/{stage_id}"),
            Vec::new(),
            CompletionPolicy::WaitForCompletion,
        )
        .map_err(|_| error(DeploymentErrorKind::InvalidInput))?;
        let submitted = Mutex::new(None);
        let executor = ObservedExecutor {
            inner: self.executor,
            submitted: &submitted,
        };
        let mut session =
            OperationSession::with_context(self.context, executor, self.limits, cancelled);
        let response = session
            .execute(operation)
            .await
            .map_err(|cause| DeploymentError {
                kind: operation_kind(&cause),
                operation: Some(Box::new(cause)),
                receipt: None,
            })?;
        if response.status != 200 {
            return Err(error(DeploymentErrorKind::InvalidResponse));
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| error(DeploymentErrorKind::InvalidResponse))?;
        if !value
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| same_id(id, stage_id))
        {
            return Err(error(DeploymentErrorKind::TargetMismatch));
        }
        let workspace = value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| uuid(id))
            .ok_or_else(|| error(DeploymentErrorKind::UnsupportedCapability))?;
        let order = value
            .get("order")
            .and_then(Value::as_u64)
            .filter(|order| *order < 10)
            .ok_or_else(|| error(DeploymentErrorKind::InvalidResponse))?;
        Ok(StageMetadata {
            pipeline_id: pipeline_id.into(),
            stage_id: stage_id.into(),
            workspace_id: workspace.into(),
            order,
            context: self.context.clone(),
        })
    }
}

fn same_context(a: &RequestContext, b: &RequestContext) -> bool {
    a.selection() == b.selection()
        && a.endpoint() == b.endpoint()
        && Arc::ptr_eq(a.credential(), b.credential())
}

/// Fully bound native request presented unchanged for immediate host approval.
pub struct PreparedDeployment {
    request: DeploymentRequest,
    source: StageMetadata,
    target: StageMetadata,
}

impl PreparedDeployment {
    /// Bind actual stage metadata and require consecutive forward, assigned stages.
    ///
    /// # Errors
    /// Rejects wrong pipeline/stage ownership, backward/skipped stages and the
    /// same workspace. Read preflight is not a concurrency or authorization lock.
    pub fn new(
        request: DeploymentRequest,
        source: StageMetadata,
        target: StageMetadata,
    ) -> Result<Self, DeploymentError> {
        if !same_id(&source.pipeline_id, request.pipeline_id())
            || !same_id(&target.pipeline_id, request.pipeline_id())
            || !same_id(&source.stage_id, request.source_stage_id())
            || !same_id(&target.stage_id, request.target_stage_id())
            || same_id(&source.workspace_id, &target.workspace_id)
            || !same_context(&source.context, &target.context)
        {
            return Err(error(DeploymentErrorKind::TargetMismatch));
        }
        if target.order != source.order + 1 {
            return Err(error(DeploymentErrorKind::UnsupportedCapability));
        }
        Ok(Self {
            request,
            source,
            target,
        })
    }
    /// Immutable request, including note and exact selected-item/all-items intent.
    pub fn request(&self) -> &DeploymentRequest {
        &self.request
    }
    /// Source and target workspace UUIDs from the native stage reads.
    pub fn workspaces(&self) -> (&str, &str) {
        (&self.source.workspace_id, &self.target.workspace_id)
    }
    /// Verify the execution/approval host still uses the stage reads' exact scope.
    ///
    /// # Errors
    /// Rejects credential-provider, account/tenant, audience or endpoint substitution.
    pub fn require_context(&self, context: &RequestContext) -> Result<(), DeploymentError> {
        if !same_context(context, &self.source.context) {
            return Err(error(DeploymentErrorKind::TargetMismatch));
        }
        Ok(())
    }
    /// Obtain an immediate, nonserializable approval for this exact request.
    ///
    /// # Errors
    /// Preserves explicit policy refusal; no mutation is performed.
    pub fn authorize(
        self,
        policy: &mut impl DeploymentApproval,
    ) -> Result<ApprovedDeployment, DeploymentError> {
        policy.authorize(&self)?;
        Ok(ApprovedDeployment { prepared: self })
    }
}

/// Caller policy verifies permissions, identity restrictions and exact-effect intent.
///
/// Approval must cover creates/overwrites/native folder propagation, all-items scope
/// when selected, and unguarded concurrency. No default "yes" policy is installed.
pub trait DeploymentApproval {
    /// Authorize this exact target and native payload, not a saved reusable plan.
    ///
    /// # Errors
    /// Returns an explicit refusal before deployment dispatch.
    fn authorize(&mut self, deployment: &PreparedDeployment) -> Result<(), DeploymentError>;
}

/// Immediate approval consumed once; not a persisted service idempotency token.
pub struct ApprovedDeployment {
    prepared: PreparedDeployment,
}

/// Native operation outcome, independent of successful output delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentOutcome {
    /// Native submission observed; terminal success has not been established.
    Accepted,
    /// Native operation explicitly reported success, not data-plane readiness.
    Succeeded,
    /// Native failure with no asserted inventory of retained effects.
    Failed,
    /// Native failure with at least one explicitly succeeded execution-plan step.
    Partial,
    /// Missing/malformed response, interruption or unsupported native state.
    Unknown,
}

/// Unprojected stage-deployment evidence; no raw tokens, headers or note are emitted.
#[derive(Clone)]
pub struct DeploymentReceipt {
    /// Exact pipeline UUID.
    pub pipeline_id: String,
    /// Exact source-stage UUID.
    pub source_stage_id: String,
    /// Exact target-stage UUID.
    pub target_stage_id: String,
    /// Explicit input scope.
    pub intent: SelectionIntent,
    /// Actual submission HTTP status, absent without a response.
    pub http_status: Option<u16>,
    /// Generic Fabric LRO ID from x-ms-operation-id; never replaced with deployment ID.
    pub operation_id: Option<String>,
    /// Native deployment-history ID from deployment-id or immediate result.
    pub deployment_id: Option<String>,
    /// Actual native outcome, never inferred from HTTP acceptance.
    pub outcome: DeploymentOutcome,
    /// Mutation uncertainty from the shared execution engine.
    pub mutation_may_have_occurred: bool,
    /// Native execution plan when observed; absence is not an empty effects claim.
    pub execution_plan: Option<Value>,
}

impl fmt::Debug for DeploymentReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeploymentReceipt")
            .field("outcome", &self.outcome)
            .field("http_status", &self.http_status)
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .field("has_execution_plan", &self.execution_plan.is_some())
            .finish_non_exhaustive()
    }
}

impl DeploymentReceipt {
    /// Serialize sanitized IDs, outcomes and step evidence; no service secrets/note.
    pub fn to_value(&self) -> Value {
        json!({
            "schemaVersion": "0.1", "operation": "deploy",
            "target": {"pipelineId": self.pipeline_id, "sourceStageId": self.source_stage_id,
                "targetStageId": self.target_stage_id},
            "selection": match self.intent { SelectionIntent::AllSupported => "all_supported", SelectionIntent::Selected => "selected" },
            "status": match self.outcome { DeploymentOutcome::Accepted => "accepted", DeploymentOutcome::Succeeded => "succeeded",
                DeploymentOutcome::Failed => "failed", DeploymentOutcome::Partial => "partial", DeploymentOutcome::Unknown => "unknown" },
            "evidence": {"httpStatus": self.http_status, "operationId": self.operation_id,
                "deploymentId": self.deployment_id, "mutationMayHaveOccurred": self.mutation_may_have_occurred,
                "executionPlan": self.execution_plan.as_ref().map(project_plan)},
            "unguardedConcurrency": true, "deploymentSupport": "source_modeled_live_unverified"
        })
    }
}

fn project_plan(plan: &Value) -> Value {
    let steps = plan.get("steps").and_then(Value::as_array).map(|steps| {
        steps
            .iter()
            .map(|step| {
                let mut result = json!({"index":step.get("index"),"status":step.get("status")});
                if let Some(source_and_target) =
                    step.get("sourceAndTarget").and_then(Value::as_object)
                {
                    let mut ids = serde_json::Map::new();
                    for field in ["sourceItemId", "targetItemId", "itemType"] {
                        if let Some(value) = source_and_target.get(field) {
                            ids.insert(field.into(), value.clone());
                        }
                    }
                    result["sourceAndTarget"] = Value::Object(ids);
                }
                if let Some(code) = step.get("error").and_then(|error| error.get("errorCode")) {
                    result["error"] = json!({"errorCode":code});
                }
                result
            })
            .collect::<Vec<_>>()
    });
    json!({"steps":steps})
}

fn response_id(response: &OperationResponse, name: &str) -> Option<String> {
    let mut values = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let (_, value) = values.next()?;
    (values.next().is_none() && uuid(value)).then(|| value.clone())
}

fn valid_acceptance(response: &OperationResponse) -> bool {
    response.status != 202
        || (response_id(response, "x-ms-operation-id").is_some()
            && response_id(response, "deployment-id").is_some())
}

struct ObservedExecutor<'a, E> {
    inner: &'a E,
    submitted: &'a Mutex<Option<OperationResponse>>,
}

impl<E: OperationExecutor> OperationExecutor for ObservedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let submitted = request.method() == ResourceMethod::Post;
            let limit = request.max_response_bytes().get();
            let response = self.inner.execute(request).await?;
            if submitted {
                let valid = valid_acceptance(&response);
                if response.body.len() > limit {
                    return Err(OperationExecutionError::ResponseLimit);
                }
                *self
                    .submitted
                    .lock()
                    .map_err(|_| OperationExecutionError::Failed)? = Some(response.clone());
                if !valid {
                    return Err(OperationExecutionError::Failed);
                }
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

impl ApprovedDeployment {
    /// Submit exactly once using the existing caller-bound operation engine.
    ///
    /// Waiting uses that same session/request budget and caller cancellation future
    /// (which must include the original total deadline). The observer only retains
    /// initial native headers; it installs no transport, runtime or polling loop.
    ///
    /// # Errors
    /// Preserves permission/identity/transport/deadline failures, both known handles
    /// and native partial effects. No error, cancellation or output repair replays POST.
    pub async fn execute<E: OperationExecutor>(
        self,
        context: &RequestContext,
        executor: &E,
        limits: OperationLimits,
        wait: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DeploymentReceipt, DeploymentError> {
        let request = &self.prepared.request;
        self.prepared.require_context(context)?;
        let mut receipt = DeploymentReceipt {
            pipeline_id: request.pipeline_id.clone(),
            source_stage_id: request.source_stage_id.clone(),
            target_stage_id: request.target_stage_id.clone(),
            intent: request.intent,
            http_status: None,
            operation_id: None,
            deployment_id: None,
            outcome: DeploymentOutcome::Unknown,
            mutation_may_have_occurred: false,
            execution_plan: None,
        };
        let submitted = Mutex::new(None);
        let observer = ObservedExecutor {
            inner: executor,
            submitted: &submitted,
        };
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &format!("/v1/deploymentPipelines/{}/deploy", request.pipeline_id),
            serde_json::to_vec(&request.body)
                .map_err(|_| error(DeploymentErrorKind::InvalidInput))?,
            if wait {
                CompletionPolicy::WaitForCompletion
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| error(DeploymentErrorKind::InvalidInput))?;
        let mut session = OperationSession::with_context(context, observer, limits, cancelled);
        let result = session.execute(operation).await;
        receipt.mutation_may_have_occurred = session
            .evidence(OperationErrorKind::InvalidResponse)
            .mutation_may_have_occurred;
        receipt.operation_id = session.operation_id().map(str::to_owned);
        let initial = submitted
            .lock()
            .map_err(|_| DeploymentError {
                kind: DeploymentErrorKind::Operation,
                operation: None,
                receipt: Some(Box::new(receipt.clone())),
            })?
            .take();
        if let Some(initial) = &initial {
            receipt.http_status = Some(initial.status);
            receipt.operation_id =
                response_id(initial, "x-ms-operation-id").or(receipt.operation_id);
            receipt.deployment_id = response_id(initial, "deployment-id");
        }
        finish(result, initial.as_ref(), receipt)
    }
}

fn finish(
    result: Result<OperationResponse, OperationError>,
    initial: Option<&OperationResponse>,
    mut receipt: DeploymentReceipt,
) -> Result<DeploymentReceipt, DeploymentError> {
    let response = result.map_err(|cause| {
        if matches!(&cause.kind, OperationErrorKind::OperationFailed { status, .. } if status == "Failed") {
            receipt.outcome = DeploymentOutcome::Failed;
        }
        DeploymentError {
        kind: if initial.is_some_and(|response| !valid_acceptance(response)) {
            DeploymentErrorKind::InvalidResponse
        } else {
            operation_kind(&cause)
        },
        operation: Some(Box::new(cause)),
        receipt: Some(Box::new(receipt.clone())),
    }})?;
    if initial.is_none() {
        return Err(DeploymentError {
            kind: DeploymentErrorKind::InvalidResponse,
            operation: None,
            receipt: Some(Box::new(receipt)),
        });
    }
    if response.status == 202 {
        receipt.outcome = DeploymentOutcome::Accepted;
        return Ok(receipt);
    }
    let value: Value = serde_json::from_slice(&response.body).map_err(|_| DeploymentError {
        kind: DeploymentErrorKind::InvalidResponse,
        operation: None,
        receipt: Some(Box::new(receipt.clone())),
    })?;
    if initial.is_some_and(|response| response.status != 202) {
        if value.get("type").and_then(Value::as_str) != Some("Deploy")
            || !value
                .get("sourceStageId")
                .and_then(Value::as_str)
                .is_some_and(|id| same_id(id, &receipt.source_stage_id))
            || !value
                .get("targetStageId")
                .and_then(Value::as_str)
                .is_some_and(|id| same_id(id, &receipt.target_stage_id))
        {
            return Err(DeploymentError {
                kind: DeploymentErrorKind::TargetMismatch,
                operation: None,
                receipt: Some(Box::new(receipt)),
            });
        }
        receipt.deployment_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| uuid(id))
            .map(str::to_owned);
        if receipt.deployment_id.is_none() {
            return Err(DeploymentError {
                kind: DeploymentErrorKind::InvalidResponse,
                operation: None,
                receipt: Some(Box::new(receipt)),
            });
        }
    }
    receipt.execution_plan = value
        .get("executionPlan")
        .filter(|plan| !plan.is_null())
        .cloned();
    if receipt.execution_plan.as_ref().is_some_and(|plan| {
        !plan
            .get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| {
                steps.iter().all(|step| {
                    step.is_object()
                        && step.get("index").and_then(Value::as_u64).is_some()
                        && step.get("status").and_then(Value::as_str).is_some()
                })
            })
    }) {
        return Err(DeploymentError {
            kind: DeploymentErrorKind::InvalidResponse,
            operation: None,
            receipt: Some(Box::new(receipt)),
        });
    }
    receipt.outcome = match value.get("status").and_then(Value::as_str) {
        Some("Succeeded") => DeploymentOutcome::Succeeded,
        Some("NotStarted" | "Running") => DeploymentOutcome::Accepted,
        Some("Failed")
            if receipt
                .execution_plan
                .as_ref()
                .and_then(|plan| plan.get("steps"))
                .and_then(Value::as_array)
                .is_some_and(|steps| {
                    steps
                        .iter()
                        .any(|step| step.get("status").and_then(Value::as_str) == Some("Succeeded"))
                }) =>
        {
            DeploymentOutcome::Partial
        }
        Some("Failed") => DeploymentOutcome::Failed,
        _ => DeploymentOutcome::Unknown,
    };
    if matches!(
        receipt.outcome,
        DeploymentOutcome::Succeeded | DeploymentOutcome::Accepted
    ) {
        Ok(receipt)
    } else {
        Err(DeploymentError {
            kind: if receipt.outcome == DeploymentOutcome::Unknown {
                DeploymentErrorKind::InvalidResponse
            } else {
                DeploymentErrorKind::DeploymentFailed
            },
            operation: None,
            receipt: Some(Box::new(receipt)),
        })
    }
}
