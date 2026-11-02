// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Incremental, version-pinned file I/O over the caller's existing executor.
//!
//! No file-sized allocation, runtime, credential discovery or write retry is
//! installed. Read a [`OneLakeFileVersion`], consume bounded ranges, and append
//! bounded chunks to a conditional [`OneLakeFileUpload`]. Only its final flush
//! establishes completion; an HTTP 202 append acknowledges uncommitted bytes.

use crate::{
    Exchange, OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeMethod, OneLakePath,
    OneLakeResponse, OneLakeWriteOutcome,
};
use std::{collections::BTreeMap, fmt, future::Future, num::NonZeroUsize, pin::pin};

const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Finite allocations for each incremental operation, not a whole-file limit.
#[derive(Clone, Copy, Debug)]
pub struct OneLakeChunkLimits {
    /// Largest requested range or supplied append, at most 4 MiB.
    pub max_chunk_bytes: NonZeroUsize,
    /// Largest service response, enforced by the existing executor.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for OneLakeChunkLimits {
    fn default() -> Self {
        Self {
            max_chunk_bytes: NonZeroUsize::new(MAX_CHUNK_BYTES).unwrap(),
            max_response_bytes: NonZeroUsize::new(MAX_CHUNK_BYTES).unwrap(),
        }
    }
}

impl OneLakeChunkLimits {
    fn validate(self) -> Result<(), OneLakeError> {
        if self.max_chunk_bytes.get() > MAX_CHUNK_BYTES {
            return Err(OneLakeError::Limit);
        }
        Ok(())
    }
}

/// Validated HEAD snapshot bound to one exact resource and a strong ETag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakeFileVersion {
    path: OneLakePath,
    length: u64,
    etag: String,
    content_type: Option<String>,
}

impl OneLakeFileVersion {
    /// Exact declared file length, including zero.
    pub const fn len(&self) -> u64 {
        self.length
    }

    /// Whether this version contains no bytes.
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Strong source ETag sent unchanged in every range's `If-Match`.
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// Validated source media type, when returned by HEAD.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
}

/// One completely validated range; never a partial or truncated success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakeReadRange {
    /// Exact starting offset in the pinned file version.
    pub offset: u64,
    /// At most the requested number of bytes.
    pub body: Vec<u8>,
    /// True only when this range ends exactly at the declared source length.
    pub eof: bool,
}

/// Explicit collision protection for destination creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OneLakeFileCreateCondition {
    /// Create only if no destination exists (`If-None-Match: *`).
    IfAbsent,
    /// Replace only this strong destination ETag (`If-Match`).
    IfMatch(String),
}

/// Incremental write failure with bounded response and side-effect evidence.
#[derive(Clone, Debug)]
pub struct OneLakeChunkError {
    /// Existing typed credential, cancellation, execution or service failure.
    pub kind: OneLakeError,
    /// Some write may have reached storage; never permission to replay.
    pub mutation_may_have_occurred: bool,
    /// Bytes acknowledged before this operation; not flushed/completed bytes.
    pub acknowledged_position: u64,
    /// Received nonconforming/accepted response, retained as untrusted evidence.
    ///
    /// HTTP failures instead retain their service code/body in `kind`.
    pub response: Option<OneLakeResponse>,
}

impl fmt::Display for OneLakeChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("incremental OneLake write failed or is incomplete")
    }
}

impl std::error::Error for OneLakeChunkError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UploadState {
    Active,
    Uncertain,
    Flushed,
}

/// One conditional file creation followed by ordered, single-attempt appends.
///
/// The session owns no transport or runtime and borrows an executor per call.
/// A lost or invalid write response permanently blocks subsequent appends and
/// flushes on this session. Dropping an in-flight append/flush future also blocks
/// further writes, even when dispatch cannot be proven. Dropping the session
/// neither flushes nor deletes the target.
pub struct OneLakeFileUpload {
    client: OneLakeClient,
    path: OneLakePath,
    length: u64,
    position: u64,
    etag: String,
    content_type: String,
    limits: OneLakeChunkLimits,
    state: UploadState,
}

