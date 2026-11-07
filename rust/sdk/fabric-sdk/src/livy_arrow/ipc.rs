// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional native Apache Arrow IPC decoding of one already-bounded raw chunk.

/// Compose native batches into one bounded standard IPC output stream.
#[path = "ipc_output.rs"]
pub mod output;

use super::{ArrowError, DecodeLimits, RawChunk, TypedDecoder};
use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_schema::{DataType, Field, IntervalUnit, Schema};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

const MAX_METADATA: usize = 1024 * 1024;
const MAX_FIELDS: usize = 1024;
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 4096;

/// Exact logical IPC schema, including recursive dictionary ordering.
///
/// Apache's native field equality ignores both dictionary IDs and ordering.
/// This wrapper additionally compares ordering at deterministic preorder field
/// positions. IDs remain stream-local and do not participate in equality.
/// Instances are created only after the decoder's bounded schema preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeIpcSchema {
    native: Schema,
    dictionary_ordering: Vec<(usize, bool)>,
}

impl NativeIpcSchema {
    /// Borrow the native schema without weakening this wrapper's equality.
    ///
    /// Comparing the returned native schemas directly ignores dictionary
    /// ordering; use this wrapper for logical stream schema identity.
    pub const fn native_schema(&self) -> &Schema {
        &self.native
    }

    fn from_native(native: Schema) -> Self {
        let dictionary_ordering = dictionary_ordering(&native);
        Self {
            native,
            dictionary_ordering,
        }
    }

    fn matches_native(&self, native: &Schema) -> bool {
        self.native_schema() == native && self.dictionary_ordering == dictionary_ordering(native)
    }
}

fn dictionary_ordering(schema: &Schema) -> Vec<(usize, bool)> {
    let mut ordering = Vec::new();
    let mut position = 0;
    for field in schema.fields() {
        collect_dictionary_ordering(field, &mut position, &mut ordering);
    }
    ordering
}

fn collect_dictionary_ordering(
    field: &Field,
    position: &mut usize,
    ordering: &mut Vec<(usize, bool)>,
) {
    let current = *position;
    *position += 1;
    if let Some(ordered) = field.dict_is_ordered() {
        ordering.push((current, ordered));
    }
    match field.data_type() {
        DataType::List(child)
        | DataType::LargeList(child)
        | DataType::FixedSizeList(child, _)
        | DataType::Map(child, _) => collect_dictionary_ordering(child, position, ordering),
        DataType::Struct(fields) => {
            for child in fields {
                collect_dictionary_ordering(child, position, ordering);
            }
        }
        _ => {}
    }
}

/// Opt-in, safe-API wrapper around Apache Arrow IPC 60.1.0.
///
/// The `livy-arrow-ipc` feature must enable `arrow-ipc`'s `lz4` and `zstd`
/// features. Uncompressed, LZ4-frame and ZSTD buffers are preflighted before
/// constructing a native reader. Unknown codecs/layouts fail closed.
///
/// Each call creates a new reader and dictionary scope. No batches are retained
/// by this decoder. [`TypedStream`](super::TypedStream) owns cross-chunk schema
/// equality through [`NativeIpcSchema`] and failure poisoning. Inspection
/// validates all batches by decoding and dropping them; consequently
/// `TypedStream` decodes a chunk twice.
///
/// Budgets conservatively include encoded bodies, decoded buffers (rounded for
/// alignment), logical validity extents, and cumulative dictionary replacements.
/// They are not a total-process-memory or application-retained-output limit.
/// See `rust/docs/livy-arrow-ipc-native.md` for supported layouts and hard caps.
#[derive(Debug, Default)]
pub struct NativeIpcDecoder;

impl NativeIpcDecoder {
    /// Create a stateless native decoder; limits are supplied with each call.
    pub const fn new() -> Self {
        Self
    }
}

impl TypedDecoder for NativeIpcDecoder {
    type Batch = RecordBatch;
    type Schema = NativeIpcSchema;

