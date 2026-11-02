// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Owner-authorized physical tree transfers, separate from the Python oracle.

use crate::{
    LocalEntry, LocalError, LocalFileReader, LocalFileSystem, LocalKind, ResourceFileHost,
};
use fabric_cli_context::{PathElement, PathKind};
use fabric_cli_mutations::{
    MutationError, NoticeKind, OneLakeResourceKind, OneLakeWriteError, OneLakeWriteTarget,
    frontdoor::{ResourceCommand, ResourceTransferHost},
    validate_copy_destination,
};
use fabric_cli_output::OutputInput;
use fabric_sdk_onelake::{
    OneLakeError, OneLakeMutationError, OneLakePath, OneLakeUpload, OneLakeWriteOutcome,
    file_ranges::{
        OneLakeChunkError, OneLakeChunkLimits, OneLakeFileCreateCondition, OneLakeFileVersion,
    },
};
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    hash::{BuildHasher, Hasher},
    num::NonZeroUsize,
    path::PathBuf,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
};

/// Phase at which an additive transfer stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeStage {
    /// Validate endpoints and snapshot complete source/destination metadata.
    Preflight,
    /// Copy directories and file bytes.
    Copy,
    /// Atomic remote rename, never automatically retried.
    Rename,
    /// Delete individually revalidated sources after every copy committed.
    SourceDelete,
}

/// Original typed cause, not a success or rollback inference.
#[derive(Debug)]
pub enum TreeFailure {
    /// Invalid input, collision, incomplete acceptance, or changed source.
    Command(MutationError),
    /// Native filesystem failure, including partial-write evidence.
    Local(LocalError),
    /// Storage metadata/read failure before a corresponding mutation.
    Read(OneLakeError),
    /// Original storage mutation failure and uncertainty.
    Storage(OneLakeMutationError),
    /// Incremental write failure, retaining acknowledged bytes and uncertainty.
    Chunk(Box<OneLakeChunkError>),
    /// Caller cancellation; started work is not rolled back.
    Cancelled,
}

/// Completed steps retained even if a later copy, delete or output write fails.
#[derive(Debug, Default)]
pub struct TreeResult {
    /// Final source-compatible envelopes, not independent commit evidence.
    pub outputs: Vec<OutputInput>,
    /// Successful native writes and exact byte counts.
    pub local_writes: Vec<(PathBuf, u64)>,
    /// Native directories created or validated before later transfer steps.
    pub local_directories: Vec<PathBuf>,
    /// Actual remote create, last acknowledged append and completed flush.
    pub uploads: Vec<(OneLakePath, OneLakeUpload)>,
    /// Acknowledged position and chunk count per upload, never completion proof.
    pub upload_progress: Vec<(OneLakePath, u64, u64)>,
    /// Actual remote directory-create responses.
    pub directories: Vec<(OneLakePath, OneLakeWriteOutcome)>,
    /// Actual atomic rename response, if selected.
    pub rename: Option<OneLakeWriteOutcome>,
    /// Received remote source-delete responses; a 202 is not completion.
    pub source_deletes: Vec<(OneLakePath, OneLakeWriteOutcome)>,
    /// Native source entries actually removed after commit.
    pub local_deletes: Vec<PathBuf>,
    /// Confirmation was explicitly declined before mutation.
    pub declined: bool,
    /// A same-path move required no mutation.
    pub unchanged: bool,
    /// Every destination file flushed before any source cleanup began.
    pub destination_copy_complete: bool,
}

/// Failed transfer retaining the complete earlier receipt and source uncertainty.
#[derive(Debug)]
pub struct TreeError {
    /// Phase that failed.
    pub stage: TreeStage,
    /// Original cause.
    pub failure: TreeFailure,
    /// Earlier committed/accepted steps, never discarded on a later failure.
    pub partial: Box<TreeResult>,
    /// Source removal/rename may have started; cancellation is not rollback.
    pub source_delete_may_have_occurred: bool,
    /// A native target write may have started, even if cancellation lost its receipt.
    pub local_write_may_have_occurred: bool,
    /// Active endpoint spellings retained for reconciliation, never Display.
    pub paths: Box<TreePaths>,
}

/// Exact active endpoint spellings, retained only as structured error evidence.
#[derive(Debug)]
pub struct TreePaths {
    /// Source selected for the failing step.
    pub source: Option<String>,
    /// Destination selected for the failing step.
    pub destination: Option<String>,
}

