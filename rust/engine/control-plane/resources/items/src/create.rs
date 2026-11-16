// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline create-policy planning and result verification for SDK, CLI and MCP consumers.
//!
//! This module mounts **no** backend adapter, endpoint, transport or runtime. It is
//! not a native create implementation and does not change the existing v1 mutations.
//! A target-specific adapter must validate its schema, resolve an exact target,
//! advertise provider guarantees, and execute the chosen action without changing
//! its semantics. Preflight observations alone cannot guarantee race-free create,
//! identity-preserving replacement, or fresh duplicate identity. The executing
//! adapter must enforce those guarantees at mutation time, including conflicts
//! introduced after preflight; delete/recreate and implicit upsert are forbidden.
//!
//! Input validation is an explicit trusted adapter boundary, not a generic JSON
//! validator. The planner owns the validated input token and never exposes a
//! constructor that treats arbitrary JSON or a boolean as proof of schema validity.
//! No dispatch, retry, polling, cancellation, credential or settings policy is added.

use serde_json::Value;
use std::{fmt, str::FromStr};

/// Requested conflict behavior, independent of the eventual action and outcome.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConflictPolicy {
    /// Create an absent exact leaf; reject an existing leaf.
    #[default]
    Error,
    /// Create an absent leaf or replace the advertised complete unit in place.
    Overwrite,
    /// Create an absent leaf or leave an exact existing correct-kind leaf untouched.
    Skip,
    /// Always create a fresh identity in an explicitly selected parent collection.
    Duplicate,
}

impl ConflictPolicy {
    /// Returns the stable policy spelling; CLI `--force` maps to `Overwrite`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Overwrite => "overwrite",
            Self::Skip => "skip",
            Self::Duplicate => "duplicate",
        }
    }

    /// Parses an exact policy spelling without aliases or normalization.
    ///
    /// # Errors
    /// Returns [`CreateError::InvalidPolicy`] for any other value.
    pub fn parse(value: &str) -> Result<Self, CreateError> {
        match value {
            "error" => Ok(Self::Error),
            "overwrite" => Ok(Self::Overwrite),
            "skip" => Ok(Self::Skip),
            "duplicate" => Ok(Self::Duplicate),
            _ => Err(CreateError::InvalidPolicy),
        }
    }
}

impl FromStr for ConflictPolicy {
    type Err = CreateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Opaque provider-native identifier, not restricted to GUID syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeId(String);

impl NativeId {
    /// Preserves a nonblank identifier verbatim, rejecting control characters.
    ///
    /// # Errors
    /// Returns [`CreateError::InvalidIdentifier`] for blank or control-bearing IDs.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateError> {
        let value = value.into();
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(CreateError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    /// Returns the unnormalized native identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded, non-secret adapter schema name and revision, not a URL to fetch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaRef(String);

impl SchemaRef {
    /// Stores an explicit schema reference of at most 256 UTF-8 bytes.
    ///
    /// # Errors
    /// Rejects blank, oversized or control-bearing references.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateError> {
        let value = value.into();
        if !valid_label(&value) || value.len() > 256 {
            return Err(CreateError::InvalidSchema);
        }
        Ok(Self(value))
    }

    /// Returns the exact adapter-owned schema reference.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact advertised replacement boundary, for example `definition/v1`.
///
/// Metadata-only resources may advertise a complete metadata unit. This is not
/// permission to treat omitted input, an empty patch, or partial metadata as a
/// complete replacement. The target's schema validator decides completeness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementUnit(String);

impl ReplacementUnit {
    /// Stores a nonblank unit identifier of at most 256 UTF-8 bytes.
    ///
    /// # Errors
    /// Rejects oversized or control-bearing unit identifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateError> {
        let value = value.into();
        if !valid_label(&value) || value.len() > 256 {
            return Err(CreateError::InvalidReplacementUnit);
        }
        Ok(Self(value))
    }

    /// Returns the exact replacement-unit identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Selection intent, never inferred from file contents or observed name counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetScope {
    /// A single exact named leaf within an existing parent.
    NamedLeaf,
    /// Explicit parent collection plus the exact name to assign to a new resource.
    ParentCollection,
}

/// Resolved native parent and unmodified Unicode name/kind.
///
/// Path parsing and resolution belong to the caller. A path-looking name is not
/// split, case-folded, normalized, escaped or interpreted here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTarget {
    scope: TargetScope,
    parent_id: NativeId,
    name: String,
    item_type: String,
}

impl CreateTarget {
    /// Constructs an explicit selector, preserving name and kind verbatim.
    ///
    /// # Errors
    /// Rejects blank or control-bearing names and kinds.
    pub fn new(
        scope: TargetScope,
        parent_id: NativeId,
        name: impl Into<String>,
        item_type: impl Into<String>,
    ) -> Result<Self, CreateError> {
        let name = name.into();
        let item_type = item_type.into();
        if !valid_label(&name) || !valid_label(&item_type) {
            return Err(CreateError::InvalidTarget);
        }
        Ok(Self {
            scope,
            parent_id,
            name,
            item_type,
        })
    }

