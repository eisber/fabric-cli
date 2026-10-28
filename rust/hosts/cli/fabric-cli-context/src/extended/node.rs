// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::sync::Arc;

use crate::{ContextError, Element, ErrorKind, NodeKind};

/// Tenant virtual collection and its corresponding leaf type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualWorkspaceKind {
    /// `.capacities` / `Capacity`.
    Capacity,
    /// `.connections` / `Connection`.
    Connection,
    /// `.gateways` / `Gateway`.
    Gateway,
    /// `.domains` / `Domain`.
    Domain,
}

impl VirtualWorkspaceKind {
    /// Canonical Python collection spelling.
    pub fn collection(self) -> &'static str {
        match self {
            Self::Capacity => ".capacities",
            Self::Connection => ".connections",
            Self::Gateway => ".gateways",
            Self::Domain => ".domains",
        }
    }
    /// Canonical leaf suffix.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Capacity => "Capacity",
            Self::Connection => "Connection",
            Self::Gateway => "Gateway",
            Self::Domain => "Domain",
        }
    }
    /// All pinned source variants, in source order.
    pub const ALL: [Self; 4] = [
        Self::Capacity,
        Self::Connection,
        Self::Gateway,
        Self::Domain,
    ];
}

/// Workspace virtual collection and its corresponding leaf type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualItemKind {
    /// `.sparkpools` / `SparkPool`.
    SparkPool,
    /// `.managedidentities` / `ManagedIdentity`.
    ManagedIdentity,
    /// `.managedprivateendpoints` / `ManagedPrivateEndpoint`.
    ManagedPrivateEndpoint,
    /// `.externaldatashares` / `ExternalDataShare`.
    ExternalDataShare,
}

impl VirtualItemKind {
    /// Canonical Python collection spelling.
    pub fn collection(self) -> &'static str {
        match self {
            Self::SparkPool => ".sparkpools",
            Self::ManagedIdentity => ".managedidentities",
            Self::ManagedPrivateEndpoint => ".managedprivateendpoints",
            Self::ExternalDataShare => ".externaldatashares",
        }
    }
    /// Canonical leaf suffix.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::SparkPool => "SparkPool",
            Self::ManagedIdentity => "ManagedIdentity",
            Self::ManagedPrivateEndpoint => "ManagedPrivateEndpoint",
            Self::ExternalDataShare => "ExternalDataShare",
        }
    }
    pub(crate) fn resource(self) -> &'static str {
        match self {
            Self::SparkPool => "Spark Pool",
            Self::ManagedIdentity => "Managed Identity",
            Self::ManagedPrivateEndpoint => "Managed Private Endpoint",
            Self::ExternalDataShare => "External Data Share",
        }
    }
    /// All pinned source variants, in source order.
    pub const ALL: [Self; 4] = [
        Self::SparkPool,
        Self::ManagedIdentity,
        Self::ManagedPrivateEndpoint,
        Self::ExternalDataShare,
    ];
}

/// Python OneLake nested type, distinct from Fabric item types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeKind {
    /// Directory.
    Folder,
    /// File.
    File,
    /// A directory directly in the `Tables` local path.
    Table,
    /// Shortcut directory, displayed with a `.Shortcut` suffix.
    Shortcut,
    /// Missing creation target or unrecognized properties type.
    Undefined,
}

impl OneLakeKind {
    /// Source enum member name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "FOLDER",
            Self::File => "FILE",
            Self::Table => "TABLE",
            Self::Shortcut => "SHORTCUT",
            Self::Undefined => "UNDEFINED",
        }
    }
}

/// Caller-classified local object, without ambient filesystem probes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalKind {
    /// File at classification time.
    File,
    /// Directory at classification time.
    Directory,
    /// New/missing path or a different filesystem object.
    Other,
}

/// Local metadata supplied by the filesystem-owning consumer.
///
/// Python's process-random hash ID and OS basename are not fabricated by this
/// virtual-path library. The caller supplies their equivalent explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalMetadata {
    /// Original input path, without Fabric normalization.
    pub path: String,
    /// OS-specific basename.
    pub name: String,
    /// Caller-owned opaque identity, not a stable Python hash promise.
    pub id: String,
    /// Classification from the caller's filesystem snapshot.
    pub kind: LocalKind,
}

