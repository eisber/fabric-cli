// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::node::invalid_context;
use super::{HierarchyLookup, OneLakeProperties, PathElement, PathKind, VirtualLookupRequest};
use crate::{Catalog, ContextError, Lookup, LookupRequest, NodeKind};
use std::sync::Arc;

/// Ordered caller-owned extended metadata, not a service cache or HTTP store.
///
/// Virtual duplicate names select the first matching scope/name. OneLake rows
/// are exact ID paths with case-sensitive names; failures can instead be
/// injected by implementing `HierarchyLookup` directly.
#[derive(Clone, Debug)]
pub struct HierarchyCatalog {
    fabric: Catalog,
    virtual_elements: Vec<Arc<PathElement>>,
    properties: Vec<(String, OneLakeProperties)>,
}

impl HierarchyCatalog {
    /// Compose immutable Fabric, virtual and OneLake snapshots supplied by a caller.
    pub fn new(
        fabric: Catalog,
        virtual_elements: Vec<Arc<PathElement>>,
        properties: Vec<(String, OneLakeProperties)>,
    ) -> Self {
        Self {
            fabric,
            virtual_elements,
            properties,
        }
    }
}

impl Lookup for HierarchyCatalog {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self.fabric.lookup(request)
    }
}

impl HierarchyLookup for HierarchyCatalog {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        let scope_valid = match request.kind {
            PathKind::VirtualWorkspaceItem(_) => {
                request.parent.kind() == PathKind::Fabric(NodeKind::Tenant)
            }
            PathKind::VirtualItem(kind) => {
                request.parent.kind() == PathKind::VirtualItemContainer(kind)
            }
            _ => false,
        };
        if !scope_valid {
            return Err(invalid_context("Invalid virtual catalog lookup scope"));
        }
        let found = self.virtual_elements.iter().find(|node| {
            if node.kind() != request.kind || node.name() != request.name.trim_matches('/') {
                return false;
            }
            match request.kind {
                PathKind::VirtualWorkspaceItem(_) => {
                    node.root().ok().as_ref() == Some(request.parent)
                }
                PathKind::VirtualItem(_) => node.parent() == Some(request.parent),
                _ => false,
            }
        });
        Ok(found.and_then(|node| node.id().filter(|id| !id.is_empty()).map(str::to_owned)))
    }
    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        Ok(self
            .properties
            .iter()
            .find(|(key, _)| key == path)
            .map(|(_, properties)| properties.clone()))
    }
}