    /// Returns the explicit selection intent.
    pub const fn scope(&self) -> TargetScope {
        self.scope
    }

    /// Returns the resolved provider-native parent identifier.
    pub fn parent_id(&self) -> &NativeId {
        &self.parent_id
    }

    /// Returns the exact requested name, including Unicode and separators.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact requested resource kind.
    pub fn item_type(&self) -> &str {
        &self.item_type
    }
}

/// Native identity that replacement and skip must preserve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceIdentity {
    native_id: NativeId,
    parent_id: NativeId,
    item_type: String,
}

impl ResourceIdentity {
    /// Binds an opaque resource ID to its native parent and exact kind.
    ///
    /// # Errors
    /// Rejects a blank or control-bearing kind.
    pub fn new(
        native_id: NativeId,
        parent_id: NativeId,
        item_type: impl Into<String>,
    ) -> Result<Self, CreateError> {
        let item_type = item_type.into();
        if !valid_label(&item_type) {
            return Err(CreateError::InvalidTarget);
        }
        Ok(Self {
            native_id,
            parent_id,
            item_type,
        })
    }

    /// Returns the resource's native ID.
    pub fn native_id(&self) -> &NativeId {
        &self.native_id
    }

    /// Returns the resource's native parent ID.
    pub fn parent_id(&self) -> &NativeId {
        &self.parent_id
    }

    /// Returns the resource's exact kind.
    pub fn item_type(&self) -> &str {
        &self.item_type
    }
}

/// Exact leaf resolution; `Missing` always means its parent exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeafState {
    /// The existing parent contains no matching named leaf.
    Missing,
    /// Exactly one named leaf was found.
    Existing(ResourceIdentity),
    /// More than one leaf matched; no first-match selection is permitted.
    Ambiguous,
    /// The exact leaf resolves to the wrong resource kind.
    WrongKind,
}

/// Caller-owned preflight observation, not a provider mutation guarantee.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Preflight {
    /// The selected parent does not exist.
    MissingParent,
    /// Exact named-leaf resolution within an existing parent.
    Leaf(LeafState),
    /// Explicit existing collection with all currently observed same-name matches.
    ///
    /// Zero, one and multiple matches are valid for duplicate. The adapter's
    /// fresh-identity guarantee must also cover identities not in this snapshot.
    Collection {
        /// Observed exact-name identities of the selected kind in this parent.
        known_matches: Vec<ResourceIdentity>,
    },
}

/// Provider guarantee for exact-leaf creation at execution time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateSupport {
    /// No approved exact-leaf creation adapter exists.
    Unsupported,
    /// Creation rejects a concurrently appearing leaf; it never becomes upsert.
    RejectExisting,
}

/// Explicit target support for leaving an existing exact leaf unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipSupport {
    /// This target does not advertise skip.
    Unsupported,
    /// An exact correct-kind existing leaf can be reported unchanged.
    ExactExistingUnchanged,
}

/// Explicit provider guarantee for collection duplicate creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateSupport {
    /// No approved repeated-name duplicate adapter exists.
    Unsupported,
    /// Repeated names are legal and every call creates a fresh native ID.
    ///
    /// The exact requested name must be retained, without auto-rename, reuse,
    /// overwrite or name-count-dependent behavior.
    FreshIdentityWithRepeatedNames,
}

