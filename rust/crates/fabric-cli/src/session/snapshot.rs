// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ReadSession;
use crate::{
    errors,
    folder_reads::{FolderGraph, folder_error},
};
use fabric_cli_context::{
    Catalog, ContextError, Element, ItemType, Lookup, LookupKind, LookupRequest, NodeKind,
    PathInput, ResolveOptions, Resolver, WorkspaceType,
};
use fabric_cli_output::CommandError;
use fabric_sdk::{
    folders::FolderListOptions, items::ItemListOptions, workspaces::WorkspaceListLimits,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
};

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum Scope {
    Workspaces,
    Items(String),
    Folders(String),
}

fn scope(request: LookupRequest<'_>) -> Scope {
    match request.kind {
        LookupKind::Workspace => Scope::Workspaces,
        LookupKind::Item => Scope::Items(request.parent.id().unwrap_or_default().to_owned()),
        LookupKind::Folder | LookupKind::NestedFolder => Scope::Folders(
            request
                .parent
                .workspace()
                .and_then(|w| w.id().map(str::to_owned))
                .unwrap_or_default(),
        ),
    }
}

#[derive(Default)]
pub(crate) struct Snapshot {
    elements: Vec<Arc<Element>>,
    loaded: BTreeSet<Scope>,
    folders: BTreeMap<String, FolderGraph>,
}

impl Lookup for Snapshot {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        if let Scope::Folders(id) = scope(request) {
            return Ok(self
                .folders
                .get(&id)
                .and_then(|graph| graph.lookup(request)));
        }
        Catalog::new(self.elements.clone()).lookup(request)
    }
}

impl Snapshot {
    #[cfg(feature = "native-read")]
    pub(crate) fn ready(&self, request: LookupRequest<'_>) -> bool {
        self.loaded.contains(&scope(request))
    }

    #[cfg(feature = "native-read")]
    pub(crate) async fn load_lookup(
        &mut self,
        session: &ReadSession<'_>,
        parent: Arc<Element>,
        kind: LookupKind,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<(), CommandError> {
        let scope = scope(LookupRequest {
            parent: &parent,
            kind,
            name: "",
        });
        self.load(session, scope, parent, cancelled).await
    }

    #[cfg(feature = "native-read")]
    pub(crate) fn elements(&self) -> &[Arc<Element>] {
        &self.elements
    }

    pub(crate) async fn resolve(
        &mut self,
        session: &ReadSession<'_>,
        current: &Arc<Element>,
        path: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Arc<Element>, CommandError> {
        let mut cancelled = pin!(cancelled);
        // Each pass loads one complete missing scope. This also bounds paths
        // that repeatedly traverse between different workspaces.
        for _ in 0..128 {
            if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
                return Err(errors::error(
                    "LongRunningOperationCancelled",
                    "Operation cancelled",
                ));
            }
            let (resolved, needed) = {
                let needed = RefCell::new(None);
                let lookup = |request: LookupRequest<'_>| {
                    let scope = scope(request);
                    if !self.loaded.contains(&scope) {
                        needed.replace(Some((scope, Arc::clone(request.parent))));
                        return Err(ContextError::lookup(
                            "SnapshotRequired",
                            "Metadata required",
                        ));
                    }
                    self.lookup(request)
                };
                let resolver = Resolver::new(&lookup, Arc::clone(current), session.home.clone());
                let resolved = resolver.resolve(PathInput::Scalar(path), ResolveOptions::default());
                (resolved, needed.into_inner())
            };
            let Some((scope, parent)) = needed else {
                return resolved.map_err(|e| errors::error(&e.code, e.message));
            };
            self.load(session, scope, parent, cancelled.as_mut())
                .await?;
        }
        Err(errors::unsupported(
            "Read path exceeds the command metadata-scope bound",
        ))
    }

    async fn load(
        &mut self,
        session: &ReadSession<'_>,
        scope: Scope,
        parent: Arc<Element>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<(), CommandError> {
        match &scope {
            Scope::Workspaces => {
                let list = session
                    .sdk
                    .list_workspaces(session.executor, WorkspaceListLimits::default(), cancelled)
                    .await
                    .map_err(|e| errors::workspace(&e.kind))?;
                for workspace in list.value {
                    let kind = match workspace.workspace_type.to_ascii_lowercase().as_str() {
                        "workspace" => WorkspaceType::Workspace,
                        "personal" => WorkspaceType::Personal,
                        _ => continue,
                    };
                    self.elements.push(
                        Element::child(
                            &workspace.display_name,
                            Some(workspace.id),
                            NodeKind::Workspace(kind),
                            Arc::clone(&parent),
                        )
                        .map_err(|e| errors::error(&e.code, e.message))?,
                    );
                }
            }
            Scope::Items(id) => {
                let list = session
                    .sdk
                    .list_items(session.executor, id, ItemListOptions::default(), cancelled)
                    .await
                    .map_err(|e| errors::item(&e.kind))?;
                for item in list.value {
                    if !item.additional_fields.contains_key("type") {
                        return Err(errors::error(
                            "InvalidResponse",
                            "Fabric returned an item without its type",
                        ));
                    }
                    let Some(kind) = item.item_type().and_then(|s| ItemType::parse(s).ok()) else {
                        continue;
                    };
                    self.elements.push(
                        Element::child(
                            &item.display_name,
                            Some(item.id),
                            NodeKind::Item(kind),
                            Arc::clone(&parent),
                        )
                        .map_err(|e| errors::error(&e.code, e.message))?,
                    );
                }
            }
            Scope::Folders(id) => {
                let list = session
                    .sdk
                    .list_folders(
                        session.executor,
                        id,
                        FolderListOptions::default(),
                        cancelled,
                    )
                    .await
                    .map_err(|e| folder_error(&e.kind))?;
                self.folders.insert(
                    id.clone(),
                    FolderGraph::new(list.value).map_err(|e| e.command_error())?,
                );
            }
        }
        self.loaded.insert(scope);
        Ok(())
    }
}
