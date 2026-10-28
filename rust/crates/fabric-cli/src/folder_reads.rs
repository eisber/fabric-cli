// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-composed folder presentation over the existing read host and SDK.

use crate::{
    ItemSort, ReadHost, WriteError, errors,
    parser::{ReadCommand, ReadKind},
    reads::{enum_name, required_id},
};
use fabric_cli_context::{
    Element, LookupKind, LookupRequest, NodeKind, PathInput, ResolveOptions, Resolver,
};
use fabric_cli_output::{
    CommandError, HiddenItems, Indent, OutputEnvelope, OutputError, OutputInput, RenderPlan,
};
use fabric_sdk::{
    folders::{
        Folder, FolderErrorKind, FolderExecutionError, FolderExecutor, FolderListLimits,
        FolderListOptions,
    },
    items::{ItemListLimits, ItemListOptions},
};
use serde_json::{Value, json};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    future::{Future, poll_fn, ready},
    io::Write,
    pin::pin,
    sync::Arc,
    task::Poll,
};

/// Additive folder-aware LS composition, borrowing every existing host policy.
///
/// Non-folder reads delegate to `ReadHost`. Folder paths must already be present
/// in its immutable lookup snapshot; this wrapper never performs HTTP in lookup.
/// Each command uses one complete folder snapshot, not an ambient/TTL cache.
pub struct FolderReadHost<'a, 'host> {
    base: &'a ReadHost<'host>,
    folders: &'a dyn FolderExecutor,
    item_limits: ItemListLimits,
    folder_limits: FolderListLimits,
    scalar_query: bool,
}

impl<'host> ReadHost<'host> {
    /// Borrow this host and a folder executor for folder-aware workspace/folder LS.
    ///
    /// Existing struct literals and all non-folder command behavior are unchanged.
    /// A shared `fabric_sdk_http::HttpExecutor` can implement all four executors.
    pub fn with_folders<'a>(
        &'a self,
        folders: &'a dyn FolderExecutor,
    ) -> FolderReadHost<'a, 'host> {
        FolderReadHost {
            base: self,
            folders,
            item_limits: ItemListLimits::default(),
            folder_limits: FolderListLimits::default(),
            scalar_query: false,
        }
    }
}

impl FolderReadHost<'_, '_> {
    /// Select Python LS/dir scalar-query truthiness for [`crate::parser::parse_args`].
    ///
    /// An empty scalar uses ordinary columns, like an absent query. The default
    /// retains the existing handler-list contract: a present joined empty query
    /// invokes path inventory. Nonempty queries and folder-context query-ignore
    /// behavior are unchanged; the command itself is never rewritten.
    pub fn with_scalar_query(mut self) -> Self {
        self.scalar_query = true;
        self
    }

    /// Apply explicit SDK traversal/body bounds without changing host configuration.
    pub fn with_limits(mut self, items: ItemListLimits, folders: FolderListLimits) -> Self {
        self.item_limits = items;
        self.folder_limits = folders;
        self
    }

