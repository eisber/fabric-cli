// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Owner-bound async framed GET consumption over a caller-owned streaming host.

use super::{
    ArrowError, Delivery, DeliveryState, FramedDecoder, RawChunk, ResponseHeaders, ResultSelectors,
    ResumeCursor, StatementHandle, StreamLimits,
};
use crate::runtime::{
    NativeDeadline, NativeError, NativeResponse, NativeStreamBody, NativeStreamExecutor,
    NativeStreamLimits, NativeStreamResponse,
};
use std::fmt;
use std::sync::Arc;

/// Explicit opt-in recovery; never permission to replay a mutation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FramedResume {
    /// One dispatch, with every failure returned immediately.
    #[default]
    Never,
    /// Recover from explicit transport evidence, enumerated HTTP statuses, and
    /// optionally truncated EOF. Evidence alone does not grant replay authority.
    /// Throttling responses are returned without automatic recovery.
    Recoverable {
        /// Maximum reconnects, additionally capped by the response budget.
        max_reconnects: u32,
        /// Treat actual HTTP EOF without a complete terminal as recoverable.
        ///
        /// This does not classify `InvalidResponse` or arbitrary I/O as replayable.
        retry_truncated_eof: bool,
    },
}

/// Finite per-response and whole-operation work/byte bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramedStreamOptions {
    /// Codec response bounds and logical unique-delivery bounds.
    pub frames: StreamLimits,
    /// Host fragment and response-body bounds, including wire framing.
    pub transport: NativeStreamLimits,
    /// Maximum opens, counting failed opens as well as returned responses.
    pub max_responses: u32,
    /// Maximum body reads across all responses, including EOF probes.
    pub max_reads: u64,
    /// Maximum wire bytes returned across all responses, including replays.
    pub max_wire_bytes: u64,
    /// Recovery is disabled by default.
    pub resume: FramedResume,
}

impl Default for FramedStreamOptions {
    fn default() -> Self {
        let frames = StreamLimits::default();
        Self {
            transport: NativeStreamLimits {
                max_fragment_bytes: 64 * 1024,
                max_body_bytes: frames.max_wire_bytes,
            },
            max_responses: 1,
            max_reads: 100_000_000,
            max_wire_bytes: frames.max_wire_bytes,
            frames,
            resume: FramedResume::Never,
        }
    }
}

impl FramedStreamOptions {
    /// Validate finite bounds before opening or allocating frame storage.
    pub fn validate(self) -> Result<Self, ArrowError> {
        self.frames.validate()?;
        self.transport
            .validate()
            .map_err(|_| ArrowError::InvalidInput)?;
        if self.max_responses == 0
            || self.max_reads == 0
            || self.max_wire_bytes < 8
            || self.transport.max_body_bytes < 8
        {
            return Err(ArrowError::InvalidInput);
        }
        Ok(self)
    }
}

/// Local observation state, never remote statement state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedStreamState {
    /// Ready to open/read/parse on the next pull.
    Ready,
    /// An executor open is in flight.
    Opening,
    /// A body read is in flight.
    Reading,
    /// A synchronous sink acknowledgement is in progress.
    Acknowledging,
    /// One complete candidate awaits explicit sink acknowledgement.
    Pending,
    /// Successful terminal and actual HTTP EOF were both validated.
    Complete,
    /// A terminal error stopped observation.
    Failed,
    /// The caller explicitly stopped local observation.
    Closed,
}

/// Sanitized operation counters, excluding application-retained output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramedStreamProgress {
    /// Local driver state.
    pub state: FramedStreamState,
    /// Executor calls, including unsuccessful opens.
    pub open_attempts: u32,
    /// Actual response objects returned by the executor.
    pub responses: u32,
    /// Reconnects consumed, never reset by successful reads.
    pub reconnects: u32,
    /// Actual body read calls, including EOF probes.
    pub read_calls: u64,
    /// Accepted bounded fragment bytes across responses, including framing/replay.
    pub wire_bytes: u64,
    /// Returned bytes in the last/current response.
    pub response_wire_bytes: u64,
    /// Complete candidates staged, including replay.
    pub staged_chunks: u64,
    /// Unique chunks whose sink callback successfully acknowledged delivery.
    pub delivered_chunks: usize,
    /// Unique acknowledged encoded payload bytes.
    pub delivered_bytes: u64,
    /// Identical acknowledged replays suppressed before the sink.
    pub replayed_chunks: u64,
    /// Responses disposed without validated terminal-plus-EOF success.
    pub discarded_responses: u32,
    /// Returned wire bytes in those discarded responses; not output rollback.
    pub discarded_wire_bytes: u64,
    /// A candidate remains unacknowledged, including one abandoned on failure.
    pub has_unacknowledged_chunk: bool,
    /// Actual EOF was observed on the last/current response.
    pub eof_received: bool,
    /// A failed sink may already have produced partial external output.
    pub sink_may_be_partial: bool,
    /// Last invalid/over-budget fragment's length, excluded from accepted counters.
    pub rejected_fragment_bytes: usize,
}

