// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{error::Error, fmt, sync::Arc};

/// An intentionally unported Python resolution branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedBranch {
    /// Tenant-level virtual administrator collections.
    VirtualWorkspace,
    /// Workspace virtual containers, including external data shares.
    VirtualItemContainer,
    /// Traversal below a Fabric item into OneLake.
    OneLake,
    /// Local filesystem resolution or fallback.
    LocalFilesystem,
    /// Ambient personal-workspace identity/discovery without explicit home.
    HomeDiscovery,
}

/// Classification separate from the Python-compatible status string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// A translated Python validation error.
    Python,
    /// A caller-supplied lookup failure.
    Lookup,
    /// A deliberately unported branch.
    Unsupported(UnsupportedBranch),
    /// Invalid metadata parentage supplied by a consumer.
    InvalidContext,
}

/// A precise status/message pair; lookup failures are not collapsed to not-found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextError {
    /// Error origin or unsupported branch.
    pub kind: ErrorKind,
    /// Python `FabricCLIError.status_code`, where behavior is translated.
    pub code: String,
    /// Python message, or an explicitly bounded-library diagnostic.
    pub message: String,
}

impl ContextError {
    /// Preserve a caller-resolved service/catalog failure without interpreting it.
    ///
    /// For a Python error, supply its normalized `status_code` and `.message`,
    /// not its raw constructor arguments (which Python defaults/period-strips).
    pub fn lookup(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Lookup,
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn python(code: &str, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Python,
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn unsupported(branch: UnsupportedBranch, path: &str) -> Self {
        Self {
            kind: ErrorKind::Unsupported(branch),
            code: "NotSupported".into(),
            message: format!("The {branch:?} branch is not ported: '{path}'"),
        }
    }
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for ContextError {}

/// A source-supported workspace suffix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceType {
    /// A regular workspace.
    Workspace,
    /// A personal workspace; discovery is the caller's responsibility.
    Personal,
}

impl WorkspaceType {
    /// Canonical Python suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Personal => "Personal",
        }
    }
}

/// A canonical item suffix from the pinned Python `ItemType` enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemType(&'static str);

impl ItemType {
    /// Parse case-insensitively, rejecting unknown types instead of guessing.
    pub fn parse(value: &str) -> Result<Self, ContextError> {
        const TYPES: &[&str] = &[
            "AISkill",
            "ApacheAirflowJob",
            "Exploration",
            "RetailDataManager",
            "Healthcaredatasolution",
            "MetricSet",
            "OrgApp",
            "SustainabilityDataSolution",
            "CosmosDBDatabase",
            "Dashboard",
            "Datamart",
            "DataPipeline",
            "DigitalTwinBuilder",
            "DigitalTwinBuilderFlow",
            "Environment",
            "Eventhouse",
            "Eventstream",
            "GraphModel",
            "KQLDashboard",
            "KQLDatabase",
            "KQLQueryset",
            "Lakehouse",
            "Map",
            "MirroredWarehouse",
            "MirroredDatabase",
            "MLExperiment",
            "MLModel",
            "Notebook",
            "Ontology",
            "PaginatedReport",
            "Reflex",
            "Report",
            "SemanticModel",
            "SparkJobDefinition",
            "SQLEndpoint",
            "Warehouse",
            "CopyJob",
            "GraphQLApi",
            "GraphQuerySet",
            "UserDataFunction",
            "MountedDataFactory",
            "SQLDatabase",
            "Dataflow",
            "VariableLibrary",
        ];
        let lowercase = value.to_lowercase();
        TYPES
            .iter()
            .find(|candidate| candidate.to_lowercase() == lowercase)
            .map(|value| Self(value))
            .ok_or_else(|| {
                ContextError::python(
                    "InvalidItemType",
                    format!("'{value}' is not a valid Fabric item type"),
                )
            })
    }

    /// Canonical case-sensitive suffix used for name lookup.
    pub fn as_str(self) -> &'static str {
        self.0
    }

    pub(crate) fn strict_name(self) -> bool {
        matches!(
            self.0,
            "Lakehouse" | "MLExperiment" | "MLModel" | "Eventstream"
        )
    }
}

