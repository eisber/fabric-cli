// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-backed owner bindings, without global registration or dispatch.

use std::fmt;

/// The seven selected CLI aliases, referring to common operation names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OntologyVerb {
    /// Create a new item or advertised schema child, never upsert.
    Create,
    /// Delete one item or advertised schema child.
    Delete,
    /// Retrieve one explicitly selected representation.
    Get,
    /// Bind an existing entity to a declared source.
    Link,
    /// Enumerate an advertised schema collection.
    List,
    /// Rename an advertised schema child while preserving identity.
    Move,
    /// Patch metadata or replace an explicitly writable representation.
    Update,
}

impl OntologyVerb {
    /// All selected aliases; no convenience or execution aliases are included.
    pub const ALL: [Self; 7] = [
        Self::Create,
        Self::Delete,
        Self::Get,
        Self::Link,
        Self::List,
        Self::Move,
        Self::Update,
    ];

    /// The common operation name, not an Ontology-specific operation identity.
    pub const fn common_name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Get => "get",
            Self::Link => "link",
            Self::List => "list",
            Self::Move => "move",
            Self::Update => "update",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|verb| verb.common_name() == name)
    }
}

/// Semantic selectors, not a parser for remote paths or native child identities.
///
/// The coordinator's future resolver must qualify these with canonical owner
/// IDs and kind-specific child identity. A native part name is not a writable
/// child path merely because it appears in a definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OntologyTarget {
    /// Item metadata, or the destination for a new item.
    Item,
    /// Exact native multipart definition envelope.
    Definition,
    /// Minimized public-model projection.
    PublicModelMinimized,
    /// Expanded public-model projection, with its independent feature gate.
    PublicModelExpanded,
    /// Internal stored TMSL++ definition.
    NativeModelStored,
    /// Internal expanded TMSL++ definition.
    NativeModelExpanded,
    /// Native V4 entity collection.
    Entities,
    /// One native V4 entity, addressed by lineage tag.
    Entity,
    /// One entity's source binding.
    EntitySourceBinding,
    /// RDF/OWL conversion, not native multipart definition.
    Owl,
    /// Internal TMDL rendering route returning 501 at the source pin.
    InternalTmdl,
    /// A separately authorized served documentation representation.
    Docs,
    /// Authored context, not inferred from another item's context store.
    Context,
}

/// Generation applicability of a source binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationSupport {
    /// Metadata/native envelopes for either known generation.
    Both,
    /// Generation 2 / native feature-level-2 model.
    Generation2,
}

/// Distinct backend contracts; none constructs a URL or selects credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    /// Fabric public item metadata API.
    PublicItem,
    /// Fabric public getDefinition/updateDefinition API.
    PublicDefinition,
    /// Gated public whole-model API, not an internal DTO.
    PublicModel,
    /// Caller-injected private workload transport and native V4 DTOs.
    NativeWorkload,
}

/// Body contract whose exact typed schema belongs to the released SDK producer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyContract {
    /// No generic input body.
    None,
    /// New item metadata, optionally with an advertised native definition.
    ItemCreate,
    /// Declared metadata fields only; absent and null remain distinct.
    MetadataPatch,
    /// Complete native multipart envelope, not arbitrary JSON or source patch.
    DefinitionReplace,
    /// Public model authoring DTO, excluding read-only expanded projections.
    PublicModelReplace,
    /// Internal TMSL++ authoring DTO, not public-model JSON.
    NativeModelReplace,
    /// Strict entity creation; no createOrReplace fallback.
    EntityCreate,
    /// Typed merge patch of one generation-2 native entity.
    EntityPatch,
    /// Kind-qualified source and mapping, requiring an accepted native contract.
    SourceBinding,
    /// Exact new name/namespace with stable identity/reference semantics.
    EntityRename,
}

/// Minimum source-observed effect; a host still needs exact scoped effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Metadata retrieval without source augmentation.
    MetadataRead,
    /// Definition retrieval can create a service operation and require write rights.
    DefinitionRead,
    /// Native discovery is billed and can resolve/cache source metadata.
    NativeDiscovery,
    /// Remote creation, subject to enforceable create-only policy and approval.
    Create,
    /// Remote update, without an inferred client concurrency condition.
    Update,
    /// Permanent or policy-qualified item deletion.
    Delete,
    /// Native child deletion needs bounded reference/cascade evidence.
    DeleteWithReferences,
    /// Binding affects source dependencies, not ACL or source data.
    BindSource,
    /// Rename must retain identity and account for references.
    RenameWithReferences,
}

/// A CLI catalog entry supported by pinned source evidence.
///
/// This is deliberately not an executable capability registration. Shared
/// schema IDs, path contracts, SDK request types, approval and typed receipts
/// must be supplied by accepted producers before a host enables a binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OntologyBinding {
    /// Existing common operation referenced by the facade alias.
    pub verb: OntologyVerb,
    /// Semantic target/representation, not an invented concrete path grammar.
    pub target: OntologyTarget,
    /// Known generations admitted by this source contract.
    pub generations: GenerationSupport,
    /// Backend whose DTO, audience and gates must remain distinct.
    pub backend: Backend,
    /// Input semantics; not a claim that a complete JSON schema is installed.
    pub body: BodyContract,
    /// Minimum declared effect, to be refined to exact resolved scope by the host.
    pub effect: Effect,
}

