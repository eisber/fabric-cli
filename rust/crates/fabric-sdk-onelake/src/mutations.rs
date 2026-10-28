// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Exchange, OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeMethod, OneLakePath};
use azure_core::http::Url;
use std::{fmt, future::Future, num::NonZeroUsize, pin::pin};

/// Caller-selected request/response allocation bounds for OneLake writes.
#[derive(Clone, Copy, Debug)]
pub struct OneLakeWriteLimits {
    /// Largest file payload before any create/append request is dispatched.
    pub max_request_bytes: NonZeroUsize,
    /// Largest response, also enforced while the shared executor streams it.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for OneLakeWriteLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// The actual completed/accepted storage response, never a fabricated file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeWriteOutcome {
    /// A non-202 successful response.
    Completed {
        /// Actual HTTP status, including 204, retained for source output behavior.
        status: u16,
    },
    /// Accepted only, not evidence of completed storage work.
    Accepted,
}

impl OneLakeWriteOutcome {
    /// Actual HTTP status, without conflating 202 with completion.
    pub const fn status(self) -> u16 {
        match self {
            Self::Completed { status } => status,
            Self::Accepted => 202,
        }
    }
}

/// Successfully received upload stages, retained on later failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeWriteStep {
    /// Target file creation response was received.
    CreateFile,
    /// Content append response was received.
    Append,
}

/// A write failed or is incomplete, with explicit possible-side-effect evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakeMutationError {
    /// Exact focused SDK failure.
    pub kind: OneLakeError,
    /// A mutation may have reached storage; never permission to replay.
    pub mutation_may_have_occurred: bool,
    /// Upload stages whose successful responses were actually received.
    pub completed_steps: Vec<OneLakeWriteStep>,
}

impl fmt::Display for OneLakeMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "OneLake write failed or is incomplete; inspect failure and side-effect evidence",
        )
    }
}
impl std::error::Error for OneLakeMutationError {}

/// Actual upload responses, following the source's create/append/flush ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakeUpload {
    /// File-create response. Source only continues when this is exactly 201.
    pub create: OneLakeWriteOutcome,
    /// Append response, absent if file creation did not return 201.
    pub append: Option<OneLakeWriteOutcome>,
    /// Flush response, absent if not attempted.
    pub flush: Option<OneLakeWriteOutcome>,
}

