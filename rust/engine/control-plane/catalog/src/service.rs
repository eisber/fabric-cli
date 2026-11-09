// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One native Domain Contributor sync; no tenant discovery, auth provider or poller.
//!
//! The caller resolves the Domain and verifies admin/identity prerequisites through
//! immediate approval. The service remains the authorization authority. An empty
//! HTTP 200 completes the action, but establishes no affected-subdomain count.

pub use fabric_core::{Audience, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationResponse, OperationSession,
};
pub use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
pub use fabric_v2_contracts as v2;
use serde_json::{Value, json};
use std::{fmt, future::Future, num::NonZeroUsize};
use v2::{
    ApprovedOperation, Availability, Binding, ContractError, Coverage, Effect, ExecutionEvidence,
    ExecutionReceipt, FailureInfo, Generation, OperationDescriptor, Outcome, Precondition,
    PreparedOperation, Prerequisite, ReceiptBuildError, Registry, ResolvedTarget, SourceEvidence,
    SourceEvidenceError, TargetKind, Verb,
};

/// Stable operation identity, independent of CLI, SDK or MCP head.
pub const OPERATION_ID: &str = "fabric.service.domain.sync";

/// Add this operation and its exact conditional binding, without replacing entries.
///
/// The existing neutral registry uses `Update` for this remote permission mutation;
/// this does not register a generic CLI update alias or broaden the v1 grammar.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    candidate.register(OperationDescriptor::new(
        OPERATION_ID,
        Verb::Update,
        json!({
            "type":"object","additionalProperties":false,
            "required":["domainId","role"],
            "properties":{
                "domainId":{"type":"string","format":"uuid"},
                "role":{"const":"Contributor"}
            }
        }),
        json!({"type":"null"}),
        vec![Effect::RemoteWrite],
    )?)?;
    candidate.bind(
        Binding::new(
            "Domain",
            "roleAssignments",
            "native",
            Generation::Any,
            OPERATION_ID,
            Availability::Conditional(vec![
                Prerequisite {
                    id: "fabric-administrator".into(),
                    description: "Caller must satisfy the Fabric administrator requirement".into(),
                },
                Prerequisite {
                    id: "domain-sync-identity".into(),
                    description: "Delegated users require Tenant.ReadWrite.All; application identities require supported Fabric admin controls and permissions".into(),
                },
            ]),
        )?
        .with_target_kind(TargetKind::TenantCollection),
    )?;
    *registry = candidate;
    Ok(())
}

/// Resolver-supplied Domain UUID under an explicit tenant, never a Fabric item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainTarget {
    target: ResolvedTarget,
    domain_id: String,
}

impl DomainTarget {
    /// Validate the resolved owner/facet and UUID locally, before approval or I/O.
    ///
    /// The shared tenant identity carries the real tenant, not a synthetic item.
    /// The exact Domain UUID is retained separately in immutable approved input.
    /// Path parsing/name lookup and its identity mapping belong to the resolver.
    pub fn new(target: ResolvedTarget, domain_id: &str) -> Result<Self, InputError> {
        if target.selection().kind != TargetKind::TenantCollection
            || target.selection().owner_type != "Domain"
            || target.selection().facet != "roleAssignments"
            || target.selection().representation != "native"
            || target.child().is_some()
            || !valid_uuid(domain_id)
        {
            return Err(InputError::Target);
        }
        Ok(Self {
            target,
            domain_id: domain_id.into(),
        })
    }

    /// Original canonical path, tenant and exact capability selector.
    pub fn resolved(&self) -> &ResolvedTarget {
        &self.target
    }

    /// Original Domain UUID, never inferred from a display name.
    pub fn domain_id(&self) -> &str {
        &self.domain_id
    }
}

/// Local failures contain no provider text or supplied identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// Wrong target owner/facet, scope, or malformed Domain identity.
    Target,
    /// Only Contributor synchronization is supported by the documented API.
    Role,
    /// Registry or prepared-operation evidence differs from this exact binding.
    Binding,
    /// Caller context must match the target's tenant and Fabric audience/origin.
    Context,
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Target => "Domain sync requires one resolved tenant-scoped Domain",
            Self::Role => "Domain sync supports only the Contributor role",
            Self::Binding => "Domain sync binding or approval evidence does not match",
            Self::Context => "Domain sync requires the target tenant's Fabric request context",
        })
    }
}
impl std::error::Error for InputError {}

