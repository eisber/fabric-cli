// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ArrowError, ArrowRequest, RawChunk, ResultSelectors, StatementHandle, StreamLimits};
use ring::digest::{SHA256, digest};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// In-memory last completely delivered chunk, never a durable checkpoint.
#[derive(Clone, Eq, PartialEq)]
pub struct ResumeCursor {
    handle: StatementHandle,
    selectors: ResultSelectors,
}

impl std::fmt::Debug for ResumeCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResumeCursor(<owner-bound in-memory state>)")
    }
}

impl ResumeCursor {
    /// Exact statement scope required for every resumed GET.
    pub const fn handle(&self) -> &StatementHandle { &self.handle }
    /// Last delivered ID/partition with original `parts` preserved.
    pub const fn selectors(&self) -> &ResultSelectors { &self.selectors }
}

/// Result of an in-memory sink delivery, not a transaction/durability receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// Sink callback completed and the local cursor advanced.
    Delivered,
    /// Identical previously delivered chunk was suppressed.
    Replayed,
}

#[derive(Eq, PartialEq)]
struct Identity {
    partition: i32,
    ordinal: i32,
    digest: [u8; 32],
}

/// Bounded logical-result identity ledger across multiple response parsers.
///
/// Retains SHA-256 identities, not old payloads. A successful callback means
/// in-memory delivery only: no exactly-once sink, persistence or restart resume.
/// A callback error poisons the ledger because external output may be partial.
pub struct DeliveryState {
    handle: StatementHandle,
    selectors: ResultSelectors,
    cursor: Option<ResumeCursor>,
    limits: StreamLimits,
    ids: HashMap<String, Identity>,
    orders: HashSet<(i32, i32)>,
    total_bytes: u64,
    identity_bytes: usize,
    failed: bool,
}

impl DeliveryState {
    /// Begin one logical iteration with a fixed owner and initial selectors.
    ///
    /// Starting at an external selector does not import historical identities or
    /// make a previous process's output deduplicated.
    pub fn new(
        handle: StatementHandle,
        selectors: ResultSelectors,
        limits: StreamLimits,
    ) -> Result<Self, ArrowError> {
        Ok(Self {
            handle,
            selectors,
            cursor: None,
            limits: limits.validate()?,
            ids: HashMap::new(),
            orders: HashSet::new(),
            total_bytes: 0,
            identity_bytes: 0,
            failed: false,
        })
    }

    /// Verify replay identity and deliver a new chunk once to this local sink.
    ///
    /// Ordinals must be unique within a partition, not sorted/contiguous. A
    /// replay ID must retain partition, ordinal and payload SHA-256. A fresh ID
    /// must not reuse an occupied position. Use a fresh `FramedDecoder` for each
    /// HTTP response so within-response duplicates are still rejected.
    pub fn deliver(
        &mut self,
        handle: &StatementHandle,
        chunk: &RawChunk,
        emit: impl FnOnce(&RawChunk) -> Result<(), ArrowError>,
    ) -> Result<Delivery, ArrowError> {
        if self.failed { return Err(ArrowError::InvalidState); }
        if handle != &self.handle { return Err(ArrowError::OwnerMismatch); }
        let result = self.deliver_inner(chunk, emit);
        if result.is_err() { self.failed = true; }
        result
    }

    fn deliver_inner(
        &mut self,
        chunk: &RawChunk,
        emit: impl FnOnce(&RawChunk) -> Result<(), ArrowError>,
    ) -> Result<Delivery, ArrowError> {
        if chunk.payload().len() > self.limits.max_frame_bytes
            || chunk.id().len() > self.limits.max_id_bytes
        { return Err(ArrowError::Limit); }
        let identity = Identity {
            partition: chunk.partition(),
            ordinal: chunk.ordinal(),
            digest: digest(&SHA256, chunk.payload()).as_ref().try_into().map_err(|_| ArrowError::InvalidState)?,
        };
        if let Some(previous) = self.ids.get(chunk.id()) {
            return if previous == &identity { Ok(Delivery::Replayed) } else { Err(ArrowError::ReplayConflict) };
        }
        let position = (chunk.partition(), chunk.ordinal());
        if self.orders.contains(&position) { return Err(ArrowError::OrdinalConflict); }
        if self.ids.len() >= self.limits.max_chunks { return Err(ArrowError::Limit); }
        let total = self.total_bytes.checked_add(chunk.payload().len() as u64)
            .filter(|total| *total <= self.limits.max_total_bytes).ok_or(ArrowError::Limit)?;
        let identity_bytes = self.identity_bytes.checked_add(chunk.id().len())
            .filter(|total| *total <= self.limits.max_identity_bytes).ok_or(ArrowError::Limit)?;
        self.ids.try_reserve(1).map_err(|_| ArrowError::Allocation)?;
        self.orders.try_reserve(1).map_err(|_| ArrowError::Allocation)?;
        let mut id = String::new();
        id.try_reserve_exact(chunk.id().len()).map_err(|_| ArrowError::Allocation)?;
        id.push_str(chunk.id());
        let cursor = ResumeCursor {
            handle: self.handle.clone(),
            selectors: self.selectors.resumed(chunk.id(), chunk.partition()),
        };
        emit(chunk).map_err(|_| ArrowError::SinkFailed)?;
        self.ids.insert(id, identity);
        self.orders.insert(position);
        self.total_bytes = total;
        self.identity_bytes = identity_bytes;
        self.cursor = Some(cursor);
        Ok(Delivery::Delivered)
    }