/// Explicit guarantees for this target and schema, supplied by its trusted adapter.
///
/// `replacement` advertises an in-place complete-unit replacement that preserves
/// native ID, kind, parent, name and all metadata outside the advertised unit.
/// It must not be advertised for an adapter implemented using delete/recreate.
/// These declarations are obligations, not facts inferred from preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateCapabilities {
    /// Target-specific input schema and revision.
    pub schema: SchemaRef,
    /// Exact-leaf creation guarantee.
    pub create: CreateSupport,
    /// Approved complete replacement unit, or unsupported.
    pub replacement: Option<ReplacementUnit>,
    /// Exact-leaf skip support.
    pub skip: SkipSupport,
    /// Repeated-name, fresh-identity duplicate guarantee.
    pub duplicate: DuplicateSupport,
}

/// Schema adapter's classification after validating the actual input.
///
/// There is deliberately no `valid: bool` or generic "any JSON" validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputDisposition {
    /// Valid input sufficient to create, but not to replace a complete unit.
    CreateOnly,
    /// Valid input sufficient to create and to replace the exact complete unit.
    CreateAndReplace(ReplacementUnit),
    /// Valid complete replacement input that is insufficient to create.
    ReplaceOnly(ReplacementUnit),
    /// Valid explicitly supplied input, but no mutation is permitted with it.
    ///
    /// An exact existing leaf can still be skipped; this says nothing about
    /// equality between supplied input and existing content.
    SkipOnly,
}

/// Per-target schema validation boundary implemented by a caller-owned adapter.
///
/// Implementations must reject unknown schemas/targets and validate every
/// explicitly supplied value, including for skip. Missing input may be sufficient
/// for a target's create defaults, but never proves replacement completeness.
/// Validation must be pure and offline, with no credential, file or service I/O.
/// This trait installs no generic plugin discovery or native adapter.
pub trait CreateInputValidator {
    /// Validates actual input before any action is planned.
    ///
    /// # Errors
    /// Use [`CreateError::InputRejected`] for invalid input or an unsupported
    /// schema/target binding. Do not copy secret input into errors.
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError>;
}

/// Planner-owned schema-validated input, bound to an exact target and schema.
///
/// There is no public constructor. Debug omits input, and borrowing rather than
/// mutating the original JSON prevents substitution after schema validation.
#[derive(Clone, PartialEq)]
pub struct ValidatedInput {
    target: CreateTarget,
    schema: SchemaRef,
    supplied: Option<Value>,
    disposition: InputDisposition,
}

impl ValidatedInput {
    /// Returns the exact target presented to the schema validator.
    pub fn target(&self) -> &CreateTarget {
        &self.target
    }

    /// Returns the schema revision used by the target adapter.
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Borrows explicitly supplied JSON, distinguishing omitted from JSON null.
    pub fn supplied(&self) -> Option<&Value> {
        self.supplied.as_ref()
    }

    /// Returns the target adapter's validated classification.
    pub fn disposition(&self) -> &InputDisposition {
        &self.disposition
    }
}

impl fmt::Debug for ValidatedInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidatedInput")
            .field("schema", &self.schema)
            .field("supplied", &self.supplied.is_some())
            .field("disposition", &self.disposition)
            .finish_non_exhaustive()
    }
}

/// Actual action selected by the pure planner, not an execution outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum CreateAction {
    /// Create the absent exact named leaf, rejecting a concurrent conflict.
    Create {
        /// Target-specific validated creation input.
        input: ValidatedInput,
    },
    /// Replace the exact full unit in place, preserving everything outside it.
    Replace {
        /// Existing native identity, kind and parent that must be retained.
        existing: ResourceIdentity,
        /// Exact complete advertised replacement boundary.
        unit: ReplacementUnit,
        /// Explicitly supplied, schema-validated complete replacement input.
        input: ValidatedInput,
    },
    /// Leave the exact existing correct-kind leaf untouched.
    Skip {
        /// Identity left unchanged; this is not a content-equality assertion.
        existing: ResourceIdentity,
    },
    /// Create in the explicit collection, retaining name and requiring a new ID.
    Duplicate {
        /// Observed same-name identities that must not be returned as the new ID.
        known_matches: Vec<ResourceIdentity>,
        /// Target-specific validated creation input.
        input: ValidatedInput,
    },
}

/// Immutable plan preserving requested policy separately from actual action.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatePlan {
    policy: ConflictPolicy,
    target: CreateTarget,
    action: CreateAction,
}

impl CreatePlan {
    /// Returns the requested policy, even when overwrite/skip selects creation.
    pub const fn policy(&self) -> ConflictPolicy {
        self.policy
    }

