// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ordered definition reads and local export over caller-owned SDK and file I/O.
//!
//! The caller resolves context, confirms the sensitivity-label warning, supplies
//! an existing local output directory, and injects credentials, execution and
//! cancellation through the shared operation session. No runtime, prompt, global
//! configuration, credential provider or HTTP/polling implementation lives here.

mod bulk;
mod codec;
mod get_value;
mod import;
mod local;
mod source_codec;
mod transfer;

pub use bulk::{
    BulkExportOutcome, BulkExportSnapshot, BulkItem, BulkScope, CommandSupport, bulk_export,
    bulk_export_payload, bulk_export_source, filter_bulk_definition, strip_bulk_parent_prefix,
};
pub use codec::{
    DefinitionFile, ExportStorage, decode_export_files, decode_export_files_for_storage,
    validate_part_path,
};
pub use get_value::{
    GetDefinitionPreprocess, GetDefinitionValue, GetDefinitionValueError,
    decode_get_definition_value,
};
pub use import::{
    DefinitionEntry, DefinitionEntryKind, DefinitionImportFileSystem, ImportAction,
    ImportDefinition, ImportOutcome, ImportTarget, PreparedImport, load_import_definition,
    prepare_import,
};
pub use local::LocalDefinitionFileSystem;
pub use transfer::{
    copy_payload, decode_set_definition, encode_set_definition, ensure_notebook_dependency,
};

use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutor, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::{fmt, num::NonZeroUsize, path::Path};

/// Finite decoded-file and serialized-output limits for one export.
#[derive(Clone, Copy, Debug)]
pub struct DefinitionLimits {
    /// Aggregate decoded and formatted output bytes.
    pub max_bytes: NonZeroUsize,
    /// Maximum parts.
    pub max_files: NonZeroUsize,
    /// Maximum portable path bytes.
    pub max_path_bytes: NonZeroUsize,
    /// Maximum portable path components.
    pub max_depth: NonZeroUsize,
}

impl Default for DefinitionLimits {
    fn default() -> Self {
        Self {
            max_bytes: NonZeroUsize::new(10 * 1024 * 1024).unwrap(),
            max_files: NonZeroUsize::new(4096).unwrap(),
            max_path_bytes: NonZeroUsize::new(1024).unwrap(),
            max_depth: NonZeroUsize::new(32).unwrap(),
        }
    }
}

/// Explicit generic item endpoint identifiers; no metadata discovery occurs here.
#[derive(Clone, Copy, Debug)]
pub struct DefinitionTarget<'a> {
    /// Fabric workspace ID.
    pub workspace_id: &'a str,
    /// Fabric item ID.
    pub item_id: &'a str,
    /// Exact source item type, used only for format selection and Notebook cleanup.
    pub item_type: &'a str,
}

impl DefinitionTarget<'_> {
    fn path(&self) -> Result<String, DefinitionErrorKind> {
        for id in [self.workspace_id, self.item_id] {
            validate_id(id)?;
        }
        Ok(format!(
            "/v1/workspaces/{}/items/{}",
            self.workspace_id, self.item_id
        ))
    }
}

fn validate_id(id: &str) -> Result<(), DefinitionErrorKind> {
    if id.len() != 36
        || !id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(DefinitionErrorKind::InvalidInput);
    }
    Ok(())
}

/// Raw ordered metadata merged with the service definition, without export cleanup.
///
/// Copy/SET consumers must use this raw value, not the locally formatted files.
/// Debug omits definition content.
pub struct DefinitionRead {
    /// Exact ordered service properties; no part decoding or Notebook mutation.
    pub item: Value,
    /// Source metadata-only fallback when definition GET returns 401 or 403.
    pub definition_access_denied: Option<u16>,
}

impl fmt::Debug for DefinitionRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefinitionRead")
            .field("item", &"<definition content>")
            .field("definition_access_denied", &self.definition_access_denied)
            .finish()
    }
}