    /// Local cursor after the last callback completed; unavailable before then.
    pub const fn cursor(&self) -> Option<&ResumeCursor> { self.cursor.as_ref() }
    /// Unique chunk count; identical replay does not consume logical budget.
    pub fn delivered_chunks(&self) -> usize { self.ids.len() }
    /// Unique payload bytes; each response separately counts replay/wire bytes.
    pub const fn delivered_bytes(&self) -> u64 { self.total_bytes }
}

/// Host-observed failure classification; this policy does no network I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    /// An actual transport interruption of a result GET.
    Transport,
    /// Missing terminal/partial frame at EOF.
    Truncated,
    /// Actual HTTP status; only the explicitly enumerated statuses are transient.
    Http(u16),
    /// Framing/media/replay invariant violation.
    Protocol,
    /// Explicit -1 failure marker.
    ServiceFailure,
    /// Typed schema conflict or unsafe/unsupported decode.
    Decode,
    /// Local cancellation, not remote cancellation.
    Cancelled,
    /// Absolute deadline expired.
    TimedOut,
    /// Dispatched mutation has an unknown outcome.
    OutcomeUnknown,
    /// Sink failed after possibly writing partial output.
    Sink,
}

impl From<ArrowError> for Failure {
    fn from(error: ArrowError) -> Self {
        match error {
            ArrowError::Truncated => Self::Truncated,
            ArrowError::Http(status) => Self::Http(status),
            ArrowError::ServiceFailure => Self::ServiceFailure,
            ArrowError::SchemaConflict | ArrowError::UnsupportedCapability => Self::Decode,
            ArrowError::TimedOut => Self::TimedOut,
            ArrowError::OutcomeUnknown => Self::OutcomeUnknown,
            ArrowError::SinkFailed | ArrowError::Output => Self::Sink,
            _ => Self::Protocol,
        }
    }
}

/// Why no further request is permitted by this retry-policy instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// Preserve the underlying nontransient failure.
    Permanent(Failure),
    /// Configured reconnect count is exhausted.
    Exhausted,
    /// Absolute operation deadline expired.
    Deadline,
    /// POST is never replayed, even for an otherwise transient failure.
    Mutation,
    /// Policy already stopped; it cannot be reset by a later call.
    AlreadyStopped,
}

/// Finite plan only: callers execute requests/close resources themselves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryDecision {
    /// Open one new GET using the same owner/current in-memory cursor.
    Reconnect {
        /// One-based reconnect number; total openings are at most this budget + 1.
        attempt: u32,
    },
    /// Stop without a new dispatch.
    Stop(StopReason),
}

/// One absolute local deadline and finite reconnect budget.
///
/// Caller pauses and reconnects do not extend the deadline. Hosts must apply
/// `remaining` to actual reads/opens, honor cancellation, close response resources
/// on every path and pass the actual failed request to `on_failure`. No
/// backoff, Retry-After engine, remote cancellation or authentication is added.
pub struct RetryPolicy {
    deadline: Instant,
    max_reconnects: u32,
    attempts: u32,
    stopped: bool,
}

impl RetryPolicy {
    /// Start one positive absolute timeout using a host-supplied monotonic clock.
    pub fn new(now: Instant, timeout: Duration, max_reconnects: u32) -> Result<Self, ArrowError> {
        if timeout.is_zero() { return Err(ArrowError::InvalidInput); }
        Ok(Self {
            deadline: now.checked_add(timeout).ok_or(ArrowError::InvalidInput)?,
            max_reconnects,
            attempts: 0,
            stopped: false,
        })
    }

    /// Remaining local time; never resets after bytes, caller pauses or retries.
    pub fn remaining(&self, now: Instant) -> Result<Duration, ArrowError> {
        if self.stopped { return Err(ArrowError::InvalidState); }
        self.deadline.checked_duration_since(now).filter(|time| !time.is_zero()).ok_or(ArrowError::TimedOut)
    }

    /// Classify and consume at most one reconnect allowance.
    ///
    /// The actual validated request, not the command spelling, determines replay
    /// safety. No POST retry can be authorized by this policy.
    pub fn on_failure(&mut self, now: Instant, failure: Failure, request: &ArrowRequest) -> RetryDecision {
        let stop = if self.stopped {
            Some(StopReason::AlreadyStopped)
        } else if !request.is_result_get() {
            Some(StopReason::Mutation)
        } else if self.remaining(now).is_err() {
            Some(StopReason::Deadline)
        } else if !matches!(failure, Failure::Transport | Failure::Truncated | Failure::Http(408 | 429 | 500 | 502 | 503 | 504)) {
            Some(StopReason::Permanent(failure))
        } else if self.attempts >= self.max_reconnects {
            Some(StopReason::Exhausted)
        } else { None };
        if let Some(reason) = stop {
            self.stopped = true;
            RetryDecision::Stop(reason)
        } else {
            self.attempts += 1;
            RetryDecision::Reconnect { attempt: self.attempts }
        }
    }
}
