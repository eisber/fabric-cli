// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::io::Cursor;

use arrow_ipc::MessageHeader;
use arrow_ipc::gen::Message::root_as_message_with_opts;
use arrow_ipc::gen::Schema::{Schema as IpcSchema, Type};
use arrow_ipc::reader::StreamReader;
use flatbuffers::{InvalidFlatbuffer, VerifierOptions};

use crate::error::limit;
use crate::result::validate_schema;
use crate::{
    LimitKind, MetadataClassification, MetadataEvidence, MetadataLocation, OutputLimits,
    TabularError, TabularResult,
};

/// Decodes exactly one explicitly terminated, uncompressed Arrow IPC stream.
///
/// Framing, body/buffer ranges, declared rows, batch counts, and all message
/// metadata are inspected before Arrow allocates record-batch buffers. Dictionary
/// batches, compressed bodies, nested/unsupported types, second schemas, trailing
/// bytes, and missing end markers fail explicitly. Schema-only streams are valid.
///
/// Nonempty custom metadata is returned as typed unknown evidence, not silently
/// interpreted as success. In particular, HTTP success or valid Arrow is not
/// sufficient evidence of Dataflow application success.
pub fn decode_arrow_stream(
    bytes: &[u8],
    limits: OutputLimits,
) -> Result<TabularResult, TabularError> {
    if bytes.len() > limits.max_bytes() {
        return Err(limit(LimitKind::Bytes, limits.max_bytes()));
    }
    preflight(bytes, limits)?;
    let mut reader = StreamReader::try_new(Cursor::new(bytes), None)
        .map_err(|_| invalid("invalid schema"))?;
    let schema = reader.schema();
    validate_schema(&schema, limits)?;
    let mut batches = Vec::new();
    let mut retained_bytes = 0usize;
    for batch in &mut reader {
        let batch = batch.map_err(|_| invalid("invalid record batch"))?;
        for array in batch.columns() {
            retained_bytes = retained_bytes
                .checked_add(array.get_array_memory_size())
                .filter(|&value| value <= limits.max_bytes())
                .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes()))?;
        }
        batches.push(batch);
    }
    TabularResult::try_new(schema, batches, limits)
}

fn preflight(bytes: &[u8], limits: OutputLimits) -> Result<(), TabularError> {
    let mut position = 0usize;
    let mut message_index = 0usize;
    let mut schema_seen = false;
    let mut rows = 0usize;
    let mut batches = 0usize;
    let mut evidence = Vec::new();
    let mut evidence_size = 0usize;
    let verifier = VerifierOptions {
        max_depth: 16,
        max_tables: limits.max_bytes(),
        max_apparent_size: limits.max_bytes().saturating_mul(4),
        ..Default::default()
    };
    loop {
        let mut metadata_length = read_u32(bytes, &mut position)?;
        if metadata_length == u32::MAX {
            metadata_length = read_u32(bytes, &mut position)?;
        }
        if metadata_length == 0 {
            if !schema_seen {
                return Err(invalid("missing schema"));
            }
            if position != bytes.len() {
                return Err(TabularError::MultipleDatasets);
            }
            if !evidence.is_empty() {
                return Err(TabularError::Metadata { evidence });
            }
            return Ok(());
        }
        let metadata_length =
            usize::try_from(metadata_length).map_err(|_| invalid("metadata length overflow"))?;
        let metadata_end = position
            .checked_add(metadata_length)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| invalid("truncated message metadata"))?;
        let message = root_as_message_with_opts(&verifier, &bytes[position..metadata_end])
            .map_err(|error| match error {
                InvalidFlatbuffer::TooManyTables | InvalidFlatbuffer::ApparentSizeTooLarge => {
                    limit(LimitKind::Bytes, limits.max_bytes())
                }
                _ => invalid("invalid message metadata"),
            })?;
        position = metadata_end;
        if let Some(metadata) = message.custom_metadata() {
            for entry in metadata {
                let key = entry.key().unwrap_or_default();
                let value = entry.value().unwrap_or_default();
                if !key.is_empty() || !value.is_empty() {
                    evidence_size = bounded_size(
                        evidence_size,
                        key.len(),
                        value.len(),
                        limits,
                    )?;
                    evidence.push(MetadataEvidence {
                        location: MetadataLocation::Message(message_index),
                        key: key.to_owned(),
                        value: value.to_owned(),
                        classification: MetadataClassification::Unknown,
                    });
                }
            }
        }
        let body_length = usize::try_from(message.bodyLength())
            .map_err(|_| invalid("negative or overflowing message body length"))?;
        let body_end = position
            .checked_add(body_length)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| invalid("truncated message body"))?;
        if !evidence.is_empty() {
            return Err(TabularError::Metadata { evidence });
        }
        match message.header_type() {
            MessageHeader::Schema => {
                if schema_seen {
                    return Err(TabularError::MultipleDatasets);
                }
                if message_index != 0 || body_length != 0 {
                    return Err(invalid("schema must be the first message and have no body"));
                }
                let schema = message
                    .header_as_schema()
                    .ok_or_else(|| invalid("missing schema header"))?;
                preflight_schema(
                    schema,
                    limits,
                    &mut evidence,
                    &mut evidence_size,
                )?;
                let schema = arrow_ipc::convert::try_fb_to_schema(schema)
                    .map_err(|_| invalid("invalid schema"))?;
                validate_schema(&schema, limits)?;
                schema_seen = true;
            }
            MessageHeader::RecordBatch => {
                if !schema_seen {
                    return Err(invalid("record batch precedes schema"));
                }
                let batch = message
                    .header_as_record_batch()
                    .ok_or_else(|| invalid("missing record batch header"))?;
                if batch.compression().is_some() {
                    return Err(invalid("compressed IPC bodies are not supported"));
                }
                batches = batches
                    .checked_add(1)
                    .filter(|&value| value <= limits.max_batches())
                    .ok_or_else(|| limit(LimitKind::Batches, limits.max_batches()))?;
                let length = usize::try_from(batch.length())
                    .map_err(|_| invalid("negative or overflowing batch row count"))?;
                rows = rows
                    .checked_add(length)
                    .filter(|&value| value <= limits.max_rows())
                    .ok_or_else(|| limit(LimitKind::Rows, limits.max_rows()))?;
                let nodes = batch.nodes().ok_or_else(|| invalid("missing field nodes"))?;
                for node in nodes {
                    let node_length = usize::try_from(node.length())
                        .map_err(|_| invalid("invalid field length"))?;
                    let nulls = usize::try_from(node.null_count())
                        .map_err(|_| invalid("invalid field null count"))?;
                    if node_length != length || nulls > node_length {
                        return Err(invalid("field lengths do not match batch"));
                    }
                }
                let buffers = batch.buffers().ok_or_else(|| invalid("missing buffers"))?;
                for buffer in buffers {
                    let offset = usize::try_from(buffer.offset())
                        .map_err(|_| invalid("invalid buffer offset"))?;
                    let length = usize::try_from(buffer.length())
                        .map_err(|_| invalid("invalid buffer length"))?;
                    offset
                        .checked_add(length)
                        .filter(|&end| end <= body_length)
                        .ok_or_else(|| invalid("buffer extends outside message body"))?;
                }
            }
            _ => return Err(invalid("unsupported IPC message type")),
        }
        position = body_end;
        message_index += 1;
    }
}