/// Explicit classification, never a success-shaped empty definition.
#[derive(Debug)]
pub enum DefinitionErrorKind {
    /// Unsafe endpoint, malformed selector or invalid input.
    InvalidInput,
    /// Malformed definition/result JSON.
    InvalidDefinition,
    /// A finite file/path/output bound was reached.
    Limit,
    /// A path escapes the output root or aliases a reserved host path.
    UnsafePath,
    /// Two paths alias, or a file is also a directory.
    Collision,
    /// Source sensitivity-label confirmation was not supplied.
    ConfirmationRequired,
    /// Definition visibility was denied; metadata-only fallback is not an export.
    DefinitionAccessDenied(u16),
    /// A source branch outside this bounded implementation.
    Unsupported(&'static str),
    /// Shared SDK operation failure and reconciliation evidence.
    Operation(Box<OperationError>),
    /// Caller-owned local filesystem failure.
    FileSystem(std::io::Error),
}

/// Export failure retaining durable completed writes and a possibly partial file.
///
/// No completed file is deleted on cancellation/failure. An incomplete write is
/// not claimed complete, and no remote request or local write is replayed.
#[derive(Debug)]
pub struct DefinitionError {
    /// Typed source/safety/failure classification.
    pub kind: DefinitionErrorKind,
    /// Portable relative paths whose writes completed.
    pub completed_paths: Vec<String>,
    /// The write that may have partially changed a file.
    pub incomplete_path: Option<String>,
}

impl From<DefinitionErrorKind> for DefinitionError {
    fn from(kind: DefinitionErrorKind) -> Self {
        Self {
            kind,
            completed_paths: Vec::new(),
            incomplete_path: None,
        }
    }
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Definition transfer failed or is incomplete; inspect classification and file/operation evidence")
    }
}

impl std::error::Error for DefinitionError {}

/// Caller-owned local filesystem policy.
///
/// Implementations must reject symlinks/reparse points, escaping paths, file/
/// directory collisions and hostile aliases before writing. The root is an
/// already-resolved existing output directory. This interface does not discover
/// the current directory/home or grant writes to arbitrary paths. Text bytes are
/// already encoded with native Python newlines; implementations write bytes
/// verbatim, replacing ordinary existing files as the source does.
pub trait DefinitionFileSystem: Send {
    /// Validate the existing root and every destination without making changes.
    fn preflight(&mut self, root: &Path, files: &[DefinitionFile]) -> std::io::Result<()>;
    /// Create the item directory, including when the definition has no parts.
    fn create_directory(&mut self, root: &Path, item_name: &str) -> std::io::Result<()>;
    /// Write one validated relative file; failure may leave partial bytes.
    fn write(&mut self, root: &Path, file: &DefinitionFile) -> std::io::Result<()>;
}

/// Completed local export, not a generic service acceptance flag.
#[derive(Debug)]
pub struct ExportOutcome {
    /// Raw merged definition, suitable for independent copy/update consumers.
    pub read: DefinitionRead,
    /// Fully written relative paths in service part order.
    pub completed_paths: Vec<String>,
}

/// Resolve the pinned Python format selectors, including absent/empty defaults.
///
/// # Errors
/// Rejects a nonempty selector not present in the selected source type's mapping.
pub fn resolve_format(
    item_type: &str,
    selector: Option<&str>,
) -> Result<Option<&'static str>, DefinitionErrorKind> {
    let selected = selector
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    let explicitly_default = selector == Some("default");
    let empty_default = matches!(
        item_type,
        "CosmosDBDatabase"
            | "DigitalTwinBuilder"
            | "DigitalTwinBuilderFlow"
            | "UserDataFunction"
            | "GraphQuerySet"
            | "VariableLibrary"
            | "Map"
            | "Environment"
            | "GraphModel"
            | "Ontology"
    );
    match (item_type, selected) {
        ("Notebook", "default" | ".ipynb") => Ok(Some("ipynb")),
        ("Notebook", ".py") => Ok(Some("fabricGitSource")),
        ("SparkJobDefinition", "default" | "SparkJobDefinitionV1") => {
            Ok(Some("SparkJobDefinitionV1"))
        }
        ("SparkJobDefinition", "SparkJobDefinitionV2") => Ok(Some("SparkJobDefinitionV2")),
        ("SemanticModel", "TMDL") => Ok(Some("TMDL")),
        ("SemanticModel", "TMSL") => Ok(Some("TMSL")),
        ("SemanticModel", "default") => Ok(None),
        (_, "default") if !explicitly_default || empty_default => Ok(None),
        _ => Err(DefinitionErrorKind::InvalidInput),
    }
}

/// Read an undecoded definition using the source default synchronous policy.
///
/// This is the SET seam: unlike export/copy it does not fetch item metadata.
/// An accepted result with no usable definition remains incomplete and fails.
///
/// # Errors
/// Retains shared request/cancellation/operation errors; invalid IDs/formats fail
/// before token acquisition. No response is reserialized into another wire shape.
pub async fn read_definition<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: DefinitionTarget<'_>,
    selector: Option<&str>,
) -> Result<Value, DefinitionError> {
    read_definition_with_format(session, target, resolve_format(target.item_type, selector)?).await
}

async fn read_definition_with_format<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: DefinitionTarget<'_>,
    format: Option<&str>,
) -> Result<Value, DefinitionError> {
    let mut path = format!("{}/getDefinition", target.path()?);
    if let Some(format) = format {
        path.push_str("?format=");
        path.push_str(format);
    }
    let response = execute(session, ResourceMethod::Post, &path).await?;
    object(&response)
}