/// Prepare exact identity, role, permissions and unguarded-concurrency evidence.
///
/// No domain enumeration, role reads, new consent or admin escalation occurs.
pub fn prepare(
    registry: &Registry,
    target: &DomainTarget,
    role: &str,
) -> Result<PreparedOperation, InputError> {
    if role != "Contributor" {
        return Err(InputError::Role);
    }
    let resolution = registry
        .resolve(Verb::Update, target.target.selection(), None)
        .map_err(|_| InputError::Binding)?;
    let mut expected = Registry::default();
    register(&mut expected).map_err(|_| InputError::Binding)?;
    let own = expected
        .resolve(Verb::Update, target.target.selection(), None)
        .map_err(|_| InputError::Binding)?;
    if resolution.operation != own.operation || resolution.binding != own.binding {
        return Err(InputError::Binding);
    }
    PreparedOperation::new(
        resolution,
        target.target.clone(),
        json!({"domainId":target.domain_id,"role":role}),
    )
    .and_then(|prepared| {
        prepared.with_preconditions(vec![
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency,
        ])
    })
    .map_err(|_| InputError::Binding)
}

/// Native observations alongside the shared receipt, never a fabricated rowset.
pub struct SyncReport {
    /// Exact approved target/input effects and completed/failed/unknown outcome.
    pub receipt: ExecutionReceipt<Value>,
    /// Original bounded response, when execution returned one.
    pub response: Option<OperationResponse>,
    /// Original shared failure with native error data and dispatch uncertainty.
    pub error: Option<OperationError>,
}

impl fmt::Debug for SyncReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncReport")
            .field("outcome", &self.receipt.outcome())
            .field("evidence", &self.receipt.evidence())
            .finish_non_exhaustive()
    }
}

/// Failure before execution, or receipt construction preserving actual observations.
pub enum SyncError {
    /// Local evidence/context rejection; no credential or HTTP work occurred.
    Input(InputError),
    /// Original outcome/evidence and native observations remain available.
    Receipt {
        /// Shared receipt-construction failure, retaining its original outcome.
        cause: Box<ReceiptBuildError<Value>>,
        /// Exact approved target, input, effects and preconditions; no credentials.
        prepared: Box<PreparedOperation>,
        /// Native Domain UUID, role and observation facts outside projection.
        source: Box<SourceEvidence>,
        /// Original bounded native response.
        response: Option<Box<OperationResponse>>,
        /// Original execution failure.
        error: Option<Box<OperationError>>,
    },
    /// Source-evidence validation failed without losing the executed receipt.
    Evidence {
        /// Shared validation failure retains the original receipt and facts.
        cause: Box<SourceEvidenceError<Value>>,
        /// Exact approved target, input, effects and preconditions; no credentials.
        prepared: Box<PreparedOperation>,
        /// Original bounded native response.
        response: Option<Box<OperationResponse>>,
        /// Original execution failure.
        error: Option<Box<OperationError>>,
    },
}

impl fmt::Debug for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => f.debug_tuple("Input").field(error).finish(),
            Self::Receipt { .. } => f.write_str("Receipt(<retained native evidence>)"),
            Self::Evidence { .. } => f.write_str("Evidence(<retained native evidence>)"),
        }
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Domain sync failed; inspect retained typed evidence")
    }
}
impl std::error::Error for SyncError {}