impl fmt::Debug for OneLakeFileUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OneLakeFileUpload")
            .field("length", &self.length)
            .field("acknowledged_position", &self.position)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl OneLakeClient {
    /// Read and validate exact source length, strong ETag and optional media type.
    ///
    /// # Errors
    /// Rejects item roots, non-file properties, missing/duplicate/malformed
    /// length or ETag, and any non-200 HEAD. Service failures stay typed.
    pub async fn file_version(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeChunkLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeFileVersion, OneLakeError> {
        limits.validate()?;
        file_path(path)?;
        let mut cancelled = pin!(cancelled);
        let response = self
            .exchange(
                executor,
                Exchange {
                    method: OneLakeMethod::Head,
                    url: path.url()?,
                    body: Vec::new(),
                    headers: vec![("Accept-Encoding".into(), "identity".into())],
                    limit: limits.max_response_bytes,
                },
                cancelled.as_mut(),
                &mut false,
            )
            .await?;
        if response.status != 200
            || !response.body.is_empty()
            || unique_header(&response.headers, "x-ms-resource-type")?
                .is_some_and(|value| !value.eq_ignore_ascii_case("file"))
        {
            return Err(OneLakeError::InvalidResponse);
        }
        identity_encoding(&response.headers)?;
        let length = decimal(required_header(&response.headers, "content-length")?)?;
        let etag = required_header(&response.headers, "etag")?;
        if !strong_etag(etag) {
            return Err(OneLakeError::InvalidResponse);
        }
        let content_type = unique_header(&response.headers, "content-type")?;
        if content_type.is_some_and(|value| !safe_header(value)) {
            return Err(OneLakeError::InvalidResponse);
        }
        Ok(OneLakeFileVersion {
            path: path.clone(),
            length,
            etag: etag.into(),
            content_type: content_type.map(str::to_owned),
        })
    }

    /// Read one bounded range pinned to the exact HEAD snapshot.
    ///
    /// The last request is clipped to the remaining bytes. At exact EOF,
    /// including a zero-byte version, returns an empty range without a GET.
    /// A nonempty range requires HTTP 206 with exact ETag, Content-Range,
    /// Content-Length and body length; ignored ranges never succeed.
    ///
    /// # Errors
    /// Invalid path/version/offset/bounds fail before dispatch. Source changes,
    /// malformed/truncated responses, service and cancellation failures stay typed.
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit version, offsets and caller capabilities."
    )]
    pub async fn read_range(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        version: &OneLakeFileVersion,
        offset: u64,
        length: NonZeroUsize,
        limits: OneLakeChunkLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeReadRange, OneLakeError> {
        limits.validate()?;
        if path != &version.path || offset > version.length {
            return Err(OneLakeError::InvalidInput);
        }
        if length.get() > limits.max_chunk_bytes.get() {
            return Err(OneLakeError::Limit);
        }
        if self.selection.audience() != crate::Audience::OneLake {
            return Err(OneLakeError::InvalidAudience);
        }
        let mut cancelled = pin!(cancelled);
        crate::or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        let count = u64::try_from(length.get())
            .map_err(|_| OneLakeError::Limit)?
            .min(version.length - offset);
        if count == 0 {
            return Ok(OneLakeReadRange {
                offset,
                body: Vec::new(),
                eof: true,
            });
        }
        if count
            > u64::try_from(limits.max_response_bytes.get()).map_err(|_| OneLakeError::Limit)?
        {
            return Err(OneLakeError::Limit);
        }
        let end = offset + count - 1;
        let response = self
            .exchange_impl(
                executor,
                Exchange {
                    method: OneLakeMethod::Get,
                    url: path.url()?,
                    body: Vec::new(),
                    headers: vec![
                        ("Range".into(), format!("bytes={offset}-{end}")),
                        ("If-Match".into(), version.etag.clone()),
                        ("Accept-Encoding".into(), "identity".into()),
                    ],
                    limit: limits.max_response_bytes,
                },
                cancelled.as_mut(),
                &mut false,
                true,
            )
            .await?;
        if response.status != 206
            || required_header(&response.headers, "etag")? != version.etag
            || decimal(required_header(&response.headers, "content-length")?)? != count
            || u64::try_from(response.body.len()).map_err(|_| OneLakeError::Limit)? != count
            || content_range(required_header(&response.headers, "content-range")?)?
                != (offset, end, version.length)
        {
            return Err(OneLakeError::InvalidResponse);
        }
        identity_encoding(&response.headers)?;
        Ok(OneLakeReadRange {
            offset,
            body: response.body,
            eof: end + 1 == version.length,
        })
    }

    /// Conditionally create a file for incremental upload of a declared length.
    ///
    /// Only HTTP 201 with a strong destination ETag creates a usable session.
    /// All later appends/flushes carry `If-Match`. No entire-file allocation or
    /// request retry occurs; zero-byte files still require an explicit flush.
    ///
    /// # Errors
    /// Validates path, condition, media type and per-request bounds before writes.
    /// Accepted/uncertain creation fails with response/side-effect evidence.
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit creation policy and caller capabilities."
    )]
    pub async fn start_file_upload(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        length: u64,
        content_type: &str,
        condition: OneLakeFileCreateCondition,
        limits: OneLakeChunkLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeFileUpload, OneLakeChunkError> {
        let mut dispatched = false;
        let mut evidence = None;
        let mut cancelled = pin!(cancelled);
        let result = async {
            limits.validate()?;
            file_path(path)?;
            if !safe_header(content_type) {
                return Err(OneLakeError::InvalidInput);
            }
            let condition = match condition {
                OneLakeFileCreateCondition::IfAbsent => ("If-None-Match".into(), "*".into()),
                OneLakeFileCreateCondition::IfMatch(etag) if strong_etag(&etag) => {
                    ("If-Match".into(), etag)
                }
                OneLakeFileCreateCondition::IfMatch(_) => return Err(OneLakeError::InvalidInput),
            };
            let mut url = path.url()?;
            url.path_segments_mut()
                .map_err(|_| OneLakeError::InvalidInput)?
                .push("");
            url.query_pairs_mut().append_pair("resource", "file");
            let response = self
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Put,
                        url,
                        body: Vec::new(),
                        headers: vec![condition, ("x-ms-content-type".into(), content_type.into())],
                        limit: limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            evidence = Some(response);
            let response = evidence.as_ref().unwrap();
            empty_write_response(response)?;
            let etag = required_header(&response.headers, "etag")?;
            if response.status != 201 || !strong_etag(etag) {
                return Err(OneLakeError::InvalidResponse);
            }
            Ok(OneLakeFileUpload {
                client: self.clone(),
                path: path.clone(),
                length,
                position: 0,
                etag: etag.into(),
                content_type: content_type.into(),
                limits,
                state: UploadState::Active,
            })
        }
        .await;
        result.map_err(|kind| OneLakeChunkError {
            kind,
            mutation_may_have_occurred: dispatched,
            acknowledged_position: 0,
            response: evidence,
        })
    }
}

