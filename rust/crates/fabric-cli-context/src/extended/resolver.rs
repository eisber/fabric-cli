// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{error::Error, fmt, sync::Arc};

use super::node::invalid_context;
use super::{
    CommandPolicy, LocalMetadata, OneLakeKind, PathElement, PathKind, VirtualItemKind,
    VirtualWorkspaceKind, item_folders,
};
use crate::{
    ContextError, HomeContext, Lookup, NodeKind, PathInput, ResolveOptions, Resolver,
    UnsupportedBranch, split_parts,
};

/// A virtual resource lookup, not an HTTP endpoint.
#[derive(Clone, Copy, Debug)]
pub struct VirtualLookupRequest<'a> {
    /// Expected typed leaf; source calls tenant scope for administrative leaves.
    pub kind: PathKind,
    /// Tenant for administrative resources; virtual container for workspace leaves.
    pub parent: &'a Arc<PathElement>,
    /// Canonical leaf full name, case-sensitive apart from its suffix.
    pub name: &'a str,
}

/// Typed properties of a caller-resolved OneLake resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakeProperties {
    /// Raw response status; the source requires exactly 200.
    pub status_code: u16,
    /// Exact `x-ms-resource-type` value; unknown/missing maps to undefined.
    pub resource_type: Option<String>,
    /// True only for exact `x-ms-onelake-shortcut-path: true`.
    pub is_shortcut: bool,
}

/// Extended caller-resolved hierarchy boundary; no service/runtime supplied.
pub trait HierarchyLookup: Lookup {
    /// Resolve a virtual resource's opaque ID; absence differs from failure.
    fn virtual_id(&self, request: VirtualLookupRequest<'_>)
    -> Result<Option<String>, ContextError>;
    /// Resolve OneLake properties for the exact source ID path, without a leading slash.
    ///
    /// `None` or `NotFound` means no resource. Other failures are not suppressed.
    /// Raw non-200 responses retain the source assertion failure as a typed
    /// `AssertionError`, not a Rust panic or successful resource.
    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError>;
}

/// Source argument to `os.path.exists`, including the source's boolean-fd quirk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalProbe<'a> {
    /// Path string.
    Path(&'a str),
    /// The `dirname and access` expression yields a bool, not a directory string.
    Boolean(bool),
}

/// Source filesystem permission probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalAccess {
    /// `os.R_OK`.
    Read,
    /// `os.W_OK`.
    Write,
}

/// Local snapshot/classification boundary owned by the filesystem consumer.
///
/// This resolver never calls the OS. Implementations must make platform basename,
/// dirname and process-random/opaque identity choices explicit. The source
/// boolean probe is retained, not executed as a real file-descriptor stat here.
pub trait LocalLookup {
    /// Equivalent of source `os.path.exists` for this explicitly typed argument.
    fn exists(&self, probe: LocalProbe<'_>) -> Result<bool, ContextError>;
    /// Caller-resolved read/write permission.
    fn access(&self, path: &str, mode: LocalAccess) -> Result<bool, ContextError>;
    /// OS-specific `dirname`, supplied by the caller.
    fn dirname(&self, path: &str) -> Result<String, ContextError>;
    /// Metadata classification for a successful local fallback/explicit local input.
    fn describe(&self, path: &str) -> Result<LocalMetadata, ContextError>;
}

/// Non-mutating diagnostics that source emits while resolving paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathNotice {
    /// Source grey diagnostic before local fallback, even if fallback fails.
    LocalFallback(String),
    /// Source external data-sharing warning on the final virtual result only.
    ExternalDataSharing,
}

impl PathNotice {
    /// Exact source diagnostic text.
    pub fn message(&self) -> &str {
        match self {
            Self::LocalFallback(message) => message,
            Self::ExternalDataSharing => {
                "Ensure tenant setting is enabled for External data sharing"
            }
        }
    }
}

/// Successful path and source diagnostics, kept separate from output rendering.
#[derive(Clone, Debug)]
pub struct PathResolution {
    /// Resolved immutable remote or local element.
    pub element: Arc<PathElement>,
    /// Diagnostics in source order.
    pub notices: Vec<PathNotice>,
}

/// Failure retains diagnostics rather than silently dropping fallback attempts.
#[derive(Clone, Debug)]
pub struct PathFailure {
    /// Precise source/caller failure.
    pub error: ContextError,
    /// Diagnostics emitted before that failure.
    pub notices: Vec<PathNotice>,
}

impl fmt::Display for PathFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, formatter)
    }
}

impl Error for PathFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// Additive full hierarchy resolver. Original bounded `Resolver` stays unchanged.
///
/// Extended lookups, filesystem metadata, command capabilities and home selection
/// are injected. Resolution never mutates current context or executes I/O.
pub struct PathResolver<'a, L: HierarchyLookup + ?Sized> {
    lookup: &'a L,
    local: Option<&'a dyn LocalLookup>,
    current: Arc<PathElement>,
    home: HomeContext,
    list_policy: CommandPolicy,
    list_command: String,
}

