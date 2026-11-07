// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded native observations for SDK v2, independent of transport and projection.
//!
//! Status and body bytes are observations, not completion classifiers. Callers
//! supply protocol-derived state, effect and coverage separately. In particular,
//! HTTP 200 does not establish remote completion, and a latest HTTP 202 observation
//! cannot be marked completed. An initial 202 remains intact after a later result.
//!
//! Retention budgets count body bytes, retained header name/value UTF-8 bytes and
//! operation-reference bytes. Counts bound structural response/header storage
//! separately; allocator bookkeeping is not part of the payload-byte budget.
//! Owned strings and bodies use boxed storage to discard caller spare capacity.
//! No diagnostic strings, unbounded poll history, logging, URL validation, HTTP
//! execution or projection are implemented here. Callers should retain bounded
//! initial/result/page observations rather than every poll or retry.
//!
//! Provider content is intentionally available through explicit getters, never
//! automatically through `Debug` or errors. A projection belongs beside an
//! outcome: it cannot replace the native observations or their metadata.

use std::fmt;

/// Caller-declared observation state, independent of HTTP status and coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    /// The caller has protocol evidence of terminal completion.
    ///
    /// This does not imply collection-wide coverage or a read-only operation.
    Completed,
    /// Work was accepted, but completion has not been established.
    Accepted,
    /// The caller intentionally reports an incomplete observation.
    Partial,
    /// The caller has classified a failure using the endpoint's protocol.
    Failed,
    /// Available evidence does not establish another state.
    Unknown,
}

/// Caller-declared effect classification; never inferred from an HTTP method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// The caller has established that the operation is read-only.
    ReadOnly,
    /// The operation can have remote write effects.
    WriteCapable,
    /// Effects are unknown and must be treated as potentially write-capable.
    UnknownWriteCapable,
}

/// The boundary to which a completeness statement applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageScope {
    /// A response, not a page or the entire collection.
    Response,
    /// One intentionally scoped page, not the entire collection.
    Page,
    /// A traversal's collection-wide coverage.
    Collection,
}

/// Invalid scope/page-count combinations, without provider content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageError {
    /// Response scope requires zero pages; page scope requires exactly one.
    InvalidPageCount,
    /// Complete collection coverage requires at least one observed page.
    CompleteCollectionWithoutPage,
}

impl fmt::Display for CoverageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPageCount => "page count does not match coverage scope",
            Self::CompleteCollectionWithoutPage => {
                "complete collection coverage requires an observed page"
            }
        })
    }
}

impl std::error::Error for CoverageError {}

/// Explicit scoped coverage; `None` is unknown, never an assumed success.
///
/// Page counts describe observed traversal, not retained response counts. A
/// caller may retain only initial/final observations while counting more pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coverage {
    scope: CoverageScope,
    complete: Option<bool>,
    pages: usize,
}

impl Coverage {
    /// Validates a coverage statement without inferring completeness.
    ///
    /// Response scope requires `pages == 0`, page scope requires `pages == 1`,
    /// and a complete collection requires at least one page, even if it is empty.
    /// Unknown or incomplete collection coverage may have zero observed pages.
    pub fn new(
        scope: CoverageScope,
        complete: Option<bool>,
        pages: usize,
    ) -> Result<Self, CoverageError> {
        match scope {
            CoverageScope::Response if pages != 0 => {
                return Err(CoverageError::InvalidPageCount);
            }
            CoverageScope::Page if pages != 1 => {
                return Err(CoverageError::InvalidPageCount);
            }
            CoverageScope::Collection if complete == Some(true) && pages == 0 => {
                return Err(CoverageError::CompleteCollectionWithoutPage);
            }
            _ => {}
        }
        Ok(Self {
            scope,
            complete,
            pages,
        })
    }

    /// Returns the precise boundary of the completeness statement.
    pub fn scope(self) -> CoverageScope {
        self.scope
    }

    /// Returns the caller's statement, preserving unknown as `None`.
    pub fn complete(self) -> Option<bool> {
        self.complete
    }

    /// Returns observed pages, which may exceed retained page responses.
    pub fn pages(self) -> usize {
        self.pages
    }
}

/// Protocol role of a native response; unrelated to success classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseRole {
    /// The initial request's response, including accepted responses.
    Initial,
    /// A retained operation-monitor observation.
    Monitor,
    /// A separately retrieved result response.
    Result,
    /// A collection page response.
    Page,
}

/// A finite retention-limit dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    /// Maximum number of retained native responses.
    Responses,
    /// Aggregate retained payload bytes.
    RetainedBytes,
    /// Body plus retained headers for an individual response.
    ResponseBytes,
    /// Input header name/value bytes before header filtering.
    HeaderBytes,
    /// Input header count before header filtering.
    Headers,
    /// Construction bound for one operation reference.
    OperationBytes,
}

/// Typed validation/retention failures; none contain provider text or bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionError {
    /// A numeric bound is zero or exceeds the platform's addressable range.
    InvalidLimit(LimitKind),
    /// Header, response and total byte limits are not nested in that order.
    InconsistentByteLimits,
    /// The native HTTP status is outside 100 through 599.
    InvalidStatus,
    /// A header name is empty, non-ASCII, or not an HTTP token.
    InvalidHeaderName,
    /// A header value contains a control character, including tab or DEL.
    InvalidHeaderValue,
    /// An input response has too many headers, including omitted headers.
    HeaderCountExceeded,
    /// Input header bytes exceed their limit, including omitted headers.
    HeaderBytesExceeded,
    /// Body plus retained headers exceed an individual response's limit.
    ResponseBytesExceeded,
    /// Appending would exceed the retained response count.
    ResponseCountExceeded,
    /// Retained response/reference bytes would exceed the aggregate budget.
    TotalBytesExceeded,
    /// Length arithmetic could not be represented.
    SizeOverflow,
    /// An opaque ID is empty or contains whitespace, controls or non-ASCII.
    InvalidOperationId,
    /// An opaque ID exceeds its explicit construction bound.
    OperationBytesExceeded,
    /// Completion requires at least one native response observation.
    CompletedWithoutResponse,
    /// The latest native 202 response is not evidence of completion.
    AcceptedResponseNotCompleted,
    /// Bounded response-list storage could not be reserved.
    AllocationFailed,
}

