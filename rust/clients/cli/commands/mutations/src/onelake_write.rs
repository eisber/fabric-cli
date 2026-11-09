// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{MutationError, MutationUi, NoticeKind, OneLakeReadError, message};
use fabric_cli_context::ItemType;
use fabric_cli_output::OutputInput;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeLimits, OneLakeMutationError, OneLakePath,
    OneLakeUpload, OneLakeWriteLimits, OneLakeWriteOutcome,
};
use std::{fmt, future::Future, pin::pin};

/// Existing context host's resolved OneLake resource type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeResourceKind {
    /// Existing file.
    File,
    /// Existing directory.
    Directory,
    /// Existing shortcut; its metadata operations use the Fabric API, not DFS delete.
    Shortcut,
    /// Existing table.
    Table,
    /// Unresolved prospective destination.
    Undefined,
}

/// An explicitly resolved OneLake target; no context or identity is discovered.
#[derive(Clone, Debug)]
pub struct OneLakeWriteTarget {
    /// Validated workspace/item-relative DFS path from the existing context host.
    pub path: OneLakePath,
    /// Source item type, parsed by the existing CLI context library.
    pub item_type: ItemType,
    /// Context resource kind, not guessed from a filename.
    pub kind: OneLakeResourceKind,
    /// Display/user path selected by the main host for source messages.
    pub display_path: String,
}

/// Typed command validation, read or mutation failure retaining side-effect data.
#[derive(Clone, Debug, PartialEq)]
pub enum OneLakeWriteError {
    /// Source-compatible local command error.
    Command(MutationError),
    /// Source file read failed before a destination write was attempted.
    Read(OneLakeError),
    /// Storage write failed or is incomplete, possibly after earlier stages.
    Storage(OneLakeMutationError),
}

impl OneLakeWriteError {
    /// Render through the existing output layer; typed reconciliation remains here.
    pub fn output(&self) -> OutputInput {
        match self {
            Self::Command(error) => error.output(),
            Self::Read(error) => OneLakeReadError::Sdk(error.clone()).output(),
            Self::Storage(error) => OneLakeReadError::Sdk(error.kind.clone()).output(),
        }
    }
}
impl fmt::Display for OneLakeWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(error) => error.fmt(formatter),
            Self::Read(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for OneLakeWriteError {}
impl From<OneLakeMutationError> for OneLakeWriteError {
    fn from(error: OneLakeMutationError) -> Self {
        Self::Storage(error)
    }
}

/// Source output alongside actual accepted/completed storage response evidence.
#[derive(Clone, Debug, Default)]
pub struct OneLakeWriteResult {
    /// Existing output envelopes, absent for declined/help/unhandled statuses.
    pub outputs: Vec<OutputInput>,
    /// Actual single-path response, never fabricated completion.
    pub outcome: Option<OneLakeWriteOutcome>,
    /// Exact create/append/flush responses for an upload.
    pub upload: Option<OneLakeUpload>,
    /// A confirmation was declined before mutation dispatch.
    pub declined: bool,
}

/// OneLake write command handlers composed with caller-owned SDK/executor/UI.
pub struct OneLakeWriteHost<'a> {
    /// Explicit storage credential/selection from the caller.
    pub client: &'a OneLakeClient,
    /// Existing shared HTTP adapter or a synthetic executor.
    pub executor: &'a dyn OneLakeExecutor,
    /// Source confirmation/progress/warning boundary.
    pub ui: &'a mut dyn MutationUi,
    /// File/source read and traversal limits.
    pub read_limits: OneLakeLimits,
    /// File/body/response write bounds.
    pub write_limits: OneLakeWriteLimits,
}