    fn inspect_schema(
        &mut self,
        chunk: &RawChunk,
        limits: DecodeLimits,
    ) -> Result<NativeIpcSchema, ArrowError> {
        let schema = preflight(chunk.payload(), limits)?;
        read_batches(chunk.payload(), &schema, &mut |_| Ok(()))?;
        Ok(schema)
    }

    fn decode(
        &mut self,
        chunk: &RawChunk,
        limits: DecodeLimits,
        emit: &mut dyn FnMut(RecordBatch) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError> {
        let schema = preflight(chunk.payload(), limits)?;
        read_batches(chunk.payload(), &schema, emit)
    }
}

fn native_error(error: arrow_schema::ArrowError) -> ArrowError {
    match error {
        arrow_schema::ArrowError::MemoryError(_) => ArrowError::Allocation,
        arrow_schema::ArrowError::NotYetImplemented(_) => ArrowError::UnsupportedCapability,
        _ => ArrowError::Protocol,
    }
}

fn read_batches(
    payload: &[u8],
    schema: &NativeIpcSchema,
    emit: &mut dyn FnMut(RecordBatch) -> Result<(), ArrowError>,
) -> Result<(), ArrowError> {
    // Nothing reaches Apache's allocating reader before the *entire* preflight.
    let mut reader = StreamReader::try_new(Cursor::new(payload), None).map_err(native_error)?;
    if !schema.matches_native(reader.schema().as_ref()) {
        return Err(ArrowError::SchemaConflict);
    }
    for batch in reader.by_ref() {
        let batch = batch.map_err(native_error)?;
        if !schema.matches_native(batch.schema().as_ref()) {
            return Err(ArrowError::SchemaConflict);
        }
        emit(batch)?;
    }
    if reader.get_ref().position() != payload.len() as u64 {
        return Err(ArrowError::TrailingBytes);
    }
    Ok(())
}

struct Budget {
    used: usize,
    limit: usize,
}

impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), ArrowError> {
        self.used = self.used.checked_add(bytes).ok_or(ArrowError::Limit)?;
        if self.used > self.limit {
            return Err(ArrowError::Limit);
        }
        Ok(())
    }
}

fn size(value: i64) -> Result<usize, ArrowError> {
    usize::try_from(value).map_err(|_| ArrowError::Protocol)
}

fn product(left: usize, right: usize) -> Result<usize, ArrowError> {
    left.checked_mul(right)
        .filter(|value| *value <= isize::MAX as usize)
        .ok_or(ArrowError::Limit)
}

fn metadata<'a>(
    values: impl IntoIterator<Item = arrow_ipc::KeyValue<'a>>,
    budget: &mut Budget,
) -> Result<(), ArrowError> {
    let mut keys = BTreeSet::new();
    for (index, entry) in values.into_iter().enumerate() {
        if index >= MAX_ENTRIES {
            return Err(ArrowError::Limit);
        }
        let key = entry.key().ok_or(ArrowError::Protocol)?;
        let value = entry.value().ok_or(ArrowError::Protocol)?;
        budget.charge(128)?;
        budget.charge(key.len())?;
        budget.charge(value.len())?;
        if !keys.insert(key) {
            return Err(ArrowError::Protocol);
        }
    }
    Ok(())
}