    /// Returns the exact immutable selection.
    pub fn target(&self) -> &CreateTarget {
        &self.target
    }

    /// Returns the selected action; no execution has occurred.
    pub fn action(&self) -> &CreateAction {
        &self.action
    }
}

/// Validates target-specific input and plans exactly one bounded action.
///
/// The validator is always called before conflict handling, including skip.
/// Unsupported policies fail even when the target is missing. Duplicate requires
/// explicit collection selection, irrespective of how many same-name leaves
/// exist. File input never switches the selected action to upsert.
///
/// # Errors
/// Rejects invalid input, unsupported guarantees, mismatched selection/preflight,
/// missing parents, ambiguous/wrong-kind leaves, conflicts, and incomplete
/// replacement input. This function performs no mutation or I/O.
pub fn plan_create(
    policy: ConflictPolicy,
    target: CreateTarget,
    preflight: Preflight,
    capabilities: &CreateCapabilities,
    supplied: Option<Value>,
    validator: &dyn CreateInputValidator,
) -> Result<CreatePlan, CreateError> {
    let input = validate_create_request(policy, target.clone(), capabilities, supplied, validator)?;
    let action = match (policy, target.scope, preflight) {
        (_, _, Preflight::MissingParent) => return Err(CreateError::MissingParent),
        (
            ConflictPolicy::Duplicate,
            TargetScope::ParentCollection,
            Preflight::Collection { known_matches },
        ) => {
            for identity in &known_matches {
                check_target_identity(&target, identity)?;
            }
            require_creation_input(&input)?;
            CreateAction::Duplicate {
                known_matches,
                input,
            }
        }
        (ConflictPolicy::Duplicate, _, _) => {
            return Err(CreateError::DuplicateRequiresCollection);
        }
        (_, TargetScope::NamedLeaf, Preflight::Leaf(leaf)) => match leaf {
            LeafState::Missing => {
                if capabilities.create == CreateSupport::Unsupported {
                    return Err(CreateError::UnsupportedCreation);
                }
                require_creation_input(&input)?;
                CreateAction::Create { input }
            }
            LeafState::Existing(existing) => {
                check_target_identity(&target, &existing)?;
                match policy {
                    ConflictPolicy::Error => return Err(CreateError::AlreadyExists),
                    ConflictPolicy::Overwrite => {
                        let unit = capabilities
                            .replacement
                            .as_ref()
                            .ok_or(CreateError::UnsupportedPolicy)?;
                        if input.supplied.is_none() {
                            return Err(CreateError::IncompleteReplacement);
                        }
                        match &input.disposition {
                            InputDisposition::CreateAndReplace(validated)
                            | InputDisposition::ReplaceOnly(validated)
                                if validated == unit => {}
                            _ => return Err(CreateError::IncompleteReplacement),
                        }
                        CreateAction::Replace {
                            existing,
                            unit: unit.clone(),
                            input,
                        }
                    }
                    ConflictPolicy::Skip => CreateAction::Skip { existing },
                    ConflictPolicy::Duplicate => {
                        return Err(CreateError::DuplicateRequiresCollection);
                    }
                }
            }
            LeafState::Ambiguous => return Err(CreateError::AmbiguousLeaf),
            LeafState::WrongKind => return Err(CreateError::WrongKind),
        },
        _ => return Err(CreateError::SelectionMismatch),
    };
    Ok(CreatePlan {
        policy,
        target,
        action,
    })
}

/// Validate schema and requested capability without inventing target existence.
///
/// Hosts may perform this pure admission before discovery. Creation/replacement
/// completeness still depends on the real preflight and is checked by
/// [`plan_create`], which reuses this same validation authority.
///
/// # Errors
/// Rejects invalid explicit input and unsupported requested policies.
pub fn validate_create_request(
    policy: ConflictPolicy,
    target: CreateTarget,
    capabilities: &CreateCapabilities,
    supplied: Option<Value>,
    validator: &dyn CreateInputValidator,
) -> Result<ValidatedInput, CreateError> {
    let disposition = validator.validate(&target, &capabilities.schema, supplied.as_ref())?;
    let input = ValidatedInput {
        target,
        schema: capabilities.schema.clone(),
        supplied,
        disposition,
    };
    validate_policy_support(policy, capabilities)?;
    Ok(input)
}

