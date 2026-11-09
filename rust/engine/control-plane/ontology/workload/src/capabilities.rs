// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-backed capability advertisement, not rollout or caller-permission proof.

/// Direct resource endpoint access, distinct from the separate native script language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAccess {
    /// Only list and get endpoints exist.
    ReadOnly,
    /// Create, read, merge patch, delete and list endpoints exist.
    NativeCrud,
}

/// One advertised private V4 resource family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceCapability {
    /// Exact collection route segment.
    pub collection: &'static str,
    /// Direct endpoint access; read-only collections are never generic writable graphs.
    pub access: ResourceAccess,
    /// True if REST members are addressed by name, not lineage tag.
    pub by_name: bool,
    /// Additional reusable-properties flight required, beyond FL2 and TMSL.
    pub requires_reusable_properties: bool,
}

/// Direct source-supported families. Every entry requires Ontology FL2 and TMSL.
pub const RESOURCES: &[ResourceCapability] = &[
    ResourceCapability {
        collection: "entityTypes",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "entityRelationships",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "dataSources",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "expressions",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "namespaces",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "relationships",
        access: ResourceAccess::NativeCrud,
        by_name: true,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "rules",
        access: ResourceAccess::NativeCrud,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "metrics",
        access: ResourceAccess::ReadOnly,
        by_name: false,
        requires_reusable_properties: false,
    },
    ResourceCapability {
        collection: "reusableProperties",
        access: ResourceAccess::ReadOnly,
        by_name: true,
        requires_reusable_properties: true,
    },
];

/// Explicit unsupported or separately owned operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityGap {
    /// Operation identifier.
    pub operation: &'static str,
    /// Concrete prerequisite/source gap, never a successful no-op.
    pub reason: &'static str,
}

/// Gaps this deterministic SDK does not hide behind raw JSON or a transport fallback.
pub const GAPS: &[CapabilityGap] = &[
    CapabilityGap {
        operation: "rdf.import",
        reason: crate::rdf::RDF_IMPORT_GAP,
    },
    CapabilityGap {
        operation: "definition.tmdl",
        reason: "native V4 controller returns 501; public multipart transfer is a different contract",
    },
    CapabilityGap {
        operation: "item.delete",
        reason: "owned by Fabric item SDK; native definition DELETE only resets collections",
    },
    CapabilityGap {
        operation: "model.public",
        reason: "public projection and gates differ; owned by public SDK adapter",
    },
    CapabilityGap {
        operation: "query.agent",
        reason: "model/query/agent execution is outside deterministic authoring",
    },
];
