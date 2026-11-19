// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    EngineBinding, EngineExecutor, QueryCapability, QueryEvidence, QueryResult, policy, result,
};
use fabric_core::cancel_with;
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeError, NativeMethod, NativeRequest, NativeResponse,
    NativeStreamLimits,
};
use std::{
    fmt,
    future::Future,
    pin::{Pin, pin},
    time::Duration,
};

/// Explicit local budgets, not native scan/cost quotas or CLI defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryLimits {
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_rows: usize,
    max_tables: usize,
    max_columns: usize,
    max_frames: usize,
}

impl QueryLimits {
    /// Construct finite budgets; zero rows requests schema-only emitted data.
    ///
    /// # Errors
    /// Rejects zero byte/shape budgets and budgets above the decoder's safety caps.
    pub fn new(
        max_request_bytes: usize,
        max_response_bytes: usize,
        max_rows: usize,
        max_tables: usize,
        max_columns: usize,
        max_frames: usize,
    ) -> Result<Self, QueryError> {
        if max_request_bytes == 0
            || max_request_bytes > 4 * 1024 * 1024
            || max_response_bytes == 0
            || max_response_bytes > 64 * 1024 * 1024
            || max_tables == 0
            || max_tables > 1024
            || max_columns == 0
            || max_columns > 4096
            || !(2..=4096).contains(&max_frames)
        {
            return Err(QueryError::new(QueryErrorKind::InvalidInput));
        }
        Ok(Self {
            max_request_bytes,
            max_response_bytes,
            max_rows,
            max_tables,
            max_columns,
            max_frames,
        })
    }

    /// Maximum exact serialized request bytes.
    pub const fn max_request_bytes(self) -> usize {
        self.max_request_bytes
    }
    /// Maximum received JSON/framing bytes before projection.
    pub const fn max_response_bytes(self) -> usize {
        self.max_response_bytes
    }
    /// Maximum retained primary rows across all tables.
    pub const fn max_rows(self) -> usize {
        self.max_rows
    }
    /// Maximum primary tables.
    pub const fn max_tables(self) -> usize {
        self.max_tables
    }
    /// Maximum columns in any native table.
    pub const fn max_columns(self) -> usize {
        self.max_columns
    }
    /// Maximum native frames, including metadata and terminal frames.
    pub const fn max_frames(self) -> usize {
        self.max_frames
    }
}

/// Exact KQL input and correlation ID; no engine selectors or arbitrary options.
#[derive(Clone)]
pub struct KqlQuery {
    text: String,
    client_request_id: String,
    server_timeout: Option<Duration>,
}

impl KqlQuery {
    /// Validate text and client correlation metadata without parsing its dialect.
    ///
    /// # Errors
    /// Rejects blank/NUL input, oversized text and malformed correlation metadata.
    pub fn new(text: &str, client_request_id: &str) -> Result<Self, QueryError> {
        if text.trim().is_empty()
            || text.contains('\0')
            || text.len() > 4 * 1024 * 1024
            || client_request_id.trim().is_empty()
            || client_request_id.len() > 1024
            || client_request_id.chars().any(char::is_control)
        {
            return Err(QueryError::new(QueryErrorKind::InvalidInput));
        }
        Ok(Self {
            text: text.into(),
            client_request_id: client_request_id.into(),
            server_timeout: None,
        })
    }

    /// Set an explicit native timeout with millisecond precision, at most one hour.
    ///
    /// # Errors
    /// Rejects zero, excessive or unrepresentable durations rather than clamping.
    pub fn with_server_timeout(mut self, timeout: Duration) -> Result<Self, QueryError> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(3600)
            || !timeout.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(QueryError::new(QueryErrorKind::InvalidInput));
        }
        self.server_timeout = Some(timeout);
        Ok(self)
    }

    /// Original query bytes, never transformed or logged by the adapter.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Construct the fixed KQL v2 read request, independent of text spelling.
    ///
    /// # Errors
    /// Rejects exact serialized-body excess before any executor/credential access.
    pub fn request(
        &self,
        binding: &EngineBinding,
        limits: QueryLimits,
    ) -> Result<NativeRequest, QueryError> {
        let body = policy::RequestBody::new(binding.database(), &self.text, self.server_timeout);
        let bytes =
            serde_json::to_vec(&body).map_err(|_| QueryError::new(QueryErrorKind::InvalidInput))?;
        if bytes.len() > limits.max_request_bytes() {
            return Err(QueryError::new(QueryErrorKind::RequestLimit));
        }
        binding
            .endpoint()
            .request(
                NativeMethod::Post,
                &["v2", "rest", "query"],
                &[],
                None,
                NativeEffect::Read,
            )
            .and_then(|request| {
                request.with_bytes(
                    bytes,
                    "application/json; charset=utf-8",
                    limits.max_request_bytes(),
                )
            })
            .and_then(|request| {
                request.with_header("x-ms-client-request-id", &self.client_request_id)
            })
            .map_err(|_| QueryError::new(QueryErrorKind::InvalidInput))
    }
}