fn preflight_schema(
    schema: IpcSchema<'_>,
    limits: OutputLimits,
    evidence: &mut Vec<MetadataEvidence>,
    evidence_size: &mut usize,
) -> Result<(), TabularError> {
    if let Some(metadata) = schema.custom_metadata() {
        for entry in metadata {
            let key = entry.key().unwrap_or_default();
            let value = entry.value().unwrap_or_default();
            *evidence_size = bounded_size(*evidence_size, key.len(), value.len(), limits)?;
            if !key.is_empty() || !value.is_empty() {
                evidence.push(MetadataEvidence {
                    location: MetadataLocation::Schema,
                    key: key.to_owned(),
                    value: value.to_owned(),
                    classification: MetadataClassification::Unknown,
                });
            }
        }
    }
    let mut schema_size = 0usize;
    if let Some(fields) = schema.fields() {
        for (column, field) in fields.iter().enumerate() {
            schema_size = bounded_size(
                schema_size,
                field.name().unwrap_or_default().len(),
                std::mem::size_of::<arrow_schema::Field>(),
                limits,
            )?;
            if field.dictionary().is_some()
                || !matches!(
                    field.type_type(),
                    Type::Int
                        | Type::FloatingPoint
                        | Type::Utf8
                        | Type::LargeUtf8
                        | Type::Binary
                        | Type::LargeBinary
                        | Type::Bool
                        | Type::Timestamp
                        | Type::Decimal
                )
                || field.children().is_some_and(|children| !children.is_empty())
            {
                return Err(TabularError::UnsupportedType {
                    column,
                    data_type: format!("{:?} (nested/dictionary types unsupported)", field.type_type()),
                });
            }
            if let Some(timestamp) = field.type_as_timestamp() {
                schema_size = bounded_size(
                    schema_size,
                    timestamp.timezone().unwrap_or_default().len(),
                    0,
                    limits,
                )?;
            }
            if let Some(metadata) = field.custom_metadata() {
                for entry in metadata {
                    let key = entry.key().unwrap_or_default();
                    let value = entry.value().unwrap_or_default();
                    *evidence_size = bounded_size(*evidence_size, key.len(), value.len(), limits)?;
                    if !key.is_empty() || !value.is_empty() {
                        evidence.push(MetadataEvidence {
                            location: MetadataLocation::Field(column),
                            key: key.to_owned(),
                            value: value.to_owned(),
                            classification: MetadataClassification::Unknown,
                        });
                    }
                }
            }
        }
    }
    if !evidence.is_empty() {
        return Err(TabularError::Metadata {
            evidence: std::mem::take(evidence),
        });
    }
    Ok(())
}

fn bounded_size(
    current: usize,
    first: usize,
    second: usize,
    limits: OutputLimits,
) -> Result<usize, TabularError> {
    current
        .checked_add(first)
        .and_then(|size| size.checked_add(second))
        .and_then(|size| size.checked_add(std::mem::size_of::<MetadataEvidence>()))
        .filter(|&size| size <= limits.max_bytes())
        .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes()))
}

fn read_u32(bytes: &[u8], position: &mut usize) -> Result<u32, TabularError> {
    let end = position
        .checked_add(4)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| invalid("missing end marker or truncated message prefix"))?;
    let data: [u8; 4] = bytes[*position..end]
        .try_into()
        .map_err(|_| invalid("invalid message prefix"))?;
    *position = end;
    Ok(u32::from_le_bytes(data))
}

fn invalid(reason: &'static str) -> TabularError {
    TabularError::InvalidIpc { reason }
}