fn schema_field(
    field: arrow_ipc::Field<'_>,
    depth: usize,
    count: &mut usize,
    budget: &mut Budget,
) -> Result<(), ArrowError> {
    if depth > MAX_DEPTH || *count >= MAX_FIELDS {
        return Err(ArrowError::Limit);
    }
    *count += 1;
    budget.charge(256)?;
    budget.charge(field.name().map_or(0, str::len))?;
    metadata(field.custom_metadata().into_iter().flatten(), budget)?;
    if field.type_().is_none() {
        return Err(ArrowError::Protocol);
    }
    let children = field.children();
    let child_count = children.map_or(0, |children| children.len());
    use arrow_ipc::Type;
    match field.type_type() {
        Type::List | Type::LargeList | Type::FixedSizeList | Type::Map => {
            if child_count != 1 {
                return Err(ArrowError::Protocol);
            }
        }
        Type::Struct_ => {}
        Type::Null
        | Type::Bool
        | Type::Int
        | Type::FloatingPoint
        | Type::Binary
        | Type::LargeBinary
        | Type::Utf8
        | Type::LargeUtf8
        | Type::FixedSizeBinary
        | Type::Decimal
        | Type::Date
        | Type::Time
        | Type::Timestamp
        | Type::Duration
        | Type::Interval => {
            if child_count != 0 {
                return Err(ArrowError::Protocol);
            }
        }
        _ => return Err(ArrowError::UnsupportedCapability),
    }
    if let Some(dictionary) = field.dictionary() {
        budget.charge(64)?;
        // Nested dictionary values and concat-based deltas need a different
        // retained-size model. Scalar dictionaries inside lists/structs are OK.
        if child_count != 0 || dictionary.dictionaryKind() != arrow_ipc::DictionaryKind::DenseArray
        {
            return Err(ArrowError::UnsupportedCapability);
        }
        let index = dictionary.indexType().ok_or(ArrowError::Protocol)?;
        if !matches!(index.bitWidth(), 8 | 16 | 32 | 64) {
            return Err(ArrowError::Protocol);
        }
    }
    if field.type_type() == Type::FixedSizeBinary
        && field
            .type_as_fixed_size_binary()
            .ok_or(ArrowError::Protocol)?
            .byteWidth()
            <= 0
    {
        return Err(ArrowError::Protocol);
    }
    if field.type_type() == Type::FixedSizeList
        && field
            .type_as_fixed_size_list()
            .ok_or(ArrowError::Protocol)?
            .listSize()
            <= 0
    {
        return Err(ArrowError::Protocol);
    }
    if field.type_type() == Type::Map {
        let entries = children.ok_or(ArrowError::Protocol)?.get(0);
        let pair = entries.children().ok_or(ArrowError::Protocol)?;
        if entries.type_type() != Type::Struct_
            || entries.nullable()
            || pair.len() != 2
            || pair.get(0).nullable()
        {
            return Err(ArrowError::Protocol);
        }
    }
    if let Some(decimal) = field.type_as_decimal() {
        let max_precision = match decimal.bitWidth() {
            32 => 9,
            64 => 18,
            128 => 38,
            256 => 76,
            _ => return Err(ArrowError::Protocol),
        };
        if decimal.precision() <= 0
            || decimal.precision() > max_precision
            || decimal.scale() > decimal.precision()
            || i8::try_from(decimal.scale()).is_err()
        {
            return Err(ArrowError::Protocol);
        }
    }
    if let Some(timestamp) = field.type_as_timestamp()
        && let Some(timezone) = timestamp.timezone()
    {
        budget.charge(timezone.len())?;
    }
    for child in children.into_iter().flatten() {
        schema_field(child, depth + 1, count, budget)?;
    }
    Ok(())
}

fn schema_preflight(
    schema: arrow_ipc::Schema<'_>,
    limits: DecodeLimits,
) -> Result<(Schema, usize), ArrowError> {
    if cfg!(target_endian = "big") || schema.endianness() != arrow_ipc::Endianness::Little {
        return Err(ArrowError::UnsupportedCapability);
    }
    let mut budget = Budget {
        used: 0,
        limit: limits
            .max_decoded_bytes
            .min(limits.max_message_bytes)
            .min(MAX_METADATA),
    };
    budget.charge(256)?;
    metadata(schema.custom_metadata().into_iter().flatten(), &mut budget)?;
    if let Some(features) = schema.features() {
        if features.len() > MAX_ENTRIES {
            return Err(ArrowError::Limit);
        }
        for feature in features {
            if feature != arrow_ipc::Feature::DICTIONARY_REPLACEMENT
                && feature != arrow_ipc::Feature::COMPRESSED_BODY
            {
                return Err(ArrowError::UnsupportedCapability);
            }
        }
    }
    let mut count = 0;
    for field in schema.fields().ok_or(ArrowError::Protocol)? {
        schema_field(field, 1, &mut count, &mut budget)?;
    }
    let converted = arrow_ipc::convert::try_fb_to_schema(schema).map_err(native_error)?;
    Ok((converted, budget.used))
}