impl TreeError {
    /// Render a sanitized command diagnostic without discarding typed evidence.
    pub fn output(&self) -> OutputInput {
        let mut output = match &self.failure {
            TreeFailure::Command(error) => error.output(),
            TreeFailure::Local(error) => MutationError::new(
                match error.kind {
                    crate::LocalErrorKind::Limit => "Limit",
                    crate::LocalErrorKind::SourceChanged => "SourceChanged",
                    crate::LocalErrorKind::Cancelled => "Cancelled",
                    _ => "InvalidPath",
                },
                if matches!(error.kind, crate::LocalErrorKind::Io(_)) {
                    "Native filesystem I/O failed".into()
                } else {
                    error.diagnostic()
                },
            )
            .output(),
            TreeFailure::Read(error) => storage_output(error),
            TreeFailure::Storage(error) => storage_output(&error.kind),
            TreeFailure::Chunk(error) => storage_output(&error.kind),
            TreeFailure::Cancelled => MutationError::new(
                "Cancelled",
                "Transfer cancelled; completed or uncertain effects are not rolled back",
            )
            .output(),
        };
        if self.stage == TreeStage::Preflight {
            return output;
        }
        let phase = match self.stage {
            TreeStage::Preflight => "preflight",
            TreeStage::Copy => "copy",
            TreeStage::Rename => "rename",
            TreeStage::SourceDelete => "source cleanup",
        };
        let removed = self.partial.local_deletes.len()
            + self
                .partial
                .source_deletes
                .iter()
                .filter(|(_, outcome)| completed(*outcome))
                .count();
        let uncertain_write = self.local_write_may_have_occurred
            || matches!(&self.failure, TreeFailure::Chunk(error) if error.mutation_may_have_occurred)
            || matches!(self.stage, TreeStage::Copy | TreeStage::Rename)
                && matches!(&self.failure, TreeFailure::Storage(error) if error.mutation_may_have_occurred)
            || self
                .partial
                .directories
                .iter()
                .any(|(_, outcome)| !completed(*outcome))
            || self
                .partial
                .rename
                .is_some_and(|outcome| !completed(outcome))
            || self
                .partial
                .uploads
                .iter()
                .any(|(_, upload)| upload.flush.is_none());
        output.message = Some(format!(
            "{}; phase: {phase}; destination copy {}; source removal {} ({removed} confirmed entries); active source cleanup {}; destination write {}; no rollback performed",
            output.message.as_deref().unwrap_or("Transfer failed"),
            if self.partial.destination_copy_complete {
                "complete"
            } else {
                "incomplete or unverified"
            },
            if removed > 0 {
                "partial"
            } else {
                "not confirmed"
            },
            if self.source_delete_may_have_occurred {
                "uncertain"
            } else {
                "not in flight"
            },
            if uncertain_write {
                "may have occurred"
            } else {
                "no active uncertainty recorded"
            },
        ));
        output
    }
}
impl fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "tree transfer failed; inspect phase, partial results and source uncertainty",
        )
    }
}
impl std::error::Error for TreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.failure {
            TreeFailure::Command(error) => Some(error),
            TreeFailure::Local(error) => Some(error),
            TreeFailure::Read(error) => Some(error),
            TreeFailure::Storage(error) => Some(error),
            TreeFailure::Chunk(error) => Some(error.as_ref()),
            TreeFailure::Cancelled => None,
        }
    }
}

