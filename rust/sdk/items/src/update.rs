// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed existing-item metadata PATCH with immediate caller approval.
//!
//! Notebook and Lakehouse use their type-specific endpoints. No definition,
//! create, pre-read/merge, server revision guard, or mutation replay is implied.
//! The caller supplies the context, executor, deadline and cancellation.

use crate::{ItemCollection, mutations::ItemMutations};
use contract::{
    ApprovedOperation, Availability, Binding, ContractError, Coverage, Effect, ExecutionEvidence,
    ExecutionReceipt, FailureInfo, Generation, OperationDescriptor, OperationHandle, Outcome,
    Precondition, PreparedOperation, ReceiptBuildError, ReceiptError, Registry, ResolvedTarget,
    TargetKind, Verb,
};
use fabric_core::{Audience, RequestContext, ServiceOrigin};
use fabric_sdk_operations::{
    OperationExecutor, OperationLimits, OperationSession, OperationWarning,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceError, ResourceOutcome};
pub use fabric_v2_contracts as contract;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

/// Common operation identity, independent of CLI or artifact-facade spelling.
pub const OPERATION_ID: &str = "item.metadata.patch";
/// Finite metadata input bound, matching the shared default response byte bound.
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Explicit requested update mode. Only native metadata patch is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Change only explicitly supplied writable fields.
    Patch,
    /// Complete replacement is outside this metadata slice.
    Replace,
}

/// Exact invocation options retained in approval evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOptions {
    /// Required explicit operation mode.
    pub mode: UpdateMode,
    /// An unsupported supplied condition must be rejected, never dropped.
    pub if_match: Option<String>,
    /// This synchronous endpoint has no advertised observer.
    pub wait: bool,
    /// Caller-enforced total deadline, including local preparation and delivery.
    pub timeout_millis: u64,
    /// Preflighted absolute no-overwrite file, when selected by the host.
    pub output_path: Option<String>,
    /// Data-only host projection; it never changes protected receipt metadata.
    pub project: Option<String>,
}

impl UpdateOptions {
    /// Explicit patch options with the selected 30-second submission deadline.
    pub fn patch() -> Self {
        Self {
            mode: UpdateMode::Patch,
            if_match: None,
            wait: false,
            timeout_millis: 30_000,
            output_path: None,
            project: None,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetadataPatch {
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    description: Option<String>,
}

fn present_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

/// Register the one operation and its positively enumerated metadata bindings.
///
/// This is transactional and performs no I/O, permission checks or execution.
/// Other artifact owners may bind the same descriptor after establishing their
/// own field/endpoint contract; membership in ItemType is not sufficient.
///
/// # Errors
/// Returns existing registry duplicate/ambiguity failures without replacement.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    candidate.register(
        OperationDescriptor::new(
            OPERATION_ID,
            Verb::Update,
            json!({
                "type":"object", "additionalProperties":false, "minProperties":1,
                "properties":{
                    "displayName":{"type":"string","minLength":1},
                    "description":{"type":"string","maxLength":256}
                }
            }),
            json!({"type":"object","required":["schemaVersion","status","meta"]}),
            vec![Effect::RemoteWrite],
        )?
        .with_optional_effects(vec![Effect::LocalRead, Effect::LocalWrite])?,
    )?;
    for owner in ["Notebook", "Lakehouse"] {
        candidate.bind(Binding::new(
            owner,
            "metadata",
            "json",
            Generation::Any,
            OPERATION_ID,
            Availability::Available,
        )?)?;
    }
    *registry = candidate;
    Ok(())
}

/// Local, sanitized failure. No request or provider text is formatted implicitly.
#[derive(Debug)]
pub enum UpdateError {
    /// Invalid identity, body, options, context or approval.
    InvalidInput,
    /// The target, mode, condition or observer has no advertised native binding.
    UnsupportedCapability,
    /// An executor context has not been explicitly supplied.
    NotConfigured,
    /// Approval belongs to a different immutable prepared request.
    ApprovalMismatch,
    /// This invocation has already consumed its sole submission attempt.
    AlreadyExecuted,
    /// The existing neutral contract rejected preparation.
    Contract(ReceiptError),
    /// A receipt could not be built; original outcome/evidence are retained.
    Receipt(Box<ReceiptBuildError<Value>>),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid typed metadata update input or context",
            Self::UnsupportedCapability => "metadata update capability is not supported",
            Self::NotConfigured => "metadata update requires an explicitly bound execution context",
            Self::ApprovalMismatch => "approval does not match this metadata update",
            Self::AlreadyExecuted => "metadata update submission has already been consumed",
            Self::Contract(_) => "metadata update preparation failed",
            Self::Receipt(_) => "metadata update receipt failed; original evidence retained",
        })
    }
}