struct Envelope<'a> {
    message: arrow_ipc::Message<'a>,
    body: &'a [u8],
}

struct Messages<'a> {
    payload: &'a [u8],
    position: usize,
    ended: bool,
    limits: DecodeLimits,
}

impl<'a> Messages<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], ArrowError> {
        let end = self.position.checked_add(length).ok_or(ArrowError::Limit)?;
        let bytes = self
            .payload
            .get(self.position..end)
            .ok_or(ArrowError::Truncated)?;
        self.position = end;
        Ok(bytes)
    }

    fn word(&mut self) -> Result<i32, ArrowError> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes(
            bytes.try_into().map_err(|_| ArrowError::Truncated)?,
        ))
    }

    fn next(&mut self) -> Result<Option<Envelope<'a>>, ArrowError> {
        if self.ended {
            return Ok(None);
        }
        let start = self.position;
        let mut length = self.word()?;
        if length == -1 {
            length = self.word()?;
        }
        let length = usize::try_from(length).map_err(|_| ArrowError::Protocol)?;
        if length == 0 {
            if self.position != self.payload.len() {
                return Err(ArrowError::TrailingBytes);
            }
            self.ended = true;
            return Ok(None);
        }
        if length > self.limits.max_message_bytes || length > MAX_METADATA {
            return Err(ArrowError::Limit);
        }
        let encoded_metadata = self.take(length)?;
        // The safe root verifier has finite depth/table/apparent-size limits;
        // schema expansion is separately bounded before native conversion.
        let message =
            arrow_ipc::root_as_message(encoded_metadata).map_err(|_| ArrowError::Protocol)?;
        if message.version() != arrow_ipc::MetadataVersion::V4
            && message.version() != arrow_ipc::MetadataVersion::V5
        {
            return Err(ArrowError::UnsupportedCapability);
        }
        let body_length = size(message.bodyLength())?;
        let encoded_length = self
            .position
            .checked_sub(start)
            .and_then(|length| length.checked_add(body_length))
            .ok_or(ArrowError::Limit)?;
        if encoded_length > self.limits.max_message_bytes {
            return Err(ArrowError::Limit);
        }
        let mut budget = Budget {
            used: 0,
            limit: self.limits.max_message_bytes.min(MAX_METADATA),
        };
        metadata(message.custom_metadata().into_iter().flatten(), &mut budget)?;
        let body = self.take(body_length)?;
        Ok(Some(Envelope { message, body }))
    }
}

struct Dictionary<'a> {
    value: &'a DataType,
    seen: bool,
}