impl FramedStreamProgress {
    fn new() -> Self {
        Self {
            state: FramedStreamState::Ready,
            open_attempts: 0,
            responses: 0,
            reconnects: 0,
            read_calls: 0,
            wire_bytes: 0,
            response_wire_bytes: 0,
            staged_chunks: 0,
            delivered_chunks: 0,
            delivered_bytes: 0,
            replayed_chunks: 0,
            discarded_responses: 0,
            discarded_wire_bytes: 0,
            has_unacknowledged_chunk: false,
            eof_received: false,
            sink_may_be_partial: false,
            rejected_fragment_bytes: 0,
        }
    }
}

/// Actual sanitized cause, retained even when a reconnect budget is exhausted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedStreamCause {
    /// Codec, header, replay, owner or sink failure.
    Arrow(ArrowError),
    /// The host's actual error; ambiguous errors are not transport classifications.
    Native(NativeError),
}

/// Why local observation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedStreamStop {
    /// Nonrecoverable failure, or recovery was not explicitly enabled.
    Fatal,
    /// The finite reconnect allowance was exhausted.
    RetryLimit,
    /// The finite response/open allowance was exhausted.
    ResponseLimit,
    /// The finite body-read allowance was exhausted.
    ReadLimit,
    /// A per-response or whole-operation wire budget was exhausted.
    WireLimit,
    /// Explicit local close, without remote cancellation.
    Closed,
    /// A prior open/read future was dropped; its observation cannot be resumed safely.
    Interrupted,
}

/// Error receipt retaining scope/cursor and progress, but never a raw payload.
#[derive(Clone)]
pub struct FramedStreamError {
    details: Arc<FramedStreamErrorDetails>,
}

struct FramedStreamErrorDetails {
    cause: FramedStreamCause,
    stop: FramedStreamStop,
    progress: FramedStreamProgress,
    handle: StatementHandle,
    selectors: ResultSelectors,
    cursor: Option<ResumeCursor>,
}

impl FramedStreamError {
    fn new(
        cause: FramedStreamCause,
        stop: FramedStreamStop,
        progress: FramedStreamProgress,
        handle: StatementHandle,
        selectors: ResultSelectors,
        cursor: Option<ResumeCursor>,
    ) -> Self {
        Self {
            details: Arc::new(FramedStreamErrorDetails {
                cause,
                stop,
                progress,
                handle,
                selectors,
                cursor,
            }),
        }
    }

    /// Actual sanitized failure.
    pub fn cause(&self) -> FramedStreamCause {
        self.details.cause
    }
    /// Policy or local-stop reason, without hiding the underlying cause.
    pub fn stop(&self) -> FramedStreamStop {
        self.details.stop
    }
    /// Snapshot including acknowledged output and possibly partial sink state.
    pub fn progress(&self) -> FramedStreamProgress {
        self.details.progress
    }
    /// Original owner-bound statement; access does not dispatch anything.
    pub fn handle(&self) -> &StatementHandle {
        &self.details.handle
    }
    /// Original explicitly selected starting window; never inferred from inline.
    pub fn initial_selectors(&self) -> &ResultSelectors {
        &self.details.selectors
    }
    /// Last acknowledged in-memory cursor, not a durable checkpoint.
    pub fn cursor(&self) -> Option<&ResumeCursor> {
        self.details.cursor.as_ref()
    }
}

impl fmt::Debug for FramedStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FramedStreamError")
            .field("cause", &self.details.cause)
            .field("stop", &self.details.stop)
            .field("progress", &self.details.progress)
            .field("has_cursor", &self.details.cursor.is_some())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for FramedStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Livy Arrow observation stopped: {:?} ({:?})",
            self.details.cause, self.details.stop
        )
    }
}

