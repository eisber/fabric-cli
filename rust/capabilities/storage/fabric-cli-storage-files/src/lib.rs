// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Physical local/OneLake copy orchestration over caller-owned SDK and local I/O.
//!
//! This library does not resolve Fabric paths, parse commands, prompt, discover
//! credentials, start a runtime, or implement a second HTTP/operation engine.

mod local;
mod resource;
mod selected;
mod upload;
pub use resource::{ResourceFileError, ResourceFileHost, ResourceFileResult};
pub use selected::SelectedFileSystem;
pub use upload::UploadResult;

pub use local::{
    LocalError, LocalErrorKind, LocalFileSystem, LocalKind, LocalReadFile, OsFileSystem,
};

use fabric_cli_mutations::{
    MutationError, MutationUi, NoticeKind, OneLakeReadError, OneLakeResourceKind,
    OneLakeWriteError, OneLakeWriteTarget,
};
use fabric_cli_output::OutputInput;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeLimits, OneLakePath,
};
use std::{
    fmt,
    future::{Future, poll_fn},
    path::{Path, PathBuf},
    pin::{Pin, pin},
    task::Poll,
};

/// Transfer phase, retained with errors instead of claiming rollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyStage {
    /// Source/target validation before copying.
    Validate,
    /// Read a local source before any storage mutation.
    LocalRead,
    /// OneLake file read before a local write.
    StorageRead,
    /// Local create/truncate/write.
    LocalWrite,
    /// Storage create/append/flush, retaining its actual partial-stage evidence.
    StorageWrite,
}

/// A caller's explicit interruption reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interruption {
    /// Caller cancellation; no rollback is inferred.
    Cancelled,
    /// Caller-selected whole-transfer deadline.
    Deadline,
}

/// Structured underlying transfer failure.
#[derive(Debug)]
pub enum CopyErrorKind {
    /// Source-compatible command validation.
    Command(Box<MutationError>),
    /// Local I/O, possibly with partial bytes/truncation.
    Local(LocalError),
    /// Actual SDK file read failure.
    Storage(OneLakeError),
    /// Released upload policy/SDK failure, with interruption reason retained.
    Upload {
        /// Original command/read/mutation error; no storage uncertainty is erased.
        error: Box<OneLakeWriteError>,
        /// Caller cancellation/deadline, when it interrupted storage execution.
        interruption: Option<Interruption>,
    },
    /// Caller interruption while awaiting local I/O.
    Interrupted(Interruption),
}

/// Failed transfer, including local path and possible side effects as data.
#[derive(Debug)]
pub struct CopyError {
    /// Where execution stopped.
    pub stage: CopyStage,
    /// Actual failure.
    pub kind: CopyErrorKind,
    /// Selected local source/destination, never printed by Display.
    pub local_path: PathBuf,
    /// Exact selected storage workspace/item/resource IDs, never printed by Display.
    pub storage_path: Box<OneLakePath>,
    /// A local create/truncate/write may have started.
    pub local_write_may_have_occurred: bool,
    /// Predicate failures suppressed by Python, retained here as explicit evidence.
    pub inspection_errors: Vec<LocalError>,
}

impl CopyError {
    /// Source-shaped diagnostic through the existing output layer.
    ///
    /// Native OS diagnostic suffixes are platform-specific. Typed uncertainty
    /// and paths remain on this object, not flattened into a success message.
    pub fn output(&self) -> OutputInput {
        match &self.kind {
            CopyErrorKind::Command(error) => error.output(),
            CopyErrorKind::Storage(error) => OneLakeReadError::Sdk(error.clone()).output(),
            CopyErrorKind::Upload { error, .. } => error.output(),
            CopyErrorKind::Local(error) => MutationError::new(
                if matches!(error.kind, LocalErrorKind::Limit) {
                    "Limit"
                } else {
                    "InvalidPath"
                },
                if self.stage == CopyStage::Validate {
                    format!("Invalid local destination: {}", error.diagnostic())
                } else {
                    format!(
                        "Error {} file: {}",
                        if self.stage == CopyStage::LocalWrite {
                            "writing"
                        } else {
                            "reading"
                        },
                        error.diagnostic(),
                    )
                },
            )
            .output(),
            CopyErrorKind::Interrupted(reason) => MutationError::new(
                match reason {
                    Interruption::Cancelled => "Cancelled",
                    Interruption::Deadline => "TimedOut",
                },
                "Local copy interrupted; a started write is not rolled back",
            )
            .output(),
        }
    }
}
impl fmt::Display for CopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("file copy failed; inspect stage, paths and side-effect evidence")
    }
}
impl std::error::Error for CopyError {}

/// Actual copy outcome and source output, not a live command parity claim.
#[derive(Debug)]
pub struct CopyResult {
    /// Existing output envelopes, empty for source-unhandled kinds/statuses.
    pub outputs: Vec<OutputInput>,
    /// Selected final local path, after directory basename selection.
    pub local_path: PathBuf,
    /// Exact selected storage workspace/item/resource IDs.
    pub storage_path: OneLakePath,
    /// Actual successful local write count; absent when no write was attempted.
    pub bytes_written: Option<usize>,
    /// Predicate failures suppressed by Python, retained as explicit evidence.
    pub inspection_errors: Vec<LocalError>,
}

/// Physical file consumer; the existing host resolves context and owns rendering.
pub struct StorageFilesHost<'a> {
    /// Explicit storage credentials/account/tenant/audience selected by the caller.
    pub client: &'a OneLakeClient,
    /// Released OneLake adapter over shared bounded HTTP.
    pub executor: &'a dyn OneLakeExecutor,
    /// Injected local I/O, e.g. the real bounded OS adapter.
    pub local: &'a dyn LocalFileSystem,
    /// Caller-owned source notices; these copy branches never confirm.
    pub ui: &'a mut dyn MutationUi,
    /// Exact bounded file response limits.
    pub read_limits: OneLakeLimits,
}