impl std::error::Error for UpdateError {}

/// Immutable, single-submission update with native conditions and effect evidence.
///
/// Preparation does not resolve names, read current metadata, acquire credentials,
/// reserve output, or grant permission. Bind context before immediate approval.
pub struct PreparedUpdate {
    prepared: PreparedOperation,
    body: Value,
    options: UpdateOptions,
    collection: ItemCollection,
    context: Option<RequestContext>,
    limits: OperationLimits,
    executed: AtomicBool,
}

impl fmt::Debug for PreparedUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedUpdate")
            .field("prepared", &self.prepared)
            .finish_non_exhaustive()
    }
}

impl PreparedUpdate {
    /// Validate one strict JSON patch and an already-resolved existing item.
    ///
    /// Absent fields remain absent. Null, unknown/duplicate fields, empty patches
    /// and nonstring values fail; empty description is an explicit string value.
    /// SDK input preserves the original requested values, not read-only metadata.
    ///
    /// # Errors
    /// Rejects unsupported modes/conditions/observers and all unadvertised targets
    /// before any credential or executor is contacted.
    pub fn new(
        target: ResolvedTarget,
        bytes: &[u8],
        options: UpdateOptions,
    ) -> Result<Self, UpdateError> {
        if options.mode != UpdateMode::Patch || options.if_match.is_some() || options.wait {
            return Err(UpdateError::UnsupportedCapability);
        }
        if options.timeout_millis == 0
            || options.timeout_millis > 86_400_000
            || options
                .project
                .as_ref()
                .is_some_and(|value| value.trim().is_empty())
            || bytes.len() > MAX_INPUT_BYTES
        {
            return Err(UpdateError::InvalidInput);
        }
        let selection = target.selection();
        let collection = match selection.owner_type.as_str() {
            "Notebook" => ItemCollection::Notebook,
            "Lakehouse" => ItemCollection::Lakehouse,
            _ => return Err(UpdateError::UnsupportedCapability),
        };
        if selection.kind != TargetKind::Item
            || selection.facet != "metadata"
            || selection.representation != "json"
            || target.child().is_some()
        {
            return Err(UpdateError::UnsupportedCapability);
        }
        let patch: MetadataPatch =
            serde_json::from_slice(bytes).map_err(|_| UpdateError::InvalidInput)?;
        if patch.display_name.is_none() && patch.description.is_none()
            || patch
                .display_name
                .as_ref()
                .is_some_and(|name| name.trim().is_empty() || name.chars().any(char::is_control))
            || patch
                .description
                .as_ref()
                .is_some_and(|value| value.chars().count() > 256)
        {
            return Err(UpdateError::InvalidInput);
        }
        let body = serde_json::to_value(patch).map_err(|_| UpdateError::InvalidInput)?;
        let limits = OperationLimits::default();
        let prepared = prepare(target, &body, &options, None, limits)?;
        Ok(Self {
            prepared,
            body,
            options,
            collection,
            context: None,
            limits,
            executed: AtomicBool::new(false),
        })
    }

    /// Bind the actual caller context and shared finite limits before approval.
    ///
    /// This consumes the unbound plan. No token is requested and no context may
    /// be rebound; approvals from an earlier plan cannot authorize this request.
    ///
    /// # Errors
    /// Rejects non-Fabric contexts and rebinding or a consumed invocation.
    pub fn with_context(
        mut self,
        context: RequestContext,
        limits: OperationLimits,
    ) -> Result<Self, UpdateError> {
        if self.context.is_some()
            || self.executed.load(Ordering::Acquire)
            || context.selection().audience() != Audience::Fabric
            || context.endpoint().origin() != ServiceOrigin::Fabric
        {
            return Err(UpdateError::InvalidInput);
        }
        self.prepared = prepare(
            self.prepared.target().clone(),
            &self.body,
            &self.options,
            Some(&context),
            limits,
        )?;
        self.context = Some(context);
        self.limits = limits;
        Ok(self)
    }

