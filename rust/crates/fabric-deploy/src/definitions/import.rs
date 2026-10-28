// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimits, LocalDefinitionFileSystem,
    codec::parse_json,
    local::{directory_chain, regular},
    validate_id, validate_part_path,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::json_dumps;
use fabric_sdk_operations::{FabricOperation, OperationExecutor, OperationSession};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fmt, fs,
    io::{self, Read},
    path::Path,
};

/// One local directory entry, not an ambient filesystem discovery result.
#[derive(Clone, Debug)]
pub struct DefinitionEntry {
    /// A single portable component, relative to the requested directory.
    pub name: String,
    /// Explicit regular file/directory classification.
    pub kind: DefinitionEntryKind,
}

/// Supported stable local import entry types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionEntryKind {
    /// Regular file, never a symlink/reparse/special object.
    File,
    /// Regular directory, never a symlink/reparse object.
    Directory,
}

/// Caller-injected local import reads, performed before asynchronous service work.
///
/// Implementations must reject links/reparse/special files, bound directory
/// enumeration and read allocation, and never silently skip inaccessible files.
/// `relative` is empty for the explicit root and otherwise portable/validated.
pub trait DefinitionImportFileSystem {
    /// List regular immediate entries without recursively following paths.
    fn entries(&mut self, root: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>>;
    /// Read one exact regular file, rejecting allocation beyond `max_bytes`.
    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>>;
}

impl DefinitionImportFileSystem for LocalDefinitionFileSystem {
    fn entries(&mut self, root: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>> {
        directory_chain(root)?;
        let current = if relative.is_empty() {
            root.to_path_buf()
        } else {
            validate_part_path(relative, self.limits).map_err(|_| invalid_path())?;
            root.join(relative)
        };
        directory_chain(&current)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(current)? {
            if entries.len() >= self.limits.max_files.get() {
                return Err(io::Error::other("definition import entry bound"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !regular(&metadata) {
                return Err(invalid_path());
            }
            entries.push(DefinitionEntry {
                name: entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid_path())?,
                kind: if metadata.is_dir() {
                    DefinitionEntryKind::Directory
                } else {
                    DefinitionEntryKind::File
                },
            });
        }
        Ok(entries)
    }

    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        directory_chain(root)?;
        validate_part_path(relative, self.limits).map_err(|_| invalid_path())?;
        let path = root.join(relative);
        directory_chain(path.parent().ok_or_else(invalid_path)?)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !regular(&metadata) || !metadata.is_file() {
            return Err(invalid_path());
        }
        if metadata.len() > max_bytes as u64 {
            return Err(io::Error::other("definition import byte bound"));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(io::Error::other("definition import byte bound"));
        }
        Ok(bytes)
    }
}

fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "unsafe definition import path")
}

/// Caller-resolved destination; no hidden discovery or name reconciliation.
#[derive(Clone, Copy, Debug)]
pub struct ImportTarget<'a> {
    /// Explicit Fabric workspace ID.
    pub workspace_id: &'a str,
    /// Existing item selects update; absent selects create.
    pub item_id: Option<&'a str>,
    /// Exact source-supported item type.
    pub item_type: &'a str,
    /// Caller-resolved context short name.
    pub display_name: &'a str,
    /// Caller-resolved destination folder; null is retained in create bodies.
    pub folder_id: Option<&'a str>,
}

/// Source-selected definition mutation, not a discovery result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportAction {
    /// POST generic items with type/folder/name/definition.
    Create,
    /// POST updateDefinition with only the definition wrapper.
    Update,
}

/// Ordered local definition plus explicitly reported Python cache exclusions.
pub struct ImportDefinition {
    /// Source `parts` array and optional already-resolved wire format.
    pub definition: Value,
    /// Full-path `cache.abf` substring exclusions in traversal order.
    pub excluded_paths: Vec<String>,
}

impl fmt::Debug for ImportDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportDefinition")
            .field("definition", &"<definition content>")
            .field("excluded_paths", &self.excluded_paths)
            .finish()
    }
}

/// Immutable bounded import input, consumed exactly once by execution.
pub struct PreparedImport {
    path: String,
    body: Vec<u8>,
    action: ImportAction,
    item_id: Option<String>,
    excluded_paths: Vec<String>,
}

impl fmt::Debug for PreparedImport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedImport")
            .field("action", &self.action)
            .field("body", &"<definition content>")
            .field("excluded_paths", &self.excluded_paths)
            .finish()
    }
}