impl OneLakeWriteHost<'_> {
    /// Execute the OneLake directory branch of `mkdir`/`create`.
    ///
    /// # Errors
    /// Preserves parameter-help and writable-root checks. Shortcut creation and
    /// Fabric item creation are distinct handlers, never DFS fallbacks.
    pub async fn create_directory(
        &mut self,
        target: &OneLakeWriteTarget,
        params_present: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        if !params_present {
            self.ui
                .notice(NoticeKind::Help, "No parameters for '.Onelake'\n");
            return Ok(OneLakeWriteResult::default());
        }
        writable(target, true)?;
        self.ui
            .notice(NoticeKind::Progress, "Creating a new Directory...");
        let outcome = self
            .client
            .create_directory(self.executor, &target.path, self.write_limits, cancelled)
            .await?;
        Ok(OneLakeWriteResult {
            outputs: if matches!(outcome.status(), 200 | 201) {
                vec![message(format!("'{}' created", target.display_path))]
            } else {
                Vec::new()
            },
            outcome: Some(outcome),
            ..OneLakeWriteResult::default()
        })
    }

    /// Execute DFS `rm`/`del` for a file or directory.
    ///
    /// # Errors
    /// Shortcut deletion fails explicitly until its separately routed Fabric
    /// metadata handler is selected; it is never recursively deleted via DFS.
    pub async fn remove(
        &mut self,
        target: &OneLakeWriteTarget,
        force: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        if target.kind == OneLakeResourceKind::Shortcut {
            return Err(command(
                "NotSupported",
                "Shortcut deletion requires the Fabric shortcut handler",
            ));
        }
        if !force
            && !self
                .ui
                .confirm_result("Are you sure?")
                .await
                .map_err(OneLakeWriteError::Command)?
        {
            self.ui
                .notice(NoticeKind::Warning, "Resource delete cancelled");
            return Ok(declined());
        }
        if force {
            self.ui
                .notice(NoticeKind::Warning, "Executing force delete...");
        }
        self.ui.notice(
            NoticeKind::Progress,
            &format!("Deleting '{}'...", target.display_path),
        );
        let outcome = self
            .client
            .delete_path(self.executor, &target.path, self.write_limits, cancelled)
            .await?;
        let output = match outcome {
            OneLakeWriteOutcome::Completed { status: 200 | 201 } => {
                Some(format!("'{}' deleted", target.display_path))
            }
            OneLakeWriteOutcome::Accepted => {
                Some(format!("'{}' is being deleted", target.display_path))
            }
            _ => None,
        };
        Ok(OneLakeWriteResult {
            outputs: output.map(|value| vec![message(value)]).unwrap_or_default(),
            outcome: Some(outcome),
            ..OneLakeWriteResult::default()
        })
    }

    /// Execute `mv`/`move` for the source-supported Lakehouse Files path branch.
    ///
    /// # Errors
    /// Preserves case-sensitive Files matching and source-only item-type check,
    /// confirmation, actual rename response and original success-message spelling.
    pub async fn rename(
        &mut self,
        source: &OneLakeWriteTarget,
        target: &OneLakeWriteTarget,
        force: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        let files = |target: &OneLakeWriteTarget| {
            let path = target
                .path
                .relative()
                .replace(".Shortcut", "")
                .replace(".shortcut", "");
            path == "Files" || path.starts_with("Files/")
        };
        if source.item_type.as_str() != "Lakehouse" || !files(source) || !files(target) {
            return Err(command(
                "NotSupported",
                "Invalid paths. Only supported within /Files",
            ));
        }
        if !force
            && !self
                .ui
                .confirm_result("Are you sure?")
                .await
                .map_err(OneLakeWriteError::Command)?
        {
            return Ok(declined());
        }
        self.ui.notice(
            NoticeKind::Progress,
            &format!(
                "Moving '{}' \u{2192} '{}'...",
                source.display_path.trim_start_matches('/'),
                target.display_path.trim_start_matches('/')
            ),
        );
        let outcome = self
            .client
            .rename_path(
                self.executor,
                &source.path,
                &target.path,
                self.write_limits,
                cancelled,
            )
            .await?;
        Ok(OneLakeWriteResult {
            outputs: if matches!(outcome.status(), 200 | 201) {
                vec![message("Move onelake file completed succesfully")]
            } else {
                Vec::new()
            },
            outcome: Some(outcome),
            ..OneLakeWriteResult::default()
        })
    }

    /// Upload caller-read local file bytes for the `cp` file-to-OneLake branch.
    ///
    /// The main filesystem host owns local path validation/read errors and must
    /// supply the actual bytes. This does not claim local filesystem orchestration.
    ///
    /// # Errors
    /// Preserves the source empty-copy rejection, supported destination check,
    /// file-name inference and exact source media-type inference.
    pub async fn upload_bytes(
        &mut self,
        target: &OneLakeWriteTarget,
        source_name: &str,
        content: &[u8],
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        writable(target, false)?;
        let target = destination(target, source_name)?;
        if content.is_empty() {
            return Err(command("InvalidInput", "Invalid copy, source is empty"));
        }
        let content_type = if target.path.relative().ends_with(".json") {
            "application/json"
        } else if target.path.relative().ends_with(".csv") {
            "text/csv"
        } else if target.path.relative().ends_with(".txt") {
            "text/plain"
        } else {
            "application/octet-stream"
        };
        let result = self
            .client
            .upload_file(
                self.executor,
                &target.path,
                content,
                content_type,
                self.write_limits,
                cancelled,
            )
            .await?;
        Ok(upload_result(result))
    }

    /// Execute a nonrecursive OneLake file-to-OneLake `cp`/`copy`.
    ///
    /// # Errors
    /// Source folders/shortcuts/tables are rejected rather than silently copied
    /// recursively. Media type and source length come from the actual GET, not
    /// an extra HEAD or ambient MIME/credential discovery.
    pub async fn copy_file(
        &mut self,
        source: &OneLakeWriteTarget,
        target: &OneLakeWriteTarget,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        writable(target, false)?;
        if source.kind != OneLakeResourceKind::File {
            return Err(command("NotSupported", "Recursive copy not supported"));
        }
        let name = source.path.relative().rsplit('/').next().unwrap_or("");
        let target = destination(target, name)?;
        self.ui.notice(
            NoticeKind::Progress,
            &format!(
                "Copying '{}' \u{2192} '{}'...",
                source.display_path.trim_start_matches('/'),
                target.display_path.trim_start_matches('/')
            ),
        );
        let mut cancelled = pin!(cancelled);
        let response = self
            .client
            .read_file(
                self.executor,
                &source.path,
                self.read_limits,
                cancelled.as_mut(),
            )
            .await
            .map_err(OneLakeWriteError::Read)?;
        let header = |name: &str| {
            response
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        let length = crate::sql_database::integer(header("content-length").unwrap_or("0"))
            .ok_or_else(|| {
                command(
                    "InvalidResponse",
                    "Source returned an invalid content length",
                )
            })?;
        if length.as_i64() == Some(0) || length.as_u64() == Some(0) {
            return Err(command("InvalidInput", "Invalid copy, source is empty"));
        }
        let content_type = header("content-type").unwrap_or("");
        let result = self
            .client
            .upload_file(
                self.executor,
                &target.path,
                &response.body,
                content_type,
                self.write_limits,
                cancelled.as_mut(),
            )
            .await?;
        Ok(upload_result(result))
    }
}