/// GET metadata then POST getDefinition and merge ordered properties like Python.
///
/// A 401/403 on the definition call returns the source metadata-only result with
/// explicit denial evidence; export subsequently rejects the missing definition.
/// Copy consumers receive untouched base64 and must not use export-cleaned parts.
///
/// # Errors
/// Invalid endpoints/selectors fail before I/O. Other failures preserve SDK
/// evidence, including an accepted operation ID and uncertain dispatch.
pub async fn read_item_definition<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: DefinitionTarget<'_>,
    selector: Option<&str>,
) -> Result<DefinitionRead, DefinitionError> {
    read_item_with_format(session, target, resolve_format(target.item_type, selector)?).await
}

/// Read metadata and raw definition for source copy/move, without a format query.
///
/// Unlike export/SET, Python's copy helper explicitly supplies `args.format=""`
/// to the API rather than resolving the item-type default. Notebook copy must
/// therefore neither request ipynb nor clear cell outputs.
///
/// # Errors
/// Preserves the shared operation/cancellation errors and explicit 401/403
/// metadata-only fallback. Missing definition is rejected by [`copy_payload`].
pub async fn read_copy_definition<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: DefinitionTarget<'_>,
) -> Result<DefinitionRead, DefinitionError> {
    read_item_with_format(session, target, None).await
}

async fn read_item_with_format<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: DefinitionTarget<'_>,
    format: Option<&str>,
) -> Result<DefinitionRead, DefinitionError> {
    let path = target.path()?;
    let mut item = object(&execute(session, ResourceMethod::Get, &path).await?)?;
    match read_definition_with_format(session, target, format).await {
        Ok(definition) => {
            item.as_object_mut()
                .ok_or(DefinitionErrorKind::InvalidDefinition)?
                .extend(
                    definition
                        .as_object()
                        .ok_or(DefinitionErrorKind::InvalidDefinition)?
                        .clone(),
                );
            Ok(DefinitionRead {
                item,
                definition_access_denied: None,
            })
        }
        Err(error) => {
            if let DefinitionErrorKind::Operation(operation) = &error.kind
                && let OperationErrorKind::Http {
                    status: status @ (401 | 403),
                    ..
                } = operation.kind
            {
                return Ok(DefinitionRead {
                    item,
                    definition_access_denied: Some(status),
                });
            }
            Err(error)
        }
    }
}

/// Export one resolved item through real SDK requests and injected local writes.
///
/// The source creates `<output>/<context item name>` and writes in part order;
/// only single-Notebook export clears existing `outputs` in the first part.
/// Caller confirmation must precede any request. Caller filesystem preflight
/// happens after read/decode and before the first write. Cancellation is checked
/// before each filesystem boundary, including after the last write.
///
/// # Errors
/// Preserves exact completed/possibly-partial local progress, explicit unsupported
/// decoding, permission and shared SDK failures. Cancellation never rolls back.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned boundaries, not ambient CLI state."
)]
pub async fn export_item<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    target: DefinitionTarget<'_>,
    item_name: &str,
    root: &Path,
    selector: Option<&str>,
    confirmed: bool,
    limits: DefinitionLimits,
) -> Result<ExportOutcome, DefinitionError> {
    export_item_impl(
        session, filesystem, target, item_name, root, selector, confirmed, limits, None,
    )
    .await
}

/// Export using the complete source per-part fallback and selected storage codec.
///
/// The caller still owns destination resolution, confirmation and actual local
/// or remote writes. Unlike the original bounded [`export_item`] API, malformed
/// suffix JSON retains the original encoded payload exactly as Python does.
///
/// # Errors
/// Preserves ordered partial-write, permission, path, bound and operation errors.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned boundaries."
)]
pub async fn export_item_for_storage<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    target: DefinitionTarget<'_>,
    item_name: &str,
    root: &Path,
    selector: Option<&str>,
    confirmed: bool,
    limits: DefinitionLimits,
    storage: ExportStorage,
) -> Result<ExportOutcome, DefinitionError> {
    export_item_impl(
        session,
        filesystem,
        target,
        item_name,
        root,
        selector,
        confirmed,
        limits,
        Some(storage),
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared implementation preserves released API behavior."
)]
async fn export_item_impl<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    target: DefinitionTarget<'_>,
    item_name: &str,
    root: &Path,
    selector: Option<&str>,
    confirmed: bool,
    limits: DefinitionLimits,
    storage: Option<ExportStorage>,
) -> Result<ExportOutcome, DefinitionError> {
    if !confirmed {
        return Err(DefinitionErrorKind::ConfirmationRequired.into());
    }
    validate_part_path(item_name, limits)?;
    if item_name.contains('/') {
        return Err(DefinitionErrorKind::UnsafePath.into());
    }
    let read = read_item_definition(session, target, selector).await?;
    write_read_item(
        session,
        filesystem,
        read,
        target.item_type,
        item_name,
        root,
        limits,
        storage,
    )
    .await
}