fn dictionary_fields<'a>(
    field: &'a Field,
    dictionaries: &mut BTreeMap<i64, Dictionary<'a>>,
) -> Result<(), ArrowError> {
    match field.data_type() {
        DataType::Dictionary(_, value) => {
            #[expect(deprecated)]
            let id = field.dict_id().ok_or(ArrowError::Protocol)?;
            if dictionaries
                .insert(id, Dictionary { value, seen: false })
                .is_some()
            {
                return Err(ArrowError::UnsupportedCapability);
            }
        }
        DataType::List(child)
        | DataType::LargeList(child)
        | DataType::FixedSizeList(child, _)
        | DataType::Map(child, _) => dictionary_fields(child, dictionaries)?,
        DataType::Struct(fields) => {
            for child in fields {
                dictionary_fields(child, dictionaries)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn preflight(payload: &[u8], limits: DecodeLimits) -> Result<NativeIpcSchema, ArrowError> {
    if limits.max_message_bytes == 0 || limits.max_decoded_bytes == 0 {
        return Err(ArrowError::InvalidInput);
    }
    let mut messages = Messages {
        payload,
        position: 0,
        ended: false,
        limits,
    };
    let first = messages.next()?.ok_or(ArrowError::Protocol)?;
    if first.message.header_type() != arrow_ipc::MessageHeader::Schema || !first.body.is_empty() {
        return Err(ArrowError::Protocol);
    }
    let version = first.message.version();
    let (schema, schema_bytes) = schema_preflight(
        first
            .message
            .header_as_schema()
            .ok_or(ArrowError::Protocol)?,
        limits,
    )?;
    let mut dictionaries = BTreeMap::new();
    for field in schema.fields() {
        dictionary_fields(field, &mut dictionaries)?;
    }
    let mut dictionary_bytes = 0_usize;
    while let Some(envelope) = messages.next()? {
        let message = envelope.message;
        if message.version() != version {
            return Err(ArrowError::Protocol);
        }
        let mut budget = Budget {
            used: 0,
            limit: limits.max_decoded_bytes,
        };
        budget.charge(schema_bytes)?;
        budget.charge(dictionary_bytes)?;
        match message.header_type() {
            arrow_ipc::MessageHeader::Schema => return Err(ArrowError::Protocol),
            arrow_ipc::MessageHeader::RecordBatch => {
                let batch = message
                    .header_as_record_batch()
                    .ok_or(ArrowError::Protocol)?;
                let mut layout = Layout::new(batch, envelope.body, version, &mut budget)?;
                let rows = size(batch.length())?;
                for field in schema.fields() {
                    layout.field(field, Some(rows), &dictionaries)?;
                }
                layout.finish()?;
            }
            arrow_ipc::MessageHeader::DictionaryBatch => {
                let dictionary = message
                    .header_as_dictionary_batch()
                    .ok_or(ArrowError::Protocol)?;
                if dictionary.isDelta() {
                    return Err(ArrowError::UnsupportedCapability);
                }
                let value = dictionaries
                    .get(&dictionary.id())
                    .ok_or(ArrowError::Protocol)?
                    .value;
                let batch = dictionary.data().ok_or(ArrowError::Protocol)?;
                let mut layout = Layout::new(batch, envelope.body, version, &mut budget)?;
                layout.data_type(value, Some(size(batch.length())?), &dictionaries)?;
                layout.finish()?;
                dictionary_bytes = budget
                    .used
                    .checked_sub(schema_bytes)
                    .ok_or(ArrowError::Limit)?;
                dictionaries
                    .get_mut(&dictionary.id())
                    .ok_or(ArrowError::Protocol)?
                    .seen = true;
            }
            _ => return Err(ArrowError::UnsupportedCapability),
        }
    }
    Ok(NativeIpcSchema::from_native(schema))
}

#[derive(Clone, Copy)]
struct Buffer<'a> {
    decoded: usize,
    raw: Option<&'a [u8]>,
}

fn buffer<'a>(
    descriptor: &arrow_ipc::Buffer,
    body: &'a [u8],
    compressed: bool,
) -> Result<Buffer<'a>, ArrowError> {
    let offset = size(descriptor.offset())?;
    let length = size(descriptor.length())?;
    let end = offset.checked_add(length).ok_or(ArrowError::Limit)?;
    let encoded = body.get(offset..end).ok_or(ArrowError::Protocol)?;
    if !compressed || encoded.is_empty() {
        return Ok(Buffer {
            decoded: encoded.len(),
            raw: Some(encoded),
        });
    }
    let header = encoded.get(..8).ok_or(ArrowError::Protocol)?;
    let declared = i64::from_le_bytes(header.try_into().map_err(|_| ArrowError::Protocol)?);
    match declared {
        -1 => Ok(Buffer {
            decoded: encoded.len() - 8,
            raw: Some(&encoded[8..]),
        }),
        0 if encoded.len() == 8 => Ok(Buffer {
            decoded: 0,
            raw: Some(&[]),
        }),
        1.. => Ok(Buffer {
            decoded: size(declared)?,
            raw: None,
        }),
        _ => Err(ArrowError::Protocol),
    }
}

struct Layout<'a, 'b> {
    batch: arrow_ipc::RecordBatch<'a>,
    body: &'a [u8],
    compressed: bool,
    node: usize,
    buffer: usize,
    budget: &'b mut Budget,
}

impl<'a, 'b> Layout<'a, 'b> {
    fn new(
        batch: arrow_ipc::RecordBatch<'a>,
        body: &'a [u8],
        version: arrow_ipc::MetadataVersion,
        budget: &'b mut Budget,
    ) -> Result<Self, ArrowError> {
        let rows = size(batch.length())?;
        product(rows, 64)?;
        budget.charge(rows.div_ceil(8))?;
        budget.charge(body.len())?;
        let nodes = batch.nodes().ok_or(ArrowError::Protocol)?;
        let buffers = batch.buffers().ok_or(ArrowError::Protocol)?;
        if nodes.len() > MAX_FIELDS || buffers.len() > MAX_FIELDS * 3 {
            return Err(ArrowError::Limit);
        }
        if batch
            .variadicBufferCounts()
            .is_some_and(|counts| !counts.is_empty())
        {
            return Err(ArrowError::UnsupportedCapability);
        }
        let compressed = if let Some(compression) = batch.compression() {
            if version != arrow_ipc::MetadataVersion::V5 {
                return Err(ArrowError::Protocol);
            }
            if compression.method() != arrow_ipc::BodyCompressionMethod::BUFFER
                || !matches!(
                    compression.codec(),
                    arrow_ipc::CompressionType::LZ4_FRAME | arrow_ipc::CompressionType::ZSTD
                )
            {
                return Err(ArrowError::UnsupportedCapability);
            }
            true
        } else {
            false
        };
        let mut previous_end = 0;
        for descriptor in buffers {
            let offset = size(descriptor.offset())?;
            let length = size(descriptor.length())?;
            if length != 0 {
                if offset < previous_end {
                    return Err(ArrowError::Protocol);
                }
                previous_end = offset.checked_add(length).ok_or(ArrowError::Limit)?;
            }
            let decoded = buffer(descriptor, body, compressed)?.decoded;
            let aligned = decoded.checked_add(63).ok_or(ArrowError::Limit)? / 64;
            budget.charge(product(aligned, 64)?)?;
            budget.charge(64)?;
        }
        Ok(Self {
            batch,
            body,
            compressed,
            node: 0,
            buffer: 0,
            budget,
        })
    }

    fn next_buffer(&mut self) -> Result<Buffer<'a>, ArrowError> {
        let buffers = self.batch.buffers().ok_or(ArrowError::Protocol)?;
        if self.buffer >= buffers.len() {
            return Err(ArrowError::Protocol);
        }
        let result = buffer(buffers.get(self.buffer), self.body, self.compressed)?;
        self.buffer += 1;
        Ok(result)
    }

    fn field(
        &mut self,
        field: &Field,
        expected: Option<usize>,
        dictionaries: &BTreeMap<i64, Dictionary<'_>>,
    ) -> Result<usize, ArrowError> {
        if let DataType::Dictionary(index, _) = field.data_type() {
            #[expect(deprecated)]
            let id = field.dict_id().ok_or(ArrowError::Protocol)?;
            if !dictionaries
                .get(&id)
                .is_some_and(|dictionary| dictionary.seen)
            {
                return Err(ArrowError::Protocol);
            }
            self.data_type(index, expected, dictionaries)
        } else {
            self.data_type(field.data_type(), expected, dictionaries)
        }
    }

    fn data_type(
        &mut self,
        data_type: &DataType,
        expected: Option<usize>,
        dictionaries: &BTreeMap<i64, Dictionary<'_>>,
    ) -> Result<usize, ArrowError> {
        let nodes = self.batch.nodes().ok_or(ArrowError::Protocol)?;
        if self.node >= nodes.len() {
            return Err(ArrowError::Protocol);
        }
        let node = nodes.get(self.node);
        self.node += 1;
        let rows = size(node.length())?;
        let nulls = size(node.null_count())?;
        product(rows, 64)?;
        if nulls > rows || expected.is_some_and(|expected| rows != expected) {
            return Err(ArrowError::Protocol);
        }
        self.budget.charge(256)?;
        self.budget.charge(rows.div_ceil(8))?;
        if data_type == &DataType::Null {
            if nulls != rows {
                return Err(ArrowError::Protocol);
            }
            return Ok(rows);
        }
        let validity = self.next_buffer()?;
        if nulls > 0 && validity.decoded < rows.div_ceil(8) {
            return Err(ArrowError::Protocol);
        }
        use DataType::*;
        match data_type {
            Boolean => require(self.next_buffer()?, rows.div_ceil(8))?,
            Utf8 | Binary | LargeUtf8 | LargeBinary => {
                let offsets = self.next_buffer()?;
                let values = self.next_buffer()?;
                let width = if matches!(data_type, LargeUtf8 | LargeBinary) {
                    8
                } else {
                    4
                };
                check_offsets(offsets, rows, width, values.decoded)?;
            }
            List(child) | LargeList(child) | Map(child, _) => {
                let offsets = self.next_buffer()?;
                let child_rows = self.field(child, None, dictionaries)?;
                let width = if matches!(data_type, LargeList(_)) {
                    8
                } else {
                    4
                };
                check_offsets(offsets, rows, width, child_rows)?;
            }
            FixedSizeList(child, length) => {
                let length = usize::try_from(*length).map_err(|_| ArrowError::Protocol)?;
                self.field(child, Some(product(rows, length)?), dictionaries)?;
            }
            Struct(fields) => {
                for field in fields {
                    self.field(field, Some(rows), dictionaries)?;
                }
            }
            _ => {
                let width = fixed_width(data_type).ok_or(ArrowError::UnsupportedCapability)?;
                require(self.next_buffer()?, product(rows, width)?)?;
            }
        }
        Ok(rows)
    }

    fn finish(self) -> Result<(), ArrowError> {
        if self.node != self.batch.nodes().ok_or(ArrowError::Protocol)?.len()
            || self.buffer != self.batch.buffers().ok_or(ArrowError::Protocol)?.len()
        {
            return Err(ArrowError::Protocol);
        }
        Ok(())
    }
}

