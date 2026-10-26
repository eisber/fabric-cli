// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionFileSystem, DefinitionLimits,
    decode_export_files, execute_body, object, validate_id, validate_part_path, write_export_files,
};
use fabric_core_json::json_dumps;
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// Exact outcomes of the caller's existing command-support check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandSupport {
    /// Source support check returned true.
    Supported,
    /// Source support check returned false.
    Unsupported,
    /// Source support check raised its Fabric command-support exception.
    Rejected,
}

/// One caller-owned cached item, in source discovery/traversal order.
#[derive(Clone, Debug)]
pub struct BulkItem {
    /// Fabric item ID.
    pub id: String,
    /// Exact source type for caller summaries; not inferred from a path suffix.
    pub item_type: String,
    /// Result of the shared source command-support policy.
    pub support: CommandSupport,
}

/// Python selects All for workspaces and Selective for recursively resolved folders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BulkScope {
    /// Send empty items with All; filter returned index against known workspace items.
    Workspace,
    /// Send supported item IDs in source recursive traversal order.
    Folder,
}

/// Caller-resolved complete immutable metadata snapshot.
///
/// The CLI/cache/context owner must finish discovery before invoking this API.
/// For folders it supplies the source depth-first item order, not a fabricated
/// flattened listing. This module owns actual definition HTTP/filter/write
/// algorithms, not another metadata cache or folder resolver.
#[derive(Clone, Copy, Debug)]
pub struct BulkExportSnapshot<'a> {
    /// Explicit Fabric workspace ID.
    pub workspace_id: &'a str,
    /// Source path with workspace and `.Folder` suffixes, without outer slashes.
    pub from_path: &'a str,
    /// Workspace versus recursive folder policy.
    pub scope: BulkScope,
    /// Complete cached workspace items or recursively resolved folder items.
    pub items: &'a [BulkItem],
}

/// Source counts and local completion evidence, never just service acceptance.
#[derive(Debug)]
pub struct BulkExportOutcome {
    /// IDs counted exported, in source response-index/folder-selection order.
    pub supported_ids: Vec<String>,
    /// Known IDs counted unsupported/missing, in source order.
    pub skipped_ids: Vec<String>,
    /// Fully written relative files in service part order.
    pub completed_paths: Vec<String>,
}

/// Build the exact source default-JSON request body for bulk definition export.
///
/// # Errors
/// Rejects unsafe IDs or explicit serialized-body bounds before a request.
pub fn bulk_export_payload(
    ids: &[String],
    limits: DefinitionLimits,
) -> Result<Vec<u8>, DefinitionError> {
    for id in ids {
        validate_id(id)?;
    }
    if ids.len() > limits.max_files.get() {
        return Err(Kind::Limit.into());
    }
    let items: Vec<_> = ids.iter().map(|id| json!({"id":id})).collect();
    let body =
        json_dumps(&json!({"items":items,"mode":if ids.is_empty() {"All"} else {"Selective"}}))
            .into_bytes();
    if body.len() > limits.max_bytes.get() {
        return Err(Kind::Limit.into());
    }
    Ok(body)
}

/// Apply Python workspace index matching and part-prefix filtering.
///
/// Unknown returned index items are ignored. Known items missing from the index
/// count as unsupported even if their type supports export. A sibling with the
/// same textual prefix is not part of an item: membership requires prefix + `/`.
/// Folder selection has already occurred and does not filter its response index.
///
/// # Errors
/// Malformed response shapes and explicit bounds fail, not empty success.
pub fn filter_bulk_definition(
    snapshot: BulkExportSnapshot<'_>,
    response: &mut Value,
    limits: DefinitionLimits,
) -> Result<(Vec<String>, Vec<String>), DefinitionError> {
    validate_snapshot(snapshot, limits)?;
    let mut supported = Vec::new();
    let mut skipped = Vec::new();
    if snapshot.scope == BulkScope::Folder {
        for item in snapshot.items {
            match item.support {
                CommandSupport::Supported => supported.push(item.id.clone()),
                CommandSupport::Rejected => skipped.push(item.id.clone()),
                // The source folder collector omits a false return entirely.
                CommandSupport::Unsupported => {}
            }
        }
    } else {
        let by_id: HashMap<_, _> = snapshot
            .items
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let index = array_or_empty(response, "itemDefinitionsIndex")?;
        if index.len() > limits.max_files.get() {
            return Err(Kind::Limit.into());
        }
        let mut prefixes = HashSet::new();
        for entry in index {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
            let Some(item) = by_id.get(id) else {
                continue;
            };
            if item.support == CommandSupport::Supported {
                let prefix = entry.get("rootPath").and_then(Value::as_str).unwrap_or("");
                prefixes.insert(prefix.to_owned());
                supported.push(id.to_owned());
            } else {
                skipped.push(id.to_owned());
            }
        }
        let known: HashSet<_> = supported.iter().chain(&skipped).cloned().collect();
        skipped.extend(
            snapshot
                .items
                .iter()
                .filter(|item| !known.contains(&item.id))
                .map(|item| item.id.clone()),
        );
        let parts = array_or_empty(response, "definitionParts")?;
        if parts.len() > limits.max_files.get() {
            return Err(Kind::Limit.into());
        }
        let parts: Vec<_> = parts
            .into_iter()
            .filter(|part| {
                let path = part.get("path").and_then(Value::as_str).unwrap_or("");
                prefixes
                    .iter()
                    .any(|prefix| path.starts_with(&format!("{prefix}/")))
            })
            .collect();
        response
            .as_object_mut()
            .ok_or(Kind::InvalidDefinition)?
            .insert("definitionParts".into(), Value::Array(parts));
    }
    if supported.is_empty() {
        return Err(Kind::Unsupported("bulk target has no exportable items").into());
    }
    Ok((supported, skipped))
}