/// Check policy capability without reading input or assuming target existence.
///
/// Hosts use this pure admission before file/stdin I/O. It grants no schema
/// evidence: admitted requests must still validate every explicit input through
/// [`validate_create_request`] and plan against real target discovery.
///
/// # Errors
/// Returns [`CreateError::UnsupportedPolicy`] for an unadvertised policy.
pub fn validate_policy_support(
    policy: ConflictPolicy,
    capabilities: &CreateCapabilities,
) -> Result<(), CreateError> {
    match policy {
        ConflictPolicy::Error if capabilities.create == CreateSupport::Unsupported => {
            return Err(CreateError::UnsupportedPolicy);
        }
        ConflictPolicy::Overwrite if capabilities.replacement.is_none() => {
            return Err(CreateError::UnsupportedPolicy);
        }
        ConflictPolicy::Skip if capabilities.skip == SkipSupport::Unsupported => {
            return Err(CreateError::UnsupportedPolicy);
        }
        ConflictPolicy::Duplicate if capabilities.duplicate == DuplicateSupport::Unsupported => {
            return Err(CreateError::UnsupportedPolicy);
        }
        _ => {}
    }
    Ok(())
}

fn require_creation_input(input: &ValidatedInput) -> Result<(), CreateError> {
    match &input.disposition {
        InputDisposition::CreateOnly | InputDisposition::CreateAndReplace(_) => Ok(()),
        InputDisposition::ReplaceOnly(_) | InputDisposition::SkipOnly => {
            Err(CreateError::InputCannotCreate)
        }
    }
}

fn check_target_identity(
    target: &CreateTarget,
    identity: &ResourceIdentity,
) -> Result<(), CreateError> {
    if identity.parent_id != target.parent_id {
        return Err(CreateError::ParentMismatch);
    }
    if identity.item_type != target.item_type {
        return Err(CreateError::WrongKind);
    }
    Ok(())
}

fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

/// Provider-owned acceptance reference, deliberately distinct from a native ID.
///
/// This is only an opaque reconciliation reference. It does not assert that a
/// resource exists, expose cancellation, or implement an asynchronous runtime.
#[derive(Clone, Eq, PartialEq)]
pub struct AcceptedHandle(String);

impl AcceptedHandle {
    /// Stores an opaque non-secret reference of at most 2048 UTF-8 bytes.
    ///
    /// # Errors
    /// Rejects blank, oversized or control-bearing handles.
    pub fn new(value: impl Into<String>) -> Result<Self, CreateError> {
        let value = value.into();
        if !valid_label(&value) || value.len() > 2048 {
            return Err(CreateError::InvalidAcceptedHandle);
        }
        Ok(Self(value))
    }

    /// Borrows the caller-owned reconciliation reference; never a resource ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AcceptedHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AcceptedHandle(<opaque>)")
    }
}

/// Adapter evidence about full replacement and unrelated-property preservation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacementEvidence {
    /// No authoritative full-unit/preservation verification was obtained.
    Unverified,
    /// The adapter verified the full exact unit and unchanged unrelated metadata.
    ///
    /// This must come from an authoritative response/read and the provider's
    /// approved in-place semantics, not merely from an HTTP success code.
    Verified(ReplacementUnit),
}

/// Resource evidence, which may be incomplete and is not itself a success.
///
/// Identity fields are independent so a known native ID or name is retained even
/// when the provider has not yet supplied the parent or kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceResult {
    /// Available resource-native ID, absent only if not obtained.
    pub native_id: Option<NativeId>,
    /// Available parent-native ID, independently of resource ID and kind.
    pub parent_id: Option<NativeId>,
    /// Available exact resource kind, independently of resource ID and parent.
    pub item_type: Option<String>,
    /// Available unnormalized name, absent only if not obtained.
    pub name: Option<String>,
    /// Evidence required to verify full replacement, not just identity retention.
    pub replacement: ReplacementEvidence,
}

/// Mutation uncertainty retained on partial, unknown and failed outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationEffect {
    /// The adapter has evidence that no mutation occurred.
    NotOccurred,
    /// A mutation may have occurred; automatic replay is not authorized.
    MayHaveOccurred,
}