fn fixed_width(data_type: &DataType) -> Option<usize> {
    use DataType::*;
    Some(match data_type {
        Int8 | UInt8 => 1,
        Int16 | UInt16 | Float16 => 2,
        Int32 | UInt32 | Float32 | Date32 | Time32(_) | Decimal32(_, _) => 4,
        Int64
        | UInt64
        | Float64
        | Date64
        | Time64(_)
        | Timestamp(_, _)
        | Duration(_)
        | Decimal64(_, _) => 8,
        Interval(IntervalUnit::YearMonth) => 4,
        Interval(IntervalUnit::DayTime) => 8,
        Interval(IntervalUnit::MonthDayNano) | Decimal128(_, _) => 16,
        Decimal256(_, _) => 32,
        FixedSizeBinary(width) => usize::try_from(*width).ok()?,
        _ => return None,
    })
}

fn require(buffer: Buffer<'_>, minimum: usize) -> Result<(), ArrowError> {
    if buffer.decoded < minimum {
        return Err(ArrowError::Protocol);
    }
    Ok(())
}

fn check_offsets(
    buffer: Buffer<'_>,
    rows: usize,
    width: usize,
    bound: usize,
) -> Result<(), ArrowError> {
    let count = rows.checked_add(1).ok_or(ArrowError::Limit)?;
    let minimum = product(count, width)?;
    require(buffer, minimum)?;
    if let Some(raw) = buffer.raw {
        let mut previous = 0;
        for bytes in raw
            .get(..minimum)
            .ok_or(ArrowError::Protocol)?
            .chunks_exact(width)
        {
            let offset = if width == 4 {
                i64::from(i32::from_le_bytes(
                    bytes.try_into().map_err(|_| ArrowError::Protocol)?,
                ))
            } else {
                i64::from_le_bytes(bytes.try_into().map_err(|_| ArrowError::Protocol)?)
            };
            let offset = size(offset)?;
            if offset < previous || offset > bound {
                return Err(ArrowError::Protocol);
            }
            previous = offset;
        }
    }
    // Compressed offset *values* are checked by Apache's default full array
    // validation, after decompression into the preflight-bounded output size.
    Ok(())
}
