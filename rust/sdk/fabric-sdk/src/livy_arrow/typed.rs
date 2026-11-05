// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ArrowError, RawChunk};

/// External decoder budgets, not a claim that the raw codec inspects IPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    /// Maximum encoded IPC message size to inspect.
    pub max_message_bytes: usize,
    /// Maximum decoded batch plus conservative cumulative dictionary bytes.
    pub max_decoded_bytes: usize,
}

impl DecodeLimits {
    fn validate(self) -> Result<Self, ArrowError> {
        if self.max_message_bytes == 0 || self.max_decoded_bytes == 0 {
            return Err(ArrowError::InvalidInput);
        }
        Ok(self)
    }
}

/// Explicit opt-in seam requiring an external **native Arrow IPC** decoder.
///
/// Implementors must inspect message metadata/compressed buffer sizes before
/// decoded allocations; bound dictionaries/nested buffers/decompression; validate
/// complete IPC streams; and call `emit` in pull order without accumulating all
/// batches. The schema must compare exact fields and field/schema metadata.
/// `inspect_schema` must not deliver batches. `decode` must enforce that every
/// batch uses that inspected schema. These are implementation prerequisites, not
/// guarantees provided by a trait declaration. No implementation ships here.
pub trait TypedDecoder {
    /// Native decoded record-batch type.
    type Batch;
    /// Exact native schema plus all field/schema metadata.
    type Schema: Clone + Eq;

    /// Inspect and validate schema within budgets without emitting data.
    fn inspect_schema(&mut self, chunk: &RawChunk, limits: DecodeLimits) -> Result<Self::Schema, ArrowError>;

    /// Decode this single chunk within budgets, handing off one batch at a time.
    fn decode(
        &mut self,
        chunk: &RawChunk,
        limits: DecodeLimits,
        emit: &mut dyn FnMut(Self::Batch) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError>;
}

/// One external decoder and schema for an entire logical iteration/reconnects.
///
/// Schema mismatch is rejected before delivering any batch of the affected
/// chunk. All failures poison this wrapper; do not retry failed typed output.
pub struct TypedStream<D: TypedDecoder> {
    decoder: D,
    schema: Option<D::Schema>,
    limits: DecodeLimits,
    failed: bool,
}

impl<D: TypedDecoder> TypedStream<D> {
    /// Explicitly opt into the caller's native decoder implementation.
    pub fn new(decoder: D, limits: DecodeLimits) -> Result<Self, ArrowError> {
        Ok(Self { decoder, schema: None, limits: limits.validate()?, failed: false })
    }

    /// Decode one already replay-filtered chunk with cross-chunk schema equality.
    pub fn decode(
        &mut self,
        chunk: &RawChunk,
        emit: &mut dyn FnMut(D::Batch) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError> {
        if self.failed { return Err(ArrowError::InvalidState); }
        let result = self.decode_inner(chunk, emit);
        if result.is_err() { self.failed = true; }
        result
    }

    fn decode_inner(
        &mut self,
        chunk: &RawChunk,
        emit: &mut dyn FnMut(D::Batch) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError> {
        let schema = self.decoder.inspect_schema(chunk, self.limits)?;
        if self.schema.as_ref().is_some_and(|previous| previous != &schema) {
            return Err(ArrowError::SchemaConflict);
        }
        self.schema = Some(schema);
        self.decoder.decode(chunk, self.limits, emit)
    }
}

/// Default explicit unsupported capability, not a fake native IPC decoder.
pub struct UnsupportedDecoder;

impl TypedDecoder for UnsupportedDecoder {
    type Batch = ();
    type Schema = ();

    fn inspect_schema(&mut self, _chunk: &RawChunk, _limits: DecodeLimits) -> Result<(), ArrowError> {
        Err(ArrowError::UnsupportedCapability)
    }

    fn decode(
        &mut self,
        _chunk: &RawChunk,
        _limits: DecodeLimits,
        _emit: &mut dyn FnMut(()) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError> {
        Err(ArrowError::UnsupportedCapability)
    }
}