impl OneLakeFileUpload {
    /// Declared final length, not an allocation bound.
    pub const fn len(&self) -> u64 {
        self.length
    }

    /// Whether the declared file is empty.
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Next required append position; accepted appends are not yet flushed.
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// True only after a validated final HTTP 200 flush.
    pub fn is_complete(&self) -> bool {
        self.state == UploadState::Flushed
    }

    /// Append exactly one nonempty bounded chunk at the next declared position.
    ///
    /// HTTP 202 is normal append acknowledgement, not whole-file completion.
    /// A returned ETag, when present, becomes the next write's condition.
    ///
    /// # Errors
    /// Rejects gaps, overlap, overflow, oversized chunks and writes past the
    /// declared total before dispatch. Once execution begins, any error or
    /// dropped future poisons the session, conservatively including failures
    /// before dispatch. Purely local validation failures leave it usable.
    pub async fn append_at(
        &mut self,
        executor: &dyn OneLakeExecutor,
        position: u64,
        content: &[u8],
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeChunkError> {
        let mut dispatched = false;
        let mut evidence = None;
        let mut cancelled = pin!(cancelled);
        let result = async {
            self.active()?;
            if content.is_empty() || position != self.position {
                return Err(OneLakeError::InvalidInput);
            }
            if content.len() > self.limits.max_chunk_bytes.get() {
                return Err(OneLakeError::Limit);
            }
            let next = position
                .checked_add(u64::try_from(content.len()).map_err(|_| OneLakeError::Limit)?)
                .filter(|next| *next <= self.length)
                .ok_or(OneLakeError::InvalidInput)?;
            let mut url = self.path.url()?;
            url.query_pairs_mut()
                .append_pair("action", "append")
                .append_pair("position", &position.to_string());
            // A dropped future cannot run the error handler after dispatch.
            self.state = UploadState::Uncertain;
            let response = self
                .client
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Patch,
                        url,
                        body: content.to_vec(),
                        headers: vec![
                            ("If-Match".into(), self.etag.clone()),
                            ("Content-Type".into(), self.content_type.clone()),
                        ],
                        limit: self.limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            evidence = Some(response);
            let response = evidence.as_ref().unwrap();
            empty_write_response(response)?;
            if response.status != 202 {
                return Err(OneLakeError::InvalidResponse);
            }
            if let Some(etag) = unique_header(&response.headers, "etag")? {
                if !strong_etag(etag) {
                    return Err(OneLakeError::InvalidResponse);
                }
                self.etag = etag.into();
            }
            self.position = next;
            self.state = UploadState::Active;
            Ok(OneLakeWriteOutcome::Accepted)
        }
        .await;
        result.map_err(|kind| self.failure(kind, dispatched, evidence))
    }

    /// Flush once, only after every declared byte has been acknowledged.
    ///
    /// # Errors
    /// Early/duplicate flushes fail locally. HTTP 202 is incomplete and poisons
    /// the session, retaining the actual response; no retry or cleanup follows.
    /// Dropping an executing flush future also permanently blocks further writes.
    pub async fn flush(
        &mut self,
        executor: &dyn OneLakeExecutor,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteOutcome, OneLakeChunkError> {
        let mut dispatched = false;
        let mut evidence = None;
        let mut cancelled = pin!(cancelled);
        let result = async {
            self.active()?;
            if self.position != self.length {
                return Err(OneLakeError::InvalidInput);
            }
            let mut url = self.path.url()?;
            url.query_pairs_mut()
                .append_pair("action", "flush")
                .append_pair("position", &self.position.to_string())
                .append_pair("close", "true");
            self.state = UploadState::Uncertain;
            let response = self
                .client
                .exchange(
                    executor,
                    Exchange {
                        method: OneLakeMethod::Patch,
                        url,
                        body: Vec::new(),
                        headers: vec![
                            ("If-Match".into(), self.etag.clone()),
                            ("x-ms-content-type".into(), self.content_type.clone()),
                        ],
                        limit: self.limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                    &mut dispatched,
                )
                .await?;
            evidence = Some(response);
            let response = evidence.as_ref().unwrap();
            empty_write_response(response)?;
            if response.status != 200 {
                return Err(OneLakeError::InvalidResponse);
            }
            if unique_header(&response.headers, "etag")?.is_some_and(|etag| !strong_etag(etag)) {
                return Err(OneLakeError::InvalidResponse);
            }
            self.state = UploadState::Flushed;
            Ok(OneLakeWriteOutcome::Completed { status: 200 })
        }
        .await;
        result.map_err(|kind| self.failure(kind, dispatched, evidence))
    }

    fn active(&self) -> Result<(), OneLakeError> {
        if self.state != UploadState::Active {
            return Err(OneLakeError::InvalidInput);
        }
        Ok(())
    }

    fn failure(
        &mut self,
        kind: OneLakeError,
        dispatched: bool,
        response: Option<OneLakeResponse>,
    ) -> OneLakeChunkError {
        if dispatched {
            self.state = UploadState::Uncertain;
        }
        OneLakeChunkError {
            kind,
            // Creation already happened, even when this particular call was invalid.
            mutation_may_have_occurred: true,
            acknowledged_position: self.position,
            response,
        }
    }
}

fn file_path(path: &OneLakePath) -> Result<(), OneLakeError> {
    if path.relative().is_empty() {
        return Err(OneLakeError::InvalidInput);
    }
    Ok(())
}

fn safe_header(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.is_ascii()
        && !value.chars().any(char::is_control)
}

fn strong_etag(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= 4096
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..value.len() - 1]
            .iter()
            .all(|byte| matches!(*byte, 0x21 | 0x23..=0x7e))
}

fn unique_header<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<Option<&'a str>, OneLakeError> {
    let mut values = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str());
    let value = values.next();
    if values.next().is_some() {
        return Err(OneLakeError::InvalidResponse);
    }
    Ok(value)
}