/// Dispatch exactly one approved Contributor sync using the shared native executor.
///
/// The caller supplies the existing cancellation/deadline future and response
/// limits on its runtime. No retries, paging, LRO observation, remote cancellation,
/// new credentials or approval fallback are installed. Response loss after
/// dispatch is Unknown, not success, rollback or permission to replay.
pub async fn synchronize(
    context: &RequestContext,
    approved: &ApprovedOperation<'_>,
    executor: impl OperationExecutor,
    mut limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<SyncReport, SyncError> {
    let prepared = approved.prepared();
    let input = prepared.input();
    let id = input
        .get("domainId")
        .and_then(Value::as_str)
        .ok_or(SyncError::Input(InputError::Binding))?;
    let target = DomainTarget::new(prepared.target().clone(), id).map_err(SyncError::Input)?;
    let mut registry = Registry::default();
    register(&mut registry).map_err(|_| SyncError::Input(InputError::Binding))?;
    let expected = prepare(&registry, &target, "Contributor").map_err(SyncError::Input)?;
    if prepared != &expected {
        return Err(SyncError::Input(InputError::Binding));
    }
    if context.endpoint().origin() != ServiceOrigin::Fabric
        || context.selection().audience() != Audience::Fabric
        || !target
            .target
            .identity()
            .tenant_id()
            .is_some_and(|tenant| tenant.eq_ignore_ascii_case(context.selection().tenant()))
    {
        return Err(SyncError::Input(InputError::Context));
    }
    let operation = FabricOperation::new(
        ResourceMethod::Post,
        &format!("/v1/admin/domains/{id}/roleAssignments/syncToSubdomains"),
        br#"{"role":"Contributor"}"#.to_vec(),
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|_| SyncError::Input(InputError::Binding))?;
    limits.max_requests = NonZeroUsize::new(1).expect("one native action");
    let mut session = OperationSession::with_context(context, executor, limits, cancelled);
    let (outcome, evidence, response, error) = match session.execute(operation).await {
        Ok(response) => {
            let outcome = if response.status == 200 && response.body.is_empty() {
                Outcome::Succeeded { data: Value::Null }
            } else {
                Outcome::Unknown {
                    error: failure(
                        "InvalidObservation",
                        "Domain sync did not return the documented empty HTTP 200 response",
                    ),
                }
            };
            let evidence = ExecutionEvidence {
                http_status: Some(response.status),
                handles: Vec::new(),
                coverage: Coverage::Unknown,
                mutation_may_have_occurred: true,
            };
            (outcome, evidence, Some(response), None)
        }
        Err(error) => {
            let (status, code) = match &error.kind {
                OperationErrorKind::Http { status, .. } => (
                    Some(*status),
                    match status {
                        401 | 403 => "AccessDenied",
                        404 => "DomainNotFound",
                        429 => "Throttled",
                        _ => "ServiceFailure",
                    },
                ),
                OperationErrorKind::Cancelled => (None, "Cancelled"),
                OperationErrorKind::Credential => (None, "CredentialFailed"),
                OperationErrorKind::Limit => (None, "LimitExceeded"),
                OperationErrorKind::Execution(_) => (None, "ExecutionFailed"),
                _ => (None, "InvalidObservation"),
            };
            let failure = failure(
                code,
                "Domain sync failed; native evidence is retained and no replay was attempted",
            );
            let undocumented_status = status.is_some_and(|status| {
                !matches!(status, 400 | 401 | 403 | 404 | 409 | 422 | 429 | 500)
            });
            let unknown =
                undocumented_status || (status.is_none() && error.mutation_may_have_occurred);
            let outcome = if unknown {
                Outcome::Unknown { error: failure }
            } else {
                Outcome::Failed { error: failure }
            };
            let evidence = ExecutionEvidence {
                http_status: status,
                handles: Vec::new(),
                coverage: Coverage::Unknown,
                mutation_may_have_occurred: error.mutation_may_have_occurred,
            };
            (outcome, evidence, None, Some(error))
        }
    };
    let source_error = match &outcome {
        Outcome::Failed { error } | Outcome::Unknown { error } | Outcome::Partial { error, .. } => {
            Some(error.clone())
        }
        _ => None,
    };
    let source = SourceEvidence {
        source: "fabric-admin-domain-sync".into(),
        http_status: evidence.http_status,
        coverage: Coverage::Unknown,
        error: source_error,
        details: Some(json!({"domainId": id, "role": "Contributor"})),
    };
    let receipt =
        ExecutionReceipt::new(approved, outcome, evidence).map_err(|cause| SyncError::Receipt {
            cause,
            prepared: Box::new(prepared.clone()),
            source: Box::new(source.clone()),
            response: response.clone().map(Box::new),
            error: error.clone().map(Box::new),
        })?;
    let receipt = receipt
        .with_source_evidence(vec![source])
        .map_err(|cause| SyncError::Evidence {
            cause,
            prepared: Box::new(prepared.clone()),
            response: response.clone().map(Box::new),
            error: error.clone().map(Box::new),
        })?;
    Ok(SyncReport {
        receipt,
        response,
        error,
    })
}

fn failure(code: &str, message: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: message.into(),
    }
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
