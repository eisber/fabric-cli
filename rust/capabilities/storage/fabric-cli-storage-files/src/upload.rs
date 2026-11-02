// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CopyError, CopyErrorKind, CopyStage, Interruption, LocalError, LocalKind, StorageFilesHost,
    or_interrupt,
};
use fabric_cli_mutations::{
    MutationError, NoticeKind, OneLakeWriteHost, OneLakeWriteTarget, file_destination,
    validate_copy_destination,
};
use fabric_cli_output::OutputInput;
use fabric_sdk_onelake::{OneLakePath, OneLakeUpload, OneLakeWriteLimits};
use std::{
    future::Future,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::pin,
};

/// Actual local-to-OneLake result; output text is separate from storage completion.
#[derive(Debug)]
pub struct UploadResult {
    /// Source-compatible output envelopes; absent when create was not 201.
    pub outputs: Vec<OutputInput>,
    /// Exact local source used by the caller.
    pub local_path: PathBuf,
    /// Final storage resource, after directory/shortcut basename resolution.
    pub storage_path: OneLakePath,
    /// Actual create/append/flush responses; any 202 remains acceptance only.
    pub upload: OneLakeUpload,
    /// Predicate errors suppressed by Python, retained here as explicit evidence.
    pub inspection_errors: Vec<LocalError>,
}

impl StorageFilesHost<'_> {
    /// Copy one physical local file to OneLake in the original handler's order.
    ///
    /// Uses released pure destination policy, then native directory/file tests,
    /// destination naming, progress, bounded binary read and released upload.
    /// The actual source-file guard remains alive through all remote stages.
    /// These branches never inspect force/recursive/collision options or prompt.
    ///
    /// # Errors
    /// Rejects directories/non-files, empty files, invalid destinations and
    /// unsupported native names before mutation. Local/remote bounds and
    /// cancellation/deadline are explicit; storage failure retains original
    /// accepted stages and uncertainty, without replay or cleanup deletion.
    pub async fn upload(
        &mut self,
        source: &Path,
        target: &OneLakeWriteTarget,
        limits: OneLakeWriteLimits,
        interrupted: impl Future<Output = Interruption> + Send,
    ) -> Result<UploadResult, CopyError> {
        let failure = |stage, kind, storage_path: &OneLakePath, inspection_errors| CopyError {
            stage,
            kind,
            local_path: source.to_owned(),
            storage_path: Box::new(storage_path.clone()),
            local_write_may_have_occurred: false,
            inspection_errors,
        };
        let shared_failure = |error| CopyErrorKind::Upload {
            error: Box::new(error),
            interruption: None,
        };
        validate_copy_destination(target).map_err(|cause| {
            failure(
                CopyStage::Validate,
                shared_failure(cause),
                &target.path,
                Vec::new(),
            )
        })?;

        let mut signal = pin!(interrupted);
        let mut inspection_errors = Vec::new();
        let first = or_interrupt(async { self.local.kind(source).await }, signal.as_mut())
            .await
            .map_err(|reason| {
                failure(
                    CopyStage::Validate,
                    CopyErrorKind::Interrupted(reason),
                    &target.path,
                    Vec::new(),
                )
            })?;
        match first {
            Ok(LocalKind::Directory) => {
                return Err(failure(
                    CopyStage::Validate,
                    command("NotSupported", "Recursive copy not supported"),
                    &target.path,
                    inspection_errors,
                ));
            }
            Err(cause) => inspection_errors.push(cause),
            _ => {}
        }
        // Source performs two independent native predicates, not a cached stat.
        let second = or_interrupt(async { self.local.kind(source).await }, signal.as_mut()).await;
        let second = match second {
            Ok(result) => result,
            Err(reason) => {
                return Err(failure(
                    CopyStage::Validate,
                    CopyErrorKind::Interrupted(reason),
                    &target.path,
                    inspection_errors,
                ));
            }
        };
        match second {
            Ok(LocalKind::File) => {}
            other => {
                if let Err(cause) = other {
                    inspection_errors.push(cause);
                }
                return Err(failure(
                    CopyStage::Validate,
                    command("InvalidPath", "Invalid source, expected file"),
                    &target.path,
                    inspection_errors,
                ));
            }
        }
        let name = match source.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => {
                return Err(failure(
                    CopyStage::Validate,
                    command("InvalidPath", "Local source file name is not valid Unicode"),
                    &target.path,
                    inspection_errors,
                ));
            }
        };
        let destination = match file_destination(target, name) {
            Ok(value) => value,
            Err(cause) => {
                return Err(failure(
                    CopyStage::Validate,
                    shared_failure(cause),
                    &target.path,
                    inspection_errors,
                ));
            }
        };
        self.ui.notice(
            NoticeKind::Progress,
            &format!(
                "Copying '{}' \u{2192} '{}'...",
                source.display(),
                destination.display_path,
            ),
        );
        let bound = limits
            .max_request_bytes
            .min(NonZeroUsize::new(4 * 1024 * 1024).unwrap());
        let content = match or_interrupt(
            async { self.local.read_for_copy(source, bound).await },
            signal.as_mut(),
        )
        .await
        {
            Ok(Ok(content)) => content,
            Ok(Err(cause)) => {
                return Err(failure(
                    CopyStage::LocalRead,
                    CopyErrorKind::Local(cause),
                    &destination.path,
                    inspection_errors,
                ));
            }
            Err(reason) => {
                return Err(failure(
                    CopyStage::LocalRead,
                    CopyErrorKind::Interrupted(reason),
                    &destination.path,
                    inspection_errors,
                ));
            }
        };
        if content.content().len() > bound.get() {
            return Err(failure(
                CopyStage::LocalRead,
                command("Limit", "Local file reached its byte bound"),
                &destination.path,
                inspection_errors,
            ));
        }
        if content.content().is_empty() {
            return Err(failure(
                CopyStage::LocalRead,
                command("InvalidInput", "Invalid copy, source is empty"),
                &destination.path,
                inspection_errors,
            ));
        }
        let mut interruption = None;
        let result = OneLakeWriteHost {
            client: self.client,
            executor: self.executor,
            ui: self.ui,
            read_limits: self.read_limits,
            write_limits: limits,
        }
        .upload_bytes(&destination, name, content.content(), async {
            interruption = Some(signal.as_mut().await);
        })
        .await;
        // Keep the native read handle alive even when the remote call fails.
        drop(content);
        match result {
            Ok(result) => match result.upload {
                Some(upload) => Ok(UploadResult {
                    outputs: result.outputs,
                    local_path: source.to_owned(),
                    storage_path: destination.path,
                    upload,
                    inspection_errors,
                }),
                None => Err(failure(
                    CopyStage::StorageWrite,
                    command(
                        "InvalidResponse",
                        "Storage upload returned no response evidence",
                    ),
                    &destination.path,
                    inspection_errors,
                )),
            },
            Err(error) => Err(failure(
                CopyStage::StorageWrite,
                CopyErrorKind::Upload {
                    error: Box::new(error),
                    interruption,
                },
                &destination.path,
                inspection_errors,
            )),
        }
    }
}

fn command(code: &str, message: &str) -> CopyErrorKind {
    CopyErrorKind::Command(Box::new(MutationError::new(code, message)))
}