/// Write an already-read item after the command resolves its export destination.
///
/// Python single export reads metadata/definition before resolving its output
/// path. This explicit read/write composition preserves that order without
/// replaying the POST or adding another decoder. The caller has already confirmed
/// the source warning and selects the local/Lakehouse byte policy.
///
/// # Errors
/// Preserves definition denial, portable paths, bounds and ordered write evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned export boundaries."
)]
pub async fn export_read_item<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    read: DefinitionRead,
    item_type: &str,
    item_name: &str,
    root: &Path,
    limits: DefinitionLimits,
    storage: ExportStorage,
) -> Result<ExportOutcome, DefinitionError> {
    validate_part_path(item_name, limits)?;
    if item_name.contains('/') {
        return Err(DefinitionErrorKind::UnsafePath.into());
    }
    write_read_item(
        session,
        filesystem,
        read,
        item_type,
        item_name,
        root,
        limits,
        Some(storage),
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared implementation preserves released API behavior."
)]
async fn write_read_item<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    read: DefinitionRead,
    item_type: &str,
    item_name: &str,
    root: &Path,
    limits: DefinitionLimits,
    storage: Option<ExportStorage>,
) -> Result<ExportOutcome, DefinitionError> {
    if let Some(status) = read.definition_access_denied {
        return Err(DefinitionErrorKind::DefinitionAccessDenied(status).into());
    }
    let mut files = match storage {
        Some(storage) => {
            decode_export_files_for_storage(&read.item, item_type == "Notebook", limits, storage)?
        }
        None => decode_export_files(&read.item, item_type == "Notebook", limits)?,
    };
    for file in &mut files {
        file.path = format!("{item_name}/{}", file.path);
        validate_part_path(&file.path, limits)?;
    }
    let completed_paths =
        write_export_files(session, filesystem, root, &files, Some(item_name)).await?;
    Ok(ExportOutcome {
        read,
        completed_paths,
    })
}

async fn write_export_files<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    filesystem: &mut dyn DefinitionFileSystem,
    root: &Path,
    files: &[DefinitionFile],
    item_name: Option<&str>,
) -> Result<Vec<String>, DefinitionError> {
    check_cancelled(session).await?;
    filesystem
        .preflight(root, files)
        .map_err(DefinitionErrorKind::FileSystem)?;
    if let Some(item_name) = item_name {
        check_cancelled(session).await?;
        filesystem
            .create_directory(root, item_name)
            .map_err(DefinitionErrorKind::FileSystem)?;
    }
    let mut completed_paths = Vec::new();
    for file in files {
        if let Err(mut error) = check_cancelled(session).await {
            error.completed_paths = completed_paths;
            return Err(error);
        }
        if let Err(error) = filesystem.write(root, file) {
            return Err(DefinitionError {
                kind: DefinitionErrorKind::FileSystem(error),
                completed_paths,
                incomplete_path: Some(file.path.clone()),
            });
        }
        completed_paths.push(file.path.clone());
    }
    if let Err(mut error) = check_cancelled(session).await {
        error.completed_paths = completed_paths;
        return Err(error);
    }
    Ok(completed_paths)
}

async fn execute<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    method: ResourceMethod,
    path: &str,
) -> Result<Vec<u8>, DefinitionError> {
    execute_body(session, method, path, Vec::new()).await
}

async fn execute_body<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    method: ResourceMethod,
    path: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, DefinitionError> {
    let operation =
        FabricOperation::new(method, path, body, CompletionPolicy::PythonSynchronous)
            .map_err(|kind| DefinitionErrorKind::Operation(Box::new(session.evidence(kind))))?;
    let response = session
        .execute(operation)
        .await
        .map_err(|error| DefinitionErrorKind::Operation(Box::new(error)))?;
    if response.status == 202 {
        return Err(DefinitionErrorKind::Operation(Box::new(
            session.evidence(OperationErrorKind::InvalidResponse),
        ))
        .into());
    }
    Ok(response.body)
}

fn object(bytes: &[u8]) -> Result<Value, DefinitionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DefinitionErrorKind::InvalidDefinition)?;
    let value = codec::parse_json(text)?.ok_or(DefinitionErrorKind::InvalidDefinition)?;
    if !value.is_object() {
        return Err(DefinitionErrorKind::InvalidDefinition.into());
    }
    Ok(value)
}

async fn check_cancelled<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
) -> Result<(), DefinitionError> {
    session
        .check_cancelled()
        .await
        .map_err(|error| DefinitionErrorKind::Operation(Box::new(error)).into())
}