/// Additive hierarchy kind. The original `NodeKind` remains unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathKind {
    /// A previously ported Fabric node.
    Fabric(NodeKind),
    /// Tenant-level virtual administrator collection.
    VirtualWorkspace(VirtualWorkspaceKind),
    /// Resource within a virtual workspace.
    VirtualWorkspaceItem(VirtualWorkspaceKind),
    /// Workspace-level virtual collection.
    VirtualItemContainer(VirtualItemKind),
    /// Resource within a workspace virtual container.
    VirtualItem(VirtualItemKind),
    /// OneLake node beneath a Fabric item.
    OneLake(OneLakeKind),
    /// Explicit caller-classified local path.
    Local(LocalKind),
}

impl PathKind {
    /// Exact Python `FabricElementType` display string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fabric(kind) => kind.as_str(),
            Self::VirtualWorkspace(_) => "VirtualWorkspace",
            Self::VirtualWorkspaceItem(_) => "VirtualWorkspaceItem",
            Self::VirtualItemContainer(_) => "VirtualItemContainer",
            Self::VirtualItem(_) => "VirtualItem",
            Self::OneLake(_) => "Onelake",
            Self::Local(_) => "LocalPath",
        }
    }
}

/// Immutable cross-domain path metadata with explicit parent links.
///
/// Wraps rather than modifies the existing Fabric metadata API, preserving
/// exhaustive `NodeKind` consumers in independently owned crates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathElement {
    short_name: String,
    id: Option<String>,
    kind: PathKind,
    parent: Option<Arc<Self>>,
    fabric: Option<Arc<Element>>,
    local: Option<LocalMetadata>,
}

impl PathElement {
    /// Adapt existing Fabric context without changing its metadata or parents.
    pub fn from_fabric(element: Arc<Element>) -> Arc<Self> {
        Arc::new(Self {
            short_name: element.short_name().into(),
            id: element.id().map(str::to_owned),
            kind: PathKind::Fabric(element.kind()),
            parent: element
                .parent()
                .map(|parent| Self::from_fabric(Arc::clone(parent))),
            fabric: Some(element),
            local: None,
        })
    }

    /// Construct typed extended metadata with source-valid parentage.
    pub fn child(
        name: impl Into<String>,
        id: Option<String>,
        kind: PathKind,
        parent: Arc<Self>,
    ) -> Result<Arc<Self>, ContextError> {
        let mut name = name.into();
        let valid = match kind {
            PathKind::VirtualWorkspace(_) => parent.kind == PathKind::Fabric(NodeKind::Tenant),
            PathKind::VirtualWorkspaceItem(kind) => parent.kind == PathKind::VirtualWorkspace(kind),
            PathKind::VirtualItemContainer(_) => {
                matches!(parent.kind, PathKind::Fabric(NodeKind::Workspace(_)))
            }
            PathKind::VirtualItem(kind) => parent.kind == PathKind::VirtualItemContainer(kind),
            PathKind::OneLake(_) => matches!(
                parent.kind,
                PathKind::Fabric(NodeKind::Item(_)) | PathKind::OneLake(_)
            ),
            _ => false,
        };
        if !valid {
            return Err(invalid_context(
                "Invalid extended Fabric metadata parent relationship",
            ));
        }
        let collection = match kind {
            PathKind::VirtualWorkspace(kind) => Some(kind.collection()),
            PathKind::VirtualItemContainer(kind) => Some(kind.collection()),
            _ => None,
        };
        if let Some(collection) = collection {
            if name.to_lowercase() != collection {
                return Err(invalid_context(
                    "Virtual collection name does not match its declared kind",
                ));
            }
            name = collection.into();
        }
        Ok(Arc::new(Self {
            short_name: name,
            id,
            kind,
            parent: Some(parent),
            fabric: None,
            local: None,
        }))
    }