/// Completed mutation response; bare acceptance is not an imported item.
#[derive(Debug)]
pub struct ImportOutcome {
    /// Source create versus update.
    pub action: ImportAction,
    /// New response item ID or the caller's existing ID.
    pub item_id: String,
    /// Source-excluded cache paths, never silently dropped.
    pub excluded_paths: Vec<String>,
}

impl PreparedImport {
    /// Read the Environment publish state using the caller's bound SDK session.
    ///
    /// Missing properties retain Python's `Unknown` state. Explicit null,
    /// scalar objects and non-string states fail rather than becoming success.
    /// This performs one read; the caller owns readiness versus completion policy.
    ///
    /// # Errors
    /// Retains validation, structured service, cancellation and operation evidence.
    pub async fn environment_publish_state<E: OperationExecutor>(
        session: &mut OperationSession<'_, E>,
        workspace_id: &str,
        item_id: &str,
    ) -> Result<String, DefinitionError> {
        validate_id(workspace_id)?;
        validate_id(item_id)?;
        let operation = FabricOperation::new(
            ResourceMethod::Get,
            &format!("/v1/workspaces/{workspace_id}/environments/{item_id}"),
            Vec::new(),
            CompletionPolicy::PythonSynchronous,
        )
        .map_err(|kind| Kind::Operation(Box::new(session.evidence(kind))))?;
        let response = session
            .execute(operation)
            .await
            .map_err(|error| Kind::Operation(Box::new(error)))?;
        let invalid = || {
            Kind::Operation(Box::new(
                session.evidence(fabric_sdk_operations::OperationErrorKind::InvalidResponse),
            ))
        };
        let data = super::object(&response.body).map_err(|_| invalid())?;
        let mut current = &data;
        for field in ["properties", "publishDetails"] {
            match current.get(field) {
                None => return Ok("unknown".into()),
                Some(value) => {
                    if !value.is_object() {
                        return Err(invalid().into());
                    }
                    current = value;
                }
            }
        }
        match current.get("state") {
            None => Ok("unknown".into()),
            Some(Value::String(state)) => Ok(state.to_lowercase()),
            Some(_) => Err(invalid().into()),
        }
    }

    /// Trigger staging publish exactly once through the existing SDK LRO policy.
    ///
    /// Use a fresh session after the definition mutation; a used mutation session
    /// is deliberately not reset. Completion of this POST does not establish
    /// Environment publish success: the caller must poll its publish state.
    ///
    /// # Errors
    /// Preserves actual service/accepted-operation errors and mutation uncertainty.
    pub async fn trigger_environment_publish<E: OperationExecutor>(
        session: &mut OperationSession<'_, E>,
        workspace_id: &str,
        item_id: &str,
    ) -> Result<(), DefinitionError> {
        validate_id(workspace_id)?;
        validate_id(item_id)?;
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &format!("/v1/workspaces/{workspace_id}/environments/{item_id}/staging/publish"),
            Vec::new(),
            CompletionPolicy::PythonSynchronous,
        )
        .map_err(|kind| Kind::Operation(Box::new(session.evidence(kind))))?;
        let response = session
            .execute(operation)
            .await
            .map_err(|error| Kind::Operation(Box::new(error)))?;
        if response.status == 202 {
            return Err(Kind::Operation(Box::new(
                session.evidence(fabric_sdk_operations::OperationErrorKind::InvalidResponse),
            ))
            .into());
        }
        Ok(())
    }

    /// Prepare Environment definition bytes for a caller-owned publish lifecycle.
    ///
    /// Unlike [`prepare_import`], which retains its original standalone generic
    /// contract, this entry point permits Environment definitions. The caller
    /// must check publish readiness before updating, execute the prepared item
    /// mutation once, then publish staging and wait for its terminal state.
    /// Preparation alone does not upload, publish, or claim command completion.
    ///
    /// # Errors
    /// Rejects non-Environment targets and retains the generic loader's complete
    /// validation, confirmation, raw-byte, ordering and finite-bound errors.
    pub fn prepare_environment(
        filesystem: &mut dyn DefinitionImportFileSystem,
        root: &Path,
        target: ImportTarget<'_>,
        selector: Option<&str>,
        confirmed: bool,
        limits: DefinitionLimits,
    ) -> Result<Self, DefinitionError> {
        if target.item_type != "Environment" {
            return Err(Kind::InvalidInput.into());
        }
        prepare_import_inner(filesystem, root, target, selector, confirmed, limits, true)
    }

    /// Exact default Python-ASCII-spaced request bytes, useful for callers/tests.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Execute once using the shared session's existing credentials and LRO policy.
    ///
    /// Use a fresh mutation session, not one that already dispatched a definition
    /// POST. No automatic uncertain-mutation retry or cancellation rollback exists.
    ///
    /// # Errors
    /// Preserves shared operation evidence. Accepted/no-result or malformed create
    /// results never become success; update accepts the source completed statuses.
    pub async fn execute<E: OperationExecutor>(
        self,
        session: &mut OperationSession<'_, E>,
    ) -> Result<ImportOutcome, DefinitionError> {
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &self.path,
            self.body,
            CompletionPolicy::PythonSynchronous,
        )
        .map_err(|kind| Kind::Operation(Box::new(session.evidence(kind))))?;
        let response = session
            .execute(operation)
            .await
            .map_err(|error| Kind::Operation(Box::new(error)))?;
        if response.status == 202 {
            return Err(Kind::Operation(Box::new(
                session.evidence(fabric_sdk_operations::OperationErrorKind::InvalidResponse),
            ))
            .into());
        }
        let item_id =
            match self.item_id {
                Some(id) => id,
                None => {
                    if !matches!(response.status, 200 | 201) {
                        return Err(Kind::Operation(Box::new(session.evidence(
                            fabric_sdk_operations::OperationErrorKind::InvalidResponse,
                        )))
                        .into());
                    }
                    let result =
                        super::object(&response.body).map_err(|_| {
                            Kind::Operation(Box::new(session.evidence(
                                fabric_sdk_operations::OperationErrorKind::InvalidResponse,
                            )))
                        })?;
                    let id = result
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| validate_id(id).is_ok())
                        .ok_or_else(|| {
                            Kind::Operation(Box::new(session.evidence(
                                fabric_sdk_operations::OperationErrorKind::InvalidResponse,
                            )))
                        })?;
                    id.to_owned()
                }
            };
        Ok(ImportOutcome {
            action: self.action,
            item_id,
            excluded_paths: self.excluded_paths,
        })
    }
}