impl fmt::Display for RetentionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLimit(_) => "retention limits must be positive and addressable",
            Self::InconsistentByteLimits => "retention byte limits are inconsistent",
            Self::InvalidStatus => "native HTTP status must be between 100 and 599",
            Self::InvalidHeaderName => "invalid response header name",
            Self::InvalidHeaderValue => "response header value contains a control character",
            Self::HeaderCountExceeded => "response header count limit exceeded",
            Self::HeaderBytesExceeded => "response header byte limit exceeded",
            Self::ResponseBytesExceeded => "native response byte limit exceeded",
            Self::ResponseCountExceeded => "retained response count limit exceeded",
            Self::TotalBytesExceeded => "aggregate retained byte limit exceeded",
            Self::SizeOverflow => "retention size arithmetic overflowed",
            Self::InvalidOperationId => "invalid opaque operation reference",
            Self::OperationBytesExceeded => "operation reference byte limit exceeded",
            Self::CompletedWithoutResponse => "completion requires a native response observation",
            Self::AcceptedResponseNotCompleted => "latest native HTTP 202 cannot be completed",
            Self::AllocationFailed => "could not reserve bounded response storage",
        })
    }
}

impl std::error::Error for RetentionError {}

/// Validated finite budgets shared by response construction and outcome admission.
///
/// Header bounds apply to all supplied headers before filtering. The individual
/// response and aggregate bounds charge only body plus retained header bytes;
/// the aggregate also charges the optional operation reference's opaque ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionLimits {
    max_responses: usize,
    max_retained_bytes: usize,
    max_response_bytes: usize,
    max_header_bytes: usize,
    max_headers: usize,
}

impl RetentionLimits {
    /// Creates positive, addressable bounds.
    ///
    /// Requires `max_header_bytes <= max_response_bytes <= max_retained_bytes`.
    /// Zero is invalid, not a spelling for unlimited. Bodies may still be empty.
    pub fn new(
        max_responses: usize,
        max_retained_bytes: usize,
        max_response_bytes: usize,
        max_header_bytes: usize,
        max_headers: usize,
    ) -> Result<Self, RetentionError> {
        for (value, kind) in [
            (max_responses, LimitKind::Responses),
            (max_retained_bytes, LimitKind::RetainedBytes),
            (max_response_bytes, LimitKind::ResponseBytes),
            (max_header_bytes, LimitKind::HeaderBytes),
            (max_headers, LimitKind::Headers),
        ] {
            validate_limit(value, kind)?;
        }
        if max_header_bytes > max_response_bytes || max_response_bytes > max_retained_bytes {
            return Err(RetentionError::InconsistentByteLimits);
        }
        Ok(Self {
            max_responses,
            max_retained_bytes,
            max_response_bytes,
            max_header_bytes,
            max_headers,
        })
    }

    /// Maximum retained observations, independent of logical page/poll counts.
    pub fn max_responses(self) -> usize {
        self.max_responses
    }

    /// Maximum aggregate retained payload bytes, including an opaque reference.
    pub fn max_retained_bytes(self) -> usize {
        self.max_retained_bytes
    }

    /// Maximum body plus retained header bytes for one response.
    pub fn max_response_bytes(self) -> usize {
        self.max_response_bytes
    }

    /// Maximum input header bytes, including headers omitted by policy.
    pub fn max_header_bytes(self) -> usize {
        self.max_header_bytes
    }

    /// Maximum input header count, including headers omitted by policy.
    pub fn max_headers(self) -> usize {
        self.max_headers
    }
}

impl Default for RetentionLimits {
    fn default() -> Self {
        Self {
            max_responses: 100,
            max_retained_bytes: 64 * 1024 * 1024,
            max_response_bytes: 4 * 1024 * 1024,
            max_header_bytes: 16 * 1024,
            max_headers: 64,
        }
    }
}

/// A retained allowlisted header. Content is visible only by explicit access.
///
/// Names are ASCII lowercase; values preserve the provider's UTF-8 bytes.
/// Duplicate values are not consolidated or interpreted.
pub struct ResponseHeader {
    name: Box<str>,
    value: Box<str>,
}

impl ResponseHeader {
    /// Returns the normalized, allowlisted header name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the provider value verbatim; do not automatically log it.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for ResponseHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseHeader")
            .field("name_bytes", &self.name.len())
            .field("value_bytes", &self.value.len())
            .finish_non_exhaustive()
    }
}

/// One native HTTP observation with exact status/body and filtered headers.
///
/// Only `content-type`, `retry-after`, `x-ms-request-id`, `request-id`,
/// `x-ms-operation-id`, `x-ms-version`, `etag`, and `last-modified` are retained,
/// matched case-insensitively. Authorization, cookies, arbitrary `Location`
/// values, and all unknown headers are omitted with an explicit omission count.
/// The whitelist is not a claim that provider values are safe to log.
///
/// All input names must be nonempty ASCII HTTP tokens. UTF-8 values are accepted
/// but all Unicode control characters are rejected, including tabs, CR/LF and
/// DEL. These checks and header budgets apply even to omitted headers.
pub struct NativeResponse {
    role: ResponseRole,
    status: u16,
    headers: Box<[ResponseHeader]>,
    body: Box<[u8]>,
    omitted_header_count: usize,
    input_header_bytes: usize,
    retained_bytes: usize,
}

impl NativeResponse {
    /// Validates and retains a native observation without interpreting its body.
    ///
    /// Rejects invalid/oversized input rather than truncating it. Status must be
    /// 100 through 599. Header order and duplicates survive filtering; safe names
    /// alone are lowercased. Empty and non-UTF-8 body bytes are preserved.
    pub fn new(
        role: ResponseRole,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        limits: RetentionLimits,
    ) -> Result<Self, RetentionError> {
        if !(100..=599).contains(&status) {
            return Err(RetentionError::InvalidStatus);
        }
        if headers.len() > limits.max_headers {
            return Err(RetentionError::HeaderCountExceeded);
        }
        if body.len() > limits.max_response_bytes {
            return Err(RetentionError::ResponseBytesExceeded);
        }
        let mut input_header_bytes = 0;
        let mut retained_bytes = body.len();
        let mut omitted_header_count = 0;
        for (name, value) in &headers {
            let bytes = checked_bytes(name.len(), value.len())?;
            input_header_bytes = checked_bytes(input_header_bytes, bytes)?;
            if input_header_bytes > limits.max_header_bytes {
                return Err(RetentionError::HeaderBytesExceeded);
            }
            if !valid_header_name(name) {
                return Err(RetentionError::InvalidHeaderName);
            }
            if value.chars().any(char::is_control) {
                return Err(RetentionError::InvalidHeaderValue);
            }
            if retained_header(name) {
                retained_bytes = checked_bytes(retained_bytes, bytes)?;
                if retained_bytes > limits.max_response_bytes {
                    return Err(RetentionError::ResponseBytesExceeded);
                }
            } else {
                omitted_header_count += 1;
            }
        }
        let headers = headers
            .into_iter()
            .filter_map(|(mut name, value)| {
                if !retained_header(&name) {
                    return None;
                }
                name.make_ascii_lowercase();
                Some(ResponseHeader {
                    name: name.into_boxed_str(),
                    value: value.into_boxed_str(),
                })
            })
            .collect();
        Ok(Self {
            role,
            status,
            headers,
            body: body.into_boxed_slice(),
            omitted_header_count,
            input_header_bytes,
            retained_bytes,
        })
    }