    /// Explicit local classification with no implicit metadata lookup.
    pub fn local(metadata: LocalMetadata) -> Arc<Self> {
        Arc::new(Self {
            short_name: metadata.name.clone(),
            id: Some(metadata.id.clone()),
            kind: PathKind::Local(metadata.kind),
            parent: None,
            fabric: None,
            local: Some(metadata),
        })
    }
    /// Unqualified source display name.
    pub fn short_name(&self) -> &str {
        &self.short_name
    }
    /// Opaque identity, absent for unmaterialized creation targets/collections.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Typed domain and subtype.
    pub fn kind(&self) -> PathKind {
        self.kind
    }
    /// Immutable parent link.
    pub fn parent(&self) -> Option<&Arc<Self>> {
        self.parent.as_ref()
    }
    /// Original Fabric node, where this is an adapted core node.
    pub fn fabric(&self) -> Option<&Arc<Element>> {
        self.fabric.as_ref()
    }
    /// Caller's explicit local metadata, never a remote fallback.
    pub fn local_metadata(&self) -> Option<&LocalMetadata> {
        self.local.as_ref()
    }
    /// Canonical source name including only the suffix appropriate to the kind.
    pub fn name(&self) -> String {
        match self.kind {
            PathKind::Fabric(_) => self.fabric.as_ref().expect("Fabric adapter").name(),
            PathKind::VirtualWorkspace(_)
            | PathKind::VirtualItemContainer(_)
            | PathKind::Local(_) => self.short_name.clone(),
            PathKind::VirtualWorkspaceItem(kind) => {
                format!("{}.{}", self.short_name, kind.suffix())
            }
            PathKind::VirtualItem(kind) => format!("{}.{}", self.short_name, kind.suffix()),
            PathKind::OneLake(OneLakeKind::Shortcut) => format!("{}.Shortcut", self.short_name),
            PathKind::OneLake(_) => self.short_name.clone(),
        }
    }
    /// Display path with the source's distinct Fabric/OneLake/local semantics.
    pub fn path(&self) -> String {
        if let Some(fabric) = &self.fabric {
            return fabric.path();
        }
        if let Some(local) = &self.local {
            return local.path.clone();
        }
        let parent = self.parent.as_ref().expect("extended remote parent");
        if matches!(self.kind, PathKind::VirtualWorkspace(_)) {
            format!(
                "{}/{}",
                parent.path().trim_end_matches('/'),
                self.name().replace('/', "\\/")
            )
        } else {
            format!("{}/{}", parent.path(), self.name())
        }
    }
    /// ID path, preserving virtual-container omissions and OneLake name segments.
    pub fn path_id(&self) -> String {
        if let Some(fabric) = &self.fabric {
            return fabric.path_id();
        }
        let Some(parent) = &self.parent else {
            return "/".into();
        };
        match self.kind {
            PathKind::VirtualItemContainer(_) => parent.path_id(),
            PathKind::OneLake(_) => format!("{}/{}", parent.path_id(), self.short_name),
            _ => format!(
                "{}/{}",
                parent.path_id().trim_end_matches('/'),
                self.id().unwrap_or("None")
            ),
        }
    }
    /// Local path relative to the containing item for OneLake nodes.
    pub fn local_path(&self) -> Option<String> {
        if !matches!(self.kind, PathKind::OneLake(_)) {
            return None;
        }
        let parent = self.parent.as_ref()?;
        Some(match parent.local_path() {
            Some(path) => format!("{path}/{}", self.short_name),
            None => self.short_name.clone(),
        })
    }
    /// First OneLake folder below the owning item.
    pub fn root_folder(&self) -> Option<&str> {
        if !matches!(self.kind, PathKind::OneLake(_)) {
            return None;
        }
        let mut current = self;
        while let Some(parent) = &current.parent {
            if !matches!(parent.kind, PathKind::OneLake(_)) {
                break;
            }
            current = parent;
        }
        Some(&current.short_name)
    }
    /// Whether this node or an ancestor is a OneLake shortcut.
    pub fn is_shortcut_path(&self) -> bool {
        self.kind == PathKind::OneLake(OneLakeKind::Shortcut)
            || self.parent.as_ref().is_some_and(|parent| {
                matches!(parent.kind, PathKind::OneLake(_)) && parent.is_shortcut_path()
            })
    }
    /// Selected tenant root; local paths explicitly have no tenant.
    pub fn root(self: &Arc<Self>) -> Result<Arc<Self>, ContextError> {
        let mut current = Arc::clone(self);
        while let Some(parent) = &current.parent {
            current = Arc::clone(parent);
        }
        if current.kind != PathKind::Fabric(NodeKind::Tenant) {
            return Err(invalid_context("LocalPath does not have a tenant"));
        }
        Ok(current)
    }
}

pub(crate) fn invalid_context(message: &str) -> ContextError {
    ContextError {
        kind: ErrorKind::InvalidContext,
        code: "InvalidContext".into(),
        message: message.into(),
    }
}
