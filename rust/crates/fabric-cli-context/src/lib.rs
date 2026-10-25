// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure bounded translation of Python Fabric path resolution.
//!
//! This is not complete `get_command_context` parity. Only remote tenant,
//! workspace, folder and item nodes are ported. The caller supplies immutable
//! current/home context and resolved metadata. No OS paths, credentials, I/O,
//! runtime, persistent context, or CLI entry-point integration are involved.
//!
#![doc = include_str!("../README.md")]

mod catalog;
mod model;

pub use catalog::{Catalog, Lookup, LookupKind, LookupRequest};
pub use model::{
    ContextError, Element, ErrorKind, ItemType, NodeKind, UnsupportedBranch, WorkspaceType,
};

use std::sync::Arc;

/// Input before Python's list-token joining and quote stripping.
#[derive(Clone, Copy, Debug)]
pub enum PathInput<'a> {
    /// A scalar argument; outer quotes are preserved.
    Scalar(&'a str),
    /// Already parsed CLI tokens, joined with a single space, then quote-stripped.
    Tokens(&'a [&'a str]),
    /// An explicitly local path; never interpreted as a Fabric name.
    Local(&'a str),
}

/// Explicit home selection, replacing Python's ambient identity discovery.
#[derive(Clone, Debug)]
pub enum HomeContext {
    /// Full workspace name, including `.Personal` or `.Workspace`.
    Workspace(String),
    /// The caller could not resolve home, with the original Python/service error.
    Unavailable(ContextError),
    /// No explicit home was supplied; ambient discovery is deliberately unported.
    Unspecified,
}

/// The two Python resolution flags, without enabling local filesystem access.
#[derive(Clone, Copy, Debug)]
pub struct ResolveOptions {
    /// Suppress only lookup `NotFound`, producing nodes with absent IDs.
    pub raise_error: bool,
    /// Local fallback is unported; requesting this mode fails explicitly.
    pub supports_local_path: bool,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            raise_error: true,
            supports_local_path: false,
        }
    }
}

/// A pure resolver borrowing its caller-owned metadata boundary.
pub struct Resolver<'a, L: Lookup + ?Sized> {
    lookup: &'a L,
    current: Arc<Element>,
    home: HomeContext,
}

impl<'a, L: Lookup + ?Sized> Resolver<'a, L> {
    /// Bind explicit current/home context and an injected lookup implementation.
    pub fn new(lookup: &'a L, current: Arc<Element>, home: HomeContext) -> Self {
        Self {
            lookup,
            current,
            home,
        }
    }

    /// Inspect current context. Resolving never changes it, even on partial failure.
    pub fn current(&self) -> &Arc<Element> {
        &self.current
    }

    /// Resolve a supported Fabric path without mutating caller or resolver context.
    pub fn resolve(
        &self,
        input: PathInput<'_>,
        options: ResolveOptions,
    ) -> Result<Arc<Element>, ContextError> {
        let original = match input {
            PathInput::Scalar(value) => value.to_owned(),
            PathInput::Tokens(values) => values.join(" ").trim_matches(['\'', '"']).to_owned(),
            PathInput::Local(value) => {
                return Err(ContextError::unsupported(
                    UnsupportedBranch::LocalFilesystem,
                    value,
                ));
            }
        };
        if options.supports_local_path {
            return Err(ContextError::unsupported(
                UnsupportedBranch::LocalFilesystem,
                &original,
            ));
        }
        let home_path;
        let (path, absolute) = if original.starts_with('/') {
            (original.as_str(), true)
        } else if original == "~" || original.starts_with("~/") {
            let name = match &self.home {
                HomeContext::Workspace(name) => name,
                HomeContext::Unavailable(error) => return Err(error.clone()),
                HomeContext::Unspecified => {
                    return Err(ContextError::unsupported(
                        UnsupportedBranch::HomeDiscovery,
                        &original,
                    ));
                }
            };
            home_path = format!("/{name}{}", &original[1..]);
            (home_path.as_str(), true)
        } else {
            (original.as_str(), false)
        };
        let path = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        if path == "/" || path == "/.." {
            return Ok(self.current.root());
        }

        let mut stack = if absolute {
            vec![self.current.root()]
        } else {
            ancestry(&self.current)
        };
        for part in split_parts(path) {
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
            let current = stack.last().expect("a tenant root is always present");
            let node = self.child(current, &part, options.raise_error)?;
            stack.push(node);
        }
        Ok(stack.pop().expect("a tenant root is always present"))
    }