/// Shared observation evidence, independent of lifecycle classification.
///
/// Adapters must carry previously obtained handles and resource fields forward
/// into later partial, unknown or failed observations. A timeout does not erase
/// an acceptance reference or known resource ID. Absence means not obtained, not
/// discarded because the latest provider response omitted the field.
///
/// There is deliberately no default empty observation or lifecycle success
/// fallback. This module does not poll, accumulate provider responses or infer
/// completion; callers retain this evidence while performing their own execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationEvidence {
    /// Opaque accepted-operation reference, retained after acceptance if obtained.
    pub operation_handle: Option<AcceptedHandle>,
    /// All available resource fields, including independently partial identity.
    pub resource: Option<ResourceResult>,
    /// Original side-effect certainty; failure is never interpreted as rollback.
    pub effect: MutationEffect,
}

/// Actual adapter observation, separate from policy and planned action.
///
/// The error type remains caller-owned; service failures are not flattened into
/// input errors. Every dispatched state retains the same observation envelope,
/// including an accepted handle and any partial resource evidence. No variant
/// authorizes retries or claims cancellation rollback.
#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionOutcome<E> {
    /// The adapter claims completion, requiring identity/result verification.
    Completed(ObservationEvidence),
    /// Only accepted; an operation handle is required, not a completed resource.
    Accepted(ObservationEvidence),
    /// The planner's skip was honored without executing a mutation.
    Skipped,
    /// Some resource evidence was obtained, but completion is not established.
    Partial(ObservationEvidence),
    /// Completion is unknown; previously obtained resource evidence is retained.
    Unknown(ObservationEvidence),
    /// Typed adapter/service failure, including uncertainty about side effects.
    Failed {
        /// Original structured failure.
        error: E,
        /// Retained acceptance, resource and side-effect evidence.
        evidence: ObservationEvidence,
    },
}

impl<E> ExecutionOutcome<E> {
    /// Borrows intact observation evidence; only an undispatched skip has none.
    pub fn evidence(&self) -> Option<&ObservationEvidence> {
        match self {
            Self::Completed(evidence)
            | Self::Accepted(evidence)
            | Self::Partial(evidence)
            | Self::Unknown(evidence)
            | Self::Failed { evidence, .. } => Some(evidence),
            Self::Skipped => None,
        }
    }
}

/// A completed resource whose required identity and action guards passed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedResource {
    identity: ResourceIdentity,
    name: String,
    evidence: ObservationEvidence,
}

impl VerifiedResource {
    /// Returns the verified final native resource identity.
    pub fn identity(&self) -> &ResourceIdentity {
        &self.identity
    }

    /// Returns the verified exact requested name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrows the intact observation, including its handle and resource fields.
    pub fn evidence(&self) -> &ObservationEvidence {
        &self.evidence
    }
}

/// Verified interpretation; only `Completed` proves mutation completion.
#[derive(Clone, Debug, PartialEq)]
pub enum VerifiedOutcome<E> {
    /// Resource completion and the action's required result guards passed.
    Completed(VerifiedResource),
    /// Exact existing identity was left unchanged, without asserting equality.
    Skipped(ResourceIdentity),
    /// Accepted only; reconciliation remains the caller's responsibility.
    Accepted(ObservationEvidence),
    /// Unverified partial evidence; not success.
    Partial(ObservationEvidence),
    /// Unknown completion; not success.
    Unknown(ObservationEvidence),
    /// Original typed failure; not success.
    Failed {
        /// Original structured adapter/service failure.
        error: E,
        /// Intact acceptance, resource and side-effect evidence.
        evidence: ObservationEvidence,
    },
}

impl<E> VerifiedOutcome<E> {
    /// Borrows retained evidence without turning its lifecycle state into success.
    pub fn evidence(&self) -> Option<&ObservationEvidence> {
        match self {
            Self::Completed(resource) => Some(resource.evidence()),
            Self::Accepted(evidence)
            | Self::Partial(evidence)
            | Self::Unknown(evidence)
            | Self::Failed { evidence, .. } => Some(evidence),
            Self::Skipped(_) => None,
        }
    }
}

/// Rejected success/acceptance claim, retaining the entire original observation.
///
/// Result verification failure is not proof of rollback. Retained evidence must
/// be reconciled by the caller rather than blindly executing the plan again.
#[derive(Clone, Debug, PartialEq)]
pub struct RejectedOutcome<E> {
    /// Specific violated result guard.
    pub error: CreateError,
    /// Intact original observation, including any returned identity or handle.
    ///
    /// Boxed to keep the error return bounded independently of adapter error size.
    pub outcome: Box<ExecutionOutcome<E>>,
}

