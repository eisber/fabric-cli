// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host-neutral operation discovery over already-resolved resource identities.
//!
//! Registration and resolution perform no I/O, authorization, credential discovery,
//! or execution. CLI aliases constrain the same registered operation used by SDK
//! and MCP consumers; they do not create another operation or tool. Filesystem-like
//! path parsing and name resolution remain caller-owned producers of [`Target`].

use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

mod receipt;
pub use receipt::{
    ApprovalPolicy, ApprovedOperation, ChildIdentity, Coverage, ExecutionEvidence,
    ExecutionReceipt, FailureInfo, ItemIdentity, OperationHandle, Outcome, Precondition,
    PreparedOperation, ReceiptBuildError, ReceiptError, ReceiptMetadata, ResolvedTarget,
    SourceEvidence, SourceEvidenceError, TargetIdentity, TenantIdentity, WorkspaceIdentity,
};

/// Identity cardinality required by an explicitly registered target binding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetKind {
    /// A caller-selected tenant collection, without a synthetic workspace.
    TenantCollection,
    /// An existing owner item, its subresource or representation.
    Item,
    /// A workspace-owned item collection or not-yet-created destination.
    WorkspaceCollection,
}

/// Selected common actions in the initial Ontology authoring slice.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    /// Create a new resource, never an implicit replacement.
    Create,
    /// Observe scoped existence without establishing mutation preconditions.
    Exists,
    /// Remove one explicitly addressed supported resource.
    Delete,
    /// Retrieve the selected metadata or representation.
    Get,
    /// Bind an existing resource using its native relationship contract.
    Link,
    /// Enumerate one advertised collection.
    List,
    /// Rename or move only where a native capability exists.
    Move,
    /// Discover bounded candidates within an explicitly selected scope.
    Search,
    /// Patch or replace an explicitly existing resource.
    Update,
}

/// Declared effects, not permission to perform them.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Effect {
    /// Read explicitly supplied local input, distinct from remote source access.
    LocalRead,
    /// Read remote metadata or source.
    RemoteRead,
    /// Write or replace remote state.
    RemoteWrite,
    /// Remove remote state.
    RemoteDelete,
    /// Write explicitly requested local output.
    LocalWrite,
    /// Query source data.
    DataQuery,
    /// Invoke a model-backed service.
    ModelExecution,
    /// Publish service state.
    Publication,
    /// Refresh a binding or metadata cache.
    BindingRefresh,
    /// Incur service-metered consumption.
    MeteredConsumption,
}

/// Whether one native operation applies to every or one explicit generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Generation {
    /// Explicit generation-independent support, for example item metadata.
    Any,
    /// A specific provider generation; unknown values are never coerced.
    Exact(u32),
}

impl Generation {
    fn overlaps(self, other: Self) -> bool {
        self == Self::Any || other == Self::Any || self == other
    }

    fn accepts(self, observed: Option<u32>) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(value) => observed == Some(value),
        }
    }
}

/// An explicit prerequisite not established by registration or source inspection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prerequisite {
    /// Stable, machine-readable prerequisite identifier.
    pub id: String,
    /// Actionable non-secret explanation.
    pub description: String,
}

/// Discovery distinguishes implemented wiring from unmet provider prerequisites.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "details", rename_all = "camelCase")]
pub enum Availability {
    /// The binding needs no additional provider-specific prerequisite declaration.
    ///
    /// Ordinary authorization and host policy still apply.
    Available,
    /// The host must explicitly verify these prerequisites before execution.
    Conditional(Vec<Prerequisite>),
    /// The operation cannot execute through this binding.
    Unsupported(String),
}

/// One operation descriptor shared by all heads.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationDescriptor {
    id: String,
    verb: Verb,
    input_schema: Value,
    output_schema: Value,
    effects: Vec<Effect>,
    optional_effects: Vec<Effect>,
}

impl OperationDescriptor {
    /// Validates a stable ID and object-shaped schemas without evaluating them.
    ///
    /// # Errors
    /// Rejects invalid IDs, nonobject schemas or duplicate effects.
    pub fn new(
        id: &str,
        verb: Verb,
        input_schema: Value,
        output_schema: Value,
        effects: Vec<Effect>,
    ) -> Result<Self, ContractError> {
        validate_token(id)?;
        if !input_schema.is_object() || !output_schema.is_object() {
            return Err(ContractError::InvalidSchema);
        }
        let mut normalized = effects.clone();
        normalized.sort();
        normalized.dedup();
        if normalized.len() != effects.len() {
            return Err(ContractError::DuplicateEffect);
        }
        Ok(Self {
            id: id.into(),
            verb,
            input_schema,
            output_schema,
            effects,
            optional_effects: Vec::new(),
        })
    }

