// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host-neutral deletion assessments and actual execution evidence.
//!
//! Metadata describes the authorized request, not proof that its effects occurred.
//! These serializable data types are not executable assessments or authorizations.

use serde::Serialize;

/// Resource type covered by an exact deletion assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationResourceType {
    /// One Fabric Notebook and its owned content.
    Notebook,
}

/// Exact identifiers, with no display-name or collection lookup.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationTarget {
    /// Canonical workspace GUID.
    pub workspace_id: String,
    /// Canonical item GUID.
    pub item_id: String,
    /// The caller's explicitly selected resource type, not a service observation.
    pub resource_type: OperationResourceType,
}

/// A bounded effect on the owning target, never on an external resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationEffectKind {
    /// Removal of the owning Notebook's item lifecycle.
    NotebookLifecycle,
    /// Removal of content owned by that Notebook.
    NotebookContent,
}

/// A stable effect identifier within an operation descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OperationEffect {
    /// Stable identifier used to correlate execution evidence.
    pub id: &'static str,
    /// The effect's scope; its target is the assessment's exact target.
    pub kind: OperationEffectKind,
}

/// Explicit capability declaration, not an optimistic default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    /// This operation cannot provide this capability.
    Unsupported,
}

/// The requested deletion mode, not a guarantee of physical erasure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionMode {
    /// Recovery must be guaranteed; unsupported by the native Notebook delete.
    Recoverable,
    /// Explicit hard-delete request, with no physical-erasure claim.
    Permanent,
}

/// Recovery semantics established by the operation contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverySemantics {
    /// Recovery and physical retention remain unknown and service-defined.
    UnknownServiceDefined,
}

/// Required trusted-host authorization for the assessed effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// An explicit host decision bound to the full immutable assessment.
    ExplicitHostAuthorization,
}

/// How the selected resource type is enforced at the mutation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetTypeEnforcement {
    /// A type-specific service endpoint enforces the resource type.
    ServiceEndpoint,
}

/// Service role required in addition to the host's explicit approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredWorkspaceRole {
    /// Hard deletion requires the workspace Admin role.
    Admin,
}

/// Available completion observation, not an independent read-after-delete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSupport {
    /// Only a terminal response is documented; no LRO support is promised.
    TerminalResponseOnly,
}

/// Requested target traversal, including rejected requests in failure metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraversalMode {
    /// Only the exact owning target and its owned content.
    ExactTarget,
    /// The caller requested unsupported recursive traversal.
    RecursiveRequested,
}

/// Requested precondition, including rejected conditional requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPrecondition {
    /// No concurrency precondition is sent.
    Unconditional,
    /// The caller requested an unsupported If-Match precondition.
    IfMatchRequested,
}

/// Requested observation policy for this invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationMode {
    /// Return service acceptance without polling or claiming completion.
    ReturnAccepted,
    /// Ask the shared executor to reconcile completion when advertised.
    WaitForCompletion,
}

/// Pinned contract references, not a runtime service capability discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OperationProvenance {
    /// Public service contract reference.
    pub service_contract: &'static str,
    /// Source commit preserving the existing hard-delete request spelling.
    pub source_revision: &'static str,
}

/// Nonsecret caller-selected execution context bound into host approval.
///
/// This data does not contain credentials or verify the executor's identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationContext {
    /// Explicit nonsecret account selector supplied to the injected provider.
    pub account: String,
    /// Explicit tenant selector supplied to the injected provider.
    pub tenant: String,
    /// Canonical HTTPS service origin, with a root path and no query.
    pub endpoint: String,
}

/// Static operation capabilities, separate from a single invocation's identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OperationDescriptor {
    /// Stable operation identifier; never used as an invocation identity.
    pub id: &'static str,
    /// Explicit resource type supported by this descriptor.
    pub resource_type: OperationResourceType,
    /// Enforcement at execution, not trust in a caller-supplied type suffix.
    pub target_type_enforcement: TargetTypeEnforcement,
    /// Exact effects on the owning target; no external targets are implied.
    pub effects: &'static [OperationEffect],
    /// Whether broad recursive traversal can be requested.
    pub recursive_support: CapabilitySupport,
    /// Whether concurrency conditions can be enforced.
    pub conditional_support: CapabilitySupport,
    /// How completion can be observed through the shared executor.
    pub observation_support: ObservationSupport,
    /// Established recovery guarantee, if any.
    pub recovery: RecoverySemantics,
    /// Required authorization before dispatch.
    pub approval: ApprovalRequirement,
    /// Service role required independently of trusted-host approval.
    pub required_workspace_role: RequiredWorkspaceRole,
    /// References behind these bounded capabilities.
    pub provenance: OperationProvenance,
}

/// Resolved requested effects retained unchanged through execution and failures.
///
/// Construction of this data alone cannot create a valid SDK assessment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationMetadata {
    /// The stable descriptor and its explicit capability declarations.
    pub descriptor: OperationDescriptor,
    /// Process-local assessment sequence, not a persistent service operation ID.
    pub invocation_id: u64,
    /// Explicit host-selected context, or absent for caller-controlled SDK hosts.
    pub context: Option<OperationContext>,
    /// Exact workspace, item and explicitly selected type.
    pub target: OperationTarget,
    /// Explicit requested permanence; never a physical-erasure guarantee.
    pub deletion_mode: DeletionMode,
    /// Exact or rejected recursive target traversal.
    pub traversal: TraversalMode,
    /// Requested concurrency condition.
    pub precondition: OperationPrecondition,
    /// Selected completion policy.
    pub observation: ObservationMode,
}

/// Actual result classification, distinct from the requested metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// A documented terminal success response was observed.
    Succeeded,
    /// Acceptance was observed; completion remains unproven.
    Accepted,
    /// Local rejection or a known service failure was observed.
    Failed,
    /// A mutation may have occurred without terminal proof.
    Unknown,
}

/// Whether terminal completion is actually established.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionEvidence {
    /// A documented terminal success response was observed.
    Completed,
    /// No terminal success proof is available.
    Unknown,
}

/// Dispatch information reported by the injected executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchEvidence {
    /// Local validation or the executor established no mutation dispatch.
    NotDispatched,
    /// A service response was observed.
    ResponseObserved,
    /// The executor cannot exclude mutation dispatch or execution.
    Uncertain,
}

/// Outcome evidence correlated to a descriptor's effect identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct EffectEvidence {
    /// Identifier from the assessed descriptor.
    pub effect_id: &'static str,
    /// Evidence classification, not a separate observation of stored content.
    pub status: OperationStatus,
}

/// Actual execution evidence; acceptance and uncertainty are not completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationEvidence {
    /// Overall observed outcome.
    pub status: OperationStatus,
    /// Terminal completion proof, if any.
    pub completion: CompletionEvidence,
    /// Service-advertised operation ID, distinct from the assessment sequence.
    pub operation_id: Option<String>,
    /// Observed HTTP status, when available.
    pub http_status: Option<u16>,
    /// Dispatch information retained even on failure.
    pub dispatch: DispatchEvidence,
    /// Whether the shared executor can exclude a mutation.
    pub mutation_may_have_occurred: bool,
    /// Evidence for each assessed owning-target effect.
    pub effects: Vec<EffectEvidence>,
}
