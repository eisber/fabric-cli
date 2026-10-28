// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    errors,
    session::{ReadSession, snapshot::Snapshot},
};
use fabric_cli_context::{
    ContextError, Element, HierarchyLookup, ItemType, Lookup, LookupKind, LookupRequest, NodeKind,
    OneLakeProperties, PathElement, PathInput, PathKind, PathResolver, ResolveOptions,
    VirtualLookupRequest, VirtualWorkspaceKind, WorkspaceType,
};
use fabric_cli_mutations::{CommandFuture, MutationError, RemovalCatalog};
use fabric_cli_output::CommandError;
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_onelake::{OneLakeError, OneLakeLimits, OneLakePath};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
};

enum Needed {
    Fabric(Arc<Element>, LookupKind),
    Capacities,
    Properties(String),
}

#[derive(Default)]
pub(crate) struct CommandContext {
    snapshot: Snapshot,
    capacities: Option<Vec<fabric_cli_local::Capacity>>,
    properties: BTreeMap<String, Option<OneLakeProperties>>,
}

impl Lookup for CommandContext {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self.snapshot.lookup(request)
    }
}

impl HierarchyLookup for CommandContext {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        if request.kind != PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity) {
            return Err(ContextError::lookup(
                "NotSupported",
                "This virtual resource metadata is not mounted",
            ));
        }
        let capacities = self.capacities.as_ref().ok_or_else(required)?;
        let name = request
            .name
            .rsplit_once('.')
            .map_or(request.name, |(name, _)| name);
        Ok(capacities
            .iter()
            .find(|capacity| capacity.display_name == name)
            .map(|capacity| capacity.id.clone()))
    }

    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        self.properties.get(path).cloned().ok_or_else(required)
    }
}

fn required() -> ContextError {
    ContextError::lookup("SnapshotRequired", "Metadata required")
}

struct Loading<'a> {
    context: &'a CommandContext,
    needed: &'a RefCell<Option<Needed>>,
}

impl Lookup for Loading<'_> {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        if !self.context.snapshot.ready(request) {
            self.needed
                .replace(Some(Needed::Fabric(request.parent.clone(), request.kind)));
            return Err(required());
        }
        self.context.lookup(request)
    }
}

impl HierarchyLookup for Loading<'_> {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        if request.kind == PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity)
            && self.context.capacities.is_none()
        {
            self.needed.replace(Some(Needed::Capacities));
            return Err(required());
        }
        self.context.virtual_id(request)
    }

    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        if !self.context.properties.contains_key(path) {
            self.needed.replace(Some(Needed::Properties(path.into())));
            return Err(required());
        }
        self.context.properties(path)
    }
}