impl StorageFilesHost<'_> {
    /// Copy a resolved OneLake file to a native local path, preserving binary bytes.
    ///
    /// `force`, `recursive` and collision options are intentionally not consulted:
    /// the pinned file handler does not inspect them or ask for confirmation.
    /// Directories/shortcuts/tables are rejected; undefined source is a no-op.
    ///
    /// # Errors
    /// No missing parent is created. Empty/failed remote reads leave the local
    /// target untouched; local write failures can leave a truncated/partial file.
    /// Cancellation drops local work cooperatively and is not rollback.
    pub async fn download(
        &mut self,
        source: &OneLakeWriteTarget,
        target: &Path,
        interrupted: impl Future<Output = Interruption> + Send,
    ) -> Result<CopyResult, CopyError> {
        let mut local_path = target.to_owned();
        let mut inspection_errors = Vec::new();
        let mut signal = pin!(interrupted);
        let error = |stage, kind, path: &Path, inspection_errors| CopyError {
            stage,
            kind,
            local_path: path.to_owned(),
            storage_path: Box::new(source.path.clone()),
            local_write_may_have_occurred: stage == CopyStage::LocalWrite,
            inspection_errors,
        };
        match source.kind {
            OneLakeResourceKind::File => {}
            OneLakeResourceKind::Undefined => {
                return Ok(CopyResult {
                    local_path,
                    storage_path: source.path.clone(),
                    outputs: Vec::new(),
                    bytes_written: None,
                    inspection_errors,
                });
            }
            _ => {
                return Err(error(
                    CopyStage::Validate,
                    CopyErrorKind::Command(Box::new(MutationError::new(
                        "NotSupported",
                        "Recursive copy not supported",
                    ))),
                    target,
                    inspection_errors,
                ));
            }
        }
        let kind = or_interrupt(async { self.local.kind(target).await }, signal.as_mut())
            .await
            .map_err(|reason| {
                error(
                    CopyStage::Validate,
                    CopyErrorKind::Interrupted(reason),
                    target,
                    Vec::new(),
                )
            })?;
        match kind {
            Ok(LocalKind::Directory) => {
                let name = source.path.relative().rsplit('/').next().unwrap_or("");
                local_path = local::child_path(target, name).map_err(|cause| {
                    error(
                        CopyStage::Validate,
                        CopyErrorKind::Local(cause),
                        target,
                        Vec::new(),
                    )
                })?;
            }
            Err(cause) => inspection_errors.push(cause),
            _ => {}
        }
        self.ui.notice(
            NoticeKind::Progress,
            &format!(
                "Copying '{}' \u{2192} '{}'...",
                source.display_path,
                local_path.display()
            ),
        );
        let mut interruption = None;
        let response = self
            .client
            .read_file(self.executor, &source.path, self.read_limits, async {
                interruption = Some(signal.as_mut().await);
            })
            .await
            .map_err(|cause| {
                error(
                    CopyStage::StorageRead,
                    interruption
                        .map(CopyErrorKind::Interrupted)
                        .unwrap_or(CopyErrorKind::Storage(cause)),
                    &local_path,
                    Vec::new(),
                )
            });
        let response = match response {
            Ok(response) => response,
            Err(mut failure) => {
                failure.inspection_errors = inspection_errors;
                return Err(failure);
            }
        };
        if response.body.is_empty() {
            return Err(error(
                CopyStage::StorageRead,
                CopyErrorKind::Command(Box::new(MutationError::new(
                    "InvalidInput",
                    "Invalid copy, source is empty",
                ))),
                &local_path,
                inspection_errors,
            ));
        }
        let expected = response.body.len();
        let write = or_interrupt(
            async { self.local.write(&local_path, response.body).await },
            signal.as_mut(),
        )
        .await;
        let bytes_written = match write {
            Ok(Ok(size)) if size == expected => size,
            Ok(Ok(size)) => {
                return Err(error(
                    CopyStage::LocalWrite,
                    CopyErrorKind::Local(LocalError {
                        kind: LocalErrorKind::Io(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "Local adapter returned an incomplete write",
                        )),
                        destination_opened: true,
                        bytes_written: size,
                    }),
                    &local_path,
                    inspection_errors,
                ));
            }
            Ok(Err(cause)) => {
                return Err(error(
                    CopyStage::LocalWrite,
                    CopyErrorKind::Local(cause),
                    &local_path,
                    inspection_errors,
                ));
            }
            Err(reason) => {
                return Err(error(
                    CopyStage::LocalWrite,
                    CopyErrorKind::Interrupted(reason),
                    &local_path,
                    inspection_errors,
                ));
            }
        };
        Ok(CopyResult {
            outputs: vec![OutputInput {
                message: Some("Done".into()),
                ..OutputInput::default()
            }],
            local_path,
            storage_path: source.path.clone(),
            bytes_written: Some(bytes_written),
            inspection_errors,
        })
    }
}

async fn or_interrupt<T>(
    future: impl Future<Output = T>,
    mut interrupted: Pin<&mut (impl Future<Output = Interruption> + Send)>,
) -> Result<T, Interruption> {
    let mut future = pin!(future);
    poll_fn(|context| {
        if let Poll::Ready(reason) = interrupted.as_mut().poll(context) {
            return Poll::Ready(Err(reason));
        }
        future.as_mut().poll(context).map(Ok)
    })
    .await
}
