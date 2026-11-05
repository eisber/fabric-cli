// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host-neutral preparation, immediate approval and execution evidence.

use crate::{Availability, ContractError, Effect, Resolution, Target};
use serde::Serialize;
use serde_json::Value;
use std::fmt;

/// Canonical owner identity established by the caller's resolver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemIdentity {
    workspace_id: String,
    item_id: String,
}

impl ItemIdentity {
    /// Validates public Fabric workspace/item UUIDs without resolving names.
    ///
    /// # Errors
    /// Rejects missing or malformed UUIDs before operation preparation.
    pub fn new(workspace_id: &str, item_id: &str) -> Result<Self, ReceiptError> {
        for id in [workspace_id, item_id] {
            if id.len() != 36
                || !id.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
            {
                return Err(ReceiptError::InvalidIdentity);
            }
        }
        Ok(Self {
            workspace_id: workspace_id.into(),
            item_id: item_id.into(),
        })
    }

    /// Returns the resolved workspace UUID, never inferred from a child name.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the resolved owner-item UUID.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
}

/// Exact resolved native child selector, not a provider URL or executable code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildIdentity {
    /// Registered native kind, such as a lineage-tag entity or name-based script.
    pub kind: String,
    /// Provider-native identifier; consumers must not change its identity domain.
    pub id: String,
}

/// One resolved resource/subresource/representation target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTarget {
    canonical_path: String,
    identity: ItemIdentity,
    selection: Target,
    child: Option<ChildIdentity>,
}

impl ResolvedTarget {
    /// Retains the resolver's canonical path and exact identity/selection.
    ///
    /// The caller owns path grammar, escaping, name resolution and child identity
    /// validation. This boundary only rejects controls, unbounded fields and URLs.
    ///
    /// # Errors
    /// Rejects malformed resolved evidence without performing resolution or I/O.
    pub fn new(
        canonical_path: &str,
        identity: ItemIdentity,
        selection: Target,
        child: Option<ChildIdentity>,
    ) -> Result<Self, ReceiptError> {
        if !canonical_path.starts_with('/')
            || canonical_path.starts_with("//")
            || !bounded_text(canonical_path, 32 * 1024)
        {
            return Err(ReceiptError::InvalidIdentity);
        }
        for selector in [
            &selection.owner_type,
            &selection.facet,
            &selection.representation,
        ] {
            crate::validate_token(selector).map_err(ReceiptError::Contract)?;
        }
        if let Some(child) = &child {
            crate::validate_token(&child.kind).map_err(ReceiptError::Contract)?;
            if !bounded_text(&child.id, 4096) {
                return Err(ReceiptError::InvalidIdentity);
            }
        }
        Ok(Self {
            canonical_path: canonical_path.into(),
            identity,
            selection,
            child,
        })
    }

    /// Returns the canonical path supplied by the resolver.
    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }

    /// Returns the owning item identity.
    pub fn identity(&self) -> &ItemIdentity {
        &self.identity
    }

    /// Returns the exact capability selector.
    pub fn selection(&self) -> &Target {
        &self.selection
    }

    /// Returns the original provider-specific child identity.
    pub fn child(&self) -> Option<&ChildIdentity> {
        self.child.as_ref()
    }
}

/// Validated input and effect evidence presented unchanged to host approval.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedOperation {
    operation_id: String,
    target: ResolvedTarget,
    input: Value,
    effects: Vec<Effect>,
    availability: Availability,
    preconditions: Vec<Precondition>,
}

impl PreparedOperation {
    /// Binds input to the discovered operation and exact resolved selection.
    ///
    /// Input-schema validation belongs to the typed operation adapter. Preparation
    /// does not assert that a conditional prerequisite has been verified.
    ///
    /// # Errors
    /// Rejects target substitution and explicitly unsupported bindings.
    pub fn new(
        resolution: Resolution<'_>,
        target: ResolvedTarget,
        input: Value,
    ) -> Result<Self, ReceiptError> {
        let binding = resolution.binding;
        if binding.operation_id() != resolution.operation.id()
            || !binding.matches(target.selection())
        {
            return Err(ReceiptError::TargetMismatch);
        }
        if matches!(binding.availability(), Availability::Unsupported(_)) {
            return Err(ReceiptError::Unsupported);
        }
        Ok(Self {
            operation_id: resolution.operation.id().into(),
            target,
            input,
            effects: resolution.operation.effects().into(),
            availability: binding.availability().clone(),
            preconditions: Vec::new(),
        })
    }

