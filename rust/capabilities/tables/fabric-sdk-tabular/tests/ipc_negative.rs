// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use std::sync::Arc;

use arrow_array::{Int64Array, RecordBatch};
use arrow_ipc::r#gen::Message::{
    BodyCompression, BodyCompressionArgs, Message, MessageArgs, RecordBatch as IpcBatch,
    RecordBatchArgs,
};
use arrow_ipc::r#gen::Schema::{KeyValue, KeyValueArgs};
use arrow_ipc::writer::StreamWriter;
use arrow_ipc::{Buffer, FieldNode, MessageHeader, MetadataVersion};
use arrow_schema::{DataType, Field, Schema};
use fabric_sdk_tabular::{
    LimitKind, MetadataClassification, MetadataLocation, OutputLimits, TabularError,
    decode_arrow_stream,
};
use flatbuffers::FlatBufferBuilder;

fn limits() -> OutputLimits {
    OutputLimits::new(100, 1_048_576, 20).unwrap()
}

fn valid_stream() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, true)]));
    let batch =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1]))]).unwrap();
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.write(&batch).unwrap();
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn schema_only_prefix() -> Vec<u8> {
    let schema = Schema::new(vec![Field::new("x", DataType::Int64, true)]);
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.finish().unwrap();
    drop(writer);
    bytes.truncate(bytes.len() - 8);
    bytes
}

fn append_message(bytes: &mut Vec<u8>, metadata: &[u8], body: &[u8]) {
    let padded = metadata.len().div_ceil(8) * 8;
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&(padded as u32).to_le_bytes());
    bytes.extend_from_slice(metadata);
    bytes.resize(bytes.len() + padded - metadata.len(), 0);
    bytes.extend_from_slice(body);
}

fn terminate(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
}

fn diagnostic_message(repetitions: usize, value: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let key = builder.create_string("Metadata");
    let value = builder.create_string(value);
    let pair = KeyValue::create(
        &mut builder,
        &KeyValueArgs {
            key: Some(key),
            value: Some(value),
        },
    );
    let metadata = builder.create_vector(&vec![pair; repetitions]);
    let message = Message::create(
        &mut builder,
        &MessageArgs {
            version: MetadataVersion::V5,
            header_type: MessageHeader::NONE,
            custom_metadata: Some(metadata),
            ..Default::default()
        },
    );
    builder.finish(message, None);
    builder.finished_data().to_vec()
}

#[test]
fn metadata_after_valid_data_is_unknown_evidence_not_application_success() {
    let mut bytes = valid_stream();
    bytes.truncate(bytes.len() - 8);
    let diagnostic = diagnostic_message(1, "{\"error\":{\"message\":\"synthetic failure\"}}");
    append_message(&mut bytes, &diagnostic, &[]);
    terminate(&mut bytes);
    let TabularError::Metadata { evidence } = decode_arrow_stream(&bytes, limits()).unwrap_err()
    else {
        panic!("expected unknown application evidence")
    };
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].location, MetadataLocation::Message(2));
    assert_eq!(evidence[0].classification, MetadataClassification::Unknown);
    assert_eq!(evidence[0].key, "Metadata");
    assert_eq!(
        evidence[0].value,
        "{\"error\":{\"message\":\"synthetic failure\"}}"
    );
}

#[test]
fn benign_unrecognized_metadata_is_not_guessed_to_be_success() {
    let mut bytes = schema_only_prefix();
    append_message(
        &mut bytes,
        &diagnostic_message(1, "{\"status\":\"ok\"}"),
        &[],
    );
    terminate(&mut bytes);
    assert!(matches!(
        decode_arrow_stream(&bytes, limits()),
        Err(TabularError::Metadata { .. })
    ));
}

#[test]
fn repeated_metadata_references_are_charged_before_copying() {
    let mut bytes = schema_only_prefix();
    append_message(&mut bytes, &diagnostic_message(50, &"x".repeat(4096)), &[]);
    terminate(&mut bytes);
    assert!(bytes.len() < 16_384);
    let limit = OutputLimits::new(100, 16_384, 20).unwrap();
    assert!(matches!(
        decode_arrow_stream(&bytes, limit),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Bytes,
            ..
        })
    ));
}

