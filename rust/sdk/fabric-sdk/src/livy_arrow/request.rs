// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ArrowError, codec::validate_id};
use crate::runtime::{NativeEffect, NativeEndpoint, NativeMethod, NativeRequest, validate_segment};
use serde_json::{Value, json};

/// Bare inline IPC response representation.
pub const ARROW_STREAM_MEDIA_TYPE: &str = "application/vnd.apache.arrow.stream";
/// Framed result request representation; successful responses require `v=2`.
pub const ARROW_CHUNKS_MEDIA_TYPE: &str = "application/vnd.livy.arrow-chunks";
const INLINE_OR_JSON: &str = "application/vnd.apache.arrow.stream, application/json;q=0.9";
const MAX_SELECTOR_BYTES: usize = 65_536;

#[derive(Clone, Eq, PartialEq)]
enum Route {
    Session { session: String, repl: Option<String> },
    HighConcurrency { execution_session: String, repl: String },
}

/// Complete caller-selected endpoint and execution-owner route.
///
/// For Fabric, pass a base ending in the exact Fabric/workspace/lakehouse Livy
/// API prefix. For native Livy, pass the native service base. There is no
/// endpoint discovery and HC management IDs are never inferred.
#[derive(Clone, Eq, PartialEq)]
pub struct StatementOwner {
    endpoint: NativeEndpoint,
    route: Route,
}

impl std::fmt::Debug for StatementOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StatementOwner(<caller-selected execution scope>)")
    }
}

impl StatementOwner {
    /// Validate a classic session or an explicitly owned native REPL.
    pub fn session(
        endpoint: NativeEndpoint,
        session_id: &str,
        repl_id: Option<&str>,
    ) -> Result<Self, ArrowError> {
        validate_owner_id(session_id)?;
        if let Some(repl) = repl_id {
            validate_owner_id(repl)?;
        }
        Ok(Self {
            endpoint,
            route: Route::Session {
                session: session_id.into(),
                repl: repl_id.map(String::from),
            },
        })
    }

    /// Validate the HC execution-session/REPL tuple, not the acquisition ID.
    pub fn high_concurrency(
        endpoint: NativeEndpoint,
        execution_session_id: &str,
        repl_id: &str,
    ) -> Result<Self, ArrowError> {
        validate_owner_id(execution_session_id)?;
        validate_owner_id(repl_id)?;
        Ok(Self {
            endpoint,
            route: Route::HighConcurrency {
                execution_session: execution_session_id.into(),
                repl: repl_id.into(),
            },
        })
    }

    fn segments(&self) -> Vec<&str> {
        match &self.route {
            Route::Session { session, repl } => {
                let mut segments = vec!["sessions", session.as_str()];
                if let Some(repl) = repl {
                    segments.extend(["repls", repl.as_str()]);
                }
                segments.push("statements");
                segments
            }
            Route::HighConcurrency { execution_session, repl } => vec![
                "highConcurrencySessions", execution_session, "repls", repl, "statements",
            ],
        }
    }
}

fn validate_owner_id(value: &str) -> Result<(), ArrowError> {
    if value.len() > 4096 {
        return Err(ArrowError::InvalidInput);
    }
    validate_segment(value).map_err(Into::into)
}

/// An owner-bound statement handle; a numeric ID alone is not a result address.
#[derive(Clone, Eq, PartialEq)]
pub struct StatementHandle {
    owner: StatementOwner,
    id: u64,
}

impl std::fmt::Debug for StatementHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StatementHandle(<owner-bound>)")
    }
}

impl StatementHandle {
    /// Bind an authoritative nonnegative statement ID to its execution owner.
    pub const fn new(owner: StatementOwner, statement_id: u64) -> Self {
        Self { owner, id: statement_id }
    }
    /// The full execution owner, including the endpoint.
    pub const fn owner(&self) -> &StatementOwner {
        &self.owner
    }
    /// The statement ID, never an HC `id` response header.
    pub const fn statement_id(&self) -> u64 {
        self.id
    }

    /// Build a single framed GET. Does not dispatch, poll or retry.
    pub fn result_request(&self, selectors: &ResultSelectors) -> Result<ArrowRequest, ArrowError> {
        let id = self.id.to_string();
        let mut segments = self.owner.segments();
        segments.extend([id.as_str(), "result"]);
        let native = self.owner.endpoint.request(
            NativeMethod::Get,
            &segments,
            &selectors.query(),
            None,
            NativeEffect::Read,
        )?.with_accept(ARROW_CHUNKS_MEDIA_TYPE)?;
        Ok(ArrowRequest {
            native,
            owner: self.owner.clone(),
            mode: RequestMode::Framed,
            resource_id: None,
        })
    }
}

/// Validated public chunk selectors, never JSON row/page or internal page-v1 cursors.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ResultSelectors {
    after_chunk: Option<String>,
    parts: Option<String>,
    from_part: Option<i32>,
}