    /// Exact immutable target/input/options/effects for the caller's policy.
    pub fn prepared(&self) -> &PreparedOperation {
        &self.prepared
    }

    /// Nonexecuting plan document. Native concurrency is explicitly unguarded.
    ///
    /// `declaredEffects` are prospective, not applied or remotely confirmed.
    pub fn plan_document(&self) -> Value {
        json!({
            "schemaVersion":"0.1","status":"succeeded",
            "meta":{
                "executionMode":"plan","operationId":OPERATION_ID,
                "target":self.prepared.target(),
                "declaredEffects":self.prepared.effects(),
                "preconditions":self.prepared.preconditions()
            },
            "data":{"mode":self.options.mode,"changedFields":self.body.as_object().expect("typed patch").keys().collect::<Vec<_>>()}
        })
    }
}

fn prepare(
    target: ResolvedTarget,
    body: &Value,
    options: &UpdateOptions,
    context: Option<&RequestContext>,
    limits: OperationLimits,
) -> Result<PreparedOperation, UpdateError> {
    let mut registry = Registry::default();
    register(&mut registry)
        .map_err(|error| UpdateError::Contract(ReceiptError::Contract(error)))?;
    let resolution = registry
        .resolve(Verb::Update, target.selection(), None)
        .map_err(|error| UpdateError::Contract(ReceiptError::Contract(error)))?;
    let mut effects = vec![Effect::LocalRead];
    let mut conditions = vec![
        Precondition::ExistingOnly,
        Precondition::UnguardedConcurrency,
    ];
    if let Some(path) = &options.output_path {
        if !std::path::Path::new(path).is_absolute() {
            return Err(UpdateError::InvalidInput);
        }
        effects.push(Effect::LocalWrite);
        conditions.push(Precondition::LocalOutput { path: path.clone() });
    }
    let input = json!({
        "body":body,"options":options,
        "context":context.map(|value| json!({
            "account":value.selection().account(),"tenant":value.selection().tenant(),
            "audience":value.selection().audience().scope(),"endpoint":value.endpoint().base_url().as_str()
        })),
        "limits":{"maxRequests":limits.max_requests.get(),"maxResponseBytes":limits.max_response_bytes.get(),
            "maxPollDelayNanos":limits.max_poll_delay.as_nanos().to_string()}
    });
    PreparedOperation::new(resolution, target, input)
        .and_then(|value| value.with_optional_effects(effects))
        .and_then(|value| value.with_preconditions(conditions))
        .map_err(UpdateError::Contract)
}

/// Retained SDK result and unprojected service failure, not a cache mutation.
#[derive(Debug)]
pub struct UpdateReport {
    /// Protected neutral receipt for the exact approved item and operation.
    pub receipt: ExecutionReceipt<Value>,
    /// Original structured SDK error; never serialized implicitly.
    pub cause: Option<Box<ResourceError>>,
    /// Shared polling warnings, retained even after incomplete execution.
    pub warnings: Vec<OperationWarning>,
    /// Original returned data withheld when identity/completion was unconfirmed.
    pub unconfirmed_data: Option<Value>,
}