impl CommandContext {
    pub(crate) async fn removal_catalog(
        &self,
        reads: &ReadSession<'_>,
        target: &Arc<Element>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RemovalSnapshot, CommandError> {
        let mut cancelled = pin!(cancelled);
        let mut elements = Vec::new();
        if target.kind() == NodeKind::Tenant {
            let list = reads
                .sdk
                .list_workspaces(reads.executor, Default::default(), cancelled.as_mut())
                .await
                .map_err(|error| errors::workspace(&error.kind))?;
            for workspace in list.value {
                let kind = match workspace.workspace_type.to_ascii_lowercase().as_str() {
                    "workspace" => WorkspaceType::Workspace,
                    "personal" => WorkspaceType::Personal,
                    _ => continue,
                };
                elements.push(
                    Element::child(
                        &workspace.display_name,
                        Some(workspace.id),
                        NodeKind::Workspace(kind),
                        target.clone(),
                    )
                    .map_err(|error| errors::error(&error.code, error.message))?,
                );
            }
        } else if matches!(target.kind(), NodeKind::Workspace(_)) {
            let id = target
                .id()
                .ok_or_else(|| errors::error("NotFound", "Workspace has no ID"))?;
            let list = reads
                .sdk
                .list_items(reads.executor, id, Default::default(), cancelled.as_mut())
                .await
                .map_err(|error| errors::item(&error.kind))?;
            let folders = if list
                .value
                .iter()
                .any(|item| item.additional_fields.contains_key("folderId"))
            {
                if let Some(folders) = self.cached_folders(target.clone())? {
                    folders
                } else {
                    let list = reads
                        .sdk
                        .list_folders(reads.executor, id, Default::default(), cancelled.as_mut())
                        .await
                        .map_err(|error| crate::folder_reads::folder_error(&error.kind))?;
                    crate::folder_reads::FolderGraph::new(list.value)
                        .map_err(|error| error.command_error())?
                        .elements(target.clone())
                        .map_err(|error| errors::error(&error.code, error.message))?
                }
            } else {
                Vec::new()
            };
            for item in list.value {
                // Source resolves folder parents before classifying item types.
                let parent = match item.additional_fields.get("folderId") {
                    None => target.clone(),
                    Some(id) => folders
                        .iter()
                        .find(|folder| folder.id() == id.as_str())
                        .filter(|_| id.is_string())
                        .cloned()
                        .ok_or_else(|| {
                            errors::error(
                                "InvalidResponse",
                                "Item folder metadata is missing or invalid",
                            )
                        })?,
                };
                if !item.additional_fields.contains_key("type") {
                    return Err(errors::error(
                        "InvalidResponse",
                        "Fabric returned an item without its type",
                    ));
                }
                let Some(kind) = item.item_type().and_then(|kind| ItemType::parse(kind).ok()) else {
                    continue;
                };
                elements.push(
                    Element::child(
                        &item.display_name,
                        Some(item.id),
                        NodeKind::Item(kind),
                        parent,
                    )
                    .map_err(|error| errors::error(&error.code, error.message))?,
                );
            }
        } else {
            return Err(errors::error("InvalidInput", "Expected a workspace or tenant"));
        }
        Ok(RemovalSnapshot {
            target: target.clone(),
            elements,
        })
    }

    pub(crate) fn cached_folders(
        &self,
        workspace: Arc<Element>,
    ) -> Result<Option<Vec<Arc<Element>>>, CommandError> {
        self.snapshot.cached_folders(workspace)
    }

    pub(crate) async fn resolve(
        &mut self,
        reads: &ReadSession<'_>,
        http: &HttpExecutor,
        current: Arc<Element>,
        path: &str,
        options: ResolveOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Arc<PathElement>, CommandError> {
        self.resolve_path(
            reads,
            http,
            PathElement::from_fabric(current),
            PathInput::Scalar(path),
            options,
            cancelled,
        )
        .await
    }

    pub(crate) async fn resolve_path(
        &mut self,
        reads: &ReadSession<'_>,
        http: &HttpExecutor,
        current: Arc<PathElement>,
        path: PathInput<'_>,
        options: ResolveOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Arc<PathElement>, CommandError> {
        let mut cancelled = pin!(cancelled);
        for _ in 0..128 {
            if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
                return Err(errors::error(
                    "LongRunningOperationCancelled",
                    "Operation cancelled",
                ));
            }
            let (result, needed) = {
                let needed = RefCell::new(None);
                let loading = Loading {
                    context: self,
                    needed: &needed,
                };
                let resolver =
                    PathResolver::new(&loading, current.clone(), reads.home.clone(), "ls")
                        .map_err(|error| errors::error(&error.code, error.message))?;
                let result = resolver.resolve(path, options);
                (result, needed.into_inner())
            };
            match needed {
                None => {
                    return result.map(|result| result.element).map_err(|failure| {
                        errors::error(&failure.error.code, failure.error.message)
                    });
                }
                Some(Needed::Fabric(parent, kind)) => {
                    self.snapshot
                        .load_lookup(reads, parent, kind, cancelled.as_mut())
                        .await?;
                }
                Some(Needed::Capacities) => {
                    let list = reads
                        .sdk
                        .list_capacities(reads.executor, Default::default(), cancelled.as_mut())
                        .await
                        .map_err(|error| errors::capacity(&error.kind))?;
                    self.capacities = Some(
                        list.value
                            .into_iter()
                            .map(|capacity| fabric_cli_local::Capacity {
                                display_name: capacity.display_name,
                                id: capacity.id,
                            })
                            .collect(),
                    );
                }
                Some(Needed::Properties(path)) => {
                    let mut parts = path.splitn(3, '/');
                    let target = OneLakePath::new(
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                    )
                    .map_err(|_| errors::error("InvalidPath", "Invalid OneLake metadata path"))?;
                    let headers = super::storage_client(reads)
                        .properties(
                            &OneLakeHttpExecutor::new(Arc::new(http.clone())),
                            &target,
                            OneLakeLimits::default(),
                            cancelled.as_mut(),
                        )
                        .await;
                    let properties = match headers {
                        Ok(headers) => {
                            let header = |name: &str| {
                                headers
                                    .iter()
                                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                                    .map(|(_, value)| value.clone())
                            };
                            Some(OneLakeProperties {
                                status_code: 200,
                                resource_type: header("x-ms-resource-type"),
                                is_shortcut: header("x-ms-onelake-shortcut-path").as_deref()
                                    == Some("true"),
                            })
                        }
                        Err(OneLakeError::Http { status: 404, .. }) => None,
                        Err(OneLakeError::Credential) => {
                            return Err(errors::error("Unauthorized", "Access is unauthorized"));
                        }
                        Err(OneLakeError::Cancelled) => {
                            return Err(errors::error(
                                "LongRunningOperationCancelled",
                                "Operation cancelled",
                            ));
                        }
                        Err(error) => return Err(errors::error("OneLakeError", error.to_string())),
                    };
                    self.properties.insert(path, properties);
                }
            }
        }
        Err(errors::unsupported(
            "Path exceeds the command metadata-scope bound",
        ))
    }
}

pub(crate) struct RemovalSnapshot {
    target: Arc<Element>,
    elements: Vec<Arc<Element>>,
}

impl RemovalSnapshot {
    fn elements(&self, target: &Arc<Element>) -> Result<Vec<Arc<Element>>, MutationError> {
        if target != &self.target {
            return Err(MutationError::new(
                "InvalidContext",
                "Removal snapshot belongs to another resource",
            ));
        }
        Ok(self.elements.clone())
    }
}

impl RemovalCatalog for RemovalSnapshot {
    fn workspaces<'a>(
        &'a mut self,
        tenant: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        Box::pin(std::future::ready(self.elements(tenant)))
    }

    fn items<'a>(
        &'a mut self,
        workspace: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        Box::pin(std::future::ready(self.elements(workspace)))
    }
}