/// Checks a caller-owned adapter observation against an immutable plan.
///
/// Missing final identity/name, changed replacement ID/kind/parent/name, reused
/// duplicate IDs, unverified replacement completeness, and explicit evidence
/// that no mutation occurred never become completed mutation success.
/// Accepted, partial, unknown and failed observations remain distinct evidence.
/// Their handle, available resource fields and effect are returned unchanged,
/// including when completion or acceptance verification rejects an observation.
///
/// # Errors
/// Returns [`RejectedOutcome`] with the original observation when an adapter
/// claims an incompatible action or violates the completed-resource guards.
pub fn verify_outcome<E>(
    plan: &CreatePlan,
    outcome: ExecutionOutcome<E>,
) -> Result<VerifiedOutcome<E>, RejectedOutcome<E>> {
    match outcome {
        ExecutionOutcome::Completed(evidence) => match verify_resource(plan, &evidence) {
            Ok(resource) => Ok(VerifiedOutcome::Completed(resource)),
            Err(error) => Err(RejectedOutcome {
                error,
                outcome: Box::new(ExecutionOutcome::Completed(evidence)),
            }),
        },
        ExecutionOutcome::Accepted(evidence) => {
            let error = if matches!(&plan.action, CreateAction::Skip { .. }) {
                Some(CreateError::UnexpectedOutcome)
            } else if evidence.operation_handle.is_none() {
                Some(CreateError::MissingAcceptedHandle)
            } else {
                None
            };
            if let Some(error) = error {
                return Err(RejectedOutcome {
                    error,
                    outcome: Box::new(ExecutionOutcome::Accepted(evidence)),
                });
            }
            Ok(VerifiedOutcome::Accepted(evidence))
        }
        ExecutionOutcome::Skipped => match &plan.action {
            CreateAction::Skip { existing } => Ok(VerifiedOutcome::Skipped(existing.clone())),
            _ => Err(RejectedOutcome {
                error: CreateError::UnexpectedOutcome,
                outcome: Box::new(ExecutionOutcome::Skipped),
            }),
        },
        ExecutionOutcome::Partial(evidence) => Ok(VerifiedOutcome::Partial(evidence)),
        ExecutionOutcome::Unknown(evidence) => Ok(VerifiedOutcome::Unknown(evidence)),
        ExecutionOutcome::Failed { error, evidence } => {
            Ok(VerifiedOutcome::Failed { error, evidence })
        }
    }
}

fn verify_resource(
    plan: &CreatePlan,
    evidence: &ObservationEvidence,
) -> Result<VerifiedResource, CreateError> {
    if matches!(&plan.action, CreateAction::Skip { .. }) {
        return Err(CreateError::UnexpectedOutcome);
    }
    if evidence.effect == MutationEffect::NotOccurred {
        return Err(CreateError::ContradictoryMutationEvidence);
    }
    let resource = evidence
        .resource
        .as_ref()
        .ok_or(CreateError::MissingResourceEvidence)?;
    let identity = ResourceIdentity::new(
        resource
            .native_id
            .clone()
            .ok_or(CreateError::MissingFinalIdentity)?,
        resource
            .parent_id
            .clone()
            .ok_or(CreateError::MissingFinalIdentity)?,
        resource
            .item_type
            .clone()
            .ok_or(CreateError::MissingFinalIdentity)?,
    )?;
    check_target_identity(&plan.target, &identity)?;
    let name = resource
        .name
        .as_ref()
        .ok_or(CreateError::MissingFinalName)?;
    if name != &plan.target.name {
        return Err(CreateError::NameChanged);
    }
    match &plan.action {
        CreateAction::Replace { existing, unit, .. } => {
            if identity.native_id != existing.native_id {
                return Err(CreateError::IdentityChanged);
            }
            if resource.replacement != ReplacementEvidence::Verified(unit.clone()) {
                return Err(CreateError::ReplacementUnverified);
            }
        }
        CreateAction::Duplicate { known_matches, .. } => {
            if known_matches
                .iter()
                .any(|existing| existing.native_id == identity.native_id)
            {
                return Err(CreateError::DuplicateIdentityReused);
            }
        }
        CreateAction::Create { .. } => {}
        CreateAction::Skip { .. } => return Err(CreateError::UnexpectedOutcome),
    }
    Ok(VerifiedResource {
        identity,
        name: name.clone(),
        evidence: evidence.clone(),
    })
}