/// Supported hierarchy nodes, not OS filesystem objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// The Fabric tenant root.
    Tenant,
    /// A regular or personal workspace.
    Workspace(WorkspaceType),
    /// A workspace folder.
    Folder,
    /// A Fabric item, not its OneLake children.
    Item(ItemType),
}

impl NodeKind {
    /// Python hierarchy class name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "Tenant",
            Self::Workspace(_) => "Workspace",
            Self::Folder => "Folder",
            Self::Item(_) => "Item",
        }
    }
}

/// Immutable metadata and parent links. IDs are opaque, never parsed as GUIDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Element {
    name: String,
    id: Option<String>,
    kind: NodeKind,
    parent: Option<Arc<Element>>,
}

impl Element {
    /// Construct an explicitly selected tenant without identity discovery.
    pub fn tenant(name: impl Into<String>, id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            id: Some(id.into()),
            kind: NodeKind::Tenant,
            parent: None,
        })
    }

    /// Construct caller-owned metadata with structurally valid, immutable parents.
    ///
    /// Existing service display names are not revalidated here: Python permits
    /// legacy names when an ID exists. Resolution validates each supplied token.
    pub fn child(
        name: impl Into<String>,
        id: Option<String>,
        kind: NodeKind,
        parent: Arc<Self>,
    ) -> Result<Arc<Self>, ContextError> {
        let valid = match kind {
            NodeKind::Tenant => false,
            NodeKind::Workspace(_) => parent.kind == NodeKind::Tenant,
            NodeKind::Folder | NodeKind::Item(_) => {
                matches!(parent.kind, NodeKind::Workspace(_) | NodeKind::Folder)
            }
        };
        if !valid {
            return Err(ContextError {
                kind: ErrorKind::InvalidContext,
                code: "InvalidContext".into(),
                message: "Invalid Fabric metadata parent relationship".into(),
            });
        }
        Ok(Arc::new(Self {
            name: name.into(),
            id,
            kind,
            parent: Some(parent),
        }))
    }

    /// Display name without the type suffix.
    pub fn short_name(&self) -> &str {
        &self.name
    }
    /// Opaque ID; absent IDs represent Python's unresolved creation targets.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Typed hierarchy classification.
    pub fn kind(&self) -> NodeKind {
        self.kind
    }
    /// Immutable parent metadata.
    pub fn parent(&self) -> Option<&Arc<Self>> {
        self.parent.as_ref()
    }
    /// Python's full name, including the canonical suffix.
    pub fn name(&self) -> String {
        let suffix = match self.kind {
            NodeKind::Tenant => "Tenant",
            NodeKind::Workspace(value) => value.as_str(),
            NodeKind::Folder => "Folder",
            NodeKind::Item(value) => value.as_str(),
        };
        format!("{}.{suffix}", self.name)
    }

    /// Fabric display path. Only workspace names escape embedded slashes in Python.
    pub fn path(&self) -> String {
        match &self.parent {
            None => "/".into(),
            Some(parent) => {
                let name = if matches!(self.kind, NodeKind::Workspace(_)) {
                    self.name().replace('/', "\\/")
                } else {
                    self.name()
                };
                format!("{}/{name}", parent.path().trim_end_matches('/'))
            }
        }
    }

    /// Python ID path: folders contribute no segment and missing IDs spell `None`.
    pub fn path_id(&self) -> String {
        match &self.parent {
            None => "/".into(),
            Some(parent) if self.kind == NodeKind::Folder => parent.path_id(),
            Some(parent) => format!(
                "{}/{}",
                parent.path_id().trim_end_matches('/'),
                self.id().unwrap_or("None")
            ),
        }
    }

    /// Owning tenant, derived only from the supplied immutable graph.
    pub fn root(self: &Arc<Self>) -> Arc<Self> {
        let mut current = Arc::clone(self);
        while let Some(parent) = current.parent() {
            current = Arc::clone(parent);
        }
        current
    }

    /// Owning workspace, or `None` at the tenant root.
    pub fn workspace(self: &Arc<Self>) -> Option<Arc<Self>> {
        let mut current = Arc::clone(self);
        loop {
            if matches!(current.kind, NodeKind::Workspace(_)) {
                return Some(current);
            }
            current = Arc::clone(current.parent()?);
        }
    }
}