    /// Returns the caller-assigned protocol role.
    pub fn role(&self) -> ResponseRole {
        self.role
    }

    /// Returns the native status unchanged, never a synthesized success status.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns exact provider body bytes; do not automatically log them.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns ordered safe headers, including duplicates and original values.
    pub fn headers(&self) -> &[ResponseHeader] {
        &self.headers
    }

    /// Returns how many input headers were omitted by the retention policy.
    ///
    /// This count exposes filtering; it is not evidence that omitted protocol
    /// headers were absent or that a continuation/operation was validated.
    pub fn omitted_header_count(&self) -> usize {
        self.omitted_header_count
    }

    /// Returns charged body plus retained header name/value bytes.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn check_limits(&self, limits: RetentionLimits) -> Result<(), RetentionError> {
        let input_count = checked_bytes(self.headers.len(), self.omitted_header_count)?;
        if input_count > limits.max_headers {
            return Err(RetentionError::HeaderCountExceeded);
        }
        if self.input_header_bytes > limits.max_header_bytes {
            return Err(RetentionError::HeaderBytesExceeded);
        }
        if self.retained_bytes > limits.max_response_bytes {
            return Err(RetentionError::ResponseBytesExceeded);
        }
        Ok(())
    }
}

impl fmt::Debug for NativeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeResponse")
            .field("role", &self.role)
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("omitted_header_count", &self.omitted_header_count)
            .field("body_bytes", &self.body.len())
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

/// Meaning assigned to an opaque reference by an external protocol validator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationReferenceKind {
    /// A nonsecret operation identifier, not an authorization credential.
    Operation,
    /// A nonsecret continuation identifier, not an arbitrary continuation URL.
    Continuation,
}

/// A bounded, caller-visible opaque ID, never automatically logged.
///
/// Construction validates size and visible ASCII only. It does not validate
/// URLs, origins, endpoint semantics, or whether the supplied ID is nonsecret.
/// The caller's protocol validator must establish those properties separately
/// before supplying a reference. This type grants no execution authority.
pub struct OperationReference {
    kind: OperationReferenceKind,
    id: Box<str>,
}

impl OperationReference {
    /// Validates a nonempty visible-ASCII ID under an explicit positive bound.
    ///
    /// Spaces, controls and non-ASCII are rejected. `max_bytes` is a local
    /// construction bound; admission also charges every ID byte to the outcome.
    pub fn new(
        kind: OperationReferenceKind,
        id: String,
        max_bytes: usize,
    ) -> Result<Self, RetentionError> {
        validate_limit(max_bytes, LimitKind::OperationBytes)?;
        if id.len() > max_bytes {
            return Err(RetentionError::OperationBytesExceeded);
        }
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(RetentionError::InvalidOperationId);
        }
        Ok(Self {
            kind,
            id: id.into_boxed_str(),
        })
    }

    /// Returns the externally established reference kind.
    pub fn kind(&self) -> OperationReferenceKind {
        self.kind
    }

    /// Returns the opaque caller-visible ID, without granting execution rights.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns charged opaque-ID bytes.
    pub fn retained_bytes(&self) -> usize {
        self.id.len()
    }
}

impl fmt::Debug for OperationReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationReference")
            .field("kind", &self.kind)
            .field("id_bytes", &self.id.len())
            .finish_non_exhaustive()
    }
}

/// Bounded ordered native observations plus independent caller-owned metadata.
///
/// New outcomes start with unknown state. Neither appending HTTP 200 nor setting
/// scoped completeness makes an outcome completed. State changes are explicit.
/// A failed append/reference update leaves previous observations, metadata and
/// accounting intact; it does not mark the outcome successful or complete.
///
/// This is not an orchestrator: stopping policy, retry/poll counters, cancellation,
/// endpoint classification and projected data remain outside this container.
pub struct Outcome {
    state: State,
    effect: Effect,
    coverage: Coverage,
    responses: Vec<NativeResponse>,
    operation_reference: Option<OperationReference>,
    retained_bytes: usize,
    limits: RetentionLimits,
}

impl Outcome {
    /// Creates an empty unknown-state outcome with explicit effect and coverage.
    pub fn new(effect: Effect, coverage: Coverage, limits: RetentionLimits) -> Self {
        Self {
            state: State::Unknown,
            effect,
            coverage,
            responses: Vec::new(),
            operation_reference: None,
            retained_bytes: 0,
            limits,
        }
    }

    /// Appends one observation transactionally, rechecking this outcome's limits.
    ///
    /// No state or coverage is inferred. A 202 cannot be appended while the
    /// outcome is marked completed; explicitly change state before retaining
    /// another accepted observation. Other statuses are not classified here.
    pub fn append(&mut self, response: NativeResponse) -> Result<(), RetentionError> {
        response.check_limits(self.limits)?;
        if self.responses.len() >= self.limits.max_responses {
            return Err(RetentionError::ResponseCountExceeded);
        }
        let retained_bytes = checked_bytes(self.retained_bytes, response.retained_bytes)?;
        if retained_bytes > self.limits.max_retained_bytes {
            return Err(RetentionError::TotalBytesExceeded);
        }
        if self.state == State::Completed && response.status == 202 {
            return Err(RetentionError::AcceptedResponseNotCompleted);
        }
        self.responses
            .try_reserve_exact(1)
            .map_err(|_| RetentionError::AllocationFailed)?;
        self.responses.push(response);
        self.retained_bytes = retained_bytes;
        Ok(())
    }

    /// Sets protocol-derived state without changing status, effects or coverage.
    ///
    /// Completion requires an observation and rejects a latest native 202 in
    /// any role, including `Result`. An earlier initial 202 is retained after a
    /// later non-202 result and does not prevent explicit completion.
    pub fn set_state(&mut self, state: State) -> Result<(), RetentionError> {
        if state == State::Completed {
            match self.responses.last() {
                None => return Err(RetentionError::CompletedWithoutResponse),
                Some(response) if response.status == 202 => {
                    return Err(RetentionError::AcceptedResponseNotCompleted);
                }
                Some(_) => {}
            }
        }
        self.state = state;
        Ok(())
    }

    /// Replaces only the explicit coverage statement, without inferring state.
    pub fn set_coverage(&mut self, coverage: Coverage) {
        self.coverage = coverage;
    }

