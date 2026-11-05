// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental, bounded Livy Arrow framed-v2/raw contracts.
//!
//! No network engine, credentials, durable checkpoints or native IPC decoder are
//! installed. Hosts supply streaming HTTP reads and explicitly opt into a typed
//! decoder. Raw framed output is not one standard Arrow IPC stream.

mod codec;
mod delivery;
mod request;
mod typed;

pub use codec::{
    FramedDecoder, RawChunk, RawInline, StreamLimits, write_frame, write_terminal,
};
pub use delivery::{
    Delivery, DeliveryState, Failure, ResumeCursor, RetryDecision, RetryPolicy, StopReason,
};
pub use request::{
    ARROW_CHUNKS_MEDIA_TYPE, ARROW_STREAM_MEDIA_TYPE, ArrowRequest, InlineFallback,
    ResponseHeaders, ResponseRepresentation, ResultSelectors, StatementHandle, StatementOwner,
    SubmitMode, SubmitOptions, validate_response_headers,
};
pub use typed::{DecodeLimits, TypedDecoder, TypedStream, UnsupportedDecoder};

/// Sanitized codec/delivery errors; no payload, code, identifiers or URLs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrowError {
    /// Invalid caller options, selector, owner or bounds.
    InvalidInput,
    /// Invalid representation, framing fields or chunk identity.
    Protocol,
    /// EOF without a terminal, including EOF at a frame boundary.
    Truncated,
    /// Livy sent the explicit eight-byte -1 failure marker.
    ServiceFailure,
    /// Bytes followed the successful eight-byte terminal marker.
    TrailingBytes,
    /// A duplicate ID occurred within one HTTP response.
    DuplicateId,
    /// A partition/ordinal pair was occupied by another chunk.
    OrdinalConflict,
    /// A replay changed its partition, ordinal or SHA-256 payload digest.
    ReplayConflict,
    /// A request/cursor was used with a different endpoint or execution owner.
    OwnerMismatch,
    /// A configured finite bound was exceeded.
    Limit,
    /// A fallible allocation could not be satisfied.
    Allocation,
    /// A failed/finished decoder or policy cannot be reused.
    InvalidState,
    /// The absolute local deadline expired; no remote cancel is implied.
    TimedOut,
    /// A sink failed; it may already have produced partial external output.
    SinkFailed,
    /// Typed IPC/schema/decompression needs an external native implementation.
    UnsupportedCapability,
    /// Exact schema equality, including metadata, failed before batch delivery.
    SchemaConflict,
    /// The actual non-success HTTP status, including deployment-gated 404.
    Http(u16),
    /// Submission was dispatched but no authoritative handle/outcome is known.
    OutcomeUnknown,
    /// Writing explicitly selected framed output failed; output may be partial.
    Output,
}

impl std::fmt::Display for ArrowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Livy Arrow error: {self:?}")
    }
}

impl std::error::Error for ArrowError {}

impl From<crate::runtime::NativeError> for ArrowError {
    fn from(error: crate::runtime::NativeError) -> Self {
        use crate::runtime::NativeError;
        match error {
            NativeError::UnsupportedCapability => Self::UnsupportedCapability,
            NativeError::Limit => Self::Limit,
            NativeError::TimedOut => Self::TimedOut,
            NativeError::Http(status) => Self::Http(status),
            NativeError::OutcomeUnknown => Self::OutcomeUnknown,
            _ => Self::InvalidInput,
        }
    }
}