impl std::error::Error for FramedStreamError {}

#[derive(Clone, Copy)]
enum RecoverySource {
    Io,
    ResponseStatus,
    Eof,
}

/// Async pull/ack driver for an existing statement's framed result GET only.
///
/// Construction does not open a response. `next_chunk` stages one borrowed
/// candidate, possibly an identical replay, and never advances delivery/cursor.
/// `acknowledge` invokes the sink only after `DeliveryState` has filtered replay.
/// Do not write candidates outside that callback: doing so bypasses deduplication.
///
/// A pending candidate prevents all further reads/opens. Acknowledgement means
/// local sink success, not durable/exactly-once delivery. Dropping this driver
/// releases local body resources and never remotely cancels a statement.
/// The injected host must enforce `NativeDeadline` during I/O; before/after
/// checks cannot preempt an executor which ignores its deadline.
pub struct FramedResultStream<'a> {
    executor: &'a dyn NativeStreamExecutor,
    handle: StatementHandle,
    selectors: ResultSelectors,
    options: FramedStreamOptions,
    deadline: NativeDeadline,
    delivery: DeliveryState,
    body: Option<Box<dyn NativeStreamBody>>,
    decoder: Option<FramedDecoder>,
    fragment: Vec<u8>,
    offset: usize,
    pending: Option<RawChunk>,
    response_limit: u64,
    declared_length: Option<u64>,
    active_response: bool,
    progress: FramedStreamProgress,
    failure: Option<FramedStreamError>,
}

impl fmt::Debug for FramedResultStream<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FramedResultStream")
            .field("progress", &self.progress)
            .finish_non_exhaustive()
    }
}

impl<'a> FramedResultStream<'a> {
    /// Bind one logical observation to an executor, exact handle and selectors.
    ///
    /// No arbitrary request or inline POST is accepted, so this driver cannot
    /// resubmit a mutation or invent a GET from an unknown inline outcome.
    pub fn new(
        executor: &'a dyn NativeStreamExecutor,
        handle: StatementHandle,
        selectors: ResultSelectors,
        options: FramedStreamOptions,
        deadline: NativeDeadline,
    ) -> Result<Self, FramedStreamError> {
        let initial_error = |cause| {
            FramedStreamError::new(
                cause,
                FramedStreamStop::Fatal,
                FramedStreamProgress {
                    state: FramedStreamState::Failed,
                    ..FramedStreamProgress::new()
                },
                handle.clone(),
                selectors.clone(),
                None,
            )
        };
        deadline
            .remaining()
            .map_err(|error| initial_error(FramedStreamCause::Native(error)))?;
        let options = options
            .validate()
            .map_err(|error| initial_error(FramedStreamCause::Arrow(error)))?;
        let delivery = DeliveryState::new(handle.clone(), selectors.clone(), options.frames)
            .map_err(|error| initial_error(FramedStreamCause::Arrow(error)))?;
        Ok(Self {
            executor,
            handle,
            selectors,
            options,
            deadline,
            delivery,
            body: None,
            decoder: None,
            fragment: Vec::new(),
            offset: 0,
            pending: None,
            response_limit: 0,
            declared_length: None,
            active_response: false,
            progress: FramedStreamProgress::new(),
            failure: None,
        })
    }

    /// Original endpoint/owner/statement binding; never replaced on recovery.
    pub const fn handle(&self) -> &StatementHandle {
        &self.handle
    }

    /// Last successful callback's in-memory cursor.
    pub const fn cursor(&self) -> Option<&ResumeCursor> {
        self.delivery.cursor()
    }

    /// Exact selectors for the next GET, bound to this driver's original handle.
    pub fn selectors(&self) -> &ResultSelectors {
        self.delivery
            .cursor()
            .map_or(&self.selectors, ResumeCursor::selectors)
    }

    /// Local progress, including incomplete responses and unacknowledged output.
    pub const fn progress(&self) -> FramedStreamProgress {
        self.progress
    }

    fn receipt(&self, cause: FramedStreamCause, stop: FramedStreamStop) -> FramedStreamError {
        FramedStreamError::new(
            cause,
            stop,
            self.progress,
            self.handle.clone(),
            self.selectors.clone(),
            self.delivery.cursor().cloned(),
        )
    }