    /// Declares exact native conditions before the request can be approved.
    ///
    /// A declaration is not proof the provider can enforce it. The typed adapter
    /// validates applicability; host policy rejects unsupported required guards.
    ///
    /// # Errors
    /// Rejects duplicates, contradictory existence/concurrency conditions and
    /// malformed native revision evidence.
    pub fn with_preconditions(
        mut self,
        preconditions: Vec<Precondition>,
    ) -> Result<Self, ReceiptError> {
        let mut seen = Vec::new();
        for condition in &preconditions {
            if seen.contains(&condition) {
                return Err(ReceiptError::InvalidPrecondition);
            }
            if let Precondition::NativeRevision { authority, value } = condition {
                crate::validate_token(authority).map_err(ReceiptError::Contract)?;
                if !bounded_text(value, 8192) {
                    return Err(ReceiptError::InvalidPrecondition);
                }
            }
            seen.push(condition);
        }
        let has_revision = preconditions
            .iter()
            .any(|condition| matches!(condition, Precondition::NativeRevision { .. }));
        if (preconditions.contains(&Precondition::CreateOnly)
            && preconditions.contains(&Precondition::ExistingOnly))
            || (has_revision && preconditions.contains(&Precondition::UnguardedConcurrency))
        {
            return Err(ReceiptError::InvalidPrecondition);
        }
        self.preconditions = preconditions;
        Ok(self)
    }

    /// Returns the shared registered operation identity.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Returns the exact target presented for approval.
    pub fn target(&self) -> &ResolvedTarget {
        &self.target
    }

    /// Returns typed-adapter-validated input, never a hidden additional request.
    pub fn input(&self) -> &Value {
        &self.input
    }

    /// Returns the complete declared effect set.
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Returns prerequisites the host must verify before authorizing dispatch.
    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    /// Returns exact native/intent conditions for approval and receipt evidence.
    pub fn preconditions(&self) -> &[Precondition] {
        &self.preconditions
    }

    /// Obtains immediate approval for this immutable request through host policy.
    ///
    /// The policy must verify all prerequisites and any authorization, secure input,
    /// output reservation, concurrency and approval requirements before returning.
    /// There is no library default policy or force/approval flag.
    ///
    /// # Errors
    /// Returns the host's exact refusal. No adapter or provider is invoked here.
    pub fn authorize<P: ApprovalPolicy>(
        &self,
        policy: &mut P,
    ) -> Result<ApprovedOperation<'_>, P::Error> {
        policy.authorize(self)?;
        Ok(ApprovedOperation { prepared: self })
    }
}

/// Exact intent/condition evidence, never authority or an implied service guard.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Precondition {
    /// The addressed resource must not already exist.
    CreateOnly,
    /// The addressed resource must already exist; no upsert.
    ExistingOnly,
    /// An exact native revision from the same authoritative resource.
    NativeRevision {
        /// Native revision authority, not a document-navigation revision.
        authority: String,
        /// Opaque source token, preserved without normalization or ordering.
        value: String,
    },
    /// The request explicitly permits irreversible deletion; policy still applies.
    PermanentDeletion,
    /// The selected native operation has no enforced caller revision condition.
    UnguardedConcurrency,
}

impl fmt::Debug for PreparedOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedOperation")
            .field("operation_id", &self.operation_id)
            .field("input", &"<validated input>")
            .field("effects", &self.effects)
            .field("availability", &self.availability)
            .finish_non_exhaustive()
    }
}

