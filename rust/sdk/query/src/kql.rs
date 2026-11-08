// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{EngineBinding, EngineExecutor, QueryCapability, QueryEvidence, QueryResult, result};
use fabric_core::cancel_with;
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeError, NativeMethod, NativeRequest, NativeResponse,
    NativeStreamLimits,
};
use serde_json::json;
use std::{fmt, future::Future, pin::pin, time::Duration};

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
        let mut options = json!({
            "query_language": "kql",
            "request_readonly": true,
            "request_readonly_hardline": true,
            "request_callout_disabled": true,
            "request_remote_entities_disabled": true,
            "request_external_data_disabled": true,
            "request_impersonation_disabled": true,
            "request_sandboxed_execution_disabled": true,
            "results_progressive_enabled": false
        });
        if let Some(timeout) = self.server_timeout {
            let secs = timeout.as_secs();
            options["servertimeout"] = json!(format!(
                "{:02}:{:02}:{:02}.{:03}",
                secs / 3600,
                (secs % 3600) / 60,
                secs % 60,
                timeout.subsec_millis()
            ));
        }
        let body = json!({
            "db": binding.database(),
            "csl": self.text,
            "properties": { "Options": options }
        });
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
    /// Native adapter is not verified for production read-intent execution.
    UnsupportedCapability,
    /// Progressive/unknown response protocol is not implemented.
    UnsupportedResponse,
    /// Missing or malformed native framing/schema/media metadata.
    InvalidResponse,
    /// Native HTTP error; status and bounded raw body are retained separately.
    Http,
    /// Terminal native frame reports errors, including partial query failure.
    BackendFailure,
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
    /// Bounded non-success provider bytes; sensitive, never formatted/logged.
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
/// Only an explicitly offline adapter is admitted in this first release.
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
    if executor.query_capability() != QueryCapability::ScriptedOffline {
        return Err(QueryError::new(QueryErrorKind::UnsupportedCapability));
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
        production_verified: false,
        http_status: None,
        content_type: None,
        client_request_id: query.client_request_id.clone(),
        activity_id: None,
        completed: false,
        locally_truncated: false,
    };
    let exchange = async {
        let stream_limits = NativeStreamLimits {
            max_fragment_bytes: limits.max_response_bytes().min(64 * 1024),
            max_body_bytes: u64::try_from(limits.max_response_bytes())
                .map_err(|_| QueryErrorKind::InvalidInput)?,
        };
        let mut response = executor
            .open(&request, stream_limits, deadline)
            .await
            .map_err(QueryErrorKind::Native)?;
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
        let mut bytes = Vec::new();
        loop {
            deadline.remaining().map_err(QueryErrorKind::Native)?;
            let fragment = response
                .body
                .read(stream_limits.max_fragment_bytes, deadline)
                .await
                .map_err(QueryErrorKind::Native)?;
            deadline.remaining().map_err(QueryErrorKind::Native)?;
            let Some(fragment) = fragment else { break };
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
        Ok(bytes)
    };
    let bytes = match cancel_with(exchange, pin!(cancelled)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(kind)) => return Err(QueryError::observed(kind, evidence)),
        Err(_) => {
            return Err(QueryError::observed(
                QueryErrorKind::Native(NativeError::Cancelled),
                evidence,
            ));
        }
    };
    if evidence.http_status != Some(200) {
        let mut error = QueryError::observed(QueryErrorKind::Http, evidence);
        error.provider_body = Some(bytes);
        return Err(error);
    }
    deadline
        .remaining()
        .map_err(|error| QueryError::observed(QueryErrorKind::Native(error), evidence.clone()))?;
    let result = result::decode(&bytes, limits, evidence.clone());
    deadline
        .remaining()
        .map_err(|error| QueryError::observed(QueryErrorKind::Native(error), evidence))?;
    result
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