macro_rules! binding {
    ($verb:ident, $target:ident, $generation:ident, $backend:ident, $body:ident, $effect:ident) => {
        OntologyBinding {
            verb: OntologyVerb::$verb,
            target: OntologyTarget::$target,
            generations: GenerationSupport::$generation,
            backend: Backend::$backend,
            body: BodyContract::$body,
            effect: Effect::$effect,
        }
    };
}

static BINDINGS: &[OntologyBinding] = &[
    binding!(Get, Item, Both, PublicItem, None, MetadataRead),
    binding!(
        Get,
        Definition,
        Both,
        PublicDefinition,
        None,
        DefinitionRead
    ),
    binding!(
        Get,
        PublicModelMinimized,
        Generation2,
        PublicModel,
        None,
        NativeDiscovery
    ),
    binding!(
        Get,
        PublicModelExpanded,
        Generation2,
        PublicModel,
        None,
        NativeDiscovery
    ),
    binding!(
        Get,
        NativeModelStored,
        Generation2,
        NativeWorkload,
        None,
        NativeDiscovery
    ),
    binding!(
        Get,
        NativeModelExpanded,
        Generation2,
        NativeWorkload,
        None,
        NativeDiscovery
    ),
    binding!(
        Get,
        Entity,
        Generation2,
        NativeWorkload,
        None,
        NativeDiscovery
    ),
    binding!(
        List,
        Entities,
        Generation2,
        NativeWorkload,
        None,
        NativeDiscovery
    ),
    binding!(Create, Item, Both, PublicItem, ItemCreate, Create),
    binding!(
        Create,
        Entities,
        Generation2,
        NativeWorkload,
        EntityCreate,
        Create
    ),
    binding!(Delete, Item, Both, PublicItem, None, Delete),
    binding!(
        Delete,
        Entity,
        Generation2,
        NativeWorkload,
        None,
        DeleteWithReferences
    ),
    binding!(Update, Item, Both, PublicItem, MetadataPatch, Update),
    binding!(
        Update,
        Entity,
        Generation2,
        NativeWorkload,
        EntityPatch,
        Update
    ),
    binding!(
        Update,
        Definition,
        Both,
        PublicDefinition,
        DefinitionReplace,
        Update
    ),
    binding!(
        Update,
        PublicModelMinimized,
        Generation2,
        PublicModel,
        PublicModelReplace,
        Update
    ),
    binding!(
        Update,
        NativeModelStored,
        Generation2,
        NativeWorkload,
        NativeModelReplace,
        Update
    ),
    binding!(
        Link,
        EntitySourceBinding,
        Generation2,
        NativeWorkload,
        SourceBinding,
        BindSource
    ),
    binding!(
        Move,
        Entity,
        Generation2,
        NativeWorkload,
        EntityRename,
        RenameWithReferences
    ),
];

/// Returns source-backed proposals, never a list of enabled remote operations.
pub fn bindings() -> &'static [OntologyBinding] {
    BINDINGS
}

/// Local target-selection failure, before SDK or credential dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionError {
    /// The facade must not expand to a different owner type.
    WrongOwnerType,
    /// Generation was absent, malformed upstream or not one of the known values.
    UnknownGeneration,
    /// The selected operation/representation has no admitted source binding.
    UnsupportedTarget,
    /// A known representation is unavailable for this generation.
    UnsupportedGeneration,
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WrongOwnerType => "The Ontology facade requires an Ontology owner.",
            Self::UnknownGeneration => "An explicit supported Ontology generation is required.",
            Self::UnsupportedTarget => {
                "This Ontology operation and representation are unsupported."
            }
            Self::UnsupportedGeneration => "This representation requires Ontology generation 2.",
        })
    }
}

impl std::error::Error for SelectionError {}

/// Selects a source proposal after authoritative type/generation resolution.
///
/// This pure check neither resolves names nor proves permission, feature gates,
/// backend availability or producer readiness. Metadata discovery can obtain
/// generation before this representation-dispatch check is called. Native
/// source authoring must obtain generation from its validated typed contract.
/// Metadata-only item creation deliberately leaves the new generation unknown.
///
/// # Errors
/// Rejects wrong owners, unknown generations and unsupported combinations.
pub fn select_binding(
    verb: OntologyVerb,
    target: OntologyTarget,
    owner_type: &str,
    generation: Option<u64>,
) -> Result<&'static OntologyBinding, SelectionError> {
    if owner_type != "Ontology" {
        return Err(SelectionError::WrongOwnerType);
    }
    if verb == OntologyVerb::Create && target == OntologyTarget::Item && generation.is_none() {
        return BINDINGS
            .iter()
            .find(|binding| binding.verb == verb && binding.target == target)
            .ok_or(SelectionError::UnsupportedTarget);
    }
    let generation = match generation {
        Some(value @ (1 | 2)) => value,
        _ => return Err(SelectionError::UnknownGeneration),
    };
    let binding = BINDINGS
        .iter()
        .find(|binding| binding.verb == verb && binding.target == target)
        .ok_or(SelectionError::UnsupportedTarget)?;
    if binding.generations == GenerationSupport::Generation2 && generation != 2 {
        return Err(SelectionError::UnsupportedGeneration);
    }
    Ok(binding)
}