/// Strip only ancestors above the source bulk target, retaining the target folder.
///
/// # Errors
/// Parts outside the required parent prefix fail before local writes.
pub fn strip_bulk_parent_prefix(
    response: &mut Value,
    from_path: &str,
) -> Result<(), DefinitionError> {
    let Some((_, path)) = from_path.split_once('/') else {
        return Ok(());
    };
    let Some((parent, _)) = path.rsplit_once('/') else {
        return Ok(());
    };
    if parent.is_empty() {
        return Ok(());
    }
    let prefix = format!(
        "/{}/",
        parent
            .split('/')
            .map(|part| part.strip_suffix(".Folder").unwrap_or(part))
            .collect::<Vec<_>>()
            .join("/")
    );
    if let Some(parts) = response.get_mut("definitionParts") {
        for part in parts.as_array_mut().ok_or(Kind::InvalidDefinition)? {
            let path = part.get("path").and_then(Value::as_str).unwrap_or("");
            let path = path.strip_prefix(&prefix).ok_or(Kind::UnsafePath)?;
            let path = format!("/{path}");
            part.as_object_mut()
                .ok_or(Kind::InvalidDefinition)?
                .insert("path".into(), json!(path));
        }
    }
    Ok(())
}

/// Execute real bulk definition export using one shared operation and file policy.
///
/// The caller resolves complete metadata/support snapshots and supplies the exact
/// source recursive/nonempty-output/sensitivity-label confirmations. No additional
/// generic overwrite prompt is invented. Workspace All and folder Selective are
/// distinct. Bulk export never applies single-Notebook output cleanup.
///
/// # Errors
/// Invalid/empty targets, missing recursive confirmation, service/LRO failures,
/// unsafe response paths and partial filesystem failures retain explicit evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit source and caller-owned boundaries."
)]
pub async fn bulk_export<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    snapshot: BulkExportSnapshot<'_>,
    root: &Path,
    recursive: bool,
    confirmed: bool,
    limits: DefinitionLimits,
) -> Result<BulkExportOutcome, DefinitionError> {
    if !recursive {
        return Err(Kind::InvalidInput.into());
    }
    if !confirmed {
        return Err(Kind::ConfirmationRequired.into());
    }
    validate_snapshot(snapshot, limits)?;
    let ids = if snapshot.scope == BulkScope::Folder {
        let ids: Vec<_> = snapshot
            .items
            .iter()
            .filter(|item| item.support == CommandSupport::Supported)
            .map(|item| item.id.clone())
            .collect();
        if ids.is_empty() {
            return Err(Kind::Unsupported("bulk folder has no exportable items").into());
        }
        ids
    } else {
        Vec::new()
    };
    let body = bulk_export_payload(&ids, limits)?;
    let path = format!(
        "/v1/workspaces/{}/items/bulkExportDefinitions?beta=true",
        snapshot.workspace_id
    );
    let mut response = object(&execute_body(session, ResourceMethod::Post, &path, body).await?)?;
    let (supported_ids, skipped_ids) = filter_bulk_definition(snapshot, &mut response, limits)?;
    strip_bulk_parent_prefix(&mut response, snapshot.from_path)?;
    let parts = array_or_empty(&response, "definitionParts")?;
    let files = decode_export_files(&json!({"definition":{"parts":parts}}), false, limits)?;
    let completed_paths = write_export_files(session, filesystem, root, &files, None).await?;
    Ok(BulkExportOutcome {
        supported_ids,
        skipped_ids,
        completed_paths,
    })
}

fn validate_snapshot(
    snapshot: BulkExportSnapshot<'_>,
    limits: DefinitionLimits,
) -> Result<(), DefinitionError> {
    validate_id(snapshot.workspace_id)?;
    validate_part_path(snapshot.from_path, limits)?;
    if snapshot.items.is_empty() {
        return Err(Kind::InvalidInput.into());
    }
    if snapshot.items.len() > limits.max_files.get() {
        return Err(Kind::Limit.into());
    }
    for item in snapshot.items {
        validate_id(&item.id)?;
    }
    Ok(())
}

fn array_or_empty(response: &Value, name: &str) -> Result<Vec<Value>, DefinitionError> {
    let response = response.as_object().ok_or(Kind::InvalidDefinition)?;
    match response.get(name) {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_array()
            .cloned()
            .ok_or_else(|| Kind::InvalidDefinition.into()),
    }
}