fn command(code: &str, message: impl Into<String>) -> OneLakeWriteError {
    OneLakeWriteError::Command(MutationError::new(code, message))
}
fn declined() -> OneLakeWriteResult {
    OneLakeWriteResult {
        declined: true,
        ..OneLakeWriteResult::default()
    }
}
fn upload_result(upload: OneLakeUpload) -> OneLakeWriteResult {
    OneLakeWriteResult {
        outputs: if upload.flush.is_some() {
            vec![message("Done")]
        } else {
            Vec::new()
        },
        upload: Some(upload),
        ..OneLakeWriteResult::default()
    }
}

fn destination(
    target: &OneLakeWriteTarget,
    name: &str,
) -> Result<OneLakeWriteTarget, OneLakeWriteError> {
    match target.kind {
        OneLakeResourceKind::Directory | OneLakeResourceKind::Shortcut => {
            if name.is_empty() || name.contains('/') {
                return Err(command("InvalidPath", "Invalid source file name"));
            }
            let mut target = target.clone();
            target.path = OneLakePath::new(
                target.path.workspace(),
                target.path.item(),
                &format!("{}/{}", target.path.relative(), name),
            )
            .map_err(OneLakeWriteError::Read)?;
            target.display_path = format!("{}/{}", target.display_path.trim_end_matches('/'), name);
            target.kind = OneLakeResourceKind::File;
            Ok(target)
        }
        OneLakeResourceKind::File | OneLakeResourceKind::Undefined => Ok(target.clone()),
        _ => Err(command(
            "InvalidPath",
            "Invalid destination, expected file or writable folder",
        )),
    }
}

/// Validate the source `cp` destination policy without dispatching or prompting.
///
/// Filesystem callers must invoke this **before** testing whether the local
/// source is a directory/file. This shares the existing upload/copy policy;
/// it does not read local files, discover context or call a storage executor.
///
/// # Errors
/// Returns the existing item-type/root diagnostic for a non-writable target.
pub fn validate_copy_destination(target: &OneLakeWriteTarget) -> Result<(), OneLakeWriteError> {
    writable(target, false)
}

/// Resolve the source file name against a previously validated copy destination.
///
/// Call after the local source directory/file checks but before reading bytes.
/// Existing files/unresolved paths are retained; folders/shortcuts gain the
/// actual source name. No read, upload, empty-body probe or request is performed.
///
/// # Errors
/// Preserves the existing destination-kind and bounded path/name failures.
pub fn file_destination(
    target: &OneLakeWriteTarget,
    source_name: &str,
) -> Result<OneLakeWriteTarget, OneLakeWriteError> {
    destination(target, source_name)
}

fn writable(target: &OneLakeWriteTarget, mkdir: bool) -> Result<(), OneLakeWriteError> {
    let supported: &[&str] = match target.item_type.as_str() {
        "Lakehouse" | "Warehouse" | "SQLDatabase" | "MirroredDatabase" | "MirroredWarehouse" => {
            &["Files"]
        }
        "SparkJobDefinition" => &["Libs", "Main"],
        "SemanticModel" | "KQLDatabase" => &[],
        _ => {
            return Err(command(
                "NotSupported",
                "This OneLake item type has no translated writable-root contract",
            ));
        }
    };
    let root = target.path.relative().split('/').next().unwrap_or("");
    if supported.contains(&root) {
        return Ok(());
    }
    let folders = format!(
        "[{}]",
        supported
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Err(command(
        "NotSupported",
        if mkdir {
            format!(
                "Cannot create folders under '{root}' for {}. Only {folders} folders are supported",
                target.item_type.as_str()
            )
        } else {
            format!(
                "Cannot write in folder '{root}' for {}. Only {folders} folders are supported",
                target.item_type.as_str()
            )
        },
    ))
}