    /// Explicitly change current context only after successful complete resolution.
    ///
    /// Command-support checks such as `FS_CD` remain the consumer's responsibility.
    /// On any error the previous current context is retained exactly.
    pub fn change_context(
        &mut self,
        input: PathInput<'_>,
        options: ResolveOptions,
    ) -> Result<Arc<Element>, ContextError> {
        let resolved = self.resolve(input, options)?;
        self.current = Arc::clone(&resolved);
        Ok(resolved)
    }

    fn child(
        &self,
        parent: &Arc<Element>,
        part: &str,
        raise_error: bool,
    ) -> Result<Arc<Element>, ContextError> {
        let suffix = if part.starts_with('.') && parent.kind() != NodeKind::Folder {
            part
        } else {
            part.rsplit('.').next().unwrap_or(part)
        };
        let invalid_path = || {
            ContextError::python(
                "InvalidPath",
                format!(
                    "The path '{}/{}' is invalid",
                    parent.path().trim_end_matches('/'),
                    part
                ),
            )
        };
        let (kind, lookup_kind, name, full_name, scope) = match parent.kind() {
            NodeKind::Tenant => {
                if is_virtual_workspace(suffix) || lower_eq(suffix, "virtualworkspace") {
                    if !is_virtual_workspace(part) {
                        return Err(ContextError::python(
                            "InvalidWorkspaceType",
                            format!("Invalid type '{part}'"),
                        ));
                    }
                    return Err(ContextError::unsupported(
                        UnsupportedBranch::VirtualWorkspace,
                        part,
                    ));
                }
                if !lower_eq(suffix, "workspace") && !lower_eq(suffix, "personal") {
                    return Err(invalid_path());
                }
                let (name, suffix) = part
                    .rsplit_once('.')
                    .filter(|(name, _)| !name.is_empty() && !name.contains('\n'))
                    .ok_or_else(|| {
                        ContextError::python(
                            "Invalid workspace name",
                            format!("Invalid workspace name '{part}'"),
                        )
                    })?;
                let workspace_type = if lower_eq(suffix, "workspace") {
                    WorkspaceType::Workspace
                } else {
                    WorkspaceType::Personal
                };
                (
                    NodeKind::Workspace(workspace_type),
                    LookupKind::Workspace,
                    name,
                    format!("{name}.{}", workspace_type.as_str()),
                    Arc::clone(parent),
                )
            }
            NodeKind::Workspace(_) | NodeKind::Folder => {
                if parent.id().is_none() {
                    return Err(not_found(parent.kind(), &parent.name()));
                }
                if matches!(parent.kind(), NodeKind::Workspace(_))
                    && (is_virtual_container(suffix) || lower_eq(suffix, "virtualitemcontainer"))
                {
                    if !is_virtual_container(part) {
                        return Err(ContextError::python(
                            "InvalidItemType",
                            format!("Invalid type '{part}'"),
                        ));
                    }
                    return Err(ContextError::unsupported(
                        UnsupportedBranch::VirtualItemContainer,
                        part,
                    ));
                }
                if lower_eq(suffix, "folder") {
                    let name = part
                        .rsplit_once('.')
                        .filter(|(name, _)| !name.contains('\n'))
                        .map_or(part, |(name, _)| name);
                    let lookup_kind = if parent.kind() == NodeKind::Folder {
                        LookupKind::NestedFolder
                    } else {
                        LookupKind::Folder
                    };
                    (
                        NodeKind::Folder,
                        lookup_kind,
                        name,
                        format!("{name}.Folder"),
                        Arc::clone(parent),
                    )
                } else {
                    let item_type = ItemType::parse(suffix);
                    if item_type.is_err() && !lower_eq(suffix, "item") {
                        return Err(invalid_path());
                    }
                    let (name, item_suffix) = part
                        .rsplit_once('.')
                        .filter(|(name, suffix)| {
                            !name.contains('\n')
                                && !suffix.is_empty()
                                && suffix.chars().all(|ch| {
                                    ch.is_ascii_alphanumeric()
                                        || matches!(
                                            ch,
                                            '_' | '-'
                                                | '\u{130}'
                                                | '\u{131}'
                                                | '\u{17f}'
                                                | '\u{212a}'
                                        )
                                })
                        })
                        .ok_or_else(|| {
                            ContextError::python(
                                "Invalid item name",
                                format!("Invalid item name '{part}'"),
                            )
                        })?;
                    let item_type = ItemType::parse(item_suffix)?;
                    if item_type.strict_name()
                        && name
                            .chars()
                            .any(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
                    {
                        return Err(ContextError::python(
                            "Special characters not supported for this item type",
                            format!(
                                "{} name '{name}' contains unsupported special characters",
                                item_type.as_str()
                            ),
                        ));
                    }
                    (
                        NodeKind::Item(item_type),
                        LookupKind::Item,
                        name,
                        format!("{name}.{}", item_type.as_str()),
                        parent
                            .workspace()
                            .expect("items and folders have workspace parents"),
                    )
                }
            }
            NodeKind::Item(_) => {
                return Err(ContextError::unsupported(
                    UnsupportedBranch::OneLake,
                    &format!("{}/{}", parent.path(), part),
                ));
            }
        };
        let request = LookupRequest {
            kind: lookup_kind,
            parent: &scope,
            name: &full_name,
        };
        let id = match self.lookup.lookup(request) {
            Ok(Some(id)) if !id.is_empty() => Some(id),
            Ok(_) => {
                if raise_error {
                    return Err(not_found(kind, &full_name));
                }
                None
            }
            Err(error) if !raise_error && error.code == "NotFound" => None,
            Err(error) => return Err(error),
        };
        Element::child(name, id, kind, Arc::clone(parent))
    }
}

fn ancestry(current: &Arc<Element>) -> Vec<Arc<Element>> {
    let mut result = vec![Arc::clone(current)];
    let mut node = current;
    while let Some(parent) = node.parent() {
        result.push(Arc::clone(parent));
        node = parent;
    }
    result.reverse();
    result
}

fn split_parts(path: &str) -> Vec<String> {
    let mut start = 0;
    let mut previous = None;
    let mut result = Vec::new();
    for (index, ch) in path.char_indices() {
        if ch == '/' && previous != Some('\\') {
            result.push(python_strip(&path[start..index].replace('\\', "")).to_owned());
            start = index + 1;
        }
        previous = Some(ch);
    }
    result.push(python_strip(&path[start..].replace('\\', "")).to_owned());
    result
}

fn python_strip(value: &str) -> &str {
    // Python str.strip also removes the four ASCII information separators.
    value.trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'))
}

fn not_found(kind: NodeKind, name: &str) -> ContextError {
    let message = if kind == NodeKind::Folder {
        format!("Folder '{name}' not found")
    } else {
        format!("The {} '{name}' could not be found", kind.as_str())
    };
    ContextError::python("NotFound", message)
}

fn is_virtual_workspace(value: &str) -> bool {
    [".capacities", ".connections", ".gateways", ".domains"]
        .iter()
        .any(|name| lower_eq(value, name))
}

fn is_virtual_container(value: &str) -> bool {
    [
        ".sparkpools",
        ".managedidentities",
        ".managedprivateendpoints",
        ".externaldatashares",
    ]
    .iter()
    .any(|name| lower_eq(value, name))
}

fn lower_eq(value: &str, lowercase: &str) -> bool {
    value.to_lowercase() == lowercase
}