impl<'a, L: HierarchyLookup + ?Sized> PathResolver<'a, L> {
    /// Bind a remote current context, explicit home and source `ls`/`dir` alias.
    ///
    /// A local path has no tenant and cannot be a Fabric current context.
    pub fn new(
        lookup: &'a L,
        current: Arc<PathElement>,
        home: HomeContext,
        list_command: &str,
    ) -> Result<Self, ContextError> {
        current.root()?;
        if !matches!(list_command, "ls" | "dir") {
            return Err(invalid_context(
                "The source list command alias must be ls or dir",
            ));
        }
        Ok(Self {
            lookup,
            local: None,
            current,
            home,
            list_policy: CommandPolicy::source_list(),
            list_command: list_command.into(),
        })
    }
    /// Compose an explicitly selected local filesystem metadata boundary.
    pub fn with_local(mut self, local: &'a dyn LocalLookup) -> Self {
        self.local = Some(local);
        self
    }
    /// Override the caller-resolved source command capability snapshot.
    pub fn with_list_policy(mut self, policy: CommandPolicy) -> Self {
        self.list_policy = policy;
        self
    }
    /// Current immutable context; resolve never changes it.
    pub fn current(&self) -> &Arc<PathElement> {
        &self.current
    }
    /// Resolve all remote hierarchy branches and conditional local fallback.
    pub fn resolve(
        &self,
        input: PathInput<'_>,
        options: ResolveOptions,
    ) -> Result<PathResolution, PathFailure> {
        let mut notices = Vec::new();
        let original = match input {
            PathInput::Scalar(value) => value.to_owned(),
            PathInput::Tokens(values) => values.join(" ").trim_matches(['\'', '"']).to_owned(),
            PathInput::Local(value) => {
                return self
                    .local_element(value)
                    .map(|element| PathResolution {
                        element,
                        notices: Vec::new(),
                    })
                    .map_err(|error| PathFailure {
                        error,
                        notices: Vec::new(),
                    });
            }
        };
        // Python performs home discovery before its try/except-local-fallback.
        let path = if original == "~" || original.starts_with("~/") {
            match &self.home {
                HomeContext::Workspace(name) => format!("/{name}{}", &original[1..]),
                HomeContext::Unavailable(error) => {
                    return Err(PathFailure {
                        error: error.clone(),
                        notices,
                    });
                }
                HomeContext::Unspecified => {
                    return Err(PathFailure {
                        error: ContextError::unsupported(
                            UnsupportedBranch::HomeDiscovery,
                            &original,
                        ),
                        notices,
                    });
                }
            }
        } else {
            original.clone()
        };
        let resolved = self.remote(&path, options.raise_error);
        match resolved {
            Ok(element) => {
                if matches!(
                    element.kind(),
                    PathKind::VirtualItem(VirtualItemKind::ExternalDataShare)
                        | PathKind::VirtualItemContainer(VirtualItemKind::ExternalDataShare)
                ) {
                    notices.push(PathNotice::ExternalDataSharing);
                }
                Ok(PathResolution { element, notices })
            }
            Err(error)
                if options.supports_local_path
                    && matches!(
                        error.code.as_str(),
                        "NotFound" | "InvalidPath" | "NotSupported"
                    ) =>
            {
                notices.push(PathNotice::LocalFallback(format!(
                    "{original} not found in Fabric, checking local file system"
                )));
                match self.fallback(&original, options.raise_error) {
                    Ok(Some(element)) => Ok(PathResolution { element, notices }),
                    Ok(None) => Err(PathFailure { error, notices }),
                    Err(error) => Err(PathFailure { error, notices }),
                }
            }
            Err(error) => Err(PathFailure { error, notices }),
        }
    }

    /// Explicit atomic context change after success, rejecting local current contexts.
    ///
    /// Consumers must apply their command capability policy before using this as
    /// a `cd` operation. There is no persistence or runtime-mode change here.
    pub fn change_context(
        &mut self,
        input: PathInput<'_>,
        options: ResolveOptions,
    ) -> Result<PathResolution, PathFailure> {
        let result = self.resolve(input, options)?;
        result.element.root().map_err(|error| PathFailure {
            error,
            notices: result.notices.clone(),
        })?;
        self.current = Arc::clone(&result.element);
        Ok(result)
    }