    /// Sets, replaces or clears an externally validated reference transactionally.
    ///
    /// The old ID's charged bytes are released only if replacement succeeds.
    /// No URL parsing, token discovery or protocol validation is performed.
    pub fn set_operation_reference(
        &mut self,
        reference: Option<OperationReference>,
    ) -> Result<(), RetentionError> {
        let old_bytes = self
            .operation_reference
            .as_ref()
            .map_or(0, OperationReference::retained_bytes);
        let new_bytes = reference
            .as_ref()
            .map_or(0, OperationReference::retained_bytes);
        let response_bytes = self
            .retained_bytes
            .checked_sub(old_bytes)
            .ok_or(RetentionError::SizeOverflow)?;
        let retained_bytes = checked_bytes(response_bytes, new_bytes)?;
        if retained_bytes > self.limits.max_retained_bytes {
            return Err(RetentionError::TotalBytesExceeded);
        }
        self.operation_reference = reference;
        self.retained_bytes = retained_bytes;
        Ok(())
    }

    /// Returns caller-declared state, not an HTTP-derived classification.
    pub fn state(&self) -> State {
        self.state
    }

    /// Returns the immutable, explicitly supplied effect classification.
    pub fn effect(&self) -> Effect {
        self.effect
    }

    /// Returns scoped completeness separately from state and effects.
    pub fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Returns immutable native observations in supplied order, including duplicates.
    pub fn responses(&self) -> &[NativeResponse] {
        &self.responses
    }

    /// Returns the optional externally validated reference, without interpreting it.
    pub fn operation_reference(&self) -> Option<&OperationReference> {
        self.operation_reference.as_ref()
    }

    /// Returns aggregate charged bytes, including any opaque reference.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Returns this outcome's immutable validated budgets.
    pub fn limits(&self) -> RetentionLimits {
        self.limits
    }
}

impl fmt::Debug for Outcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Outcome")
            .field("state", &self.state)
            .field("effect", &self.effect)
            .field("coverage", &self.coverage)
            .field("response_count", &self.responses.len())
            .field(
                "has_operation_reference",
                &self.operation_reference.is_some(),
            )
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

fn validate_limit(value: usize, kind: LimitKind) -> Result<(), RetentionError> {
    if value == 0 || value > isize::MAX as usize {
        return Err(RetentionError::InvalidLimit(kind));
    }
    Ok(())
}