/// Submit once through existing type-specific metadata PATCH and shared policy.
///
/// The approval must borrow this exact context-bound request. The caller's
/// cancellation future must cover its total deadline. Accepted/unknown/partial
/// cannot prove a rename; hosts should invalidate affected metadata/path caches
/// rather than apply the requested value speculatively.
///
/// # Errors
/// Local approval/context/reuse errors send no request. Provider errors return a
/// classified report retaining original evidence; no retry or rollback occurs.
pub async fn execute<E: OperationExecutor>(
    update: &PreparedUpdate,
    approval: &ApprovedOperation<'_>,
    executor: E,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<UpdateReport, UpdateError> {
    if !std::ptr::eq(update.prepared(), approval.prepared()) {
        return Err(UpdateError::ApprovalMismatch);
    }
    let context = update.context.as_ref().ok_or(UpdateError::NotConfigured)?;
    if update.executed.swap(true, Ordering::AcqRel) {
        return Err(UpdateError::AlreadyExecuted);
    }
    let identity = update.prepared.target().identity();
    let workspace = identity.workspace_id().ok_or(UpdateError::InvalidInput)?;
    let item = identity.item_id().ok_or(UpdateError::InvalidInput)?;
    let mut session = OperationSession::with_context(context, executor, update.limits, cancelled);
    let result = ItemMutations::update_with_completion(
        &mut session,
        workspace,
        update.collection,
        item,
        update.body.clone(),
        CompletionPolicy::ReturnAccepted,
    )
    .await;
    let handles = |id: Option<String>| {
        id.into_iter()
            .map(|id| OperationHandle::FabricOperation { id })
            .collect::<Vec<_>>()
    };
    let mut unconfirmed_data = None;
    let (outcome, evidence, cause) = match result {
        Ok(ResourceOutcome::Completed { status, data }) => {
            let data = data.unwrap_or(Value::Null);
            let valid = status == 200
                && data
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.eq_ignore_ascii_case(item))
                && data.get("type").and_then(Value::as_str)
                    == Some(update.prepared.target().selection().owner_type.as_str())
                && data.get("displayName").is_some_and(Value::is_string)
                && data.get("workspaceId").is_none_or(|value| {
                    value
                        .as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(workspace))
                });
            let confirmed = valid
                && update
                    .body
                    .as_object()
                    .expect("typed patch")
                    .iter()
                    .all(|(key, value)| data.get(key) == Some(value));
            let outcome = if confirmed {
                Outcome::Succeeded { data }
            } else if valid {
                Outcome::Partial {
                    data,
                    error: info(
                        "UpdateNotConfirmed",
                        "The response did not confirm every requested field; no overwrite or replay was attempted.",
                    ),
                }
            } else {
                unconfirmed_data = Some(data);
                Outcome::Unknown {
                    error: info(
                        "InvalidResponse",
                        "The response did not establish the addressed item metadata; mutation outcome is unknown.",
                    ),
                }
            };
            let coverage = if confirmed {
                Coverage::Complete
            } else if valid {
                Coverage::Incomplete {
                    reasons: vec!["Requested metadata fields were not fully confirmed.".into()],
                    continuation: None,
                }
            } else {
                Coverage::Unknown
            };
            (
                outcome,
                ExecutionEvidence {
                    http_status: Some(status),
                    handles: handles(session.operation_id().map(str::to_owned)),
                    coverage,
                    mutation_may_have_occurred: true,
                },
                None,
            )
        }
        Ok(ResourceOutcome::Accepted { operation_id }) => {
            let handles = handles(operation_id);
            let outcome = if handles.is_empty() {
                Outcome::Unknown {
                    error: info(
                        "AcceptedUnobservable",
                        "Accepted work has no usable observation handle; completion is unknown.",
                    ),
                }
            } else {
                Outcome::Accepted
            };
            (
                outcome,
                ExecutionEvidence {
                    http_status: Some(202),
                    handles,
                    coverage: Coverage::Unknown,
                    mutation_may_have_occurred: true,
                },
                None,
            )
        }
        Err(error) => {
            let uncertain = error.operation_id.is_some()
                || (error.mutation_may_have_occurred && error.status.is_none());
            let failure = if error.status == Some(412) {
                info(
                    "PreconditionFailed",
                    "The provider rejected a precondition; no refetch or overwrite was attempted.",
                )
            } else if uncertain {
                info(
                    "CompletionUnknown",
                    "Metadata update completion is unknown; do not replay the mutation.",
                )
            } else {
                info(
                    &error.code,
                    "Metadata update failed; original typed SDK evidence is retained.",
                )
            };
            let outcome = if uncertain {
                Outcome::Unknown { error: failure }
            } else {
                Outcome::Failed { error: failure }
            };
            let evidence = ExecutionEvidence {
                http_status: error.status,
                handles: handles(error.operation_id.clone()),
                coverage: Coverage::Unknown,
                mutation_may_have_occurred: error.mutation_may_have_occurred,
            };
            (outcome, evidence, Some(Box::new(error)))
        }
    };
    let receipt =
        ExecutionReceipt::new(approval, outcome, evidence).map_err(UpdateError::Receipt)?;
    Ok(UpdateReport {
        receipt,
        cause,
        warnings: session.warnings().to_vec(),
        unconfirmed_data,
    })
}

fn info(code: &str, message: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: message.into(),
    }
}