impl std::fmt::Debug for ResultSelectors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResultSelectors(<redacted>)")
    }
}

impl ResultSelectors {
    /// Bound opaque selector text and reject controls/negative partitions.
    ///
    /// `parts` is forwarded verbatim: no undocumented server grammar is assumed.
    /// When both cursor and partition are supplied, validate their ownership.
    pub fn new(
        after_chunk: Option<String>,
        parts: Option<String>,
        from_part: Option<i32>,
    ) -> Result<Self, ArrowError> {
        if from_part.is_some_and(|part| part < 0)
            || [after_chunk.as_ref(), parts.as_ref()].into_iter().flatten().any(|text| {
                text.len() > MAX_SELECTOR_BYTES || text.chars().any(char::is_control)
            })
        {
            return Err(ArrowError::InvalidInput);
        }
        if let (Some(id), Some(partition)) = (&after_chunk, from_part) {
            validate_id(id.as_bytes(), partition).map_err(|_| ArrowError::InvalidInput)?;
        }
        Ok(Self { after_chunk, parts, from_part })
    }

    /// Last completely delivered chunk, if explicitly selected.
    pub fn after_chunk(&self) -> Option<&str> {
        self.after_chunk.as_deref()
    }
    /// Uninterpreted server partition-selection expression.
    pub fn parts(&self) -> Option<&str> {
        self.parts.as_deref()
    }
    /// Owning starting partition, not a row offset.
    pub const fn from_part(&self) -> Option<i32> {
        self.from_part
    }

    pub(super) fn resumed(&self, id: &str, partition: i32) -> Self {
        Self {
            after_chunk: Some(id.into()),
            parts: self.parts.clone(),
            from_part: Some(partition),
        }
    }

    fn query(&self) -> Vec<(&str, String)> {
        let mut result = Vec::new();
        if let Some(id) = &self.after_chunk { result.push(("after_chunk", id.clone())); }
        if let Some(parts) = &self.parts { result.push(("parts", parts.clone())); }
        if let Some(partition) = self.from_part { result.push(("from_part", partition.to_string())); }
        result
    }
}

/// Submission response selection. Both modes are a single, nonreplayable POST.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitMode {
    /// JSON statement metadata; optional wait is 0..=10000 milliseconds.
    Handle {
        /// Explicit server wait or omission, not a client retry/deadline.
        sync_wait_ms: Option<u32>,
    },
    /// Bare IPC, or explicitly opted-in JSON handle fallback; wait is 1..=10000.
    Inline {
        /// Required positive bounded server wait.
        sync_wait_ms: u32,
        /// Permit a successful JSON handle, never automatic re-submission.
        allow_json_fallback: bool,
    },
}

/// Borrowed Arrow submission options; no JSON-module dependency or execution.
pub struct SubmitOptions<'a> {
    /// Executable statement text, bounded before copying into JSON.
    pub code: &'a str,
    /// Provider-native kind, usually `sql`; no codec/kind support is inferred.
    pub kind: &'a str,
    /// Provider-native codec, usually `none`; forwarded without transcoding.
    pub codec: &'a str,
    /// Explicit response representation and bounded server wait.
    pub mode: SubmitMode,
}

impl SubmitOptions<'_> {
    /// Build one Arrow POST and its required caller-supplied UUID header.
    ///
    /// `resource_id` is correlation, not an idempotency/replay guarantee. The host
    /// generates it without SDK credential or backend discovery. `max_code_bytes`
    /// bounds UTF-8 code bytes; JSON escaping can expand these by up to sixfold.
    pub fn request(
        &self,
        owner: &StatementOwner,
        resource_id: &str,
        max_code_bytes: usize,
    ) -> Result<ArrowRequest, ArrowError> {
        if max_code_bytes == 0
            || self.code.trim().is_empty()
            || self.kind.trim().is_empty()
            || self.kind.len() > 256
            || self.codec.len() > 256
            || self.kind.chars().any(char::is_control)
            || self.codec.chars().any(char::is_control)
            || !valid_uuid(resource_id)
        {
            return Err(ArrowError::InvalidInput);
        }
        if self.code.len() > max_code_bytes { return Err(ArrowError::Limit); }
        let (wait, accept) = match self.mode {
            SubmitMode::Handle { sync_wait_ms } => {
                if sync_wait_ms.is_some_and(|wait| wait > 10_000) {
                    return Err(ArrowError::InvalidInput);
                }
                (sync_wait_ms, "application/json")
            }
            SubmitMode::Inline { sync_wait_ms, allow_json_fallback } => {
                if !(1..=10_000).contains(&sync_wait_ms) {
                    return Err(ArrowError::InvalidInput);
                }
                (Some(sync_wait_ms), if allow_json_fallback { INLINE_OR_JSON } else { ARROW_STREAM_MEDIA_TYPE })
            }
        };
        let query = wait.map(|wait| vec![("sync_wait_ms", wait.to_string())]).unwrap_or_default();
        let native = owner.endpoint.request(
            NativeMethod::Post,
            &owner.segments(),
            &query,
            Some(json!({"code":self.code,"kind":self.kind,"format":"arrow","codec":self.codec})),
            NativeEffect::Execute,
        )?.with_accept(accept)?;
        Ok(ArrowRequest {
            native,
            owner: owner.clone(),
            mode: match self.mode {
                SubmitMode::Handle { .. } => RequestMode::Handle,
                SubmitMode::Inline { allow_json_fallback, .. } => RequestMode::Inline { allow_json_fallback },
            },
            resource_id: Some(resource_id.into()),
        })
    }
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36 && value.bytes().enumerate().all(|(index, byte)| {
        if [8, 13, 18, 23].contains(&index) { byte == b'-' } else { byte.is_ascii_hexdigit() }
    })
}