fn crafted_batch(
    rows: i64,
    node_rows: i64,
    buffer_offset: i64,
    buffer_length: i64,
    body_length: i64,
    compressed: bool,
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let nodes = builder.create_vector(&[FieldNode::new(node_rows, 0)]);
    let buffers =
        builder.create_vector(&[Buffer::new(0, 0), Buffer::new(buffer_offset, buffer_length)]);
    let compression =
        compressed.then(|| BodyCompression::create(&mut builder, &BodyCompressionArgs::default()));
    let batch = IpcBatch::create(
        &mut builder,
        &RecordBatchArgs {
            length: rows,
            nodes: Some(nodes),
            buffers: Some(buffers),
            compression,
            ..Default::default()
        },
    );
    let message = Message::create(
        &mut builder,
        &MessageArgs {
            version: MetadataVersion::V5,
            header_type: MessageHeader::RecordBatch,
            header: Some(batch.as_union_value()),
            bodyLength: body_length,
            ..Default::default()
        },
    );
    builder.finish(message, None);
    builder.finished_data().to_vec()
}

#[test]
fn declared_row_bomb_is_rejected_before_body_allocation() {
    let mut bytes = schema_only_prefix();
    append_message(
        &mut bytes,
        &crafted_batch(i64::MAX, i64::MAX, 0, 0, 0, false),
        &[],
    );
    terminate(&mut bytes);
    assert!(matches!(
        decode_arrow_stream(&bytes, limits()),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Rows,
            ..
        })
    ));
}

#[test]
fn negative_overflowing_and_out_of_body_ranges_are_rejected_without_panics() {
    for (rows, node_rows, offset, length, body_length) in [
        (-1, -1, 0, 0, 0),
        (1, -1, 0, 0, 0),
        (1, 2, 0, 0, 0),
        (1, 1, -1, 0, 0),
        (1, 1, 0, -1, 0),
        (1, 1, i64::MAX, i64::MAX, 0),
        (1, 1, 0, 8, 0),
        (1, 1, 0, 0, -1),
        (1, 1, 0, 0, i64::MAX),
    ] {
        let mut bytes = schema_only_prefix();
        append_message(
            &mut bytes,
            &crafted_batch(rows, node_rows, offset, length, body_length, false),
            &[],
        );
        terminate(&mut bytes);
        assert!(matches!(
            decode_arrow_stream(&bytes, limits()),
            Err(TabularError::InvalidIpc { .. })
        ));
    }
}

#[test]
fn compression_is_rejected_even_for_a_tiny_body() {
    let mut bytes = schema_only_prefix();
    append_message(&mut bytes, &crafted_batch(1, 1, 0, 0, 0, true), &[]);
    terminate(&mut bytes);
    assert!(matches!(
        decode_arrow_stream(&bytes, limits()),
        Err(TabularError::InvalidIpc {
            reason: "compressed IPC bodies are not supported"
        })
    ));
}

#[test]
fn invalid_array_buffers_are_reported_as_errors() {
    let mut bytes = schema_only_prefix();
    append_message(&mut bytes, &crafted_batch(1, 1, 0, 0, 0, false), &[]);
    terminate(&mut bytes);
    assert!(matches!(
        decode_arrow_stream(&bytes, limits()),
        Err(TabularError::InvalidIpc { .. })
    ));
}

#[test]
fn legacy_length_prefixes_and_legacy_end_markers_are_supported() {
    let source = valid_stream();
    let mut legacy = Vec::new();
    let mut offset = 0usize;
    while offset < source.len() {
        assert_eq!(&source[offset..offset + 4], &u32::MAX.to_le_bytes());
        let size = u32::from_le_bytes(source[offset + 4..offset + 8].try_into().unwrap()) as usize;
        legacy.extend_from_slice(&(size as u32).to_le_bytes());
        offset += 8;
        if size == 0 {
            break;
        }
        let metadata = &source[offset..offset + size];
        let message = arrow_ipc::r#gen::Message::root_as_message(metadata).unwrap();
        let body = message.bodyLength() as usize;
        legacy.extend_from_slice(&source[offset..offset + size + body]);
        offset += size + body;
    }
    let decoded = decode_arrow_stream(&legacy, limits()).unwrap();
    assert_eq!(decoded.num_rows(), 1);
}