    /// Declares effects an adapter may select only for explicit request options.
    ///
    /// Required effects cannot be removed by invocation preparation.
    ///
    /// # Errors
    /// Rejects repeats or overlap with required effects.
    pub fn with_optional_effects(mut self, effects: Vec<Effect>) -> Result<Self, ContractError> {
        let mut unique = Vec::new();
        for effect in &effects {
            if self.effects.contains(effect) || unique.contains(effect) {
                return Err(ContractError::DuplicateEffect);
            }
            unique.push(*effect);
        }
        self.optional_effects = effects;
        Ok(self)
    }

    /// Returns the single operation identity, independent of the selected head.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the common action.
    pub const fn verb(&self) -> Verb {
        self.verb
    }

    /// Returns the registered input schema.
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Returns the registered output schema.
    pub fn output_schema(&self) -> &Value {
        &self.output_schema
    }

    /// Returns the declared effect set in author-supplied order.
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Returns explicitly selectable effects, not automatically enabled effects.
    pub fn optional_effects(&self) -> &[Effect] {
        &self.optional_effects
    }
}

/// A provider-qualified target selection after path and identity resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    /// Whether an existing item or its real parent collection is addressed.
    pub kind: TargetKind,
    /// Exact provider item type, not a display name or permission grant.
    pub owner_type: String,
    /// Stable registered facet, not an arbitrary provider URL.
    pub facet: String,
    /// Native representation, distinct from the host's receipt serialization.
    pub representation: String,
    /// Observed generation, or unknown when the provider did not establish it.
    pub generation: Option<u32>,
}

/// One positively enumerated target-to-operation binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    target_kind: TargetKind,
    owner_type: String,
    facet: String,
    representation: String,
    generation: Generation,
    operation_id: String,
    availability: Availability,
}

impl Binding {
    /// Creates an explicit target binding; no pluralization or fallback occurs.
    ///
    /// # Errors
    /// Rejects invalid selector tokens and empty prerequisite declarations.
    pub fn new(
        owner_type: &str,
        facet: &str,
        representation: &str,
        generation: Generation,
        operation_id: &str,
        availability: Availability,
    ) -> Result<Self, ContractError> {
        for token in [owner_type, facet, representation, operation_id] {
            validate_token(token)?;
        }
        match &availability {
            Availability::Conditional(requirements) => {
                if requirements.is_empty() {
                    return Err(ContractError::InvalidAvailability);
                }
                let mut ids: Vec<&str> = Vec::new();
                for requirement in requirements {
                    validate_token(&requirement.id)?;
                    if requirement.description.trim().is_empty()
                        || ids.contains(&requirement.id.as_str())
                    {
                        return Err(ContractError::InvalidAvailability);
                    }
                    ids.push(requirement.id.as_str());
                }
            }
            Availability::Unsupported(reason) if reason.trim().is_empty() => {
                return Err(ContractError::InvalidAvailability);
            }
            Availability::Available | Availability::Unsupported(_) => {}
        }
        Ok(Self {
            target_kind: TargetKind::Item,
            owner_type: owner_type.into(),
            facet: facet.into(),
            representation: representation.into(),
            generation,
            operation_id: operation_id.into(),
            availability,
        })
    }

    /// Returns the exact owner type constraint.
    pub fn owner_type(&self) -> &str {
        &self.owner_type
    }

    /// Returns the operation identity this binding reuses.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Returns declared availability without implying provider access.
    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    /// Explicitly binds a workspace collection instead of the default item scope.
    pub fn with_target_kind(mut self, kind: TargetKind) -> Self {
        self.target_kind = kind;
        self
    }

    fn same_scope(&self, other: &Self) -> bool {
        self.target_kind == other.target_kind
            && self.owner_type == other.owner_type
            && self.facet == other.facet
            && self.representation == other.representation
            && self.generation.overlaps(other.generation)
    }

    fn matches(&self, target: &Target) -> bool {
        self.target_kind == target.kind
            && self.owner_type == target.owner_type
            && self.facet == target.facet
            && self.representation == target.representation
            && self.generation.accepts(target.generation)
    }
}

/// A resolved operation and its exact capability binding, not execution approval.
#[derive(Clone, Copy, Debug)]
pub struct Resolution<'a> {
    /// The shared operation definition used by SDK, CLI and MCP consumers.
    pub operation: &'a OperationDescriptor,
    /// The selected provider-specific capability and prerequisites.
    pub binding: &'a Binding,
}

/// Validated, deterministic operation registration with type-constrained aliases.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    operations: BTreeMap<String, OperationDescriptor>,
    aliases: BTreeMap<String, String>,
    bindings: Vec<Binding>,
}

impl Registry {
    /// Adds one shared operation without replacing an existing registration.
    ///
    /// # Errors
    /// Rejects duplicate operation identities.
    pub fn register(&mut self, operation: OperationDescriptor) -> Result<(), ContractError> {
        if self.operations.contains_key(operation.id()) {
            return Err(ContractError::DuplicateOperation);
        }
        self.operations.insert(operation.id.clone(), operation);
        Ok(())
    }