#[derive(Clone)]
enum Endpoint {
    Local(PathBuf),
    Remote(OneLakeWriteTarget),
}
impl Endpoint {
    fn display(&self) -> String {
        match self {
            Self::Local(path) => path.to_string_lossy().into_owned(),
            Self::Remote(target) => target.display_path.clone(),
        }
    }
    fn child(&self, relative: &str) -> Result<Self, TreeFailure> {
        if relative.is_empty() {
            return Ok(self.clone());
        }
        match self {
            Self::Local(path) => {
                let mut path = path.clone();
                for part in relative.split('/') {
                    path = crate::local::child_path(&path, part).map_err(TreeFailure::Local)?;
                }
                Ok(Self::Local(path))
            }
            Self::Remote(target) => {
                let mut target = target.clone();
                target.path = OneLakePath::new(
                    target.path.workspace(),
                    target.path.item(),
                    &format!("{}/{}", target.path.relative(), relative),
                )
                .map_err(TreeFailure::Read)?;
                target.display_path =
                    format!("{}/{}", target.display_path.trim_end_matches('/'), relative);
                target.kind = OneLakeResourceKind::Undefined;
                Ok(Self::Remote(target))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    relative: String,
    directory: bool,
    local: Option<LocalEntry>,
    version: Option<serde_json::Value>,
}

enum FileSource {
    Local(Box<dyn LocalFileReader>),
    Remote(OneLakePath, OneLakeFileVersion),
}

impl FileSource {
    fn len(&self) -> u64 {
        match self {
            Self::Local(reader) => reader.len(),
            Self::Remote(_, version) => version.len(),
        }
    }
}

struct Transfer<'host, 'borrow> {
    host: &'borrow mut ResourceFileHost<'host>,
    result: TreeResult,
    stage: TreeStage,
    source_delete: bool,
    local_write: bool,
    source_path: Option<String>,
    destination_path: Option<String>,
}

impl ResourceFileHost<'_> {
    /// Execute the authorized additive file/directory behavior over resolved paths.
    ///
    /// Directory copies require `--recursive`; moves include the entire selected
    /// tree. Existing directory destinations append the source basename. Every
    /// copy must commit before source cleanup. Atomic remote renames are preferred
    /// within one item; cross-item/workspace/endpoint moves use checked copy then deletion.
    /// The original `execute`/`execute_resolved` remain unchanged oracle adapters.
    ///
    /// # Errors
    /// Retains bounded traversal, collision, link, cancellation and partial effects.
    /// No uncertain write, rename or delete is retried or treated as rollback.
    pub async fn execute_full_resolved(
        &mut self,
        command: &ResourceCommand,
        source: &Arc<PathElement>,
        target: &Arc<PathElement>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TreeResult, TreeError> {
        let mut transfer = Transfer {
            host: self,
            result: TreeResult::default(),
            stage: TreeStage::Preflight,
            source_delete: false,
            local_write: false,
            source_path: None,
            destination_path: None,
        };
        let mut cancelled = pin!(cancelled);
        match transfer
            .run(command, source, target, cancelled.as_mut())
            .await
        {
            Ok(()) => Ok(transfer.result),
            Err(failure) => Err(TreeError {
                stage: transfer.stage,
                failure,
                partial: Box::new(transfer.result),
                source_delete_may_have_occurred: transfer.source_delete,
                local_write_may_have_occurred: transfer.local_write,
                paths: Box::new(TreePaths {
                    source: transfer.source_path,
                    destination: transfer.destination_path,
                }),
            }),
        }
    }
}

impl Transfer<'_, '_> {
    fn chunk_limits(&self) -> OneLakeChunkLimits {
        let response = self
            .host
            .storage
            .read_limits
            .max_response_bytes
            .min(self.host.storage.write_limits.max_response_bytes)
            .min(NonZeroUsize::new(4 * 1024 * 1024).unwrap());
        OneLakeChunkLimits {
            max_chunk_bytes: self
                .host
                .storage
                .write_limits
                .max_request_bytes
                .min(response),
            max_response_bytes: response,
        }
    }

    async fn read_chunk(
        &self,
        source: &mut FileSource,
        offset: u64,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Vec<u8>, TreeFailure> {
        let limits = self.chunk_limits();
        match source {
            FileSource::Local(reader) => interrupt(
                reader.read_at(offset, limits.max_chunk_bytes),
                cancelled.as_mut(),
            )
            .await?
            .map_err(TreeFailure::Local),
            FileSource::Remote(path, version) => self
                .host
                .storage
                .client
                .read_range(
                    self.host.storage.executor,
                    path,
                    version,
                    offset,
                    limits.max_chunk_bytes,
                    limits,
                    cancelled,
                )
                .await
                .map(|range| range.body)
                .map_err(TreeFailure::Read),
        }
    }

    fn files(&self) -> Result<&dyn LocalFileSystem, TreeFailure> {
        self.host.files.ok_or_else(|| {
            command(
                "NotSupported",
                "Transfer requires an explicit local tree capability",
            )
        })
    }

    fn endpoint(
        command: &ResourceCommand,
        path: &Arc<PathElement>,
    ) -> Result<Endpoint, TreeFailure> {
        if let Some(local) = path.local_metadata() {
            return Ok(Endpoint::Local(local.path.clone().into()));
        }
        if matches!(path.kind(), PathKind::OneLake(_)) {
            return ResourceTransferHost::storage_target(command, path)
                .map(Endpoint::Remote)
                .map_err(storage_failure);
        }
        Err(command_error(
            "NotSupported",
            "Physical transfer requires local or OneLake endpoints",
        ))
    }

    async fn kind(
        &self,
        endpoint: &Endpoint,
        known: bool,
        cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<LocalKind, TreeFailure> {
        match endpoint {
            Endpoint::Local(path) => interrupt(self.files()?.kind(path), cancelled)
                .await?
                .map_err(TreeFailure::Local),
            Endpoint::Remote(target) if known => Ok(match target.kind {
                OneLakeResourceKind::File => LocalKind::File,
                OneLakeResourceKind::Directory | OneLakeResourceKind::Shortcut => {
                    LocalKind::Directory
                }
                OneLakeResourceKind::Undefined => LocalKind::Missing,
                _ => LocalKind::Other,
            }),
            Endpoint::Remote(target) => {
                match self
                    .host
                    .storage
                    .client
                    .properties(
                        self.host.storage.executor,
                        &target.path,
                        self.host.storage.read_limits,
                        cancelled,
                    )
                    .await
                {
                    Ok(headers) => {
                        if header(&headers, "x-ms-onelake-shortcut-path") == Some("true")
                            && header(&headers, "x-ms-resource-type") != Some("file")
                        {
                            return Err(command(
                                "NotSupported",
                                "Recursive transfer cannot traverse a shortcut",
                            ));
                        }
                        match header(&headers, "x-ms-resource-type") {
                            Some("directory") => Ok(LocalKind::Directory),
                            Some("file") | None => Ok(LocalKind::File),
                            _ => Err(command(
                                "InvalidResponse",
                                "Storage returned an invalid resource type",
                            )),
                        }
                    }
                    Err(OneLakeError::Http { status: 404, .. }) => Ok(LocalKind::Missing),
                    Err(error) => Err(TreeFailure::Read(error)),
                }
            }
        }
    }

    async fn snapshot(
        &self,
        endpoint: &Endpoint,
        cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Vec<Entry>, TreeFailure> {
        match endpoint {
            Endpoint::Local(path) => {
                let entries = interrupt(
                    self.files()?
                        .snapshot_tree(path, self.host.storage.read_limits.max_paths),
                    cancelled,
                )
                .await?
                .map_err(TreeFailure::Local)?;
                if entries.is_empty()
                    || entries.len() > self.host.storage.read_limits.max_paths.get()
                {
                    return Err(command(
                        "InvalidResponse",
                        "Local tree snapshot is empty or exceeds its entry bound",
                    ));
                }
                let mut seen = std::collections::BTreeSet::new();
                let mut entries: Vec<Entry> = entries
                    .into_iter()
                    .map(|local| {
                        if !matches!(local.kind, LocalKind::File | LocalKind::Directory)
                            || local.relative.components().count() > 256
                            || !seen.insert(local.relative.clone())
                        {
                            return Err(command(
                                "InvalidResponse",
                                "Invalid or duplicate local tree entry",
                            ));
                        }
                        let mut parts = Vec::new();
                        for part in local.relative.components() {
                            let std::path::Component::Normal(part) = part else {
                                return Err(command("InvalidPath", "Invalid local tree entry"));
                            };
                            let part = part.to_str().ok_or_else(|| {
                                command("InvalidPath", "Local name is not Unicode")
                            })?;
                            parts.push(part.to_owned());
                        }
                        Ok(Entry {
                            relative: parts.join("/"),
                            directory: local.kind == LocalKind::Directory,
                            local: Some(local),
                            version: None,
                        })
                    })
                    .collect::<Result<_, _>>()?;
                entries.sort_by(|left, right| left.relative.cmp(&right.relative));
                if !entries[0].relative.is_empty() {
                    return Err(command(
                        "InvalidResponse",
                        "Local tree snapshot omitted its root",
                    ));
                }
                let kinds: BTreeMap<_, _> = entries
                    .iter()
                    .map(|entry| (entry.relative.as_str(), entry.directory))
                    .collect();
                if entries.iter().skip(1).any(|entry| {
                    let parent = entry
                        .relative
                        .rsplit_once('/')
                        .map_or("", |(parent, _)| parent);
                    kinds.get(parent) != Some(&true)
                }) {
                    return Err(command(
                        "InvalidResponse",
                        "Local tree snapshot omitted a directory parent",
                    ));
                }
                Ok(entries)
            }
            Endpoint::Remote(target) if target.kind == OneLakeResourceKind::File => {
                Ok(vec![Entry {
                    relative: String::new(),
                    directory: false,
                    local: None,
                    version: None,
                }])
            }
            Endpoint::Remote(target) => {
                let list = self
                    .host
                    .storage
                    .client
                    .list(
                        self.host.storage.executor,
                        &target.path,
                        true,
                        self.host.storage.read_limits,
                        cancelled,
                    )
                    .await
                    .map_err(TreeFailure::Read)?;
                let prefix = format!("{}/{}/", target.path.item(), target.path.relative());
                let mut entries = BTreeMap::new();
                entries.insert(
                    String::new(),
                    Entry {
                        relative: String::new(),
                        directory: true,
                        local: None,
                        version: None,
                    },
                );
                let mut seen = std::collections::BTreeSet::new();
                for row in list.paths {
                    if row
                        .get("isShortcut")
                        .is_some_and(|value| value == true || value == "true")
                    {
                        return Err(command(
                            "NotSupported",
                            "Recursive transfer cannot traverse a shortcut",
                        ));
                    }
                    let name = row["name"]
                        .as_str()
                        .ok_or_else(|| command("InvalidResponse", "Invalid OneLake tree entry"))?;
                    let relative = name.strip_prefix(&prefix).ok_or_else(|| {
                        command(
                            "InvalidResponse",
                            "OneLake listing escaped the selected source",
                        )
                    })?;
                    OneLakePath::new("validation", "validation", relative)
                        .map_err(TreeFailure::Read)?;
                    if relative.is_empty()
                        || relative.split('/').count() > 256
                        || !seen.insert(relative.to_owned())
                    {
                        return Err(command(
                            "InvalidResponse",
                            "Duplicate or invalid OneLake tree entry",
                        ));
                    }
                    let directory = match row.get("isDirectory") {
                        Some(serde_json::Value::Bool(value)) => *value,
                        Some(serde_json::Value::String(value)) if value == "true" => true,
                        Some(serde_json::Value::String(value)) if value == "false" => false,
                        None => false,
                        _ => {
                            return Err(command(
                                "InvalidResponse",
                                "Invalid OneLake directory flag",
                            ));
                        }
                    };
                    let mut parent = relative;
                    while let Some((ancestor, _)) = parent.rsplit_once('/') {
                        if entries
                            .get(ancestor)
                            .is_some_and(|entry: &Entry| !entry.directory)
                        {
                            return Err(command("InvalidResponse", "File is also a tree parent"));
                        }
                        entries.entry(ancestor.to_owned()).or_insert_with(|| Entry {
                            relative: ancestor.into(),
                            directory: true,
                            local: None,
                            version: None,
                        });
                        parent = ancestor;
                    }
                    if entries
                        .get(relative)
                        .is_some_and(|entry| entry.directory != directory)
                    {
                        return Err(command(
                            "InvalidResponse",
                            "Conflicting OneLake tree entries",
                        ));
                    }
                    entries.insert(
                        relative.into(),
                        Entry {
                            relative: relative.into(),
                            directory,
                            local: None,
                            version: Some(row),
                        },
                    );
                    if entries.len() > self.host.storage.read_limits.max_paths.get() {
                        return Err(TreeFailure::Read(OneLakeError::Limit));
                    }
                }
                Ok(entries.into_values().collect())
            }
        }
    }

    async fn run(
        &mut self,
        resource: &ResourceCommand,
        source_path: &Arc<PathElement>,
        target_path: &Arc<PathElement>,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<(), TreeFailure> {
        interrupt(async {}, cancelled.as_mut()).await?;
        let (moving, recursive, force, block) = match resource {
            ResourceCommand::Copy {
                force,
                recursive,
                block_on_path_collision,
                ..
            } => (false, *recursive, *force, *block_on_path_collision),
            ResourceCommand::Move {
                force, recursive, ..
            } => (true, *recursive, *force, false),
            _ => return Err(command("InvalidInput", "Expected a copy or move command")),
        };
        if resource.args().skill.is_some() {
            return Err(command(
                "NotSupported",
                "File --skill request headers are not supported",
            ));
        }
        let source = Self::endpoint(resource, source_path)?;
        let selected_target = Self::endpoint(resource, target_path)?;
        self.source_path = Some(source.display());
        self.destination_path = Some(selected_target.display());
        if matches!(&source, Endpoint::Remote(target) if matches!(target.kind, OneLakeResourceKind::Shortcut | OneLakeResourceKind::Table))
        {
            return Err(command(
                "NotSupported",
                "Physical tree transfer cannot read shortcut or table sources",
            ));
        }
        if let Endpoint::Remote(target) = &selected_target {
            validate_copy_destination(target).map_err(storage_failure)?;
        }
        let source_kind = self.kind(&source, true, cancelled.as_mut()).await?;
        if !matches!(source_kind, LocalKind::File | LocalKind::Directory) {
            return Err(command(
                "InvalidPath",
                "Source must be a regular file or directory",
            ));
        }
        if source_kind == LocalKind::Directory && !recursive && !moving {
            return Err(command(
                "NotSupported",
                "Directory copy requires --recursive",
            ));
        }
        if source_kind == LocalKind::Directory
            && matches!(&selected_target, Endpoint::Remote(target) if target.kind == OneLakeResourceKind::Shortcut)
        {
            return Err(command(
                "NotSupported",
                "Recursive transfer cannot traverse a shortcut",
            ));
        }
        let target_kind = self
            .kind(&selected_target, true, cancelled.as_mut())
            .await?;
        if target_kind == LocalKind::Other {
            return Err(command(
                "NotSupported",
                "Transfer destination cannot be a shortcut, table or special file",
            ));
        }
        let selected_same = match (&source, &selected_target) {
            (Endpoint::Local(from), Endpoint::Local(to)) => {
                interrupt(self.files()?.canonical_path(from), cancelled.as_mut())
                    .await?
                    .map_err(TreeFailure::Local)?
                    == interrupt(self.files()?.canonical_path(to), cancelled.as_mut())
                        .await?
                        .map_err(TreeFailure::Local)?
            }
            (Endpoint::Remote(from), Endpoint::Remote(to)) => from.path == to.path,
            _ => false,
        };
        if selected_same {
            if !moving {
                return Err(command(
                    "InvalidPath",
                    "Source and destination are the same path",
                ));
            }
            self.result.unchanged = true;
            self.done(resource);
            return Ok(());
        }
        let name = if target_kind == LocalKind::Directory {
            match &source {
                Endpoint::Local(path) => {
                    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                        name.to_owned()
                    } else {
                        let actual =
                            interrupt(self.files()?.canonical_path(path), cancelled.as_mut())
                                .await?
                                .map_err(TreeFailure::Local)?;
                        actual.file_name().and_then(|part| part.to_str()).ok_or_else(|| command("InvalidPath", "Source root has no basename; select an explicit destination path"))?.to_owned()
                    }
                }
                Endpoint::Remote(target) => target
                    .path
                    .relative()
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .into(),
            }
        } else {
            String::new()
        };
        let target = if target_kind == LocalKind::Directory {
            selected_target.child(&name)?
        } else {
            selected_target.clone()
        };
        self.destination_path = Some(target.display());
        let same = match (&source, &target) {
            (Endpoint::Local(from), Endpoint::Local(to)) => {
                let from = interrupt(self.files()?.canonical_path(from), cancelled.as_mut())
                    .await?
                    .map_err(TreeFailure::Local)?;
                let to = interrupt(self.files()?.canonical_path(to), cancelled.as_mut())
                    .await?
                    .map_err(TreeFailure::Local)?;
                if from != to && (to.starts_with(&from) || from.starts_with(&to)) {
                    return Err(command(
                        "InvalidPath",
                        "Source and destination trees overlap",
                    ));
                }
                from == to
            }
            (Endpoint::Remote(from), Endpoint::Remote(to))
                if from
                    .path
                    .workspace()
                    .eq_ignore_ascii_case(to.path.workspace())
                    && from.path.item().eq_ignore_ascii_case(to.path.item()) =>
            {
                let from = from.path.relative();
                let to = to.path.relative();
                if from != to
                    && (to.starts_with(&format!("{from}/")) || from.starts_with(&format!("{to}/")))
                {
                    return Err(command(
                        "InvalidPath",
                        "Source and destination trees overlap",
                    ));
                }
                from == to
            }
            _ => false,
        };
        if same {
            if !moving {
                return Err(command(
                    "InvalidPath",
                    "Source and destination are the same path",
                ));
            }
            self.result.unchanged = true;
            self.done(resource);
            return Ok(());
        }
        let entries = self.snapshot(&source, cancelled.as_mut()).await?;
        if entries
            .first()
            .is_none_or(|entry| entry.directory != (source_kind == LocalKind::Directory))
        {
            return Err(command(
                "SourceChanged",
                "Source kind changed while preparing the copy",
            ));
        }
        if moving
            && matches!(&source, Endpoint::Local(_))
            && entries.iter().any(|entry| {
                !entry.directory
                    && entry
                        .local
                        .as_ref()
                        .is_none_or(|entry| entry.modified.is_none())
            })
        {
            return Err(command(
                "SourceChanged",
                "Source version metadata is unavailable; move source will not be deleted",
            ));
        }
        if moving && let Endpoint::Remote(from) = &source {
            validate_copy_destination(from).map_err(storage_failure)?;
        }
        let root_known = matches!((&selected_target, &target), (Endpoint::Local(a), Endpoint::Local(b)) if a == b)
            || matches!((&selected_target, &target), (Endpoint::Remote(a), Endpoint::Remote(b)) if a.path == b.path);
        let root_kind = self.kind(&target, root_known, cancelled.as_mut()).await?;
        let mut plan = Vec::new();
        let mut destination_versions = BTreeMap::new();
        let mut local_destinations = std::collections::BTreeSet::new();
        for entry in &entries {
            let destination = target.child(&entry.relative)?;
            if let Endpoint::Local(path) = &destination {
                let canonical = interrupt(self.files()?.canonical_path(path), cancelled.as_mut())
                    .await?
                    .map_err(TreeFailure::Local)?;
                #[cfg(windows)]
                let canonical = local_destination_key(canonical)?;
                if !local_destinations.insert(canonical) {
                    return Err(command(
                        "AmbiguousDestination",
                        "Multiple source entries map to an ambiguous native destination; no files were copied",
                    ));
                }
            }
            let kind = if entry.relative.is_empty() {
                root_kind
            } else if root_kind == LocalKind::Missing {
                LocalKind::Missing
            } else {
                self.kind(&destination, false, cancelled.as_mut()).await?
            };
            if kind == LocalKind::Other
                || kind == LocalKind::Directory && !entry.directory
                || kind == LocalKind::File && entry.directory
            {
                return Err(command(
                    "InvalidPath",
                    "Source and destination types collide",
                ));
            }
            if kind != LocalKind::Missing && block {
                return Err(command("AlreadyExists", "Destination path collision"));
            }
            if kind == LocalKind::File && !force {
                return Err(command(
                    "AlreadyExists",
                    "Destination file overwrite requires --force",
                ));
            }
            if kind == LocalKind::File
                && let Endpoint::Remote(target) = &destination
            {
                let version = self
                    .host
                    .storage
                    .client
                    .file_version(
                        self.host.storage.executor,
                        &target.path,
                        self.chunk_limits(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(TreeFailure::Read)?;
                destination_versions.insert(entry.relative.clone(), version.etag().to_owned());
            }
            plan.push((entry.clone(), destination, kind));
        }
        if moving
            && !force
            && !interrupt(
                self.host.storage.ui.confirm_result("Are you sure?"),
                cancelled.as_mut(),
            )
            .await?
            .map_err(TreeFailure::Command)?
        {
            self.result.declined = true;
            return Ok(());
        }
        if moving
            && let (Endpoint::Remote(from), Endpoint::Remote(to)) = (&source, &target)
            && from
                .path
                .workspace()
                .eq_ignore_ascii_case(to.path.workspace())
            && from.path.item().eq_ignore_ascii_case(to.path.item())
            && (root_kind == LocalKind::Missing
                || source_kind == LocalKind::File && root_kind == LocalKind::File)
        {
            if self.snapshot(&source, cancelled.as_mut()).await? != entries
                || self.kind(&source, false, cancelled.as_mut()).await? != source_kind
                || self.kind(&target, false, cancelled.as_mut()).await? != root_kind
            {
                return Err(command(
                    "SourceChanged",
                    "Transfer paths changed after preflight; rename refused",
                ));
            }
            self.host.storage.ui.notice(
                NoticeKind::Progress,
                &format!(
                    "Moving '{}' \u{2192} '{}'...",
                    source.display(),
                    target.display()
                ),
            );
            self.stage = TreeStage::Rename;
            self.source_delete = true;
            let outcome = self
                .host
                .storage
                .client
                .rename_path_checked(
                    self.host.storage.executor,
                    &from.path,
                    &to.path,
                    self.host.storage.write_limits,
                    root_kind == LocalKind::File,
                    cancelled.as_mut(),
                )
                .await
                .map_err(TreeFailure::Storage)?;
            self.result.rename = Some(outcome);
            if !completed(outcome) {
                return Err(command(
                    "Incomplete",
                    "Storage accepted the rename; completion is unverified and must not be retried",
                ));
            }
            self.done(resource);
            return Ok(());
        }
        self.host.storage.ui.notice(
            NoticeKind::Progress,
            &format!(
                "{} '{}' \u{2192} '{}'...",
                if moving { "Moving" } else { "Copying" },
                source.display(),
                target.display()
            ),
        );
        self.stage = TreeStage::Copy;
        let mut etags = BTreeMap::new();
        let fingerprint_state = std::collections::hash_map::RandomState::new();
        let mut fingerprints = BTreeMap::new();
        for (entry, destination, kind) in &plan {
            self.source_path = Some(source.child(&entry.relative)?.display());
            self.destination_path = Some(destination.display());
            interrupt(async {}, cancelled.as_mut()).await?;
            if *kind != LocalKind::Missing
                && self.kind(destination, false, cancelled.as_mut()).await? != *kind
            {
                return Err(command(
                    "DestinationChanged",
                    "Destination changed after preflight; write refused",
                ));
            }
            if entry.directory {
                if *kind == LocalKind::Directory {
                    continue;
                }
                match destination {
                    Endpoint::Local(path) => {
                        self.local_write = true;
                        interrupt(self.files()?.create_directory(path), cancelled.as_mut())
                            .await?
                            .map_err(TreeFailure::Local)?;
                        self.result.local_directories.push(path.clone());
                        self.local_write = false;
                    }
                    Endpoint::Remote(target) => {
                        let outcome = self
                            .host
                            .storage
                            .client
                            .create_directory(
                                self.host.storage.executor,
                                &target.path,
                                self.host.storage.write_limits,
                                cancelled.as_mut(),
                            )
                            .await
                            .map_err(TreeFailure::Storage)?;
                        self.result.directories.push((target.path.clone(), outcome));
                        if !completed(outcome) {
                            return Err(command(
                                "Incomplete",
                                "Directory creation is accepted, not committed",
                            ));
                        }
                    }
                }
                continue;
            }
            let selected_source = source.child(&entry.relative)?;
            let (mut reader, content_type) = match &selected_source {
                Endpoint::Local(path) => {
                    let read = interrupt(
                        self.files()?.open_reader(
                            path,
                            entry.local.as_ref().ok_or_else(|| {
                                command("InvalidResponse", "Missing local source snapshot")
                            })?,
                        ),
                        cancelled.as_mut(),
                    )
                    .await?
                    .map_err(TreeFailure::Local)?;
                    (FileSource::Local(read), media_type(destination).to_owned())
                }
                Endpoint::Remote(target) => {
                    let version = self
                        .host
                        .storage
                        .client
                        .file_version(
                            self.host.storage.executor,
                            &target.path,
                            self.chunk_limits(),
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(TreeFailure::Read)?;
                    if let Some(expected) = entry
                        .version
                        .as_ref()
                        .and_then(|row| row.get("etag"))
                        .and_then(serde_json::Value::as_str)
                        && expected != version.etag()
                    {
                        return Err(command(
                            "SourceChanged",
                            "Source version changed after preflight",
                        ));
                    }
                    etags.insert(entry.relative.clone(), version.etag().to_owned());
                    let media = version
                        .content_type()
                        .unwrap_or("application/octet-stream")
                        .to_owned();
                    (FileSource::Remote(target.path.clone(), version), media)
                }
            };
            let mut fingerprint = fingerprint_state.build_hasher();
            let length = reader.len();
            let mut position = 0;
            match destination {
                Endpoint::Local(path) => {
                    self.local_write = true;
                    let mut writer = interrupt(
                        self.files()?
                            .open_writer(path, length, *kind == LocalKind::File),
                        cancelled.as_mut(),
                    )
                    .await?
                    .map_err(TreeFailure::Local)?;
                    while position < length {
                        let bytes = self
                            .read_chunk(&mut reader, position, cancelled.as_mut())
                            .await?;
                        validate_chunk(&bytes, position, length, self.chunk_limits())?;
                        fingerprint.write(&bytes);
                        let count = bytes.len() as u64;
                        interrupt(writer.append_at(position, bytes), cancelled.as_mut())
                            .await?
                            .map_err(TreeFailure::Local)?;
                        position += count;
                    }
                    let written = interrupt(writer.finish(), cancelled.as_mut())
                        .await?
                        .map_err(TreeFailure::Local)?;
                    if written != length {
                        return Err(command(
                            "InvalidResponse",
                            "Local adapter returned an incomplete write",
                        ));
                    }
                    self.result.local_writes.push((path.clone(), written));
                    self.local_write = false;
                }
                Endpoint::Remote(target) => {
                    let condition = if let Some(etag) = destination_versions.get(&entry.relative) {
                        OneLakeFileCreateCondition::IfMatch(etag.clone())
                    } else {
                        OneLakeFileCreateCondition::IfAbsent
                    };
                    let mut upload = self
                        .host
                        .storage
                        .client
                        .start_file_upload(
                            self.host.storage.executor,
                            &target.path,
                            length,
                            &content_type,
                            condition,
                            self.chunk_limits(),
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(chunk_failure)?;
                    let index = self.result.uploads.len();
                    self.result.uploads.push((
                        target.path.clone(),
                        OneLakeUpload {
                            create: OneLakeWriteOutcome::Completed { status: 201 },
                            append: None,
                            flush: None,
                        },
                    ));
                    self.result
                        .upload_progress
                        .push((target.path.clone(), 0, 0));
                    while position < length {
                        let bytes = self
                            .read_chunk(&mut reader, position, cancelled.as_mut())
                            .await?;
                        validate_chunk(&bytes, position, length, self.chunk_limits())?;
                        fingerprint.write(&bytes);
                        let outcome = upload
                            .append_at(
                                self.host.storage.executor,
                                position,
                                &bytes,
                                cancelled.as_mut(),
                            )
                            .await
                            .map_err(chunk_failure)?;
                        self.result.uploads[index].1.append = Some(outcome);
                        position += bytes.len() as u64;
                        self.result.upload_progress[index].1 = position;
                        self.result.upload_progress[index].2 += 1;
                    }
                    let outcome = upload
                        .flush(self.host.storage.executor, cancelled.as_mut())
                        .await
                        .map_err(chunk_failure)?;
                    self.result.uploads[index].1.flush = Some(outcome);
                }
            }
            if moving && matches!(&selected_source, Endpoint::Local(_)) {
                fingerprints.insert(entry.relative.clone(), fingerprint.finish());
            }
        }
        self.result.destination_copy_complete = true;
        self.source_path = Some(source.display());
        self.destination_path = Some(target.display());
        if self.snapshot(&source, cancelled.as_mut()).await? != entries {
            return Err(command(
                "SourceChanged",
                "Source changed while copying; no source cleanup will be attempted",
            ));
        }
        if moving {
            if matches!(&source, Endpoint::Remote(_)) {
                for entry in entries.iter().filter(|entry| entry.directory) {
                    let Endpoint::Remote(target) = source.child(&entry.relative)? else {
                        unreachable!()
                    };
                    self.source_path = Some(target.display_path.clone());
                    let properties = self
                        .host
                        .storage
                        .client
                        .properties(
                            self.host.storage.executor,
                            &target.path,
                            self.host.storage.read_limits,
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(TreeFailure::Read)?;
                    if header(&properties, "x-ms-resource-type") != Some("directory")
                        || header(&properties, "x-ms-onelake-shortcut-path") == Some("true")
                    {
                        return Err(command(
                            "SourceChanged",
                            "Source directory changed after copying; deletion refused",
                        ));
                    }
                    let etag = header(&properties, "etag").ok_or_else(|| {
                        command(
                            "SourceChanged",
                            "Source directory ETag is unavailable; deletion refused",
                        )
                    })?;
                    etags.insert(entry.relative.clone(), etag.to_owned());
                }
            }
            self.stage = TreeStage::SourceDelete;
            for entry in entries.iter().rev() {
                interrupt(async {}, cancelled.as_mut()).await?;
                let selected = source.child(&entry.relative)?;
                self.source_path = Some(selected.display());
                self.destination_path = Some(target.child(&entry.relative)?.display());
                match selected {
                    Endpoint::Local(path) => {
                        let expected = entry.local.as_ref().ok_or_else(|| {
                            command("InvalidResponse", "Missing local source snapshot")
                        })?;
                        let verification = if entry.directory {
                            None
                        } else {
                            let mut reader = interrupt(
                                self.files()?.open_reader(&path, expected),
                                cancelled.as_mut(),
                            )
                            .await?
                            .map_err(TreeFailure::Local)?;
                            let mut fingerprint = fingerprint_state.build_hasher();
                            let mut position = 0;
                            while position < reader.len() {
                                let bytes = interrupt(
                                    reader.read_at(position, self.chunk_limits().max_chunk_bytes),
                                    cancelled.as_mut(),
                                )
                                .await?
                                .map_err(TreeFailure::Local)?;
                                validate_chunk(
                                    &bytes,
                                    position,
                                    reader.len(),
                                    self.chunk_limits(),
                                )?;
                                fingerprint.write(&bytes);
                                position += bytes.len() as u64;
                            }
                            if fingerprints.get(&entry.relative) != Some(&fingerprint.finish()) {
                                return Err(command(
                                    "SourceChanged",
                                    "Source content changed after copying; deletion refused",
                                ));
                            }
                            Some(reader)
                        };
                        self.source_delete = true;
                        interrupt(
                            self.files()?.remove_copied(&path, expected),
                            cancelled.as_mut(),
                        )
                        .await?
                        .map_err(TreeFailure::Local)?;
                        self.result.local_deletes.push(path);
                        self.source_delete = false;
                        drop(verification);
                    }
                    Endpoint::Remote(target) => {
                        let etag = Some(
                            etags
                                .get(&entry.relative)
                                .ok_or_else(|| {
                                    command("SourceChanged", "Copied source ETag is unavailable")
                                })?
                                .as_str(),
                        );
                        self.source_delete = true;
                        let outcome = self
                            .host
                            .storage
                            .client
                            .delete_path_checked(
                                self.host.storage.executor,
                                &target.path,
                                etag,
                                self.host.storage.write_limits,
                                cancelled.as_mut(),
                            )
                            .await
                            .map_err(TreeFailure::Storage)?;
                        self.result
                            .source_deletes
                            .push((target.path.clone(), outcome));
                        if !completed(outcome) {
                            return Err(command(
                                "Incomplete",
                                "Source deletion accepted; completion unverified and no retry is safe",
                            ));
                        }
                        self.source_delete = false;
                    }
                }
            }
        }
        self.done(resource);
        Ok(())
    }

    fn done(&mut self, resource: &ResourceCommand) {
        self.result.outputs.push(OutputInput {
            command: Some(resource.name().into()),
            message: Some("Done".into()),
            ..OutputInput::default()
        });
    }
}

fn command(code: &str, message: &str) -> TreeFailure {
    command_error(code, message)
}
#[cfg(windows)]
fn local_destination_key(canonical: PathBuf) -> Result<PathBuf, TreeFailure> {
    let path = canonical
        .to_str()
        .ok_or_else(|| command("InvalidPath", "Native destination identity is not Unicode"))?;
    // Comparison only: retain native spelling for every actual operation.
    // This deliberately also rejects some case-sensitive/expanded-case
    // distinctions rather than claiming to emulate a volume's lookup table.
    Ok(PathBuf::from(
        path.chars()
            .flat_map(char::to_lowercase)
            .flat_map(char::to_uppercase)
            .collect::<String>(),
    ))
}
fn chunk_failure(error: OneLakeChunkError) -> TreeFailure {
    TreeFailure::Chunk(Box::new(error))
}
fn storage_output(error: &OneLakeError) -> OutputInput {
    if let OneLakeError::Http { status, .. } = error {
        MutationError::new(
            "StorageHttpError",
            format!("Storage request failed (HTTP {status})"),
        )
        .output()
    } else {
        OneLakeWriteError::Read(error.clone()).output()
    }
}
fn validate_chunk(
    bytes: &[u8],
    position: u64,
    length: u64,
    limits: OneLakeChunkLimits,
) -> Result<(), TreeFailure> {
    if bytes.is_empty()
        || bytes.len() > limits.max_chunk_bytes.get()
        || bytes.len() as u64 != (length - position).min(limits.max_chunk_bytes.get() as u64)
    {
        return Err(command(
            "InvalidResponse",
            "Source returned an incomplete or oversized range",
        ));
    }
    Ok(())
}
fn command_error(code: &str, message: &str) -> TreeFailure {
    TreeFailure::Command(MutationError::new(code, message))
}
fn storage_failure(error: OneLakeWriteError) -> TreeFailure {
    match error {
        OneLakeWriteError::Command(error) => TreeFailure::Command(error),
        OneLakeWriteError::Read(error) => TreeFailure::Read(error),
        OneLakeWriteError::Storage(error) => TreeFailure::Storage(error),
    }
}
fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
fn completed(outcome: OneLakeWriteOutcome) -> bool {
    matches!(outcome, OneLakeWriteOutcome::Completed { .. })
}
fn media_type(endpoint: &Endpoint) -> &'static str {
    let path = match endpoint {
        Endpoint::Local(path) => path.to_str().unwrap_or(""),
        Endpoint::Remote(target) => target.path.relative(),
    };
    if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".csv") {
        "text/csv"
    } else if path.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}
async fn interrupt<T>(
    future: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, TreeFailure> {
    let mut future = pin!(future);
    poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(TreeFailure::Cancelled));
        }
        future.as_mut().poll(context).map(Ok)
    })
    .await
}