    fn remote(&self, path: &str, raise_error: bool) -> Result<Arc<PathElement>, ContextError> {
        let absolute = path.starts_with('/');
        let path = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        let root = self.current.root()?;
        if path == "/" || path == "/.." {
            return Ok(root);
        }
        let mut stack = if absolute {
            vec![root]
        } else {
            let mut stack = vec![Arc::clone(&self.current)];
            let mut node = &self.current;
            while let Some(parent) = node.parent() {
                stack.push(Arc::clone(parent));
                node = parent;
            }
            stack.reverse();
            stack
        };
        let parts = split_parts(path);
        for (index, part) in parts.iter().enumerate() {
            match part.as_str() {
                "" | "." => continue,
                ".." => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                    continue;
                }
                value if value.starts_with("...") => {
                    return Err(ContextError::python(
                        "InvalidPath",
                        "The specified path is invalid",
                    ));
                }
                _ => {}
            }
            let parent = stack.last().expect("selected tenant");
            stack.push(self.child(parent, part, raise_error, index + 1 == parts.len())?);
        }
        Ok(stack.pop().expect("selected tenant"))
    }

    fn child(
        &self,
        parent: &Arc<PathElement>,
        part: &str,
        raise_error: bool,
        final_part: bool,
    ) -> Result<Arc<PathElement>, ContextError> {
        match parent.kind() {
            PathKind::Fabric(NodeKind::Tenant) => {
                if let Some(kind) = VirtualWorkspaceKind::ALL
                    .into_iter()
                    .find(|kind| part.to_lowercase() == kind.collection())
                {
                    return PathElement::child(
                        kind.collection(),
                        None,
                        PathKind::VirtualWorkspace(kind),
                        Arc::clone(parent),
                    );
                }
            }
            PathKind::Fabric(NodeKind::Workspace(_)) if parent.id().is_some() => {
                if let Some(kind) = VirtualItemKind::ALL
                    .into_iter()
                    .find(|kind| part.to_lowercase() == kind.collection())
                {
                    return PathElement::child(
                        kind.collection(),
                        None,
                        PathKind::VirtualItemContainer(kind),
                        Arc::clone(parent),
                    );
                }
            }
            PathKind::VirtualWorkspace(kind) => {
                let (name, suffix) = virtual_name(
                    part,
                    &VirtualWorkspaceKind::ALL.map(VirtualWorkspaceKind::suffix),
                )?;
                if suffix != kind.suffix() {
                    return Err(wrong_context(suffix, "VirtualWorkspace"));
                }
                let tenant = parent.root()?;
                let id = self.virtual_id(
                    VirtualLookupRequest {
                        kind: PathKind::VirtualWorkspaceItem(kind),
                        parent: &tenant,
                        name: &format!("{name}.{suffix}"),
                    },
                    kind.suffix(),
                    raise_error,
                )?;
                return PathElement::child(
                    name,
                    id,
                    PathKind::VirtualWorkspaceItem(kind),
                    Arc::clone(parent),
                );
            }
            PathKind::VirtualItemContainer(kind) => {
                let (name, suffix) =
                    virtual_name(part, &VirtualItemKind::ALL.map(VirtualItemKind::suffix))?;
                if suffix != kind.suffix() {
                    return Err(wrong_context(suffix, "VirtualItemContainer"));
                }
                let id = self.virtual_id(
                    VirtualLookupRequest {
                        kind: PathKind::VirtualItem(kind),
                        parent,
                        name: &format!("{name}.{suffix}"),
                    },
                    kind.resource(),
                    raise_error,
                )?;
                return PathElement::child(
                    name,
                    id,
                    PathKind::VirtualItem(kind),
                    Arc::clone(parent),
                );
            }
            PathKind::Fabric(NodeKind::Item(kind)) => {
                self.list_policy.check(parent, &self.list_command)?;
                if parent.id().is_none() {
                    return Err(ContextError::python(
                        "NotFound",
                        format!("The Item '{}' could not be found", parent.name()),
                    ));
                }
                let folders = item_folders(kind);
                let folder = folders.iter().find(|folder| folder.to_lowercase() == part.to_lowercase()).ok_or_else(|| ContextError::python("NotSupported", format!("The folder '{part}' could not be found in item '{}'. Valid folders are: {}", parent.name(), folders.join(", "))))?;
                return PathElement::child(
                    *folder,
                    Some("0000".into()),
                    PathKind::OneLake(OneLakeKind::Folder),
                    Arc::clone(parent),
                );
            }
            PathKind::OneLake(_) => {
                return self.onelake_child(parent, part, raise_error, final_part);
            }
            PathKind::VirtualWorkspaceItem(_) | PathKind::VirtualItem(_) | PathKind::Local(_) => {
                return Err(ContextError::python(
                    "InvalidPath",
                    format!("Traversing into '{}' is not supported", parent.path()),
                ));
            }
            _ => {}
        }
        let fabric = parent
            .fabric()
            .ok_or_else(|| invalid_context("Expected Fabric metadata adapter"))?;
        Resolver::new(self.lookup, Arc::clone(fabric), self.home.clone())
            .child(fabric, part, raise_error)
            .map(PathElement::from_fabric)
    }

    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
        resource: &str,
        raise_error: bool,
    ) -> Result<Option<String>, ContextError> {
        match self.lookup.virtual_id(request) {
            Ok(Some(id)) if !id.is_empty() => Ok(Some(id)),
            Ok(_) if !raise_error => Ok(None),
            Ok(_) => Err(ContextError::python(
                "NotFound",
                format!("The {resource} '{}' could not be found", request.name),
            )),
            Err(error) if !raise_error && error.code == "NotFound" => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        match self.lookup.properties(path) {
            Err(error) if error.code == "NotFound" => Ok(None),
            outcome => outcome,
        }
    }

    fn onelake_child(
        &self,
        parent: &Arc<PathElement>,
        part: &str,
        raise_error: bool,
        final_part: bool,
    ) -> Result<Arc<PathElement>, ContextError> {
        let mut name = part.replace(".Shortcut", "").replace(".shortcut", "");
        let mut path = format!("{}/{name}", parent.path_id())
            .trim_start_matches('/')
            .to_owned();
        let mut properties = self.properties(&path)?;
        if properties.is_none() {
            name = part.into();
            path = format!("{}/{name}", parent.path_id())
                .trim_start_matches('/')
                .to_owned();
            properties = self.properties(&path)?;
        }
        let (id, kind) = match properties {
            None if !raise_error && final_part => (None, OneLakeKind::Undefined),
            None => {
                return Err(ContextError::python(
                    "NotFound",
                    format!("The path '{path}' could not be found"),
                ));
            }
            Some(properties) if properties.status_code != 200 => {
                return Err(ContextError::python("AssertionError", ""));
            }
            Some(properties) => (
                Some("0000".into()),
                match properties.resource_type.as_deref() {
                    Some("directory") if properties.is_shortcut => OneLakeKind::Shortcut,
                    Some("directory") if parent.local_path().as_deref() == Some("Tables") => {
                        OneLakeKind::Table
                    }
                    Some("directory") => OneLakeKind::Folder,
                    Some("file") => OneLakeKind::File,
                    _ => OneLakeKind::Undefined,
                },
            ),
        };
        PathElement::child(name, id, PathKind::OneLake(kind), Arc::clone(parent))
    }

    fn local_element(&self, path: &str) -> Result<Arc<PathElement>, ContextError> {
        let local = self
            .local
            .ok_or_else(|| ContextError::unsupported(UnsupportedBranch::LocalFilesystem, path))?;
        let metadata = local.describe(path)?;
        if metadata.path != path {
            return Err(invalid_context(
                "Caller local metadata changed the requested path",
            ));
        }
        Ok(PathElement::local(metadata))
    }

    fn fallback(
        &self,
        path: &str,
        raise_error: bool,
    ) -> Result<Option<Arc<PathElement>>, ContextError> {
        let local = self
            .local
            .ok_or_else(|| ContextError::unsupported(UnsupportedBranch::LocalFilesystem, path))?;
        if local.exists(LocalProbe::Path(path))? && local.access(path, LocalAccess::Read)? {
            return self.local_element(path).map(Some);
        }
        if !raise_error {
            let directory = local.dirname(path)?;
            let exists = if directory.is_empty() {
                local.exists(LocalProbe::Path(""))?
            } else {
                let directory = local.dirname(path)?;
                let writable = local.access(&directory, LocalAccess::Write)?;
                local.exists(LocalProbe::Boolean(writable))?
            };
            if exists {
                return self.local_element(path).map(Some);
            }
        }
        Ok(None)
    }
}

fn virtual_name<'a>(
    part: &'a str,
    types: &[&'static str],
) -> Result<(&'a str, &'static str), ContextError> {
    let (name, suffix) = part
        .rsplit_once('.')
        .filter(|(name, suffix)| {
            !name.contains('\n')
                && !suffix.is_empty()
                && suffix.chars().all(|ch| {
                    ch.is_ascii_alphanumeric()
                        || matches!(
                            ch,
                            '_' | '-' | '\u{130}' | '\u{131}' | '\u{17f}' | '\u{212a}'
                        )
                })
        })
        .ok_or_else(|| {
            ContextError::python("Invalid item name", format!("Invalid item name '{part}'"))
        })?;
    let kind = types
        .iter()
        .find(|kind| kind.to_lowercase() == suffix.to_lowercase())
        .copied()
        .ok_or_else(|| {
            ContextError::python(
                "InvalidItemType",
                format!("'{suffix}' is not a valid Fabric item type"),
            )
        })?;
    Ok((name, kind))
}

fn wrong_context(suffix: &str, context: &str) -> ContextError {
    ContextError::python(
        "InvalidPath",
        format!("The item type '{suffix}' is not supported in the context '{context}'"),
    )
}