impl OneLakeClient {
    /// Create a directory, preserving the source's `If-None-Match: *` condition.
    ///
    /// # Errors
    /// Unsafe/root paths, credentials, cancellation, bounded execution and service
    /// failures are explicit. Lost PUT responses are never automatically replayed.
    pub async fn create_directory(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeWriteLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeMutationError> {
        self.single_write(
            executor,
            directory_url(path, "resource", "directory").map(|url| Exchange {
                method: OneLakeMethod::Put,
                url,
                body: Vec::new(),
                headers: vec![("If-None-Match".into(), "*".into())],
                limit: limits.max_response_bytes,
            }),
            cancelled,
        )
        .await
    }

    /// Recursively delete a file/directory with the source query spelling.
    ///
    /// # Errors
    /// Rejects an item-root delete. Cancellation is not storage rollback.
    pub async fn delete_path(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeWriteLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeMutationError> {
        self.single_write(
            executor,
            directory_url(path, "recursive", "true").map(|url| Exchange {
                method: OneLakeMethod::Delete,
                url,
                body: Vec::new(),
                headers: Vec::new(),
                limit: limits.max_response_bytes,
            }),
            cancelled,
        )
        .await
    }

    /// Rename/move a path with the source-provided rename header.
    ///
    /// The SDK preserves the caller's explicit workspace/item selection and
    /// constructs both paths from validated resources, never arbitrary origins.
    ///
    /// # Errors
    /// Rejects item-root or non-ASCII rename headers before credential acquisition.
    pub async fn rename_path(
        &self,
        executor: &dyn OneLakeExecutor,
        source: &OneLakePath,
        target: &OneLakePath,
        limits: OneLakeWriteLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeMutationError> {
        let mut dispatched = false;
        let mut cancelled = pin!(cancelled);
        let result = async {
            writable(source)?;
            writable(target)?;
            let source = format!(
                "{}/{}/{}",
                source.workspace(),
                source.item(),
                source.relative()
            );
            if !source.is_ascii() {
                return Err(OneLakeError::InvalidInput);
            }
            let response = self
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Put,
                        url: target.url()?,
                        body: Vec::new(),
                        headers: vec![("x-ms-rename-source".into(), source)],
                        limit: limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            Ok(outcome(response.status))
        }
        .await;
        result.map_err(|kind| failure(kind, dispatched, Vec::new()))
    }

    /// Upload exact bytes through create, one append at zero, then byte-count flush.
    ///
    /// The existing HTTP executor owns framing/Content-Length; source media-type
    /// headers and byte positions are retained. No chunking/retry/runtime is added.
    ///
    /// # Errors
    /// Validates bounds/media type before creating a file. On partial failure the
    /// returned stages and mutation uncertainty are explicit; no cleanup/replay is
    /// invented. Accepted intermediate stages are retained rather than completed.
    pub async fn upload_file(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        content: &[u8],
        content_type: &str,
        limits: OneLakeWriteLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeUpload, OneLakeMutationError> {
        self.upload_file_impl(
            executor,
            path,
            content,
            content_type,
            limits,
            false,
            content.len(),
            cancelled,
        )
        .await
    }

    /// Upload source export bytes, omitting an empty append and retaining its position.
    ///
    /// Python's Lakehouse export flushes `len(data)`: default ASCII JSON and
    /// binary use their byte length, while ordinary Unicode text uses character
    /// length. The command codec supplies that explicit source position. No
    /// request is replayed, and actual create/append/flush responses are retained.
    ///
    /// # Errors
    /// Applies the same credential, path, bounds, cancellation and uncertain
    /// mutation policy as [`Self::upload_file`]. Positions larger than the byte
    /// payload are invalid and fail before create.
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit source upload policy and caller capabilities."
    )]
    pub async fn upload_export_file(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        content: &[u8],
        flush_position: usize,
        content_type: &str,
        limits: OneLakeWriteLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeUpload, OneLakeMutationError> {
        self.upload_file_impl(
            executor,
            path,
            content,
            content_type,
            limits,
            true,
            flush_position,
            cancelled,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Shared implementation preserves released upload behavior."
    )]
    async fn upload_file_impl(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        content: &[u8],
        content_type: &str,
        limits: OneLakeWriteLimits,
        omit_empty_append: bool,
        flush_position: usize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeUpload, OneLakeMutationError> {
        let mut dispatched = false;
        let mut steps = Vec::new();
        let mut cancelled = pin!(cancelled);
        let result = async {
            writable(path)?;
            if flush_position > content.len() {
                return Err(OneLakeError::InvalidInput);
            }
            if content.len() > limits.max_request_bytes.get() || content.len() > 4 * 1024 * 1024 {
                return Err(OneLakeError::Limit);
            }
            if content_type.is_empty()
                || !content_type.is_ascii()
                || content_type.chars().any(char::is_control)
                || content_type.len().saturating_mul(2).saturating_add(38) > 16 * 1024
            {
                return Err(OneLakeError::InvalidInput);
            }
            let created = self
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Put,
                        url: directory_url(path, "resource", "file")?,
                        body: Vec::new(),
                        headers: Vec::new(),
                        limit: limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            steps.push(OneLakeWriteStep::CreateFile);
            let mut result = OneLakeUpload {
                create: outcome(created.status),
                append: None,
                flush: None,
            };
            if created.status != 201 {
                return Ok(result);
            }
            if !omit_empty_append || !content.is_empty() {
                let mut append = path.url()?;
                append
                    .query_pairs_mut()
                    .append_pair("action", "append")
                    .append_pair("position", "0");
                let response = self
                    .exchange(
                        executor,
                        Exchange {
                            method: OneLakeMethod::Patch,
                            url: append,
                            body: content.to_vec(),
                            headers: vec![
                                ("Content-Type".into(), content_type.into()),
                                ("x-ms-content-type".into(), content_type.into()),
                            ],
                            limit: limits.max_response_bytes,
                        },
                        cancelled.as_mut(),
                        &mut dispatched,
                    )
                    .await?;
                result.append = Some(outcome(response.status));
                steps.push(OneLakeWriteStep::Append);
            }
            let mut flush = path.url()?;
            flush
                .query_pairs_mut()
                .append_pair("action", "flush")
                .append_pair("position", &flush_position.to_string());
            let response = self
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Patch,
                        url: flush,
                        body: Vec::new(),
                        headers: vec![("x-ms-content-type".into(), content_type.into())],
                        limit: limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            result.flush = Some(outcome(response.status));
            Ok(result)
        }
        .await;
        result.map_err(|kind| failure(kind, dispatched, steps))
    }

    async fn single_write(
        &self,
        executor: &dyn OneLakeExecutor,
        exchange: Result<Exchange, OneLakeError>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeMutationError> {
        let mut dispatched = false;
        let mut cancelled = pin!(cancelled);
        let result = async {
            let response = self
                .exchange(executor, exchange?, cancelled.as_mut(), &mut dispatched)
                .await?;
            Ok(outcome(response.status))
        }
        .await;
        result.map_err(|kind| failure(kind, dispatched, Vec::new()))
    }
}

fn writable(path: &OneLakePath) -> Result<(), OneLakeError> {
    if path.relative().is_empty() {
        return Err(OneLakeError::InvalidInput);
    }
    Ok(())
}

fn directory_url(path: &OneLakePath, key: &str, value: &str) -> Result<Url, OneLakeError> {
    writable(path)?;
    let mut url = path.url()?;
    url.path_segments_mut()
        .map_err(|_| OneLakeError::InvalidInput)?
        .push("");
    url.query_pairs_mut().append_pair(key, value);
    Ok(url)
}

fn outcome(status: u16) -> OneLakeWriteOutcome {
    if status == 202 {
        OneLakeWriteOutcome::Accepted
    } else {
        OneLakeWriteOutcome::Completed { status }
    }
}

fn failure(
    kind: OneLakeError,
    dispatched: bool,
    completed_steps: Vec<OneLakeWriteStep>,
) -> OneLakeMutationError {
    OneLakeMutationError {
        kind,
        mutation_may_have_occurred: dispatched,
        completed_steps,
    }
}