#[derive(Clone, Copy)]
enum RequestMode {
    Framed,
    Handle,
    Inline { allow_json_fallback: bool },
}

/// Native request plus headers not yet representable on `NativeRequest`.
///
/// Hosts must apply [`Self::headers`], use a caller-owned streaming transport,
/// reject redirects/HTTP decompression and dispatch POST exactly once. The
/// whole-body `NativeExecutor` seam is not suitable for streaming Arrow bodies.
pub struct ArrowRequest {
    native: NativeRequest,
    owner: StatementOwner,
    mode: RequestMode,
    resource_id: Option<String>,
}

impl std::fmt::Debug for ArrowRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ArrowRequest").field("native", &self.native).finish_non_exhaustive()
    }
}

impl ArrowRequest {
    /// Validated method, route, query, body and effect.
    pub const fn native(&self) -> &NativeRequest { &self.native }

    pub(super) fn is_result_get(&self) -> bool {
        matches!(self.mode, RequestMode::Framed) && self.native.method() == NativeMethod::Get
    }

    /// Exact headers to apply, including Accept and POST Content-Type/correlation.
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        [
            Some(("accept", self.native.accept())),
            self.resource_id.as_ref().map(|_| ("content-type", "application/json")),
            self.resource_id.as_deref().map(|id| ("x-ms-livy-resource-id", id)),
        ].into_iter().flatten()
    }

    /// Validate the actual HTTP status, representation and body-level headers.
    pub fn validate_response(
        &self,
        headers: ResponseHeaders<'_>,
        max_body_bytes: u64,
    ) -> Result<ResponseRepresentation, ArrowError> {
        let expected = match self.mode {
            RequestMode::Framed => ResponseRepresentation::FramedV2,
            RequestMode::Handle => ResponseRepresentation::JsonHandle,
            RequestMode::Inline { .. } => ResponseRepresentation::InlineIpc,
        };
        let allow_fallback = matches!(self.mode, RequestMode::Inline { allow_json_fallback: true });
        validate_response_headers(headers, expected, allow_fallback, max_body_bytes)
    }

    /// Construct a bounded owner-bound JSON reader only after explicit opt-in.
    ///
    /// Call `validate_response` first and only use this for `JsonHandle`.
    pub fn inline_fallback(&self, max_json_bytes: usize) -> Result<InlineFallback, ArrowError> {
        if !matches!(self.mode, RequestMode::Inline { allow_json_fallback: true }) {
            return Err(ArrowError::Protocol);
        }
        if max_json_bytes == 0 { return Err(ArrowError::InvalidInput); }
        Ok(InlineFallback {
            owner: self.owner.clone(),
            body: Vec::new(),
            max_bytes: max_json_bytes,
            failed: false,
        })
    }
}

/// Borrowed, bounded HTTP response metadata from the host.
#[derive(Clone, Copy)]
pub struct ResponseHeaders<'a> {
    /// Actual response status; 404 is not a transient deployment probe.
    pub status: u16,
    /// Actual Content-Type.
    pub content_type: Option<&'a str>,
    /// Content-Encoding; only absent or identity is accepted.
    pub content_encoding: Option<&'a str>,
    /// Optional encoded HTTP body length, not payload/decoded length.
    pub content_length: Option<&'a str>,
}

/// Selected response shape, without suggesting that raw IPC is decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseRepresentation {
    /// Public framed-v2 chunks.
    FramedV2,
    /// Bare inline IPC bytes; not framed-v2.
    InlineIpc,
    /// JSON metadata, or explicitly opted-in inline statement handle.
    JsonHandle,
}