    fn release_response(&mut self, successful: bool) {
        self.body = None;
        self.decoder = None;
        self.fragment = Vec::new();
        self.offset = 0;
        if self.active_response {
            if !successful {
                self.progress.discarded_responses += 1;
                self.progress.discarded_wire_bytes += self.progress.response_wire_bytes;
            }
            self.active_response = false;
        }
    }

    fn fail(&mut self, cause: FramedStreamCause, stop: FramedStreamStop) -> FramedStreamError {
        self.release_response(false);
        self.pending = None;
        self.progress.state = FramedStreamState::Failed;
        let error = self.receipt(cause, stop);
        self.failure = Some(error.clone());
        error
    }

    fn check_deadline(&mut self) -> Result<(), FramedStreamError> {
        self.deadline
            .remaining()
            .map(|_| ())
            .map_err(|error| self.fail(FramedStreamCause::Native(error), FramedStreamStop::Fatal))
    }

    fn recover(
        &mut self,
        cause: FramedStreamCause,
        source: RecoverySource,
    ) -> Result<(), FramedStreamError> {
        self.release_response(false);
        self.check_deadline()?;
        let FramedResume::Recoverable {
            max_reconnects,
            retry_truncated_eof,
        } = self.options.resume
        else {
            return Err(self.fail(cause, FramedStreamStop::Fatal));
        };
        let recoverable = match (source, cause) {
            (RecoverySource::Io, FramedStreamCause::Native(NativeError::Transport))
            | (
                RecoverySource::Io,
                FramedStreamCause::Native(NativeError::Http(408 | 500 | 502 | 503 | 504)),
            )
            | (
                RecoverySource::ResponseStatus,
                FramedStreamCause::Arrow(ArrowError::Http(408 | 500 | 502 | 503 | 504)),
            ) => true,
            (RecoverySource::Eof, FramedStreamCause::Arrow(ArrowError::Truncated)) => {
                retry_truncated_eof
            }
            _ => false,
        };
        if !recoverable {
            return Err(self.fail(cause, FramedStreamStop::Fatal));
        }
        if self.progress.reconnects >= max_reconnects {
            return Err(self.fail(cause, FramedStreamStop::RetryLimit));
        }
        if self.progress.open_attempts >= self.options.max_responses {
            return Err(self.fail(cause, FramedStreamStop::ResponseLimit));
        }
        self.progress.reconnects += 1;
        self.progress.state = FramedStreamState::Ready;
        Ok(())
    }

    async fn open(&mut self) -> Result<(), FramedStreamError> {
        self.check_deadline()?;
        if self.progress.open_attempts >= self.options.max_responses {
            return Err(self.fail(
                FramedStreamCause::Arrow(ArrowError::Limit),
                FramedStreamStop::ResponseLimit,
            ));
        }
        let remaining = self.options.max_wire_bytes - self.progress.wire_bytes;
        self.response_limit = remaining
            .min(self.options.transport.max_body_bytes)
            .min(self.options.frames.max_wire_bytes);
        if self.response_limit < 8 {
            return Err(self.fail(
                FramedStreamCause::Arrow(ArrowError::Limit),
                FramedStreamStop::WireLimit,
            ));
        }
        let limits = NativeStreamLimits {
            max_fragment_bytes: self
                .options
                .transport
                .max_fragment_bytes
                .min(usize::try_from(self.response_limit).unwrap_or(usize::MAX)),
            max_body_bytes: self.response_limit,
        };
        let selectors = self
            .delivery
            .cursor()
            .map_or(&self.selectors, ResumeCursor::selectors);
        let request = self
            .handle
            .result_request(selectors)
            .map_err(|error| self.fail(FramedStreamCause::Arrow(error), FramedStreamStop::Fatal))?;
        self.check_deadline()?;
        self.progress.open_attempts += 1;
        self.progress.response_wire_bytes = 0;
        self.progress.eof_received = false;
        self.progress.state = FramedStreamState::Opening;
        let opened = self
            .executor
            .open(request.native(), limits, &self.deadline)
            .await;
        self.progress.state = FramedStreamState::Ready;
        // Take ownership before the post-call deadline check, including late responses.
        let response = match opened {
            Ok(response) => response,
            Err(error) => {
                self.check_deadline()?;
                return self.recover(FramedStreamCause::Native(error), RecoverySource::Io);
            }
        };
        self.progress.responses += 1;
        self.active_response = true;
        let NativeStreamResponse {
            status,
            content_type,
            headers,
            body,
        } = response;
        self.body = Some(body);
        self.check_deadline()?;
        let metadata = NativeResponse {
            status,
            content_type,
            headers,
            body: Vec::new(),
        };
        let validated = validate_metadata(&metadata, &request, self.response_limit);
        match validated {
            Ok(length) => {
                self.declared_length = length;
                let mut limits = self.options.frames;
                limits.max_wire_bytes = limits.max_wire_bytes.min(self.response_limit);
                self.decoder = Some(FramedDecoder::new(limits).map_err(|error| {
                    self.fail(FramedStreamCause::Arrow(error), FramedStreamStop::Fatal)
                })?);
                Ok(())
            }
            Err(cause) => {
                let throttled = matches!(cause, FramedStreamCause::Arrow(ArrowError::Http(429)))
                    || (matches!(cause, FramedStreamCause::Arrow(ArrowError::Http(503)))
                        && metadata
                            .headers
                            .keys()
                            .any(|name| name.eq_ignore_ascii_case("retry-after")));
                if throttled {
                    return Err(self.fail(cause, FramedStreamStop::Fatal));
                }
                self.recover(cause, RecoverySource::ResponseStatus)
            }
        }
    }