    /// Registers a facade as an owner constraint, never another operation.
    ///
    /// # Errors
    /// Rejects malformed or duplicate alias names.
    pub fn alias(&mut self, alias: &str, owner_type: &str) -> Result<(), ContractError> {
        validate_token(alias)?;
        validate_token(owner_type)?;
        if self.aliases.contains_key(alias) {
            return Err(ContractError::DuplicateAlias);
        }
        self.aliases.insert(alias.into(), owner_type.into());
        Ok(())
    }

    /// Binds an enumerated target to a previously registered operation.
    ///
    /// # Errors
    /// Rejects missing operation IDs and generation-overlapping bindings for
    /// the same owner/facet/representation/action instead of selecting first.
    pub fn bind(&mut self, binding: Binding) -> Result<(), ContractError> {
        let operation = self
            .operations
            .get(binding.operation_id())
            .ok_or(ContractError::UnknownOperation)?;
        if self.bindings.iter().any(|existing| {
            existing.same_scope(&binding)
                && self.operations[existing.operation_id()].verb() == operation.verb()
        }) {
            return Err(ContractError::AmbiguousBinding);
        }
        self.bindings.push(binding);
        self.bindings.sort_by(|left, right| {
            (
                left.target_kind,
                &left.owner_type,
                &left.facet,
                &left.representation,
                &left.operation_id,
                generation_order(left.generation),
            )
                .cmp(&(
                    right.target_kind,
                    &right.owner_type,
                    &right.facet,
                    &right.representation,
                    &right.operation_id,
                    generation_order(right.generation),
                ))
        });
        Ok(())
    }

    /// Discovers the same operation through a generic action or facade alias.
    ///
    /// This does not evaluate conditions, acquire credentials or call an adapter.
    ///
    /// # Errors
    /// Rejects malformed targets, unknown/wrong-owner aliases and unadvertised
    /// generation/facet/representation combinations.
    pub fn resolve(
        &self,
        verb: Verb,
        target: &Target,
        alias: Option<&str>,
    ) -> Result<Resolution<'_>, ContractError> {
        for token in [&target.owner_type, &target.facet, &target.representation] {
            validate_token(token)?;
        }
        if let Some(alias) = alias {
            let owner = self.aliases.get(alias).ok_or(ContractError::UnknownAlias)?;
            if owner != &target.owner_type {
                return Err(ContractError::WrongOwnerType);
            }
        }
        let binding = self
            .bindings
            .iter()
            .find(|binding| {
                binding.matches(target) && self.operations[binding.operation_id()].verb() == verb
            })
            .ok_or(ContractError::UnsupportedTarget)?;
        let operation = &self.operations[binding.operation_id()];
        Ok(Resolution { operation, binding })
    }

    /// Lists operation descriptors in locale-independent operation-ID order.
    pub fn operations(&self) -> impl Iterator<Item = &OperationDescriptor> {
        self.operations.values()
    }

    /// Lists positively enumerated bindings in deterministic selector order.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

fn generation_order(generation: Generation) -> (bool, u32) {
    match generation {
        Generation::Any => (false, 0),
        Generation::Exact(value) => (true, value),
    }
}

fn validate_token(token: &str) -> Result<(), ContractError> {
    if token.is_empty()
        || token.len() > 128
        || !token.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(ContractError::InvalidSelector);
    }
    Ok(())
}

/// Registration/resolution failure; diagnostics never echo untrusted selectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractError {
    /// A selector is not a bounded stable registration token.
    InvalidSelector,
    /// Input/output schemas must be JSON objects.
    InvalidSchema,
    /// Effects must not be repeated.
    DuplicateEffect,
    /// An availability declaration is empty or contradictory.
    InvalidAvailability,
    /// An operation identity already exists.
    DuplicateOperation,
    /// A facade alias already exists.
    DuplicateAlias,
    /// A binding references an unregistered operation.
    UnknownOperation,
    /// A facade alias is not registered.
    UnknownAlias,
    /// A facade's type constraint does not match the resolved owner.
    WrongOwnerType,
    /// Two bindings could select different implementations for the same action.
    AmbiguousBinding,
    /// No exact advertised target combination supports the action.
    UnsupportedTarget,
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSelector => "invalid operation registration selector",
            Self::InvalidSchema => "operation schemas must be JSON objects",
            Self::DuplicateEffect => "operation effect declared more than once",
            Self::InvalidAvailability => "invalid capability availability declaration",
            Self::DuplicateOperation => "operation identity already registered",
            Self::DuplicateAlias => "facade alias already registered",
            Self::UnknownOperation => "capability references an unregistered operation",
            Self::UnknownAlias => "facade alias is not registered",
            Self::WrongOwnerType => "facade does not support the resolved owner type",
            Self::AmbiguousBinding => "capability bindings overlap for the same action",
            Self::UnsupportedTarget => "operation is not supported for the resolved target",
        })
    }
}

impl std::error::Error for ContractError {}