    /// Dispatch folder LS, delegating every other branch to the original host.
    ///
    /// Invalid graphs fail explicitly with `NotSupported` and a safety category;
    /// partial SDK rows are never emitted. Python's nonprogress loop on orphan,
    /// null-parent and cyclic graphs is not reproduced.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RenderPlan, OutputError> {
        match self.execute(command, cancelled).await {
            Ok(Some(input)) => {
                OutputEnvelope::new(self.base.timestamp.clone(), input).render(Indent::Four)
            }
            Ok(None) => Ok(RenderPlan {
                stream: fabric_cli_output::OutputStream::Stdout,
                exit_code: 0,
                payload: Vec::new(),
            }),
            Err(error) => error
                .envelope(self.base.timestamp.clone(), Some(command.name.clone()))
                .render(Indent::Four),
        }
    }

    pub(crate) async fn execute(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<OutputInput>, CommandError> {
        self.execute_with_source_operations(command, None, cancelled)
            .await
    }

    pub(crate) async fn execute_with_source_operations(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn fabric_sdk_operations::OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<OutputInput>, CommandError> {
        let mut cancelled = pin!(cancelled);
        if command.kind != ReadKind::List
            || command
                .output_format
                .as_deref()
                .is_some_and(|format| format != "json")
            || command.output.is_some()
            || command.force
            || command.verbose
        {
            return self
                .base
                .execute_with_source_operations(command, operations, cancelled)
                .await;
        }
        // Preserve cancellation priority, without polling a completed future again.
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return self
                .base
                .execute_with_source_operations(command, operations, ready(()))
                .await;
        }
        let tokens: Vec<_> = command.path.iter().map(String::as_str).collect();
        let resolver = Resolver::new(
            self.base.lookup,
            Arc::clone(&self.base.current),
            self.base.home.clone(),
        );
        let target = resolver.resolve(PathInput::Tokens(&tokens), ResolveOptions::default());
        let target = match target {
            Ok(target)
                if target.kind() == NodeKind::Folder
                    || (matches!(target.kind(), NodeKind::Workspace(_))
                        && self.base.config.folder_listing_enabled) =>
            {
                target
            }
            _ => {
                return self
                    .base
                    .execute_with_source_operations(command, operations, cancelled)
                    .await;
            }
        };
        self.list(command, target, cancelled).await.map(Some)
    }

    /// Dispatch and write using the base host's timestamp and caller-owned streams.
    ///
    /// Stream failures retain the original I/O error; no flush or retry is added.
    pub async fn run(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, WriteError> {
        let plan = self.dispatch(command, cancelled).await?;
        plan.write_to(stdout, stderr)?;
        Ok(plan.exit_code)
    }

    async fn graph(
        &self,
        workspace: &Element,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<FolderGraph, CommandError> {
        let folders = self
            .base
            .sdk
            .list_folders(
                self.folders,
                required_id(workspace)?,
                FolderListOptions {
                    limits: self.folder_limits,
                    ..FolderListOptions::default()
                },
                cancelled,
            )
            .await
            .map_err(|error| folder_error(&error.kind))?;
        FolderGraph::new(folders.value).map_err(GraphFailure::command_error)
    }

    async fn list(
        &self,
        command: &ReadCommand,
        target: Arc<Element>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OutputInput, CommandError> {
        let mut cancelled = pin!(cancelled);
        let workspace = target
            .workspace()
            .expect("validated workspace/folder context");
        let items = self
            .base
            .sdk
            .list_items(
                self.base.items,
                required_id(&workspace)?,
                ItemListOptions {
                    limits: self.item_limits,
                    ..ItemListOptions::default()
                },
                cancelled.as_mut(),
            )
            .await
            .map_err(|error| errors::item(&error.kind))?;
        let mut graph = None;
        let mut typed_items = Vec::new();
        for item in items.value {
            // Python evaluates get_folder before accessing/validating item["type"].
            let parent = if let Some(folder_id) = item.additional_fields.get("folderId") {
                if graph.is_none() {
                    graph = Some(self.graph(&workspace, cancelled.as_mut()).await?);
                }
                folder_id
                    .as_str()
                    .and_then(|id| graph.as_ref().expect("loaded graph").first_context(id))
            } else {
                Some(0)
            };
            if !item.additional_fields.contains_key("type") {
                return Err(errors::error(
                    "InvalidResponse",
                    "Fabric returned an item without its type",
                ));
            }
            let Some(kind) = item
                .item_type()
                .and_then(|value| fabric_cli_context::ItemType::parse(value).ok())
            else {
                continue;
            };
            typed_items.push((item, kind, parent));
        }
        let graph = match graph {
            Some(graph) => graph,
            None => self.graph(&workspace, cancelled.as_mut()).await?,
        };
        if typed_items.iter().any(|(_, _, parent)| parent.is_none()) {
            return Err(GraphFailure::ItemParent.command_error());
        }
        let target_context = graph.target_context(&target);
        let mut elements = Vec::new();
        for &index in &graph.order {
            let node = &graph.nodes[index];
            if Some(node.parent) == target_context {
                elements.push((
                    node.folder.display_name.as_str(),
                    "Folder",
                    format!("{}.Folder", node.folder.display_name),
                    node.folder.id.as_str(),
                ));
            }
        }
        for (item, kind, parent) in &typed_items {
            if *parent == target_context {
                elements.push((
                    item.display_name.as_str(),
                    enum_name(*kind),
                    format!("{}.{}", item.display_name, kind.as_str()),
                    item.id.as_str(),
                ));
            }
        }
        elements.sort_by_cached_key(|(name, kind, _, _)| match self.base.config.item_sort {
            ItemSort::ByName => (name.to_lowercase(), (*kind).to_owned()),
            ItemSort::ByType => ((*kind).to_owned(), name.to_lowercase()),
        });
        let rows: Vec<_> = elements
            .into_iter()
            .map(|(_, _, name, id)| {
                if command.long {
                    json!({"name":name, "id":id})
                } else {
                    json!({"name":name})
                }
            })
            .collect();
        let folder_context = target.kind() == NodeKind::Folder;
        // The source folder handler prints directly: it never evaluates --query
        // or emits workspace virtual collections, even under --all.
        let data = if folder_context {
            Value::Array(rows)
        } else if !self.scalar_query && command.query.as_deref() == Some("") {
            // A present Python nargs list is truthy even when its joined query is empty.
            self.base
                .projection
                .project(Value::Array(rows), Some(""), false)?
        } else {
            self.base.list_projection(
                rows,
                if command.long {
                    &["name", "id"]
                } else {
                    &["name"]
                },
                command,
            )?
        };
        let hidden_data =
            (!folder_context && (command.all || self.base.config.show_hidden)).then(|| {
                HiddenItems::Strings(vec![
                    ".sparkpools".into(),
                    ".managedidentities".into(),
                    ".managedprivateendpoints".into(),
                    ".externaldatashares".into(),
                ])
            });
        Ok(OutputInput {
            command: Some(command.name.clone()),
            data: Some(data),
            hidden_data,
            ..OutputInput::default()
        })
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum GraphFailure {
    #[error("Folder graph is unsupported (NullParent safety boundary)")]
    NullParent,
    #[error("Folder graph is unsupported (NonStringParent safety boundary)")]
    NonStringParent,
    #[error("Folder graph is unsupported (UnresolvedParent safety boundary)")]
    UnresolvedParent,
    #[error("Folder graph is unsupported (Cycle safety boundary)")]
    Cycle,
    #[error("Folder item parent is unsupported (ItemParent safety boundary)")]
    ItemParent,
}

impl GraphFailure {
    pub(crate) fn command_error(self) -> CommandError {
        errors::unsupported(&self.to_string())
    }
}

struct FolderNode {
    folder: Folder,
    parent: usize,
    context: usize,
}

// Compact, interned parent contexts avoid recursive equality/drop and repeated
// ancestor scans. Roots are context zero; equal source Folder objects share a key.
pub(crate) struct FolderGraph {
    nodes: Vec<FolderNode>,
    order: Vec<usize>,
    first_by_id: BTreeMap<String, usize>,
    contexts: BTreeMap<(usize, String, String), usize>,
}

impl FolderGraph {
    /// Materialize the validated source-ordered graph for transfer snapshots.
    #[cfg(feature = "native-read")]
    pub(crate) fn elements(
        &self,
        workspace: Arc<Element>,
    ) -> Result<Vec<Arc<Element>>, fabric_cli_context::ContextError> {
        let mut contexts = BTreeMap::from([(0, workspace)]);
        let mut depths = BTreeMap::from([(0, 0usize)]);
        let max_depth = fabric_deploy::definitions::DefinitionLimits::default()
            .max_depth
            .get();
        let mut elements = Vec::with_capacity(self.order.len());
        for &index in &self.order {
            let node = &self.nodes[index];
            let depth = depths[&node.parent] + 1;
            if depth > max_depth {
                return Err(fabric_cli_context::ContextError::lookup(
                    "Limit",
                    "Folder transfer depth bound reached",
                ));
            }
            let parent = contexts[&node.parent].clone();
            let element = Element::child(
                &node.folder.display_name,
                Some(node.folder.id.clone()),
                NodeKind::Folder,
                parent,
            )?;
            contexts
                .entry(node.context)
                .or_insert_with(|| element.clone());
            depths.entry(node.context).or_insert(depth);
            elements.push(element);
        }
        Ok(elements)
    }

    pub(crate) fn new(folders: Vec<Folder>) -> Result<Self, GraphFailure> {
        let mut graph = Self {
            nodes: folders
                .into_iter()
                .map(|folder| FolderNode {
                    folder,
                    parent: 0,
                    context: 0,
                })
                .collect(),
            order: Vec::new(),
            first_by_id: BTreeMap::new(),
            contexts: BTreeMap::new(),
        };
        let mut waiting: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut roots = Vec::new();
        for (index, node) in graph.nodes.iter().enumerate() {
            match node.folder.parent_folder_id() {
                None => roots.push(index),
                Some(Value::String(parent)) => {
                    waiting.entry(parent.clone()).or_default().push(index)
                }
                Some(Value::Null) => return Err(GraphFailure::NullParent),
                Some(_) => return Err(GraphFailure::NonStringParent),
            }
        }
        let mut ready = BinaryHeap::new();
        for index in roots {
            graph.append(index, 0);
        }
        for id in graph.first_by_id.keys() {
            if let Some(children) = waiting.remove(id) {
                for child in children {
                    ready.push(Reverse((0, child)));
                }
            }
        }
        while let Some(Reverse((pass, index))) = ready.pop() {
            let parent_id = graph.nodes[index]
                .folder
                .parent_folder_id()
                .and_then(Value::as_str)
                .expect("validated non-root parent");
            let parent = graph.first_by_id[parent_id];
            let id = graph.nodes[index].folder.id.clone();
            let first = !graph.first_by_id.contains_key(&id);
            graph.append(index, parent);
            if first && let Some(children) = waiting.remove(&id) {
                for child in children {
                    // Python's in-place list-comprehension passes can discover
                    // later children immediately, but earlier ones on the next pass.
                    ready.push(Reverse((pass + usize::from(child < index), child)));
                }
            }
        }
        if graph.order.len() != graph.nodes.len() {
            let all_ids: std::collections::BTreeSet<_> = graph
                .nodes
                .iter()
                .map(|node| node.folder.id.as_str())
                .collect();
            return Err(if waiting.keys().any(|id| !all_ids.contains(id.as_str())) {
                GraphFailure::UnresolvedParent
            } else {
                GraphFailure::Cycle
            });
        }
        Ok(graph)
    }

    pub(crate) fn lookup(&self, request: LookupRequest<'_>) -> Option<String> {
        let context = if request.kind == LookupKind::NestedFolder {
            self.target_context(request.parent)?
        } else {
            0
        };
        self.order.iter().find_map(|&index| {
            let node = &self.nodes[index];
            let name = format!("{}.Folder", node.folder.display_name);
            (name == request.name.trim_matches('/')
                && (request.kind == LookupKind::Folder || node.parent == context))
                .then(|| node.folder.id.clone())
        })
    }

    fn append(&mut self, index: usize, parent: usize) {
        let node = &mut self.nodes[index];
        node.parent = parent;
        let next = self.contexts.len() + 1;
        node.context = *self
            .contexts
            .entry((
                parent,
                node.folder.display_name.clone(),
                node.folder.id.clone(),
            ))
            .or_insert(next);
        self.first_by_id
            .entry(node.folder.id.clone())
            .or_insert(node.context);
        self.order.push(index);
    }

    fn first_context(&self, id: &str) -> Option<usize> {
        self.first_by_id.get(id).copied()
    }

    fn target_context(&self, target: &Arc<Element>) -> Option<usize> {
        let mut ancestors = Vec::new();
        let mut current = target.as_ref();
        while current.kind() == NodeKind::Folder {
            ancestors.push(current);
            current = current.parent()?.as_ref();
        }
        let mut context = 0;
        for folder in ancestors.into_iter().rev() {
            context = *self.contexts.get(&(
                context,
                folder.short_name().to_owned(),
                folder.id()?.to_owned(),
            ))?;
        }
        Some(context)
    }
}

pub(crate) fn folder_error(kind: &FolderErrorKind) -> CommandError {
    match kind {
        FolderErrorKind::Http {
            status,
            code,
            service_error,
            ..
        } => errors::http(*status, code, service_error.as_deref()),
        FolderErrorKind::Cancelled
        | FolderErrorKind::Execution(FolderExecutionError::Cancelled) => {
            errors::error("LongRunningOperationCancelled", "Operation cancelled")
        }
        FolderErrorKind::Credential => errors::error("Unauthorized", "Access is unauthorized"),
        FolderErrorKind::InvalidResponse => errors::error(
            "InvalidResponse",
            "Fabric returned an invalid folder response",
        ),
        FolderErrorKind::InvalidIdentifier => {
            errors::error("InvalidInput", "Invalid folder request identifier")
        }
        _ => errors::error("ReadFailed", "Folder operation did not complete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, parent: Option<&str>) -> Folder {
        let mut fields = BTreeMap::new();
        if let Some(parent) = parent {
            fields.insert("parentFolderId".into(), json!(parent));
        }
        Folder {
            id: id.into(),
            display_name: id.into(),
            additional_fields: fields,
        }
    }

    #[test]
    fn scheduler_preserves_root_first_and_in_pass_discovery_order() {
        let graph = FolderGraph::new(vec![
            folder("early", Some("p")),
            folder("p", Some("r")),
            folder("r", None),
            folder("late", Some("p")),
            folder("other", None),
        ])
        .unwrap();
        assert_eq!(graph.order, [2, 4, 1, 3, 0]);
    }

    #[test]
    fn reversed_deep_graph_is_bounded_without_recursive_equality_or_drop() {
        let count = 20_000;
        let folders = (0..count)
            .rev()
            .map(|index| {
                folder(
                    &index.to_string(),
                    (index > 0).then(|| (index - 1).to_string()).as_deref(),
                )
            })
            .collect();
        let graph = FolderGraph::new(folders).unwrap();
        assert_eq!(graph.order.len(), count);
        assert_eq!(graph.contexts.len(), count);
        assert_eq!(graph.first_context("19999"), Some(count));
    }
}