impl fmt::Debug for KqlQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KqlQuery(<redacted>)")
    }
}

/// Sanitized failure classification; provider evidence is exposed separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryErrorKind {
    /// Invalid or mismatched exact endpoint/database/audience/identity binding.
    InvalidBinding,
    /// Invalid local input or limits.
    InvalidInput,
    /// Serialized request exceeded its byte cap.
    RequestLimit,
    /// Response bytes or shape exceeded a finite bound.
    ResponseLimit,
    /// Native adapter does not implement the required fixed read-intent policy.
    UnsupportedCapability,
    /// Progressive/unknown response protocol is not implemented.
    UnsupportedResponse,
    /// Missing or malformed native framing/schema/media metadata.
    InvalidResponse,
    /// Native HTTP error; status and bounded raw body are retained separately.
    Http,
    /// Terminal native frame reports errors, including partial query failure.
    BackendFailure,
    /// Native completion status reports warnings; not a clean complete result.
    BackendWarning,
    /// Service reports actual query cancellation.
    BackendCancelled,
    /// Local retained-row cap dropped rows; native scan cost was not limited.
    RowLimit,
    /// Existing native executor/body failed; no replay or cancellation implied.
    Native(NativeError),
}

/// Query failure with sanitized formatting and explicitly inspectable evidence.
pub struct QueryError {
    /// Machine-readable failure classification.
    pub kind: QueryErrorKind,
    /// Execution observation, if an exchange was attempted.
    pub evidence: Option<Box<QueryEvidence>>,
    /// Validated partial native tables/diagnostics, never successful completion.
    pub partial: Option<Box<QueryResult>>,
    /// Bounded provider bytes on failure, including malformed HTTP-200 payloads.
    /// Sensitive, never formatted/logged; a stream failure retains received bytes.
    pub provider_body: Option<Vec<u8>>,
}

impl QueryError {
    pub(crate) fn new(kind: QueryErrorKind) -> Self {
        Self {
            kind,
            evidence: None,
            partial: None,
            provider_body: None,
        }
    }
    pub(crate) fn observed(kind: QueryErrorKind, evidence: QueryEvidence) -> Self {
        Self {
            evidence: Some(Box::new(evidence)),
            ..Self::new(kind)
        }
    }
    pub(crate) fn with_partial(kind: QueryErrorKind, result: QueryResult) -> Self {
        Self {
            evidence: Some(Box::new(result.evidence.clone())),
            partial: Some(Box::new(result)),
            ..Self::new(kind)
        }
    }
    pub(crate) fn with_provider_body(mut self, bytes: Vec<u8>) -> Self {
        self.provider_body = Some(bytes);
        self
    }
}

impl fmt::Debug for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryError")
            .field("kind", &self.kind)
            .field("evidence", &self.evidence)
            .field("partial", &self.partial.is_some())
            .finish()
    }
}
impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KQL query did not complete: {:?}", self.kind)
    }
}
impl std::error::Error for QueryError {}

