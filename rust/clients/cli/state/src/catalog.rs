// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::sync::Arc;

use crate::{ContextError, Element, ErrorKind, NodeKind};

/// Source lookup operation; service execution and caching belong to the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupKind {
    /// Workspace by exact full name in the selected tenant.
    Workspace,
    /// Folder by exact full name anywhere in the workspace (Python quirk).
    Folder,
    /// Nested folder by exact full name and parent.
    NestedFolder,
    /// Item by exact full name anywhere in the workspace (Python quirk).
    Item,
}

/// One synchronous, already-resolved metadata lookup, never an HTTP request.
#[derive(Clone, Copy, Debug)]
pub struct LookupRequest<'a> {
    /// Which Python mem-store boundary is being requested.
    pub kind: LookupKind,
    /// Selected tenant, workspace or folder at that boundary.
    pub parent: &'a Arc<Element>,
    /// Canonical suffix, case-sensitive display name.
    pub name: &'a str,
}

/// Caller-resolved lookup boundary.
///
/// Implement this over a preloaded immutable snapshot or caller-resolved oracle.
/// No network, authentication, retry, runtime or ambient state is supplied here.
/// `None` means absent; failures retain their exact code/message. Matching and
/// duplicate ordering must follow the Python mem-store contract.
pub trait Lookup {
    /// Resolve an opaque ID for this canonical name and scope.
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError>;
}

impl<F> Lookup for F
where
    F: Fn(LookupRequest<'_>) -> Result<Option<String>, ContextError>,
{
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self(request)
    }
}

/// An ordered immutable graph oracle, useful for offline callers and fixtures.
///
/// Preserves first-match duplicate selection, workspace-wide item/root-folder
/// matching, and exact display-name case. It is not a service cache or SDK.
#[derive(Clone, Debug)]
pub struct Catalog {
    elements: Vec<Arc<Element>>,
}

impl Catalog {
    /// Retain caller-supplied metadata in source lookup order.
    pub fn new(elements: Vec<Arc<Element>>) -> Self {
        Self { elements }
    }

    /// Inspect the immutable metadata snapshot.
    pub fn elements(&self) -> &[Arc<Element>] {
        &self.elements
    }
}

impl Lookup for Catalog {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        let valid_scope = match request.kind {
            LookupKind::Workspace => request.parent.kind() == NodeKind::Tenant,
            LookupKind::Folder | LookupKind::Item => {
                matches!(request.parent.kind(), NodeKind::Workspace(_))
            }
            LookupKind::NestedFolder => request.parent.kind() == NodeKind::Folder,
        };
        if !valid_scope {
            return Err(ContextError {
                kind: ErrorKind::InvalidContext,
                code: "InvalidContext".into(),
                message: "Invalid Fabric catalog lookup scope".into(),
            });
        }
        let workspace = request.parent.workspace();
        let lookup_name = if request.kind == LookupKind::Workspace {
            request.name
        } else {
            request.name.trim_matches('/')
        };
        let found = self.elements.iter().find(|element| {
            if element.name() != lookup_name {
                return false;
            }
            match request.kind {
                LookupKind::Workspace => {
                    matches!(element.kind(), NodeKind::Workspace(_))
                        && element.parent() == Some(request.parent)
                }
                LookupKind::Folder => {
                    element.kind() == NodeKind::Folder && element.workspace() == workspace
                }
                LookupKind::NestedFolder => {
                    element.kind() == NodeKind::Folder && element.parent() == Some(request.parent)
                }
                LookupKind::Item => {
                    matches!(element.kind(), NodeKind::Item(_)) && element.workspace() == workspace
                }
            }
        });
        Ok(found.and_then(|element| element.id().filter(|id| !id.is_empty()).map(str::to_owned)))
    }
}