fn required_header<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, OneLakeError> {
    unique_header(headers, name)?.ok_or(OneLakeError::InvalidResponse)
}

fn decimal(value: &str) -> Result<u64, OneLakeError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OneLakeError::InvalidResponse);
    }
    value.parse().map_err(|_| OneLakeError::InvalidResponse)
}

fn content_range(value: &str) -> Result<(u64, u64, u64), OneLakeError> {
    let (interval, total) = value
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
        .ok_or(OneLakeError::InvalidResponse)?;
    let (start, end) = interval
        .split_once('-')
        .ok_or(OneLakeError::InvalidResponse)?;
    let (start, end, total) = (decimal(start)?, decimal(end)?, decimal(total)?);
    if start > end || end >= total {
        return Err(OneLakeError::InvalidResponse);
    }
    Ok((start, end, total))
}

fn identity_encoding(headers: &BTreeMap<String, String>) -> Result<(), OneLakeError> {
    if unique_header(headers, "content-encoding")?
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(OneLakeError::InvalidResponse);
    }
    Ok(())
}

fn empty_write_response(response: &OneLakeResponse) -> Result<(), OneLakeError> {
    if !response.body.is_empty()
        || unique_header(&response.headers, "content-length")?
            .map(decimal)
            .transpose()?
            .is_some_and(|length| length != 0)
    {
        return Err(OneLakeError::InvalidResponse);
    }
    Ok(())
}