    /// Pull until one complete candidate is staged, or terminal plus EOF succeeds.
    ///
    /// Repeated pulls while pending return the same borrowed candidate without
    /// I/O. Only `acknowledge` performs replay-filtered sink delivery. Dropping
    /// this future during I/O disposes its body; a subsequent pull fails closed.
    pub async fn next_chunk(&mut self) -> Result<Option<&RawChunk>, FramedStreamError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        match self.progress.state {
            FramedStreamState::Complete => return Ok(None),
            FramedStreamState::Closed => {
                return Err(self.receipt(
                    FramedStreamCause::Native(NativeError::Cancelled),
                    FramedStreamStop::Closed,
                ));
            }
            FramedStreamState::Opening
            | FramedStreamState::Reading
            | FramedStreamState::Acknowledging => {
                return Err(self.fail(
                    FramedStreamCause::Native(NativeError::Cancelled),
                    FramedStreamStop::Interrupted,
                ));
            }
            _ => {}
        }
        self.check_deadline()?;
        if self.pending.is_some() {
            return Ok(self.pending.as_ref());
        }
        loop {
            if self.body.is_none() {
                self.open().await?;
                if self.body.is_none() {
                    continue;
                }
            }
            self.check_deadline()?;
            if self.offset < self.fragment.len() {
                let parsed = self
                    .decoder
                    .as_mut()
                    .ok_or(ArrowError::InvalidState)
                    .and_then(|decoder| decoder.push(&self.fragment[self.offset..]));
                let consumed = parsed.map_err(|error| {
                    self.fail(FramedStreamCause::Arrow(error), FramedStreamStop::Fatal)
                })?;
                self.offset += consumed;
                if let Some(chunk) = self.decoder.as_mut().and_then(FramedDecoder::drain) {
                    self.progress.staged_chunks =
                        self.progress.staged_chunks.checked_add(1).ok_or_else(|| {
                            self.fail(
                                FramedStreamCause::Arrow(ArrowError::Limit),
                                FramedStreamStop::Fatal,
                            )
                        })?;
                    self.progress.has_unacknowledged_chunk = true;
                    self.progress.state = FramedStreamState::Pending;
                    self.pending = Some(chunk);
                    self.check_deadline()?;
                    return Ok(self.pending.as_ref());
                }
                if consumed == 0 {
                    return Err(self.fail(
                        FramedStreamCause::Arrow(ArrowError::InvalidState),
                        FramedStreamStop::Fatal,
                    ));
                }
                continue;
            }
            self.fragment = Vec::new();
            self.offset = 0;
            if self.progress.read_calls >= self.options.max_reads {
                return Err(self.fail(
                    FramedStreamCause::Arrow(ArrowError::Limit),
                    FramedStreamStop::ReadLimit,
                ));
            }
            let remaining = (self.response_limit - self.progress.response_wire_bytes)
                .min(self.options.max_wire_bytes - self.progress.wire_bytes);
            let remaining = self.declared_length.map_or(remaining, |length| {
                remaining.min(length - self.progress.response_wire_bytes)
            });
            // Actual EOF requires a read even at an exact byte boundary.
            // A conforming host returns EOF or Limit without exceeding its budget.
            let max_bytes = self
                .options
                .transport
                .max_fragment_bytes
                .min(usize::try_from(remaining.max(1)).unwrap_or(usize::MAX));
            self.check_deadline()?;
            self.progress.read_calls += 1;
            self.progress.state = FramedStreamState::Reading;
            let mut body = self.body.take().ok_or_else(|| {
                self.receipt(
                    FramedStreamCause::Arrow(ArrowError::InvalidState),
                    FramedStreamStop::Fatal,
                )
            })?;
            let read = body.read(max_bytes, &self.deadline).await;
            self.progress.state = FramedStreamState::Ready;
            if let Err(error) = self.deadline.remaining() {
                drop(body);
                return Err(self.fail(FramedStreamCause::Native(error), FramedStreamStop::Fatal));
            }
            match read {
                Err(error) => {
                    drop(body);
                    self.recover(FramedStreamCause::Native(error), RecoverySource::Io)?;
                }
                Ok(None) => {
                    drop(body);
                    self.progress.eof_received = true;
                    if self
                        .declared_length
                        .is_some_and(|length| length != self.progress.response_wire_bytes)
                    {
                        return Err(self.fail(
                            FramedStreamCause::Native(NativeError::InvalidResponse),
                            FramedStreamStop::Fatal,
                        ));
                    }
                    let result = self
                        .decoder
                        .as_mut()
                        .ok_or(ArrowError::InvalidState)
                        .and_then(FramedDecoder::finish);
                    match result {
                        Ok(()) => {
                            self.release_response(true);
                            self.progress.state = FramedStreamState::Complete;
                            return Ok(None);
                        }
                        Err(error) => {
                            // A complete declared HTTP body without a Livy terminal
                            // is a format failure, not evidence of interrupted I/O.
                            if self.declared_length.is_some() {
                                return Err(self.fail(
                                    FramedStreamCause::Arrow(error),
                                    FramedStreamStop::Fatal,
                                ));
                            }
                            self.recover(FramedStreamCause::Arrow(error), RecoverySource::Eof)?;
                        }
                    }
                }
                Ok(Some(bytes)) => {
                    if bytes.is_empty() || bytes.len() > max_bytes {
                        self.progress.rejected_fragment_bytes = bytes.len();
                        drop(body);
                        return Err(self.fail(
                            FramedStreamCause::Native(NativeError::InvalidResponse),
                            FramedStreamStop::Fatal,
                        ));
                    }
                    if bytes.len() as u64 > remaining {
                        self.progress.rejected_fragment_bytes = bytes.len();
                        drop(body);
                        return Err(self.fail(
                            FramedStreamCause::Arrow(ArrowError::Limit),
                            FramedStreamStop::WireLimit,
                        ));
                    }
                    self.progress.wire_bytes += bytes.len() as u64;
                    self.progress.response_wire_bytes += bytes.len() as u64;
                    self.fragment = bytes;
                    self.body = Some(body);
                }
            }
        }
    }

    /// Deliver the pending candidate through the existing replay-filtered ledger.
    ///
    /// Scope must match the original handle. The synchronous callback is the
    /// explicit successful sink acknowledgement; it is not a persistence claim.
    /// Callback errors poison observation and may leave partial sink output.
    pub fn acknowledge(
        &mut self,
        handle: &StatementHandle,
        emit: impl FnOnce(&RawChunk) -> Result<(), ArrowError>,
    ) -> Result<Delivery, FramedStreamError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.progress.state == FramedStreamState::Closed {
            return Err(self.receipt(
                FramedStreamCause::Native(NativeError::Cancelled),
                FramedStreamStop::Closed,
            ));
        }
        self.check_deadline()?;
        if self.progress.state != FramedStreamState::Pending || self.pending.is_none() {
            return Err(self.fail(
                FramedStreamCause::Arrow(ArrowError::InvalidState),
                FramedStreamStop::Fatal,
            ));
        }
        let chunk = self.pending.as_ref().ok_or_else(|| {
            self.receipt(
                FramedStreamCause::Arrow(ArrowError::InvalidState),
                FramedStreamStop::Fatal,
            )
        })?;
        // The body is local across user code, so unwinding also disposes it.
        // An interrupted acknowledgement cannot be reused after catch_unwind.
        let body = self.body.take();
        self.progress.state = FramedStreamState::Acknowledging;
        self.progress.sink_may_be_partial = true;
        let mut called = false;
        let mut deadline_expired = false;
        let deadline = &self.deadline;
        let result = self.delivery.deliver(handle, chunk, |chunk| {
            if deadline.remaining().is_err() {
                deadline_expired = true;
                return Err(ArrowError::TimedOut);
            }
            called = true;
            let emitted = emit(chunk);
            if deadline.remaining().is_err() {
                deadline_expired = true;
                return Err(ArrowError::TimedOut);
            }
            emitted
        });
        match result {
            Ok(delivery) => {
                self.body = body;
                self.progress.sink_may_be_partial = false;
                self.progress.delivered_chunks = self.delivery.delivered_chunks();
                self.progress.delivered_bytes = self.delivery.delivered_bytes();
                if delivery == Delivery::Replayed {
                    self.progress.replayed_chunks += 1;
                }
                self.pending = None;
                self.progress.has_unacknowledged_chunk = false;
                self.progress.state = FramedStreamState::Ready;
                self.check_deadline()?;
                Ok(delivery)
            }
            Err(error) => {
                drop(body);
                self.progress.sink_may_be_partial = called;
                let cause = if deadline_expired {
                    FramedStreamCause::Native(NativeError::TimedOut)
                } else {
                    FramedStreamCause::Arrow(error)
                };
                Err(self.fail(cause, FramedStreamStop::Fatal))
            }
        }
    }

    /// Stop local observation and dispose the active body without remote cancel.
    ///
    /// Preserves the original handle, acknowledged cursor/counters, and flags
    /// describing abandoned candidates/partial responses.
    pub fn close(&mut self) {
        self.release_response(false);
        self.pending = None;
        if !matches!(
            self.progress.state,
            FramedStreamState::Complete | FramedStreamState::Failed
        ) {
            self.progress.state = FramedStreamState::Closed;
        }
    }
}