/// Caller-owned immediate policy; never installed implicitly by an SDK.
pub trait ApprovalPolicy {
    /// The host's refusal type, preserving its original structured cause.
    type Error;

    /// Verify prerequisites and approve this exact immutable target/input/effects.
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error>;
}

/// Immediate host approval borrowed from one immutable prepared request.
///
/// It cannot be deserialized, persisted as a reusable grant, or rebound to another
/// request. An adapter still enforces the native endpoint's authorization.
#[derive(Debug)]
pub struct ApprovedOperation<'a> {
    prepared: &'a PreparedOperation,
}

impl ApprovedOperation<'_> {
    /// Returns exactly the prepared request approved by the host.
    pub fn prepared(&self) -> &PreparedOperation {
        self.prepared
    }
}

/// Typed handle namespaces; observing one never acquires another's cancel API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OperationHandle {
    /// Public Fabric operation identity, not a conversation or item identity.
    FabricOperation {
        /// Provider-returned operation ID.
        id: String,
    },
    /// Native conversation and, when established, exact turn.
    Conversation {
        /// Original service-returned conversation ID.
        conversation_id: String,
        /// Service-returned turn ID, not guessed from an observation timestamp.
        turn_id: Option<String>,
    },
    /// Provider-advertised MCP task identity.
    McpTask {
        /// Original MCP task ID.
        id: String,
    },
}

/// Observed scope completeness, independent of successful request delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Coverage {
    /// The explicitly requested scope was fully observed.
    Complete,
    /// A finite scope remains unobserved, denied, unavailable or outside a budget.
    Incomplete {
        /// Source-backed reasons; these must not be hidden by an empty data array.
        reasons: Vec<String>,
        /// Native opaque continuation when genuine resumability exists.
        continuation: Option<String>,
    },
    /// The source cannot establish completeness for this operation.
    Unknown,
}

/// Retained native execution evidence that projection must not replace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEvidence {
    /// Actual observed HTTP status, absent when no response exists.
    pub http_status: Option<u16>,
    /// Original provider handles needed for observation or reconciliation.
    pub handles: Vec<OperationHandle>,
    /// Actual requested-scope coverage.
    pub coverage: Coverage,
    /// Whether a remote mutation may have happened despite local failure.
    pub mutation_may_have_occurred: bool,
}

/// Stable failure classification and an explicit host-approved diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureInfo {
    /// Machine-readable error code.
    pub code: String,
    /// Safe message selected by the operation/host, never an implicit raw error dump.
    pub message: String,
}

/// Distinct terminal, nonterminal and uncertain execution outcomes.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome<T> {
    /// Completion is established for the declared operation/scope.
    Succeeded {
        /// Typed result; an empty valid result differs from unavailable data.
        data: T,
    },
    /// Work was accepted; completion has not been established.
    Accepted,
    /// Some scoped work/results exist, but the operation is incomplete.
    Partial {
        /// Actual completed data, not a fabricated complete result.
        data: T,
        /// Classified incomplete effects or failures.
        error: FailureInfo,
    },
    /// Failure was observed; original evidence still determines possible effects.
    Failed {
        /// Classified failure.
        error: FailureInfo,
    },
    /// The provider's final outcome is not established.
    Unknown {
        /// Classified uncertainty; no automatic replay is authorized.
        error: FailureInfo,
    },
}

/// Common v2 envelope with immutable operation/target evidence outside data.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionReceipt<T> {
    schema_version: &'static str,
    #[serde(flatten)]
    outcome: Outcome<T>,
    meta: ReceiptMetadata,
}

/// Scoped unprojected receipt metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptMetadata {
    operation_id: String,
    target: ResolvedTarget,
    declared_effects: Vec<Effect>,
    preconditions: Vec<Precondition>,
    evidence: ExecutionEvidence,
}

/// Receipt construction failed without discarding the original execution result.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiptBuildError<T> {
    /// Why the result cannot be presented as a valid receipt.
    pub kind: ReceiptError,
    /// Original outcome, including any typed completed data.
    pub outcome: Outcome<T>,
    /// Original handles, effects uncertainty and coverage.
    pub evidence: ExecutionEvidence,
}