/// Sanitized planning and result-guard errors, without caller input or secrets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateError {
    /// Unknown conflict-policy spelling.
    InvalidPolicy,
    /// Blank or control-bearing native identifier.
    InvalidIdentifier,
    /// Invalid bounded schema reference.
    InvalidSchema,
    /// Invalid bounded replacement-unit reference.
    InvalidReplacementUnit,
    /// Blank or control-bearing resource name/kind.
    InvalidTarget,
    /// Invalid opaque acceptance reference.
    InvalidAcceptedHandle,
    /// The target-specific schema adapter rejected the input or schema binding.
    InputRejected,
    /// Requested policy is not advertised, even for an absent target.
    UnsupportedPolicy,
    /// Missing-leaf creation is not supported by this adapter.
    UnsupportedCreation,
    /// Supplied validated input is insufficient to create.
    InputCannotCreate,
    /// Replacement lacks explicit complete input for the exact advertised unit.
    IncompleteReplacement,
    /// Selected parent does not exist.
    MissingParent,
    /// Preflight shape does not match explicit selection intent.
    SelectionMismatch,
    /// Duplicate was not given an explicit existing parent collection.
    DuplicateRequiresCollection,
    /// Exact leaf already exists under the default policy.
    AlreadyExists,
    /// Multiple leaves match the exact selector.
    AmbiguousLeaf,
    /// Observed kind differs from the selected kind.
    WrongKind,
    /// Observed parent differs from the selected native parent.
    ParentMismatch,
    /// Accepted observation lacks a provider-owned operation reference.
    MissingAcceptedHandle,
    /// Claimed completion contains no resource evidence.
    MissingResourceEvidence,
    /// A completed mutating action contradicts explicit evidence of no mutation.
    ContradictoryMutationEvidence,
    /// Claimed completion lacks a verified final resource ID/parent/kind.
    MissingFinalIdentity,
    /// Claimed completion lacks a verified final name.
    MissingFinalName,
    /// Completed replacement changed native ID.
    IdentityChanged,
    /// Completed operation changed the exact requested name.
    NameChanged,
    /// Complete-unit replacement and unrelated-metadata preservation are unverified.
    ReplacementUnverified,
    /// Claimed duplicate returned a previously observed native identity.
    DuplicateIdentityReused,
    /// Observation is incompatible with the planned mutation or skip.
    UnexpectedOutcome,
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPolicy => "Unknown create conflict policy.",
            Self::InvalidIdentifier => "Invalid native identifier.",
            Self::InvalidSchema => "Invalid target schema reference.",
            Self::InvalidReplacementUnit => "Invalid replacement unit.",
            Self::InvalidTarget => "Invalid create target.",
            Self::InvalidAcceptedHandle => "Invalid acceptance reference.",
            Self::InputRejected => "Target schema rejected create input.",
            Self::UnsupportedPolicy => "Target does not support the requested policy.",
            Self::UnsupportedCreation => "Target does not support exact-leaf creation.",
            Self::InputCannotCreate => "Validated input is insufficient to create.",
            Self::IncompleteReplacement => "Explicit complete replacement input is required.",
            Self::MissingParent => "Selected parent does not exist.",
            Self::SelectionMismatch => "Preflight does not match the selected target.",
            Self::DuplicateRequiresCollection => {
                "Duplicate requires an explicit parent collection."
            }
            Self::AlreadyExists => "Selected resource already exists.",
            Self::AmbiguousLeaf => "Selected leaf is ambiguous.",
            Self::WrongKind => "Observed resource kind does not match.",
            Self::ParentMismatch => "Observed resource parent does not match.",
            Self::MissingAcceptedHandle => "Accepted result lacks an operation reference.",
            Self::MissingResourceEvidence => "Completed result lacks resource evidence.",
            Self::ContradictoryMutationEvidence => {
                "Completed mutation contradicts evidence that no mutation occurred."
            }
            Self::MissingFinalIdentity => "Completed result lacks a final resource identity.",
            Self::MissingFinalName => "Completed result lacks a final resource name.",
            Self::IdentityChanged => "Replacement changed the native resource identity.",
            Self::NameChanged => "Completed result changed the requested resource name.",
            Self::ReplacementUnverified => "Complete replacement and preservation are unverified.",
            Self::DuplicateIdentityReused => "Duplicate reused an existing native identity.",
            Self::UnexpectedOutcome => "Execution outcome does not match the planned action.",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CreateError {}