/// Execute exactly one bounded read-intent KQL request on the caller's runtime.
///
/// Offline executors and structurally qualified Kusto v2 adapters are admitted.
/// Production execution requires an explicit positive server timeout within the
/// remaining local budget; no timeout, audience or identity is guessed.
/// Binding equality precedes dispatch; policy is never inferred from text.
/// The executor and body enforce the same absolute deadline; cancellation drops
/// local work only. HTTP EOF and all terminal metadata precede success.
///
/// # Errors
/// Rejects unsupported/mismatched bindings and input before opening a stream.
/// Preserves typed native failures, service errors and validated partial data.
pub async fn query_kql(
    binding: &EngineBinding,
    executor: &dyn EngineExecutor,
    query: &KqlQuery,
    limits: QueryLimits,
    deadline: &NativeDeadline,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<QueryResult, QueryError> {
    if binding != executor.binding() {
        return Err(QueryError::new(QueryErrorKind::InvalidBinding));
    }
    let capability = executor.query_capability();
    match capability {
        QueryCapability::ScriptedOffline | QueryCapability::ReadOnlyKustoV2 => {}
        QueryCapability::UnverifiedProduction => {
            return Err(QueryError::new(QueryErrorKind::UnsupportedCapability));
        }
    }
    if capability == QueryCapability::ReadOnlyKustoV2 && query.server_timeout.is_none() {
        return Err(QueryError::new(QueryErrorKind::InvalidInput));
    }
    let request = query.request(binding, limits)?;
    let remaining = deadline
        .remaining()
        .map_err(|error| QueryError::new(QueryErrorKind::Native(error)))?;
    if query
        .server_timeout
        .is_some_and(|timeout| timeout > remaining)
    {
        return Err(QueryError::new(QueryErrorKind::InvalidInput));
    }
    let mut evidence = QueryEvidence {
        binding: binding.clone(),
        language: "kql",
        transport: "kusto-http-v2",
        protocol_version: "v2.0",
        read_policy: "request_readonly_hardline",
        adapter_capability: capability,
        production_verified: false,
        http_eof: false,
        http_status: None,
        content_type: None,
        client_request_id: query.client_request_id.clone(),
        activity_id: None,
        completed: false,
        locally_truncated: false,
    };
    let mut bytes = Vec::new();
    let mut cancelled = pin!(cancelled);
    let exchange = async {
        let stream_limits = NativeStreamLimits {
            max_fragment_bytes: limits.max_response_bytes().min(64 * 1024),
            max_body_bytes: u64::try_from(limits.max_response_bytes())
                .map_err(|_| QueryErrorKind::InvalidInput)?,
        };
        let mut response = cancel_with(
            executor.open(&request, stream_limits, deadline),
            cancelled.as_mut(),
        )
        .await
        .map_err(|_| QueryErrorKind::Native(NativeError::Cancelled))?
        .map_err(QueryErrorKind::Native)?;
        deadline.remaining().map_err(QueryErrorKind::Native)?;
        evidence.http_status = Some(response.status);
        let metadata = NativeResponse {
            status: response.status,
            content_type: response.content_type.take(),
            body: Vec::new(),
            headers: std::mem::take(&mut response.headers),
        };
        let header_media = metadata
            .header("content-type")
            .map_err(QueryErrorKind::Native)?;
        evidence.activity_id = metadata
            .header("x-ms-activity-id")
            .map_err(QueryErrorKind::Native)?
            .map(str::to_owned);
        evidence.content_type = metadata.content_type.clone();
        if !(100..=599).contains(&metadata.status)
            || (metadata.status == 200
                && (!valid_json_media(metadata.content_type.as_deref())
                    || header_media.is_some_and(|media| !valid_json_media(Some(media)))))
        {
            return Err(QueryErrorKind::InvalidResponse);
        }
        loop {
            deadline.remaining().map_err(QueryErrorKind::Native)?;
            let fragment = cancel_with(
                response
                    .body
                    .read(stream_limits.max_fragment_bytes, deadline),
                cancelled.as_mut(),
            )
            .await
            .map_err(|_| QueryErrorKind::Native(NativeError::Cancelled))?
            .map_err(QueryErrorKind::Native)?;
            deadline.remaining().map_err(QueryErrorKind::Native)?;
            let Some(fragment) = fragment else {
                evidence.http_eof = true;
                break;
            };
            if fragment.is_empty()
                || fragment.len() > stream_limits.max_fragment_bytes
                || bytes
                    .len()
                    .checked_add(fragment.len())
                    .is_none_or(|size| size > limits.max_response_bytes())
            {
                return Err(QueryErrorKind::ResponseLimit);
            }
            bytes.extend_from_slice(&fragment);
        }
        Ok(())
    };
    match exchange.await {
        Ok(()) => {}
        Err(kind) => {
            return Err(QueryError::observed(kind, evidence).with_provider_body(bytes));
        }
    }
    if evidence.http_status != Some(200) {
        let mut error = QueryError::observed(QueryErrorKind::Http, evidence);
        error.provider_body = Some(bytes);
        return Err(error);
    }
    result::decode(bytes, limits, evidence, deadline, cancelled.as_mut()).await
}

pub(crate) async fn check_observation(
    deadline: &NativeDeadline,
    cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<(), QueryErrorKind> {
    deadline.remaining().map_err(QueryErrorKind::Native)?;
    cancel_with(async {}, cancelled)
        .await
        .map_err(|_| QueryErrorKind::Native(NativeError::Cancelled))?;
    deadline
        .remaining()
        .map(|_| ())
        .map_err(QueryErrorKind::Native)
}

fn valid_json_media(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let mut parts = value.split(';').map(str::trim);
        parts
            .next()
            .is_some_and(|media| media.eq_ignore_ascii_case("application/json"))
            && parts.all(|parameter| parameter.eq_ignore_ascii_case("charset=utf-8"))
    })
}