/// Read, sort, validate and encode local source files before service work.
///
/// Root files precede sorted depth-first directories, matching Python os.walk.
/// Report byPath validation precedes the full-path cache.abf exclusion. Raw file
/// bytes are base64 encoded without newline normalization or Notebook transforms.
/// The caller owns command-support checks, context discovery, initial/overwrite
/// prompts and cache updates. Environment import fails before reads/mutations
/// until its separate readiness/publish lifecycle is implemented.
///
/// # Errors
/// Invalid IDs/selectors, denied confirmation, unsafe local entries, Report
/// validation, filesystem errors and aggregate input/request bounds fail before
/// credentials or service mutation. No unsupported local files silently vanish.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned preparation boundaries."
)]
pub fn prepare_import(
    filesystem: &mut dyn DefinitionImportFileSystem,
    root: &Path,
    target: ImportTarget<'_>,
    selector: Option<&str>,
    confirmed: bool,
    limits: DefinitionLimits,
) -> Result<PreparedImport, DefinitionError> {
    prepare_import_inner(filesystem, root, target, selector, confirmed, limits, false)
}

#[allow(
    clippy::too_many_arguments,
    reason = "The lifecycle opt-in preserves the existing generic preparation API."
)]
fn prepare_import_inner(
    filesystem: &mut dyn DefinitionImportFileSystem,
    root: &Path,
    target: ImportTarget<'_>,
    selector: Option<&str>,
    confirmed: bool,
    limits: DefinitionLimits,
    environment_lifecycle: bool,
) -> Result<PreparedImport, DefinitionError> {
    if !confirmed {
        return Err(Kind::ConfirmationRequired.into());
    }
    validate_id(target.workspace_id)?;
    for id in [target.item_id, target.folder_id].into_iter().flatten() {
        validate_id(id)?;
    }
    if target.display_name.is_empty() || target.item_type.is_empty() {
        return Err(Kind::InvalidInput.into());
    }
    if target.item_type == "Environment" && !environment_lifecycle {
        return Err(Kind::Unsupported("Environment readiness/publish lifecycle").into());
    }
    let format = super::resolve_format(target.item_type, selector)?;
    let loaded = load_import_definition(filesystem, root, format, limits)?;
    let definition = loaded.definition;
    let (path, payload, action) = match target.item_id {
        Some(item) => (
            format!(
                "/v1/workspaces/{}/items/{item}/updateDefinition",
                target.workspace_id
            ),
            json!({"definition":definition}),
            ImportAction::Update,
        ),
        None => (
            format!("/v1/workspaces/{}/items", target.workspace_id),
            json!({"type":target.item_type,"folderId":target.folder_id,
                "displayName":target.display_name,"definition":definition}),
            ImportAction::Create,
        ),
    };
    let body = json_dumps(&payload).into_bytes();
    if body.len() > limits.max_bytes.get() {
        return Err(Kind::Limit.into());
    }
    Ok(PreparedImport {
        path,
        body,
        action,
        item_id: target.item_id.map(str::to_owned),
        excluded_paths: loaded.excluded_paths,
    })
}