/// Validate media/version, HTTP compression and optional encoded body length.
///
/// Bound the actual streamed bytes too; a declared length is not a byte budget.
pub fn validate_response_headers(
    headers: ResponseHeaders<'_>,
    expected: ResponseRepresentation,
    allow_inline_json_fallback: bool,
    max_body_bytes: u64,
) -> Result<ResponseRepresentation, ArrowError> {
    if !(200..300).contains(&headers.status) { return Err(ArrowError::Http(headers.status)); }
    if max_body_bytes == 0 { return Err(ArrowError::InvalidInput); }
    if headers.content_encoding.is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
        return Err(ArrowError::Protocol);
    }
    if let Some(length) = headers.content_length {
        if length.is_empty() || length.len() > 128 || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ArrowError::Protocol);
        }
        if length.parse::<u64>().map_err(|_| ArrowError::Limit)? > max_body_bytes {
            return Err(ArrowError::Limit);
        }
    }
    let content_type = headers.content_type.ok_or(ArrowError::Protocol)?;
    if content_type.len() > 4096 || content_type.chars().any(char::is_control) {
        return Err(ArrowError::Protocol);
    }
    let mut pieces = content_type.split(';');
    let media = pieces.next().ok_or(ArrowError::Protocol)?.trim();
    let mut parameters = Vec::new();
    let mut version = None;
    for piece in pieces {
        let (name, value) = piece.split_once('=').ok_or(ArrowError::Protocol)?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty()
            || parameters.iter().any(|previous: &&str| previous.eq_ignore_ascii_case(name))
        { return Err(ArrowError::Protocol); }
        parameters.push(name);
        let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            &value[1..value.len()-1]
        } else if value.contains('"') { return Err(ArrowError::Protocol); } else { value };
        if name.eq_ignore_ascii_case("v") { version = Some(value); }
    }
    match expected {
        ResponseRepresentation::FramedV2 if media.eq_ignore_ascii_case(ARROW_CHUNKS_MEDIA_TYPE) && version == Some("2") => Ok(expected),
        ResponseRepresentation::InlineIpc if media.eq_ignore_ascii_case(ARROW_STREAM_MEDIA_TYPE) => Ok(expected),
        ResponseRepresentation::JsonHandle if media.eq_ignore_ascii_case("application/json") => Ok(expected),
        ResponseRepresentation::InlineIpc if allow_inline_json_fallback && media.eq_ignore_ascii_case("application/json") => Ok(ResponseRepresentation::JsonHandle),
        _ => Err(ArrowError::Protocol),
    }
}

/// Bounded incremental JSON fallback, distinct from binary stream buffering.
///
/// No polling, result fetch, POST replay or remote cancellation is performed.
pub struct InlineFallback {
    owner: StatementOwner,
    body: Vec<u8>,
    max_bytes: usize,
    failed: bool,
}

impl InlineFallback {
    /// Append bounded JSON bytes; failure poisons this response.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ArrowError> {
        if self.failed { return Err(ArrowError::InvalidState); }
        let Some(length) = self.body.len().checked_add(bytes.len())
            .filter(|length| *length <= self.max_bytes) else {
            self.failed = true;
            return Err(ArrowError::Limit);
        };
        if length > self.body.capacity() {
            let capacity = self.body.capacity().saturating_mul(2)
                .max(length).min(self.max_bytes);
            self.body.try_reserve_exact(capacity - self.body.len()).map_err(|_| {
                self.failed = true;
                ArrowError::Allocation
            })?;
        }
        self.body.extend_from_slice(bytes);
        Ok(())
    }

    /// Resolve body `id` and/or decimal `statementId`, matching if both exist.
    ///
    /// Deliberately accepts no HC `id` header. Missing, malformed or conflicting
    /// IDs mean an unknown dispatched submission outcome, not safe retry.
    /// The native handle supports IDs through `u64::MAX`.
    pub fn finish(self, statement_id_header: Option<&str>) -> Result<StatementHandle, ArrowError> {
        if self.failed { return Err(ArrowError::InvalidState); }
        let value: Value = serde_json::from_slice(&self.body).map_err(|_| ArrowError::OutcomeUnknown)?;
        let object = value.as_object().ok_or(ArrowError::OutcomeUnknown)?;
        let body_id = match object.get("id") {
            None | Some(Value::Null) => None,
            Some(id) => Some(id.as_u64().ok_or(ArrowError::OutcomeUnknown)?),
        };
        let header_id = statement_id_header.map(|header| {
            if header.is_empty() || header.len() > 128 || !header.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ArrowError::OutcomeUnknown);
            }
            header.parse::<u64>().map_err(|_| ArrowError::OutcomeUnknown)
        }).transpose()?;
        if body_id.zip(header_id).is_some_and(|(body, header)| body != header) {
            return Err(ArrowError::OutcomeUnknown);
        }
        let id = body_id.or(header_id).ok_or(ArrowError::OutcomeUnknown)?;
        Ok(StatementHandle::new(self.owner, id))
    }
}