fn checked_bytes(first: usize, second: usize) -> Result<usize, RetentionError> {
    first
        .checked_add(second)
        .ok_or(RetentionError::SizeOverflow)
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn retained_header(name: &str) -> bool {
    [
        "content-type",
        "retry-after",
        "x-ms-request-id",
        "request-id",
        "x-ms-operation-id",
        "x-ms-version",
        "etag",
        "last-modified",
    ]
    .iter()
    .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    fn limits(
        responses: usize,
        total: usize,
        response: usize,
        headers: usize,
        count: usize,
    ) -> RetentionLimits {
        RetentionLimits::new(responses, total, response, headers, count)
            .expect("valid synthetic limits")
    }

    fn coverage(scope: CoverageScope, complete: Option<bool>, pages: usize) -> Coverage {
        Coverage::new(scope, complete, pages).expect("valid synthetic coverage")
    }

    fn unknown_outcome(limits: RetentionLimits) -> Outcome {
        Outcome::new(
            Effect::UnknownWriteCapable,
            coverage(CoverageScope::Response, None, 0),
            limits,
        )
    }

    fn response(role: ResponseRole, status: u16, body: &[u8]) -> NativeResponse {
        NativeResponse::new(
            role,
            status,
            Vec::new(),
            body.to_vec(),
            RetentionLimits::default(),
        )
        .expect("valid synthetic response")
    }

    fn headers(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    }

    fn reference(id: &str) -> OperationReference {
        OperationReference::new(OperationReferenceKind::Operation, id.into(), 1024)
            .expect("valid synthetic opaque ID")
    }

    #[test]
    fn binary_and_empty_bodies_round_trip_without_status_normalization() {
        for (status, body) in [
            (100, &b""[..]),
            (200, &b"\0\xff\xfe{}\r\n"[..]),
            (202, &b"accepted"[..]),
            (204, &b""[..]),
            (304, &b"not normalized"[..]),
            (429, &b"\xff"[..]),
            (599, &b""[..]),
        ] {
            let native = response(ResponseRole::Initial, status, body);
            assert_eq!(native.status(), status);
            assert_eq!(native.body(), body);
            assert_eq!(native.retained_bytes(), body.len());
        }
    }

    #[test]
    fn all_roles_preserve_native_status_and_bytes() {
        for role in [
            ResponseRole::Initial,
            ResponseRole::Monitor,
            ResponseRole::Result,
            ResponseRole::Page,
        ] {
            let native = response(role, 202, b"\xff");
            assert_eq!(native.role(), role);
            assert_eq!(native.status(), 202);
            assert_eq!(native.body(), b"\xff");
        }
    }

    #[test]
    fn invalid_native_statuses_are_rejected_without_echoing_input() {
        for status in [0, 99, 600, u16::MAX] {
            assert_eq!(
                NativeResponse::new(
                    ResponseRole::Initial,
                    status,
                    Vec::new(),
                    Vec::new(),
                    RetentionLimits::default()
                )
                .expect_err("invalid native status"),
                RetentionError::InvalidStatus
            );
        }
    }

    #[test]
    fn whitelist_keeps_order_duplicates_and_verbatim_utf8_values() {
        let native = NativeResponse::new(
            ResponseRole::Page,
            206,
            headers(&[
                ("ETag", "first"),
                ("Content-Type", "application/json; label=é"),
                ("etag", "second"),
                ("Retry-After", "7"),
                ("X-MS-REQUEST-ID", "request-a"),
                ("Request-ID", "request-b"),
                ("X-MS-Operation-Id", "operation-a"),
                ("X-MS-Version", "2026-01-01"),
                ("Last-Modified", "yesterday"),
            ]),
            b"native".to_vec(),
            RetentionLimits::default(),
        )
        .expect("safe headers");
        let actual: Vec<_> = native
            .headers()
            .iter()
            .map(|header| (header.name(), header.value()))
            .collect();
        assert_eq!(
            actual,
            [
                ("etag", "first"),
                ("content-type", "application/json; label=é"),
                ("etag", "second"),
                ("retry-after", "7"),
                ("x-ms-request-id", "request-a"),
                ("request-id", "request-b"),
                ("x-ms-operation-id", "operation-a"),
                ("x-ms-version", "2026-01-01"),
                ("last-modified", "yesterday"),
            ]
        );
        assert_eq!(native.omitted_header_count(), 0);
        let header_bytes: usize = actual
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum();
        assert_eq!(native.retained_bytes(), header_bytes + 6);
    }

    #[test]
    fn dangerous_and_unknown_headers_are_omitted_with_explicit_count() {
        let native = NativeResponse::new(
            ResponseRole::Initial,
            202,
            headers(&[
                ("Authorization", "Bearer synthetic-secret"),
                ("set-cookie", "synthetic-cookie"),
                ("Cookie", "synthetic-cookie"),
                ("Location", "https://example.invalid/synthetic-token"),
                ("x-unknown", "policy-evidence"),
                ("etag", "retained"),
                ("X-Unknown", "more-evidence"),
            ]),
            b"accepted".to_vec(),
            RetentionLimits::default(),
        )
        .expect("filtered response");
        assert_eq!(native.status(), 202);
        assert_eq!(native.body(), b"accepted");
        assert_eq!(native.omitted_header_count(), 6);
        assert_eq!(native.headers().len(), 1);
        assert_eq!(native.headers()[0].name(), "etag");
        assert_eq!(native.headers()[0].value(), "retained");
        assert_eq!(native.retained_bytes(), 8 + 4 + 8);
    }

    #[test]
    fn all_header_name_controls_non_ascii_and_invalid_tokens_are_rejected() {
        for name in [
            "",
            "é",
            "white space",
            "bad:name",
            "bad/name",
            "bad\0",
            "bad\r\n",
        ] {
            assert_eq!(
                NativeResponse::new(
                    ResponseRole::Initial,
                    200,
                    vec![(name.into(), "value".into())],
                    Vec::new(),
                    RetentionLimits::default()
                )
                .expect_err("invalid header name"),
                RetentionError::InvalidHeaderName
            );
        }
        for control in 0_u8..=127 {
            if control.is_ascii_control() {
                let name = format!("x-{}", char::from(control));
                assert_eq!(
                    NativeResponse::new(
                        ResponseRole::Initial,
                        200,
                        vec![(name, String::new())],
                        Vec::new(),
                        RetentionLimits::default()
                    )
                    .expect_err("control in name"),
                    RetentionError::InvalidHeaderName
                );
            }
        }
    }

    #[test]
    fn every_http_token_character_is_valid_even_if_header_is_omitted() {
        let native = NativeResponse::new(
            ResponseRole::Initial,
            200,
            headers(&[("!#$%&'*+-.^_`|~012AZaz", "")]),
            Vec::new(),
            RetentionLimits::default(),
        )
        .expect("valid token header name");
        assert_eq!(native.omitted_header_count(), 1);
        assert!(native.headers().is_empty());
    }

    #[test]
    fn controls_are_rejected_even_in_omitted_header_values() {
        for name in ["etag", "authorization", "x-unknown"] {
            for value in ["nul\0", "tab\t", "line\r\n", "del\u{7f}", "nel\u{85}"] {
                assert_eq!(
                    NativeResponse::new(
                        ResponseRole::Initial,
                        200,
                        headers(&[(name, value)]),
                        Vec::new(),
                        RetentionLimits::default()
                    )
                    .expect_err("control in header value"),
                    RetentionError::InvalidHeaderValue
                );
            }
        }
    }

    #[test]
    fn body_and_header_exact_limits_accept_and_one_extra_byte_rejects() {
        let bounds = limits(2, 20, 10, 5, 1);
        let exact = NativeResponse::new(
            ResponseRole::Initial,
            200,
            headers(&[("ETag", "x")]),
            vec![0; 5],
            bounds,
        )
        .expect("exact body plus header limit");
        assert_eq!(exact.retained_bytes(), 10);
        assert_eq!(
            NativeResponse::new(
                ResponseRole::Initial,
                200,
                headers(&[("etag", "x")]),
                vec![0; 6],
                bounds
            )
            .expect_err("response too large"),
            RetentionError::ResponseBytesExceeded
        );
        assert_eq!(
            NativeResponse::new(
                ResponseRole::Initial,
                200,
                headers(&[("etag", "xx")]),
                Vec::new(),
                bounds
            )
            .expect_err("headers too large"),
            RetentionError::HeaderBytesExceeded
        );
        assert_eq!(
            NativeResponse::new(ResponseRole::Initial, 200, Vec::new(), vec![0; 11], bounds)
                .expect_err("body too large"),
            RetentionError::ResponseBytesExceeded
        );
    }

    #[test]
    fn header_count_and_bytes_include_omitted_input() {
        let bounds = limits(2, 20, 10, 5, 1);
        let omitted = NativeResponse::new(
            ResponseRole::Initial,
            200,
            headers(&[("x", "1234")]),
            vec![0; 10],
            bounds,
        )
        .expect("exact input header bound");
        assert_eq!(omitted.omitted_header_count(), 1);
        assert_eq!(omitted.retained_bytes(), 10);
        assert_eq!(
            NativeResponse::new(
                ResponseRole::Initial,
                200,
                headers(&[("x", "12345")]),
                Vec::new(),
                bounds
            )
            .expect_err("omitted header still bounded"),
            RetentionError::HeaderBytesExceeded
        );
        assert_eq!(
            NativeResponse::new(
                ResponseRole::Initial,
                200,
                headers(&[("x", ""), ("x", "")]),
                Vec::new(),
                bounds
            )
            .expect_err("omitted headers count"),
            RetentionError::HeaderCountExceeded
        );
    }

    #[test]
    fn default_limits_are_finite_and_documented() {
        let bounds = RetentionLimits::default();
        assert_eq!(bounds.max_responses(), 100);
        assert_eq!(bounds.max_retained_bytes(), 64 * 1024 * 1024);
        assert_eq!(bounds.max_response_bytes(), 4 * 1024 * 1024);
        assert_eq!(bounds.max_header_bytes(), 16 * 1024);
        assert_eq!(bounds.max_headers(), 64);
    }

    #[test]
    fn invalid_numeric_limits_are_rejected() {
        for invalid in [0, usize::MAX] {
            for (values, kind) in [
                ([invalid, 100, 50, 20, 5], LimitKind::Responses),
                ([10, invalid, 50, 20, 5], LimitKind::RetainedBytes),
                ([10, 100, invalid, 20, 5], LimitKind::ResponseBytes),
                ([10, 100, 50, invalid, 5], LimitKind::HeaderBytes),
                ([10, 100, 50, 20, invalid], LimitKind::Headers),
            ] {
                let [responses, total, response, headers, count] = values;
                assert_eq!(
                    RetentionLimits::new(responses, total, response, headers, count),
                    Err(RetentionError::InvalidLimit(kind))
                );
            }
        }
        for bounds in [
            RetentionLimits::new(1, 10, 11, 5, 1),
            RetentionLimits::new(1, 10, 10, 11, 1),
        ] {
            assert_eq!(bounds, Err(RetentionError::InconsistentByteLimits));
        }
    }

    #[test]
    fn length_overflow_is_a_typed_error_not_wraparound() {
        assert_eq!(
            checked_bytes(usize::MAX, 1),
            Err(RetentionError::SizeOverflow)
        );
        assert_eq!(checked_bytes(usize::MAX - 1, 1), Ok(usize::MAX));
    }

    #[test]
    fn coverage_preserves_unknown_and_scope_without_collection_inference() {
        for complete in [None, Some(false), Some(true)] {
            let response = coverage(CoverageScope::Response, complete, 0);
            assert_eq!(response.scope(), CoverageScope::Response);
            assert_eq!(response.complete(), complete);
            assert_eq!(response.pages(), 0);
            let page = coverage(CoverageScope::Page, complete, 1);
            assert_eq!(page.scope(), CoverageScope::Page);
            assert_eq!(page.complete(), complete);
            assert_eq!(page.pages(), 1);
            let collection = coverage(CoverageScope::Collection, complete, 3);
            assert_eq!(collection.scope(), CoverageScope::Collection);
            assert_eq!(collection.complete(), complete);
            assert_eq!(collection.pages(), 3);
        }
        assert_eq!(
            coverage(CoverageScope::Collection, None, 0).complete(),
            None
        );
        assert_eq!(
            coverage(CoverageScope::Collection, Some(false), 0).complete(),
            Some(false)
        );
    }

    #[test]
    fn invalid_coverage_page_counts_are_rejected() {
        for (scope, pages) in [
            (CoverageScope::Response, 1),
            (CoverageScope::Page, 0),
            (CoverageScope::Page, 2),
        ] {
            assert_eq!(
                Coverage::new(scope, None, pages),
                Err(CoverageError::InvalidPageCount)
            );
        }
        assert_eq!(
            Coverage::new(CoverageScope::Collection, Some(true), 0),
            Err(CoverageError::CompleteCollectionWithoutPage)
        );
    }

    #[test]
    fn outcome_state_effect_and_coverage_are_separate_and_explicit() {
        for effect in [
            Effect::ReadOnly,
            Effect::WriteCapable,
            Effect::UnknownWriteCapable,
        ] {
            let page_coverage = coverage(CoverageScope::Page, Some(true), 1);
            let mut outcome = Outcome::new(effect, page_coverage, RetentionLimits::default());
            assert_eq!(outcome.state(), State::Unknown);
            outcome
                .append(response(ResponseRole::Initial, 200, b"{}"))
                .expect("retain HTTP 200");
            assert_eq!(outcome.state(), State::Unknown);
            for state in [
                State::Accepted,
                State::Partial,
                State::Failed,
                State::Unknown,
                State::Completed,
            ] {
                outcome.set_state(state).expect("explicit protocol state");
                assert_eq!(outcome.state(), state);
                assert_eq!(outcome.effect(), effect);
                assert_eq!(outcome.coverage(), page_coverage);
                assert_eq!(outcome.responses()[0].status(), 200);
            }
            outcome.set_coverage(coverage(CoverageScope::Collection, None, 10));
            assert_eq!(outcome.state(), State::Completed);
            assert_eq!(outcome.coverage().complete(), None);
            assert_eq!(outcome.coverage().pages(), 10);
            assert_eq!(outcome.responses().len(), 1);
        }
    }

    #[test]
    fn no_latest_202_role_can_be_completed() {
        for role in [
            ResponseRole::Initial,
            ResponseRole::Monitor,
            ResponseRole::Result,
            ResponseRole::Page,
        ] {
            let mut outcome = unknown_outcome(RetentionLimits::default());
            outcome
                .append(response(role, 202, b"accepted"))
                .expect("retain accepted observation");
            outcome.set_state(State::Accepted).expect("accepted state");
            assert_eq!(
                outcome.set_state(State::Completed),
                Err(RetentionError::AcceptedResponseNotCompleted)
            );
            assert_eq!(outcome.state(), State::Accepted);
            assert_eq!(outcome.coverage().complete(), None);
            assert_eq!(outcome.responses()[0].status(), 202);
        }
    }

    #[test]
    fn completion_requires_an_observation_but_other_states_do_not() {
        let mut outcome = unknown_outcome(RetentionLimits::default());
        for state in [
            State::Accepted,
            State::Partial,
            State::Failed,
            State::Unknown,
        ] {
            outcome.set_state(state).expect("state without response");
            assert_eq!(outcome.state(), state);
        }
        assert_eq!(
            outcome.set_state(State::Completed),
            Err(RetentionError::CompletedWithoutResponse)
        );
        assert_eq!(outcome.state(), State::Unknown);
    }

    #[test]
    fn initial_202_survives_explicit_completion_after_result_200() {
        let mut outcome = unknown_outcome(RetentionLimits::default());
        outcome
            .append(response(
                ResponseRole::Initial,
                202,
                b"initial accepted bytes",
            ))
            .expect("initial response");
        outcome.set_state(State::Accepted).expect("accepted");
        outcome
            .append(response(ResponseRole::Monitor, 200, b"still running"))
            .expect("monitor response");
        assert_eq!(outcome.state(), State::Accepted);
        outcome
            .append(response(ResponseRole::Result, 200, b"final result bytes"))
            .expect("result response");
        assert_eq!(outcome.state(), State::Accepted);
        outcome
            .set_state(State::Completed)
            .expect("protocol completed");
        assert_eq!(outcome.state(), State::Completed);
        assert_eq!(outcome.coverage().complete(), None);
        assert_eq!(outcome.responses()[0].status(), 202);
        assert_eq!(outcome.responses()[0].body(), b"initial accepted bytes");
        assert_eq!(outcome.responses()[2].body(), b"final result bytes");
    }

    #[test]
    fn completed_outcome_rejects_a_new_202_without_mutating_prior_observations() {
        let mut outcome = unknown_outcome(RetentionLimits::default());
        outcome
            .append(response(ResponseRole::Result, 200, b"done"))
            .expect("result");
        outcome.set_state(State::Completed).expect("complete");
        assert_eq!(
            outcome.append(response(ResponseRole::Initial, 202, b"new accepted")),
            Err(RetentionError::AcceptedResponseNotCompleted)
        );
        assert_eq!(outcome.state(), State::Completed);
        assert_eq!(outcome.retained_bytes(), 4);
        assert_eq!(outcome.responses().len(), 1);
        assert_eq!(outcome.responses()[0].body(), b"done");
        outcome
            .set_state(State::Accepted)
            .expect("new observation phase");
        outcome
            .append(response(ResponseRole::Initial, 202, b"new accepted"))
            .expect("explicit accepted phase");
    }

    #[test]
    fn ordered_pages_and_duplicate_bodies_and_headers_are_not_coalesced() {
        let mut outcome = Outcome::new(
            Effect::ReadOnly,
            coverage(CoverageScope::Collection, Some(false), 3),
            RetentionLimits::default(),
        );
        for body in [b"page-b", b"page-a", b"page-b"] {
            let native = NativeResponse::new(
                ResponseRole::Page,
                200,
                headers(&[("etag", "a"), ("ETAG", "b"), ("etag", "a")]),
                body.to_vec(),
                RetentionLimits::default(),
            )
            .expect("page response");
            outcome.append(native).expect("retain page");
        }
        let actual: Vec<_> = outcome
            .responses()
            .iter()
            .map(NativeResponse::body)
            .collect();
        assert_eq!(actual, [&b"page-b"[..], &b"page-a"[..], &b"page-b"[..]]);
        for native in outcome.responses() {
            assert_eq!(native.role(), ResponseRole::Page);
            let values: Vec<_> = native.headers().iter().map(ResponseHeader::value).collect();
            assert_eq!(values, ["a", "b", "a"]);
        }
        assert_eq!(outcome.coverage().complete(), Some(false));
        assert_eq!(outcome.state(), State::Unknown);
    }

    #[test]
    fn failed_append_at_total_bound_keeps_every_prior_observation_and_metadata() {
        let bounds = limits(3, 10, 10, 1, 1);
        let mut outcome = unknown_outcome(bounds);
        outcome
            .set_operation_reference(Some(reference("op")))
            .expect("reference");
        outcome
            .append(response(ResponseRole::Initial, 202, b"12345678"))
            .expect("exact aggregate bound");
        outcome.set_state(State::Partial).expect("explicit partial");
        let before_coverage = outcome.coverage();
        assert_eq!(outcome.retained_bytes(), 10);
        assert_eq!(
            outcome.append(response(ResponseRole::Monitor, 200, b"x")),
            Err(RetentionError::TotalBytesExceeded)
        );
        assert_eq!(outcome.retained_bytes(), 10);
        assert_eq!(outcome.responses().len(), 1);
        assert_eq!(outcome.responses()[0].status(), 202);
        assert_eq!(outcome.responses()[0].body(), b"12345678");
        assert_eq!(
            outcome.operation_reference().expect("old reference").id(),
            "op"
        );
        assert_eq!(outcome.state(), State::Partial);
        assert_eq!(outcome.effect(), Effect::UnknownWriteCapable);
        assert_eq!(outcome.coverage(), before_coverage);
        assert_eq!(outcome.limits(), bounds);
    }

    #[test]
    fn zero_byte_observations_still_obey_the_response_count_limit() {
        let mut outcome = unknown_outcome(limits(2, 1, 1, 1, 1));
        for _ in 0..2 {
            outcome
                .append(response(ResponseRole::Monitor, 204, b""))
                .expect("bounded empty response");
        }
        assert_eq!(
            outcome.append(response(ResponseRole::Monitor, 204, b"")),
            Err(RetentionError::ResponseCountExceeded)
        );
        assert_eq!(outcome.responses().len(), 2);
        assert_eq!(outcome.retained_bytes(), 0);
        assert_eq!(outcome.state(), State::Unknown);
    }

    #[test]
    fn zero_body_headers_are_charged_to_aggregate_bound() {
        let bounds = limits(3, 10, 10, 10, 2);
        let mut outcome = unknown_outcome(bounds);
        for _ in 0..2 {
            let native = NativeResponse::new(
                ResponseRole::Page,
                200,
                headers(&[("etag", "x")]),
                Vec::new(),
                bounds,
            )
            .expect("header-only response");
            outcome.append(native).expect("exact total header bound");
        }
        assert_eq!(outcome.retained_bytes(), 10);
        let native = NativeResponse::new(
            ResponseRole::Page,
            200,
            headers(&[("etag", "")]),
            Vec::new(),
            bounds,
        )
        .expect("extra header-only response");
        assert_eq!(
            outcome.append(native),
            Err(RetentionError::TotalBytesExceeded)
        );
        assert_eq!(outcome.responses().len(), 2);
        assert_eq!(outcome.retained_bytes(), 10);
    }

    #[test]
    fn admission_rechecks_tighter_response_header_byte_and_count_limits() {
        let mut outcome = unknown_outcome(limits(3, 20, 10, 5, 1));
        for (entries, body, expected) in [
            (
                vec![("x", ""), ("y", "")],
                Vec::new(),
                RetentionError::HeaderCountExceeded,
            ),
            (
                vec![("x", "12345")],
                Vec::new(),
                RetentionError::HeaderBytesExceeded,
            ),
            (vec![], vec![0; 11], RetentionError::ResponseBytesExceeded),
        ] {
            let native = NativeResponse::new(
                ResponseRole::Initial,
                200,
                headers(&entries),
                body,
                RetentionLimits::default(),
            )
            .expect("valid under larger construction limits");
            assert_eq!(outcome.append(native), Err(expected));
            assert!(outcome.responses().is_empty());
            assert_eq!(outcome.retained_bytes(), 0);
        }
    }

    #[test]
    fn operation_reference_is_explicit_bounded_and_not_a_url_validator() {
        let operation = reference("opaque-operation-42");
        assert_eq!(operation.kind(), OperationReferenceKind::Operation);
        assert_eq!(operation.id(), "opaque-operation-42");
        assert_eq!(operation.retained_bytes(), 19);
        let continuation = OperationReference::new(
            OperationReferenceKind::Continuation,
            "opaque-page-2".into(),
            13,
        )
        .expect("opaque continuation");
        assert_eq!(continuation.kind(), OperationReferenceKind::Continuation);
        assert_eq!(continuation.id(), "opaque-page-2");
        // Syntax acceptance is deliberately not URL-origin or secret validation.
        assert!(
            OperationReference::new(
                OperationReferenceKind::Operation,
                "https://example.invalid/not-validated".into(),
                100
            )
            .is_ok()
        );
    }

    #[test]
    fn operation_reference_rejects_invalid_ids_and_limits_without_echoing() {
        for id in ["", "space here", "\0", "line\r\n", "\u{7f}", "é", "\t"] {
            assert_eq!(
                OperationReference::new(OperationReferenceKind::Operation, id.into(), 100)
                    .expect_err("invalid opaque ID"),
                RetentionError::InvalidOperationId
            );
        }
        for bound in [0, usize::MAX] {
            assert_eq!(
                OperationReference::new(OperationReferenceKind::Operation, "op".into(), bound)
                    .expect_err("invalid reference bound"),
                RetentionError::InvalidLimit(LimitKind::OperationBytes)
            );
        }
        assert_eq!(
            OperationReference::new(OperationReferenceKind::Operation, "1234".into(), 3)
                .expect_err("reference too large"),
            RetentionError::OperationBytesExceeded
        );
        assert_eq!(
            OperationReference::new(OperationReferenceKind::Operation, "123".into(), 3)
                .expect("exact reference bound")
                .id(),
            "123"
        );
    }

    #[test]
    fn replacing_and_clearing_references_releases_only_successfully_replaced_bytes() {
        let mut outcome = unknown_outcome(limits(2, 10, 10, 1, 1));
        outcome
            .append(response(ResponseRole::Initial, 200, b"123456"))
            .expect("body");
        outcome
            .set_operation_reference(Some(reference("abcd")))
            .expect("exact aggregate bound");
        assert_eq!(outcome.retained_bytes(), 10);
        assert_eq!(
            outcome.set_operation_reference(Some(reference("abcde"))),
            Err(RetentionError::TotalBytesExceeded)
        );
        assert_eq!(
            outcome.operation_reference().expect("old reference").id(),
            "abcd"
        );
        assert_eq!(outcome.retained_bytes(), 10);
        outcome
            .set_operation_reference(Some(reference("x")))
            .expect("smaller replacement");
        assert_eq!(
            outcome.operation_reference().expect("new reference").id(),
            "x"
        );
        assert_eq!(outcome.retained_bytes(), 7);
        outcome
            .set_operation_reference(None)
            .expect("clear reference");
        assert!(outcome.operation_reference().is_none());
        assert_eq!(outcome.retained_bytes(), 6);
        assert_eq!(outcome.responses()[0].body(), b"123456");
    }

    #[test]
    fn reference_alone_is_bounded_even_without_responses() {
        let mut outcome = unknown_outcome(limits(1, 4, 4, 1, 1));
        assert_eq!(
            outcome.set_operation_reference(Some(reference("12345"))),
            Err(RetentionError::TotalBytesExceeded)
        );
        assert!(outcome.operation_reference().is_none());
        assert_eq!(outcome.retained_bytes(), 0);
        outcome
            .set_operation_reference(Some(reference("1234")))
            .expect("exact aggregate bound");
        assert_eq!(outcome.retained_bytes(), 4);
    }

    #[test]
    fn debug_omits_all_provider_content_including_nested_headers_and_reference() {
        let native = NativeResponse::new(
            ResponseRole::Initial,
            202,
            headers(&[
                ("ETag", "synthetic-allowed-token"),
                ("Location", "https://example.invalid/synthetic-url"),
                ("Authorization", "Bearer synthetic-credential"),
            ]),
            b"synthetic-body-secret".to_vec(),
            RetentionLimits::default(),
        )
        .expect("synthetic response");
        let header_debug = format!("{:?}", native.headers());
        let response_debug = format!("{native:?}");
        let operation = reference("synthetic-opaque-secret");
        let operation_debug = format!("{operation:?}");
        let mut outcome = unknown_outcome(RetentionLimits::default());
        outcome.append(native).expect("retain response");
        outcome
            .set_operation_reference(Some(operation))
            .expect("retain reference");
        let outcome_debug = format!("{outcome:?}");
        for text in [header_debug, response_debug, operation_debug, outcome_debug] {
            for secret in [
                "synthetic-allowed-token",
                "synthetic-url",
                "synthetic-credential",
                "synthetic-body-secret",
                "synthetic-opaque-secret",
                "https://",
                "Bearer",
                "ETag",
                "etag",
            ] {
                assert!(
                    !text.contains(secret),
                    "Debug must exclude provider content"
                );
            }
        }
        assert_eq!(outcome.responses()[0].body(), b"synthetic-body-secret");
        assert_eq!(
            outcome.responses()[0].headers()[0].value(),
            "synthetic-allowed-token"
        );
        assert_eq!(
            outcome.operation_reference().expect("explicit access").id(),
            "synthetic-opaque-secret"
        );
    }

    #[test]
    fn scalar_projection_does_not_replace_native_metadata_or_response() {
        let mut outcome = Outcome::new(
            Effect::ReadOnly,
            coverage(CoverageScope::Page, Some(true), 1),
            RetentionLimits::default(),
        );
        outcome
            .append(response(ResponseRole::Page, 206, b"{\"value\":42}"))
            .expect("native observation");
        outcome
            .set_state(State::Partial)
            .expect("partial collection");
        let projected_scalar = outcome.responses()[0].body().len();
        assert_eq!(projected_scalar, 12);
        assert_eq!(outcome.state(), State::Partial);
        assert_eq!(outcome.effect(), Effect::ReadOnly);
        assert_eq!(outcome.coverage().scope(), CoverageScope::Page);
        assert_eq!(outcome.coverage().complete(), Some(true));
        assert_eq!(outcome.responses()[0].status(), 206);
        assert_eq!(outcome.responses()[0].body(), b"{\"value\":42}");
    }

    #[test]
    fn zero_body_omitted_headers_are_counted_without_retaining_their_bytes() {
        let bounds = limits(2, 5, 5, 5, 1);
        let mut outcome = unknown_outcome(bounds);
        for _ in 0..2 {
            let native = NativeResponse::new(
                ResponseRole::Page,
                204,
                headers(&[("x", "1234")]),
                Vec::new(),
                bounds,
            )
            .expect("bounded omitted input");
            outcome.append(native).expect("retain omission evidence");
        }
        assert_eq!(outcome.retained_bytes(), 0);
        assert_eq!(outcome.responses()[0].omitted_header_count(), 1);
        assert_eq!(outcome.responses()[1].omitted_header_count(), 1);
        assert_eq!(outcome.responses().len(), 2);
    }

    #[test]
    fn caller_spare_capacity_is_not_retained_as_unbudgeted_payload_storage() {
        let mut body = Vec::with_capacity(4096);
        body.extend_from_slice(b"ok");
        let mut name = String::with_capacity(4096);
        name.push_str("ETag");
        let mut value = String::with_capacity(4096);
        value.push('x');
        let native = NativeResponse::new(
            ResponseRole::Initial,
            200,
            vec![(name, value)],
            body,
            limits(1, 7, 7, 5, 1),
        )
        .expect("only logical payload bytes retained in boxed storage");
        assert_eq!(native.retained_bytes(), 7);
        assert_eq!(native.body.len(), 2);
        assert_eq!(native.headers[0].name.len(), 4);
        assert_eq!(native.headers[0].value.len(), 1);
    }
}