impl<T> fmt::Display for ReceiptBuildError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl<T: fmt::Debug> std::error::Error for ReceiptBuildError<T> {}

impl<T> ExecutionReceipt<T> {
    /// Builds a receipt retaining the exact approved request's identity/effects.
    ///
    /// # Errors
    /// Rejects empty/invalid handles, impossible HTTP statuses and healthy-looking
    /// complete outcomes after incomplete coverage or acceptance-only evidence.
    pub fn new(
        approved: &ApprovedOperation<'_>,
        outcome: Outcome<T>,
        evidence: ExecutionEvidence,
    ) -> Result<Self, Box<ReceiptBuildError<T>>> {
        if let Err(kind) = validate_evidence(&outcome, &evidence) {
            return Err(Box::new(ReceiptBuildError {
                kind,
                outcome,
                evidence,
            }));
        }
        Ok(Self {
            schema_version: "0.1",
            outcome,
            meta: ReceiptMetadata {
                operation_id: approved.prepared.operation_id.clone(),
                target: approved.prepared.target.clone(),
                declared_effects: approved.prepared.effects.clone(),
                preconditions: approved.prepared.preconditions.clone(),
                evidence,
            },
        })
    }

    /// Returns the actual outcome, not a boolean success approximation.
    pub fn outcome(&self) -> &Outcome<T> {
        &self.outcome
    }

    /// Returns original evidence, preserved even if rendering later fails.
    pub fn evidence(&self) -> &ExecutionEvidence {
        &self.meta.evidence
    }

    /// Returns the resolved scoped target retained outside projected data.
    pub fn target(&self) -> &ResolvedTarget {
        &self.meta.target
    }

    /// Returns original conditions outside result-data projection.
    pub fn preconditions(&self) -> &[Precondition] {
        &self.meta.preconditions
    }
}

fn validate_evidence<T>(
    outcome: &Outcome<T>,
    evidence: &ExecutionEvidence,
) -> Result<(), ReceiptError> {
    if evidence
        .http_status
        .is_some_and(|status| !(100..=599).contains(&status))
        || evidence.handles.iter().any(|handle| match handle {
            OperationHandle::FabricOperation { id } | OperationHandle::McpTask { id } => {
                !bounded_text(id, 4096)
            }
            OperationHandle::Conversation {
                conversation_id,
                turn_id,
            } => {
                !bounded_text(conversation_id, 4096)
                    || turn_id.as_ref().is_some_and(|id| !bounded_text(id, 4096))
            }
        })
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    if matches!(outcome, Outcome::Accepted) && evidence.handles.is_empty() {
        return Err(ReceiptError::MissingHandle);
    }
    if matches!(outcome, Outcome::Accepted)
        && evidence
            .http_status
            .is_some_and(|status| !(200..300).contains(&status))
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    if let Coverage::Incomplete { reasons, .. } = &evidence.coverage
        && (reasons.is_empty() || reasons.iter().any(|reason| reason.trim().is_empty()))
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    if matches!(outcome, Outcome::Succeeded { .. })
        && (matches!(&evidence.coverage, Coverage::Incomplete { .. })
            || evidence
                .http_status
                .is_some_and(|status| status >= 400 || status == 202))
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    Ok(())
}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

/// Invalid preparation or contradictory execution evidence, never a provider retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptError {
    /// A resolver supplied malformed resource/subresource evidence.
    InvalidIdentity,
    /// Native preconditions are malformed, repeated or contradictory.
    InvalidPrecondition,
    /// The discovered binding does not match the approved target.
    TargetMismatch,
    /// This capability is explicitly unsupported.
    Unsupported,
    /// Acceptance did not retain any source-backed observation handle.
    MissingHandle,
    /// Outcome and evidence contradict one another or contain malformed fields.
    InvalidEvidence,
    /// Registration selector validation failed.
    Contract(ContractError),
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid v2 preparation or execution evidence")
    }
}

impl std::error::Error for ReceiptError {}