/// Load the source's deterministic local definition with an already-resolved format.
///
/// This is the pure file-to-definition boundary; the command preparation above
/// separately validates a user selector and builds create/update request shapes.
/// Bytes are never newline-normalized or Notebook-output-cleaned.
///
/// # Errors
/// Rejects nonabsolute roots, unsafe or inaccessible entries, invalid Report
/// references, and explicit traversal/decoded-byte bounds before service work.
pub fn load_import_definition(
    filesystem: &mut dyn DefinitionImportFileSystem,
    root: &Path,
    format: Option<&str>,
    limits: DefinitionLimits,
) -> Result<ImportDefinition, DefinitionError> {
    if !root.is_absolute() {
        return Err(Kind::UnsafePath.into());
    }
    let mut collector = Collector {
        filesystem,
        root,
        limits,
        visited: 0,
        remaining: limits.max_bytes.get(),
        parts: Vec::new(),
        excluded: Vec::new(),
    };
    collector.collect("", 0)?;
    let mut definition = json!({"parts":collector.parts});
    if let Some(format) = format.filter(|format| !format.is_empty()) {
        definition["format"] = json!(format);
    }
    Ok(ImportDefinition {
        definition,
        excluded_paths: collector.excluded,
    })
}

struct Collector<'a> {
    filesystem: &'a mut dyn DefinitionImportFileSystem,
    root: &'a Path,
    limits: DefinitionLimits,
    visited: usize,
    remaining: usize,
    parts: Vec<Value>,
    excluded: Vec<String>,
}

impl Collector<'_> {
    fn collect(&mut self, relative: &str, depth: usize) -> Result<(), DefinitionError> {
        if depth >= self.limits.max_depth.get() {
            return Err(Kind::Limit.into());
        }
        let mut entries = self
            .filesystem
            .entries(self.root, relative)
            .map_err(Kind::FileSystem)?;
        self.visited = self.visited.checked_add(entries.len()).ok_or(Kind::Limit)?;
        if self.visited > self.limits.max_files.get() {
            return Err(Kind::Limit.into());
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut seen = HashSet::new();
        for entry in &entries {
            validate_part_path(&entry.name, self.limits)?;
            if entry.name.contains('/') || !seen.insert(entry.name.to_lowercase()) {
                return Err(Kind::Collision.into());
            }
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.kind == DefinitionEntryKind::File)
        {
            let path = if relative.is_empty() {
                entry.name.clone()
            } else {
                format!("{relative}/{}", entry.name)
            };
            validate_part_path(&path, self.limits)?;
            let full = self.root.join(&path).to_string_lossy().into_owned();
            let report = full.contains("definition.pbir");
            let excluded = full.contains("cache.abf");
            if excluded && !report {
                self.excluded.push(path);
                continue;
            }
            let bytes = self
                .filesystem
                .read(self.root, &path, self.remaining)
                .map_err(Kind::FileSystem)?;
            self.remaining = self.remaining.checked_sub(bytes.len()).ok_or(Kind::Limit)?;
            if report {
                let text = std::str::from_utf8(&bytes).map_err(|_| Kind::InvalidDefinition)?;
                let report = parse_json(text)?.ok_or(Kind::InvalidDefinition)?;
                let report = report.as_object().ok_or(Kind::InvalidDefinition)?;
                if let Some(reference) = report.get("datasetReference") {
                    let reference = reference.as_object().ok_or(Kind::InvalidDefinition)?;
                    if reference
                        .get("byPath")
                        .is_some_and(|value| !value.is_null())
                    {
                        return Err(
                            Kind::Unsupported("Report byPath; switch to byConnection").into()
                        );
                    }
                }
            }
            if excluded {
                self.excluded.push(path);
            } else {
                self.parts.push(json!({"path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64"}));
            }
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.kind == DefinitionEntryKind::Directory)
        {
            let path = if relative.is_empty() {
                entry.name.clone()
            } else {
                format!("{relative}/{}", entry.name)
            };
            validate_part_path(&path, self.limits)?;
            self.collect(&path, depth + 1)?;
        }
        Ok(())
    }
}