fn validate_metadata(
    metadata: &NativeResponse,
    request: &super::ArrowRequest,
    max_bytes: u64,
) -> Result<Option<u64>, FramedStreamCause> {
    // Reuse the shared bounded/control/duplicate validation, including headers
    // unrelated to the three representation fields consumed below.
    for name in metadata.headers.keys() {
        metadata.header(name).map_err(FramedStreamCause::Native)?;
    }
    let header_type = metadata
        .header("content-type")
        .map_err(FramedStreamCause::Native)?;
    for value in [metadata.content_type.as_deref(), header_type]
        .into_iter()
        .flatten()
    {
        if value.len() > 4096 {
            return Err(FramedStreamCause::Arrow(ArrowError::Limit));
        }
        if value.chars().any(char::is_control) {
            return Err(FramedStreamCause::Native(NativeError::InvalidResponse));
        }
    }
    if let (Some(field), Some(header)) = (metadata.content_type.as_deref(), header_type)
        && !field.eq_ignore_ascii_case(header)
    {
        return Err(FramedStreamCause::Native(NativeError::InvalidResponse));
    }
    let content_length = metadata
        .header("content-length")
        .map_err(FramedStreamCause::Native)?;
    let declared_length = content_length
        .map(|length| {
            if length.is_empty()
                || length.len() > 128
                || !length.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(FramedStreamCause::Native(NativeError::InvalidResponse));
            }
            let length = length
                .parse::<u64>()
                .map_err(|_| FramedStreamCause::Native(NativeError::InvalidResponse))?;
            if length > max_bytes {
                return Err(FramedStreamCause::Arrow(ArrowError::Limit));
            }
            Ok(length)
        })
        .transpose()?;
    request
        .validate_response(
            ResponseHeaders {
                status: metadata.status,
                content_type: metadata.content_type.as_deref().or(header_type),
                content_encoding: metadata
                    .header("content-encoding")
                    .map_err(FramedStreamCause::Native)?,
                content_length,
            },
            max_bytes,
        )
        .map_err(FramedStreamCause::Arrow)?;
    Ok(declared_length)
}
