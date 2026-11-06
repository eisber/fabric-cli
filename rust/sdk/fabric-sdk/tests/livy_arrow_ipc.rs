// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native IPC fixtures, written by Apache Arrow rather than a schema mock.

#![cfg(feature = "livy-arrow-ipc")]
#![deny(unnameable_test_items)]

use arrow_array::{
    ArrayRef, BooleanArray, DictionaryArray, Int32Array, ListArray, RecordBatch,
    RecordBatchOptions, StringArray, StringViewArray, StructArray, types::Int32Type,
};
use arrow_ipc::{
    CompressionType, MessageHeader,
    reader::StreamReader,
    writer::{DictionaryHandling, IpcWriteOptions, StreamWriter},
};
use arrow_schema::{DataType, Field, Schema};
use fabric_sdk::livy_arrow::{
    ArrowError, DecodeLimits, FramedDecoder, NativeIpcDecoder, RawChunk, StreamLimits,
    TypedDecoder, TypedStream,
    ipc::{
        NativeIpcSchema,
        output::{IpcOutputLimits, LocalIpcCompletion, NativeIpcOutput},
    },
    write_frame, write_terminal,
};
use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::ops::Range;
use std::sync::Arc;

use fabric_sdk::{
    livy_arrow::{
        FramedResultStream, FramedResume, FramedStreamCause, FramedStreamOptions,
        FramedStreamState, ResultSelectors, StatementHandle, StatementOwner,
        ipc::result::{IpcResultCause, write_ipc_result},
    },
    runtime::{
        NativeDeadline, NativeEndpoint, NativeError, NativeFuture, NativeRequest, NativeStreamBody,
        NativeStreamExecutor, NativeStreamLimits, NativeStreamResponse,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

const LIMITS: DecodeLimits = DecodeLimits {
    max_message_bytes: 8 * 1024 * 1024,
    max_decoded_bytes: 8 * 1024 * 1024,
};

fn chunk(payload: Vec<u8>) -> RawChunk {
    RawChunk::new(
        0,
        0,
        "part-00000-000".into(),
        payload,
        StreamLimits::default(),
    )
    .unwrap()
}

fn write(batches: &[RecordBatch], codec: Option<CompressionType>) -> Vec<u8> {
    write_options(
        batches,
        IpcWriteOptions::default()
            .try_with_compression(codec)
            .unwrap(),
    )
}

fn write_options(batches: &[RecordBatch], options: IpcWriteOptions) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer =
        StreamWriter::try_new_with_options(&mut bytes, &batches[0].schema(), options).unwrap();
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn schema_only(schema: &Schema) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, schema).unwrap();
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn primitive() -> RecordBatch {
    RecordBatch::try_from_iter([(
        "value",
        Arc::new(Int32Array::from(vec![1, 2, 3])) as ArrayRef,
    )])
    .unwrap()
}

fn fixture() -> RecordBatch {
    let numbers = Arc::new(Int32Array::from(vec![Some(7), None, Some(-9)])) as ArrayRef;
    let strings = Arc::new(StringArray::from(vec![Some("one"), None, Some("")])) as ArrayRef;
    let lists = Arc::new(ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
        Some(vec![Some(1), None, Some(3)]),
        None,
        Some(vec![]),
    ])) as ArrayRef;
    let structs = Arc::new(StructArray::from(vec![(
        Arc::new(Field::new("flag", DataType::Boolean, true)),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])) as ArrayRef,
    )])) as ArrayRef;
    let dictionary: DictionaryArray<Int32Type> =
        vec![Some("red"), None, Some("blue")].into_iter().collect();
    let arrays = vec![
        numbers,
        strings,
        lists,
        structs,
        Arc::new(dictionary) as ArrayRef,
    ];
    let fields = ["number", "text", "list", "struct", "dictionary"]
        .into_iter()
        .zip(&arrays)
        .map(|(name, array)| {
            Field::new(name, array.data_type().clone(), true).with_metadata(HashMap::from([(
                "field-key".into(),
                format!("field-{name}"),
            )]))
        })
        .collect::<Vec<_>>();
    let schema = Schema::new_with_metadata(
        fields,
        HashMap::from([("schema-key".into(), "offline-native-writer".into())]),
    );
    RecordBatch::try_new(Arc::new(schema), arrays).unwrap()
}

fn dictionary_ordering_fixture(ordering: &[bool], nested: bool) -> RecordBatch {
    let mut columns = ordering
        .iter()
        .enumerate()
        .map(|(index, ordered)| {
            let dictionary: DictionaryArray<Int32Type> =
                ["alpha", "beta", "alpha"].into_iter().map(Some).collect();
            let array = Arc::new(dictionary) as ArrayRef;
            let field = Field::new(
                format!("dictionary-{index}"),
                array.data_type().clone(),
                false,
            )
            .with_dict_is_ordered(*ordered);
            (Arc::new(field), array)
        })
        .collect::<Vec<_>>();
    if nested {
        for name in ["inner", "outer"] {
            let array = Arc::new(StructArray::from(columns)) as ArrayRef;
            let field = Field::new(name, array.data_type().clone(), false);
            columns = vec![(Arc::new(field), array)];
        }
    }
    let (fields, arrays): (Vec<_>, Vec<_>) = columns.into_iter().unzip();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).unwrap()
}

fn assert_dictionary_ordering_conflict(first: &[bool], second: &[bool], nested: bool) {
    let first = chunk(write(&[dictionary_ordering_fixture(first, nested)], None));
    let second = chunk(write(&[dictionary_ordering_fixture(second, nested)], None));
    let first_schema: NativeIpcSchema = NativeIpcDecoder::new()
        .inspect_schema(&first, LIMITS)
        .unwrap();
    let second_schema = NativeIpcDecoder::new()
        .inspect_schema(&second, LIMITS)
        .unwrap();
    assert_eq!(first_schema.native_schema(), second_schema.native_schema());
    assert_eq!(first_schema, first_schema.clone());
    assert_ne!(first_schema, second_schema);
    for raw in [&first, &second] {
        let mut emitted = 0;
        NativeIpcDecoder::new()
            .decode(raw, LIMITS, &mut |batch| {
                assert_eq!(batch.num_rows(), 3);
                emitted += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(emitted, 1);
    }
    let mut stream = TypedStream::new(NativeIpcDecoder::new(), LIMITS).unwrap();
    let mut emitted = 0;
    let mut sink = |_| {
        emitted += 1;
        Ok(())
    };
    stream.decode(&first, &mut sink).unwrap();
    assert_eq!(
        stream.decode(&second, &mut sink),
        Err(ArrowError::SchemaConflict),
    );
    assert_eq!(emitted, 1);
}

#[derive(Clone)]
struct Span {
    complete: Range<usize>,
    metadata: Range<usize>,
    body: Range<usize>,
}

fn spans(bytes: &[u8]) -> Vec<Span> {
    let mut position = 0;
    let mut spans = Vec::new();
    loop {
        let start = position;
        assert_eq!(&bytes[position..position + 4], &[255; 4]);
        let length = i32::from_le_bytes(bytes[position + 4..position + 8].try_into().unwrap());
        position += 8;
        if length == 0 {
            assert_eq!(position, bytes.len());
            break;
        }
        let metadata = position..position + usize::try_from(length).unwrap();
        let message = arrow_ipc::root_as_message(&bytes[metadata.clone()]).unwrap();
        position = metadata.end;
        let body = position..position + usize::try_from(message.bodyLength()).unwrap();
        position = body.end;
        spans.push(Span {
            complete: start..position,
            metadata,
            body,
        });
    }
    spans
}

fn remap_second_dictionary(first: &[u8]) -> Vec<u8> {
    let mut second = first.to_vec();
    let mut locations = Vec::new();
    for span in spans(&second) {
        let message = arrow_ipc::root_as_message(&second[span.metadata.clone()]).unwrap();
        if let Some(schema) = message.header_as_schema() {
            let dictionary = schema.fields().unwrap().get(1).dictionary().unwrap();
            assert_eq!(dictionary.id(), 1);
            let offset = dictionary
                ._tab
                .vtable()
                .get(arrow_ipc::DictionaryEncoding::VT_ID);
            assert_ne!(offset, 0);
            locations.push(span.metadata.start + dictionary._tab.loc() + usize::from(offset));
        }
        if let Some(dictionary) = message.header_as_dictionary_batch()
            && dictionary.id() == 1
        {
            let offset = dictionary
                ._tab
                .vtable()
                .get(arrow_ipc::DictionaryBatch::VT_ID);
            assert_ne!(offset, 0);
            locations.push(span.metadata.start + dictionary._tab.loc() + usize::from(offset));
        }
    }
    assert_eq!(locations.len(), 2);
    for position in locations {
        second[position..position + 8].copy_from_slice(&7_i64.to_le_bytes());
    }
    second
}

fn buffer_location(bytes: &[u8], index: usize) -> (usize, Range<usize>) {
    for span in spans(bytes) {
        let message = arrow_ipc::root_as_message(&bytes[span.metadata.clone()]).unwrap();
        if let Some(batch) = message.header_as_record_batch() {
            let descriptor = batch.buffers().unwrap().get(index);
            let location = descriptor.0.as_ptr() as usize - bytes.as_ptr() as usize;
            let start = span.body.start + usize::try_from(descriptor.offset()).unwrap();
            return (
                location,
                start..start + usize::try_from(descriptor.length()).unwrap(),
            );
        }
    }
    panic!("writer did not produce a record batch");
}

fn compressed_buffer(bytes: &[u8], kind: MessageHeader) -> Range<usize> {
    for span in spans(bytes) {
        let message = arrow_ipc::root_as_message(&bytes[span.metadata.clone()]).unwrap();
        if message.header_type() != kind {
            continue;
        }
        let batch = message
            .header_as_record_batch()
            .or_else(|| {
                message
                    .header_as_dictionary_batch()
                    .and_then(|dictionary| dictionary.data())
            })
            .unwrap();
        assert!(batch.compression().is_some());
        for descriptor in batch.buffers().unwrap() {
            let start = span.body.start + usize::try_from(descriptor.offset()).unwrap();
            let end = start + usize::try_from(descriptor.length()).unwrap();
            if end - start >= 8
                && i64::from_le_bytes(bytes[start..start + 8].try_into().unwrap()) > 0
            {
                return start..end;
            }
        }
    }
    panic!("fixture must contain a genuinely compressed buffer");
}

fn minimum_decoded_limit(bytes: &[u8]) -> usize {
    let mut low = 1;
    let mut high = LIMITS.max_decoded_bytes;
    while low < high {
        let middle = low + (high - low) / 2;
        let result = NativeIpcDecoder::new().inspect_schema(
            &chunk(bytes.to_vec()),
            DecodeLimits {
                max_decoded_bytes: middle,
                ..LIMITS
            },
        );
        match result {
            Ok(_) => high = middle,
            Err(ArrowError::Limit) => low = middle + 1,
            Err(error) => panic!("unexpected fixture error: {error}"),
        }
    }
    low
}

#[test]
fn native_writer_round_trips_types_nulls_nested_dictionary_and_metadata() {
    for codec in [
        None,
        Some(CompressionType::LZ4_FRAME),
        Some(CompressionType::ZSTD),
    ] {
        let expected = fixture();
        let raw = chunk(write(&[expected.clone(), expected.clone()], codec));
        let schema = NativeIpcDecoder::new()
            .inspect_schema(&raw, LIMITS)
            .unwrap();
        assert_eq!(schema.native_schema(), expected.schema().as_ref());
        let mut count = 0;
        NativeIpcDecoder::new()
            .decode(&raw, LIMITS, &mut |actual| {
                assert_eq!(actual, expected);
                assert_eq!(actual.num_rows(), 3);
                assert_eq!(actual.column(0).null_count(), 1);
                assert_eq!(actual.column(1).null_count(), 1);
                assert_eq!(actual.column(2).null_count(), 1);
                assert_eq!(actual.column(4).null_count(), 1);
                assert_eq!(
                    actual.schema().metadata().get("schema-key").unwrap(),
                    "offline-native-writer"
                );
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 2);
    }
}

#[test]
fn schema_only_and_zero_row_batches_are_real_streams() {
    let batch = primitive().slice(0, 0);
    let raw = chunk(schema_only(batch.schema().as_ref()));
    assert_eq!(
        NativeIpcDecoder::new()
            .inspect_schema(&raw, LIMITS)
            .unwrap()
            .native_schema(),
        batch.schema().as_ref(),
    );
    let mut count = 0;
    NativeIpcDecoder::new()
        .decode(&raw, LIMITS, &mut |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 0);
    NativeIpcDecoder::new()
        .decode(&chunk(write(&[batch], None)), LIMITS, &mut |batch| {
            assert_eq!(batch.num_rows(), 0);
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn typed_stream_compares_field_and_schema_metadata_before_chunk_delivery() {
    let first = primitive();
    for schema in [
        Schema::new_with_metadata(
            first.schema().fields().clone(),
            HashMap::from([("different".into(), "schema".into())]),
        ),
        Schema::new(vec![first.schema().field(0).clone().with_metadata(
            HashMap::from([("different".into(), "field".into())]),
        )]),
        Schema::new(vec![Field::new("renamed", DataType::Int32, false)]),
        Schema::new(vec![Field::new("value", DataType::Int32, true)]),
    ] {
        let second = RecordBatch::try_new(Arc::new(schema), first.columns().to_vec()).unwrap();
        let mut stream = TypedStream::new(NativeIpcDecoder::new(), LIMITS).unwrap();
        let mut emitted = 0;
        let mut sink = |_| {
            emitted += 1;
            Ok(())
        };
        stream
            .decode(&chunk(write(std::slice::from_ref(&first), None)), &mut sink)
            .unwrap();
        assert_eq!(
            stream.decode(&chunk(write(&[second], None)), &mut sink),
            Err(ArrowError::SchemaConflict),
        );
        assert_eq!(
            stream.decode(&chunk(write(std::slice::from_ref(&first), None)), &mut sink),
            Err(ArrowError::InvalidState),
        );
        assert_eq!(emitted, 1);
    }
}

#[test]
fn typed_stream_rejects_dictionary_ordering_changes_before_second_emission() {
    assert_dictionary_ordering_conflict(&[false], &[true], false);
    assert_dictionary_ordering_conflict(&[true], &[false], false);
}

#[test]
fn typed_stream_preserves_nested_dictionary_ordering_by_field_position() {
    assert_dictionary_ordering_conflict(&[false, false], &[true, false], true);
    assert_dictionary_ordering_conflict(&[false, false], &[false, true], true);
    assert_dictionary_ordering_conflict(&[false, true], &[true, false], true);
}

#[test]
fn dictionary_ids_are_stream_local_even_for_ordered_dictionaries() {
    let first = write(&[dictionary_ordering_fixture(&[false, true], false)], None);
    let second = remap_second_dictionary(&first);
    let first = chunk(first);
    let second = chunk(second);
    let first_schema = NativeIpcDecoder::new()
        .inspect_schema(&first, LIMITS)
        .unwrap();
    let second_schema = NativeIpcDecoder::new()
        .inspect_schema(&second, LIMITS)
        .unwrap();
    #[expect(deprecated)]
    let first_id = first_schema.native_schema().field(1).dict_id();
    #[expect(deprecated)]
    let second_id = second_schema.native_schema().field(1).dict_id();
    assert_eq!(first_id, Some(1));
    assert_eq!(second_id, Some(7));
    assert_eq!(first_schema, second_schema);
    let mut stream = TypedStream::new(NativeIpcDecoder::new(), LIMITS).unwrap();
    let mut emitted = 0;
    for raw in [&first, &second] {
        stream
            .decode(raw, &mut |batch| {
                assert_eq!(batch.num_rows(), 3);
                emitted += 1;
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(emitted, 2);
}

#[test]
fn complete_stream_requires_eos_and_rejects_all_truncations_and_trailing_bytes() {
    let bytes = write(&[primitive()], None);
    for cut in 1..bytes.len() {
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&chunk(bytes[..cut].to_vec()), LIMITS),
            Err(ArrowError::Truncated),
            "cut={cut}",
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(trailing), LIMITS),
        Err(ArrowError::TrailingBytes),
    );
    let mut joined = bytes.clone();
    joined.extend_from_slice(&bytes);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(joined), LIMITS),
        Err(ArrowError::TrailingBytes),
    );
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(vec![255; 8]), LIMITS),
        Err(ArrowError::Protocol),
    );
    assert_eq!(
        NativeIpcDecoder::new()
            .inspect_schema(&chunk(vec![255, 255, 255, 255, 0, 0, 0, 0]), LIMITS),
        Err(ArrowError::Protocol),
    );
}

#[test]
fn malformed_flatbuffer_duplicate_schema_and_missing_initial_schema_fail() {
    let original = write(&[primitive()], None);
    let parts = spans(&original);
    let mut duplicate = original[parts[0].complete.clone()].to_vec();
    duplicate.extend_from_slice(&original);
    let missing = original[parts[1].complete.start..].to_vec();
    let mut malformed = original.clone();
    malformed[parts[0].metadata.start..parts[0].metadata.start + 4]
        .copy_from_slice(&u32::MAX.to_le_bytes());
    for bytes in [duplicate, missing, malformed] {
        let raw = chunk(bytes);
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&raw, LIMITS),
            Err(ArrowError::Protocol),
        );
        let mut emitted = 0;
        assert_eq!(
            NativeIpcDecoder::new().decode(&raw, LIMITS, &mut |_| {
                emitted += 1;
                Ok(())
            }),
            Err(ArrowError::Protocol),
        );
        assert_eq!(emitted, 0);
    }
}

#[test]
fn bad_buffer_extents_nodes_and_nested_offsets_fail_preflight() {
    let original = write(&[primitive()], None);
    let (descriptor, _) = buffer_location(&original, 1);
    for (offset, length) in [(-1_i64, 12_i64), (i64::MAX, 12), (0, i64::MAX), (0, 1)] {
        let mut bytes = original.clone();
        bytes[descriptor..descriptor + 8].copy_from_slice(&offset.to_le_bytes());
        bytes[descriptor + 8..descriptor + 16].copy_from_slice(&length.to_le_bytes());
        assert!(
            NativeIpcDecoder::new()
                .inspect_schema(&chunk(bytes), LIMITS)
                .is_err()
        );
    }
    let mut overflow = original.clone();
    let node_position = {
        let span = &spans(&overflow)[1];
        let message = arrow_ipc::root_as_message(&overflow[span.metadata.clone()]).unwrap();
        let node = message
            .header_as_record_batch()
            .unwrap()
            .nodes()
            .unwrap()
            .get(0);
        node.0.as_ptr() as usize - overflow.as_ptr() as usize
    };
    overflow[node_position..node_position + 8].copy_from_slice(&i64::MAX.to_le_bytes());
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(overflow), LIMITS),
        Err(ArrowError::Limit),
    );
    let lists = Arc::new(ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
        Some(vec![Some(1), Some(2)]),
    ])) as ArrayRef;
    let list_batch = RecordBatch::try_from_iter([("list", lists)]).unwrap();
    let mut bytes = write(&[list_batch], None);
    let (_, offsets) = buffer_location(&bytes, 1);
    bytes[offsets.start + 4..offsets.start + 8].copy_from_slice(&i32::MAX.to_le_bytes());
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(bytes), LIMITS),
        Err(ArrowError::Protocol),
    );
}

#[test]
fn encoded_and_decoded_limits_are_checked_before_native_reader() {
    let raw = chunk(write(&[primitive()], None));
    for limits in [
        DecodeLimits {
            max_message_bytes: 1,
            ..LIMITS
        },
        DecodeLimits {
            max_decoded_bytes: 1,
            ..LIMITS
        },
    ] {
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&raw, limits),
            Err(ArrowError::Limit),
        );
    }
    for limits in [
        DecodeLimits {
            max_message_bytes: 0,
            ..LIMITS
        },
        DecodeLimits {
            max_decoded_bytes: 0,
            ..LIMITS
        },
    ] {
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&raw, limits),
            Err(ArrowError::InvalidInput),
        );
    }
    let batch = RecordBatch::try_from_iter([(
        "large",
        Arc::new(Int32Array::from(vec![42; 8192])) as ArrayRef,
    )])
    .unwrap();
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(
            &chunk(write(&[batch], None)),
            DecodeLimits {
                max_decoded_bytes: 1024,
                ..LIMITS
            },
        ),
        Err(ArrowError::Limit),
    );
}

#[test]
fn encoded_limit_is_per_complete_message_not_per_stream() {
    let batch = RecordBatch::try_from_iter([(
        "large",
        Arc::new(Int32Array::from(vec![42; 1024])) as ArrayRef,
    )])
    .unwrap();
    let bytes = write(&[batch.clone(), batch.clone(), batch], None);
    let largest_message = spans(&bytes)
        .iter()
        .map(|span| span.complete.len())
        .max()
        .unwrap();
    assert!(bytes.len() > largest_message * 2);
    let raw = chunk(bytes);
    NativeIpcDecoder::new()
        .inspect_schema(
            &raw,
            DecodeLimits {
                max_message_bytes: largest_message,
                ..LIMITS
            },
        )
        .unwrap();
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(
            &raw,
            DecodeLimits {
                max_message_bytes: largest_message - 1,
                ..LIMITS
            },
        ),
        Err(ArrowError::Limit),
    );
}

#[test]
fn compressed_declared_bombs_fail_before_decompressing_invalid_body() {
    for codec in [CompressionType::LZ4_FRAME, CompressionType::ZSTD] {
        let batch = RecordBatch::try_from_iter([(
            "large",
            Arc::new(Int32Array::from(vec![42; 8192])) as ArrayRef,
        )])
        .unwrap();
        let dictionary: DictionaryArray<Int32Type> = ["a".repeat(8192), "b".repeat(8192)]
            .iter()
            .map(|value| Some(value.as_str()))
            .collect();
        let dict_batch =
            RecordBatch::try_from_iter([("dictionary", Arc::new(dictionary) as ArrayRef)]).unwrap();
        for (batch, kind) in [
            (batch, MessageHeader::RecordBatch),
            (dict_batch, MessageHeader::DictionaryBatch),
        ] {
            let mut bytes = write(&[batch], Some(codec));
            let range = compressed_buffer(&bytes, kind);
            bytes[range.start..range.start + 8].copy_from_slice(&(1024_i64 * 1024).to_le_bytes());
            bytes[range.start + 8..range.end].fill(0xff);
            let raw = chunk(bytes);
            let limits = DecodeLimits {
                max_decoded_bytes: 64 * 1024,
                ..LIMITS
            };
            // Apache would report malformed compression, not our Limit. The
            // limit must win without ever handing this body to its decoder.
            assert_eq!(
                NativeIpcDecoder::new().inspect_schema(&raw, limits),
                Err(ArrowError::Limit),
            );
            let mut emitted = 0;
            assert_eq!(
                NativeIpcDecoder::new().decode(&raw, limits, &mut |_| {
                    emitted += 1;
                    Ok(())
                }),
                Err(ArrowError::Limit),
            );
            assert_eq!(emitted, 0);
        }
    }
}

#[test]
fn compressed_output_cannot_exceed_a_forged_smaller_decoded_header() {
    for codec in [CompressionType::LZ4_FRAME, CompressionType::ZSTD] {
        let batch = RecordBatch::try_from_iter([(
            "text",
            Arc::new(StringArray::from(vec!["x".repeat(32768)])) as ArrayRef,
        )])
        .unwrap();
        let mut bytes = write(&[batch], Some(codec));
        let range = compressed_buffer(&bytes, MessageHeader::RecordBatch);
        let (_, offsets) = buffer_location(&bytes, 1);
        assert_eq!(
            i64::from_le_bytes(bytes[offsets.start..offsets.start + 8].try_into().unwrap()),
            -1,
        );
        bytes[offsets.start + 12..offsets.start + 16].copy_from_slice(&8_i32.to_le_bytes());
        // The small offsets buffer is uncompressed. Keep it consistent with
        // the forged length so preflight succeeds and native decompression
        // must reject the still-valid payload that actually expands to 32768.
        bytes[range.start..range.start + 8].copy_from_slice(&8_i64.to_le_bytes());
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&chunk(bytes), LIMITS),
            Err(ArrowError::Protocol),
        );
    }
}

#[test]
fn dictionary_replacements_are_cumulative_and_never_escape_the_chunk() {
    let batches = (0..8)
        .map(|index| {
            let values = [format!("{index}{}", "a".repeat(4096)), "b".repeat(4096)];
            let dictionary: DictionaryArray<Int32Type> =
                values.iter().map(|value| Some(value.as_str())).collect();
            RecordBatch::try_from_iter([("dictionary", Arc::new(dictionary) as ArrayRef)]).unwrap()
        })
        .collect::<Vec<_>>();
    let single = write(&batches[..1], None);
    let minimum = minimum_decoded_limit(&single);
    let many = write(&batches, None);
    let mut decoder = NativeIpcDecoder::new();
    assert_eq!(
        decoder.inspect_schema(
            &chunk(many),
            DecodeLimits {
                max_decoded_bytes: minimum * 2,
                ..LIMITS
            },
        ),
        Err(ArrowError::Limit),
    );
    decoder
        .inspect_schema(&chunk(single.clone()), LIMITS)
        .unwrap();
    let mut missing_dictionary = Vec::new();
    for span in spans(&single) {
        let message = arrow_ipc::root_as_message(&single[span.metadata.clone()]).unwrap();
        if message.header_type() != MessageHeader::DictionaryBatch {
            missing_dictionary.extend_from_slice(&single[span.complete]);
        }
    }
    missing_dictionary.extend_from_slice(&single[single.len() - 8..]);
    assert_eq!(
        decoder.inspect_schema(&chunk(missing_dictionary), LIMITS),
        Err(ArrowError::Protocol),
    );
}

#[test]
fn unsupported_delta_and_view_layouts_fail_explicitly() {
    let batches = [vec!["a", "a"], vec!["a", "b"]]
        .into_iter()
        .map(|values| {
            let dictionary: DictionaryArray<Int32Type> = values.into_iter().map(Some).collect();
            RecordBatch::try_from_iter([("dictionary", Arc::new(dictionary) as ArrayRef)]).unwrap()
        })
        .collect::<Vec<_>>();
    let bytes = write_options(
        &batches,
        IpcWriteOptions::default().with_dictionary_handling(DictionaryHandling::Delta),
    );
    assert!(spans(&bytes).into_iter().any(|span| {
        arrow_ipc::root_as_message(&bytes[span.metadata])
            .unwrap()
            .header_as_dictionary_batch()
            .is_some_and(|dictionary| dictionary.isDelta())
    }));
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(bytes), LIMITS),
        Err(ArrowError::UnsupportedCapability),
    );
    let batch = RecordBatch::try_from_iter([(
        "view",
        Arc::new(StringViewArray::from(vec!["one", "two"])) as ArrayRef,
    )])
    .unwrap();
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(write(&[batch], None)), LIMITS),
        Err(ArrowError::UnsupportedCapability),
    );
}

#[test]
fn schema_expansion_and_depth_are_bounded_before_conversion() {
    let huge_metadata = Schema::new_with_metadata(
        vec![Field::new("value", DataType::Int32, false)],
        HashMap::from([("large".into(), "x".repeat(32 * 1024))]),
    );
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(
            &chunk(schema_only(&huge_metadata)),
            DecodeLimits {
                max_decoded_bytes: 4096,
                ..LIMITS
            },
        ),
        Err(ArrowError::Limit),
    );
    let mut data_type = DataType::Int32;
    for _ in 0..40 {
        data_type = DataType::List(Arc::new(Field::new("item", data_type, true)));
    }
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(
            &chunk(schema_only(&Schema::new(vec![Field::new(
                "deep", data_type, true
            )]))),
            LIMITS,
        ),
        Err(ArrowError::Limit),
    );
}

#[test]
fn sink_error_stops_before_emitting_the_next_batch() {
    let batch = RecordBatch::try_from_iter([(
        "text",
        Arc::new(StringArray::from(vec!["valid"])) as ArrayRef,
    )])
    .unwrap();
    let mut bytes = write(&[batch.clone(), batch], None);
    let value_position = {
        let parts = spans(&bytes);
        let span = parts.last().unwrap();
        let message = arrow_ipc::root_as_message(&bytes[span.metadata.clone()]).unwrap();
        let values = message
            .header_as_record_batch()
            .unwrap()
            .buffers()
            .unwrap()
            .get(2);
        span.body.start + usize::try_from(values.offset()).unwrap()
    };
    bytes[value_position] = 0xff;
    let raw = chunk(bytes);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&raw, LIMITS),
        Err(ArrowError::Protocol),
    );
    let mut count = 0;
    assert_eq!(
        NativeIpcDecoder::new().decode(&raw, LIMITS, &mut |_| {
            count += 1;
            Err(ArrowError::SinkFailed)
        }),
        Err(ArrowError::SinkFailed),
    );
    assert_eq!(count, 1);
    count = 0;
    assert_eq!(
        NativeIpcDecoder::new().decode(&raw, LIMITS, &mut |_| {
            count += 1;
            Ok(())
        }),
        Err(ArrowError::Protocol),
    );
    assert_eq!(count, 1);
}

const OUTPUT_LIMITS: IpcOutputLimits = IpcOutputLimits {
    max_output_bytes: 8 * 1024 * 1024,
    max_rows: 1000,
    max_batches: 100,
};

fn output_schema(batch: &RecordBatch) -> NativeIpcSchema {
    NativeIpcDecoder::new()
        .inspect_schema(&chunk(write(std::slice::from_ref(batch), None)), LIMITS)
        .unwrap()
}

fn encoded_output(batches: &[RecordBatch]) -> (Vec<u8>, LocalIpcCompletion) {
    let mut bytes = Vec::new();
    let mut output =
        NativeIpcOutput::new(&mut bytes, output_schema(&batches[0]), OUTPUT_LIMITS).unwrap();
    for batch in batches {
        output.write_batch(batch.clone()).unwrap();
    }
    let completion = output.finish().unwrap();
    drop(output);
    assert_eq!(completion.output_bytes, bytes.len());
    (bytes, completion)
}

fn output_schema_bytes(schema: &NativeIpcSchema) -> usize {
    let mut bytes = Vec::new();
    let output = NativeIpcOutput::new(&mut bytes, schema.clone(), OUTPUT_LIMITS).unwrap();
    let size = output.bytes_written();
    drop(output);
    assert_eq!(size, bytes.len());
    size
}

fn assert_single_output(bytes: &[u8], schema: &NativeIpcSchema, expected: &[RecordBatch]) {
    assert_eq!(&bytes[bytes.len() - 8..], &[255, 255, 255, 255, 0, 0, 0, 0]);
    assert_eq!(
        spans(bytes)
            .iter()
            .filter(|span| {
                arrow_ipc::root_as_message(&bytes[span.metadata.clone()])
                    .unwrap()
                    .header_type()
                    == MessageHeader::Schema
            })
            .count(),
        1,
    );
    let mut reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
    assert_eq!(reader.schema().as_ref(), schema.native_schema());
    for batch in expected {
        assert_eq!(&reader.next().unwrap().unwrap(), batch);
    }
    assert!(reader.next().is_none());
    assert_eq!(reader.get_ref().position(), bytes.len() as u64);
    assert_eq!(
        &NativeIpcDecoder::new()
            .inspect_schema(&chunk(bytes.to_vec()), LIMITS)
            .unwrap(),
        schema,
    );
}

struct OutputTestSink {
    bytes: Vec<u8>,
    max_write: usize,
    fail_after: Option<usize>,
    failure_kind: io::ErrorKind,
    fail_flush: bool,
    writes: usize,
    flushes: usize,
}

impl Default for OutputTestSink {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            max_write: usize::MAX,
            fail_after: None,
            failure_kind: io::ErrorKind::Other,
            fail_flush: false,
            writes: 0,
            flushes: 0,
        }
    }
}

impl Write for OutputTestSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self
            .fail_after
            .is_some_and(|limit| self.bytes.len() >= limit)
        {
            return Err(io::Error::new(
                self.failure_kind,
                "top-secret path: Q:/private/data.arrow",
            ));
        }
        let remaining = self
            .fail_after
            .map_or(usize::MAX, |limit| limit - self.bytes.len());
        let count = bytes.len().min(self.max_write).min(remaining);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            return Err(io::Error::other(
                "top-secret flush path: Q:/private/data.arrow",
            ));
        }
        Ok(())
    }
}

#[test]
fn framed_independent_chunks_become_one_standard_stream_with_dictionary_replacement() {
    let first = fixture();
    let dictionary: DictionaryArray<Int32Type> =
        [Some("green"), None, Some("gold")].into_iter().collect();
    let mut columns = first.columns().to_vec();
    columns[4] = Arc::new(dictionary);
    let second = RecordBatch::try_new(first.schema(), columns).unwrap();
    let first_chunk = chunk(write(std::slice::from_ref(&first), None));
    let second_chunk = RawChunk::new(
        1,
        0,
        "part-00001-000".into(),
        write(std::slice::from_ref(&second), None),
        StreamLimits::default(),
    )
    .unwrap();
    let schema = NativeIpcDecoder::new()
        .inspect_schema(&first_chunk, LIMITS)
        .unwrap();
    let mut wire = Vec::new();
    write_frame(&mut wire, &first_chunk).unwrap();
    write_frame(&mut wire, &second_chunk).unwrap();
    write_terminal(&mut wire).unwrap();
    let mut bytes = Vec::new();
    let mut output = NativeIpcOutput::new(&mut bytes, schema.clone(), OUTPUT_LIMITS).unwrap();
    let mut framed = FramedDecoder::new(StreamLimits::default()).unwrap();
    let mut typed = TypedStream::new(NativeIpcDecoder::new(), LIMITS).unwrap();
    let mut remaining = wire.as_slice();
    let mut chunks = 0;
    while !remaining.is_empty() {
        let consumed = framed.push(remaining).unwrap();
        remaining = &remaining[consumed..];
        let ready = framed.drain();
        assert!(consumed != 0 || ready.is_some());
        if let Some(raw) = ready {
            chunks += 1;
            typed
                .decode(&raw, &mut |batch| output.write_batch(batch))
                .unwrap();
        }
    }
    framed.finish().unwrap();
    let completion = output.finish().unwrap();
    drop(output);
    assert_eq!(chunks, 2);
    assert_eq!(completion.rows, 6);
    assert_eq!(completion.batches, 2);
    assert_eq!(completion.output_bytes, bytes.len());
    assert_single_output(&bytes, &schema, &[first, second]);
    let dictionaries = spans(&bytes)
        .into_iter()
        .filter(|span| {
            let message = arrow_ipc::root_as_message(&bytes[span.metadata.clone()]).unwrap();
            if let Some(dictionary) = message.header_as_dictionary_batch() {
                assert!(!dictionary.isDelta());
                true
            } else {
                false
            }
        })
        .count();
    assert_eq!(dictionaries, 2);
}

#[test]
fn output_accepts_local_dictionary_id_remapping_and_keeps_ordering() {
    let expected = dictionary_ordering_fixture(&[false, true], false);
    let first = write(std::slice::from_ref(&expected), None);
    let second = remap_second_dictionary(&first);
    let schema = NativeIpcDecoder::new()
        .inspect_schema(&chunk(first.clone()), LIMITS)
        .unwrap();
    let mut bytes = Vec::new();
    let mut output = NativeIpcOutput::new(&mut bytes, schema.clone(), OUTPUT_LIMITS).unwrap();
    for payload in [first, second] {
        NativeIpcDecoder::new()
            .decode(&chunk(payload), LIMITS, &mut |batch| {
                output.write_batch(batch)
            })
            .unwrap();
    }
    assert_eq!(output.finish().unwrap().batches, 2);
    drop(output);
    assert_single_output(&bytes, &schema, &[expected.clone(), expected]);
}

#[test]
fn output_empty_results_require_and_preserve_known_schema() {
    for native in [fixture().schema().as_ref().clone(), Schema::empty()] {
        let schema = NativeIpcDecoder::new()
            .inspect_schema(&chunk(schema_only(&native)), LIMITS)
            .unwrap();
        let mut bytes = Vec::new();
        let mut output = NativeIpcOutput::new(&mut bytes, schema.clone(), OUTPUT_LIMITS).unwrap();
        let completion = output.finish().unwrap();
        assert_eq!(completion.rows, 0);
        assert_eq!(completion.batches, 0);
        let finished_bytes = output.bytes_written();
        assert_eq!(output.finish(), Err(ArrowError::InvalidState));
        assert_eq!(output.abort(), Err(ArrowError::InvalidState));
        assert_eq!(output.bytes_written(), finished_bytes);
        drop(output);
        assert_eq!(completion.output_bytes, bytes.len());
        assert_single_output(&bytes, &schema, &[]);
    }
    let batch = primitive().slice(0, 0);
    let schema = output_schema(&batch);
    let (bytes, completion) = encoded_output(std::slice::from_ref(&batch));
    assert_eq!(completion.rows, 0);
    assert_eq!(completion.batches, 1);
    assert_single_output(&bytes, &schema, &[batch]);
}

#[test]
fn output_rejects_metadata_nullability_and_nested_dictionary_ordering_changes_before_writes() {
    let first = primitive();
    let mut conflicts = vec![
        (
            first.clone(),
            RecordBatch::try_new(
                Arc::new(Schema::new_with_metadata(
                    first.schema().fields().clone(),
                    HashMap::from([("changed".into(), "metadata".into())]),
                )),
                first.columns().to_vec(),
            )
            .unwrap(),
        ),
        (
            first.clone(),
            RecordBatch::try_new(
                Arc::new(Schema::new(vec![Field::new(
                    "value",
                    DataType::Int32,
                    true,
                )])),
                first.columns().to_vec(),
            )
            .unwrap(),
        ),
        (
            first.clone(),
            RecordBatch::try_new(
                Arc::new(Schema::new(vec![
                    first
                        .schema()
                        .field(0)
                        .clone()
                        .with_metadata(HashMap::from([(
                            "changed".into(),
                            "field-metadata".into(),
                        )])),
                ])),
                first.columns().to_vec(),
            )
            .unwrap(),
        ),
    ];
    for nested in [false, true] {
        conflicts.push((
            dictionary_ordering_fixture(&[false, true], nested),
            dictionary_ordering_fixture(&[true, false], nested),
        ));
    }
    for (first, second) in conflicts {
        let mut bytes = Vec::new();
        let mut output =
            NativeIpcOutput::new(&mut bytes, output_schema(&first), OUTPUT_LIMITS).unwrap();
        output.write_batch(first.clone()).unwrap();
        let before = output.bytes_written();
        assert_eq!(output.write_batch(second), Err(ArrowError::SchemaConflict));
        assert_eq!(output.bytes_written(), before);
        assert_eq!(output.write_batch(first), Err(ArrowError::InvalidState));
        assert_eq!(output.finish(), Err(ArrowError::InvalidState));
        assert_eq!(output.bytes_written(), before);
    }
}

#[test]
fn output_requires_positive_limits_before_schema_write() {
    let schema = output_schema(&primitive());
    for limits in [
        IpcOutputLimits {
            max_output_bytes: 0,
            ..OUTPUT_LIMITS
        },
        IpcOutputLimits {
            max_rows: 0,
            ..OUTPUT_LIMITS
        },
        IpcOutputLimits {
            max_batches: 0,
            ..OUTPUT_LIMITS
        },
    ] {
        let mut sink = OutputTestSink::default();
        assert_eq!(
            NativeIpcOutput::new(&mut sink, schema.clone(), limits).unwrap_err(),
            ArrowError::InvalidInput,
        );
        assert_eq!(sink.writes, 0);
        assert!(sink.bytes.is_empty());
    }
    let mut sink = OutputTestSink::default();
    assert_eq!(
        NativeIpcOutput::new(
            &mut sink,
            schema,
            IpcOutputLimits {
                max_output_bytes: 1,
                ..OUTPUT_LIMITS
            },
        )
        .unwrap_err(),
        ArrowError::Limit,
    );
    assert_eq!(sink.writes, 0);
    assert!(sink.bytes.is_empty());
}

#[test]
fn output_byte_limit_covers_schema_batch_and_eos_before_exceeding_cap() {
    let batch = primitive();
    let schema = output_schema(&batch);
    let (expected, _) = encoded_output(std::slice::from_ref(&batch));
    let mut exact = Vec::new();
    let mut output = NativeIpcOutput::new(
        &mut exact,
        schema.clone(),
        IpcOutputLimits {
            max_output_bytes: expected.len(),
            ..OUTPUT_LIMITS
        },
    )
    .unwrap();
    output.write_batch(batch.clone()).unwrap();
    output.finish().unwrap();
    drop(output);
    assert_eq!(exact, expected);
    let prefix = output_schema_bytes(&schema);
    let mut limited = Vec::new();
    let mut output = NativeIpcOutput::new(
        &mut limited,
        schema.clone(),
        IpcOutputLimits {
            max_output_bytes: prefix + 1,
            ..OUTPUT_LIMITS
        },
    )
    .unwrap();
    assert_eq!(output.write_batch(batch.clone()), Err(ArrowError::Limit));
    assert_eq!(output.bytes_written(), prefix);
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    drop(output);
    assert_eq!(limited.len(), prefix);
    let mut limited = Vec::new();
    let mut output = NativeIpcOutput::new(
        &mut limited,
        schema,
        IpcOutputLimits {
            max_output_bytes: expected.len() - 1,
            ..OUTPUT_LIMITS
        },
    )
    .unwrap();
    output.write_batch(batch).unwrap();
    assert_eq!(output.finish(), Err(ArrowError::Limit));
    let failed_bytes = output.bytes_written();
    assert!(failed_bytes < expected.len());
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    assert_eq!(output.bytes_written(), failed_bytes);
    drop(output);
    assert_eq!(limited.len(), failed_bytes);
}

#[test]
fn output_row_and_batch_limits_are_cumulative_and_count_zero_row_batches() {
    let batch = primitive();
    for limits in [
        IpcOutputLimits {
            max_rows: 3,
            ..OUTPUT_LIMITS
        },
        IpcOutputLimits {
            max_batches: 1,
            ..OUTPUT_LIMITS
        },
    ] {
        let mut bytes = Vec::new();
        let mut output = NativeIpcOutput::new(&mut bytes, output_schema(&batch), limits).unwrap();
        output.write_batch(batch.clone()).unwrap();
        let before = output.bytes_written();
        assert_eq!(output.write_batch(batch.clone()), Err(ArrowError::Limit));
        assert_eq!(output.bytes_written(), before);
        assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    }
    let mut bytes = Vec::new();
    let mut output = NativeIpcOutput::new(
        &mut bytes,
        output_schema(&batch),
        IpcOutputLimits {
            max_rows: 1,
            max_batches: 1,
            ..OUTPUT_LIMITS
        },
    )
    .unwrap();
    output.write_batch(batch.slice(0, 0)).unwrap();
    let before = output.bytes_written();
    assert_eq!(
        output.write_batch(batch.slice(0, 0)),
        Err(ArrowError::Limit)
    );
    assert_eq!(output.bytes_written(), before);
}

#[test]
fn output_checks_row_wire_representation_and_cumulative_integer_overflow() {
    let native = Arc::new(Schema::empty());
    let schema = NativeIpcDecoder::new()
        .inspect_schema(&chunk(schema_only(&native)), LIMITS)
        .unwrap();
    let batch = |rows| {
        RecordBatch::try_new_with_options(
            native.clone(),
            vec![],
            &RecordBatchOptions::new().with_row_count(Some(rows)),
        )
        .unwrap()
    };
    let limits = IpcOutputLimits {
        max_rows: usize::MAX,
        ..OUTPUT_LIMITS
    };
    if i64::try_from(usize::MAX).is_err() {
        let mut bytes = Vec::new();
        let mut output = NativeIpcOutput::new(&mut bytes, schema.clone(), limits).unwrap();
        let before = output.bytes_written();
        assert_eq!(
            output.write_batch(batch(usize::MAX)),
            Err(ArrowError::Limit)
        );
        assert_eq!(output.bytes_written(), before);
    }
    let large = usize::MAX / 2;
    let mut bytes = Vec::new();
    let mut output = NativeIpcOutput::new(&mut bytes, schema, limits).unwrap();
    output.write_batch(batch(large)).unwrap();
    output.write_batch(batch(large)).unwrap();
    let before = output.bytes_written();
    assert_eq!(output.write_batch(batch(2)), Err(ArrowError::Limit));
    assert_eq!(output.bytes_written(), before);
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
}

#[test]
fn output_retries_legal_short_writes_and_finishes_only_once() {
    let batch = fixture();
    let schema = output_schema(&batch);
    let mut sink = OutputTestSink {
        max_write: 3,
        ..OutputTestSink::default()
    };
    let mut output =
        NativeIpcOutput::new(&mut sink as &mut dyn Write, schema.clone(), OUTPUT_LIMITS).unwrap();
    output.write_batch(batch.clone()).unwrap();
    let completion = output.finish().unwrap();
    assert_eq!(
        output.write_batch(batch.clone()),
        Err(ArrowError::InvalidState)
    );
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    assert_eq!(output.bytes_written(), completion.output_bytes);
    drop(output);
    assert!(sink.writes > 10);
    assert_eq!(sink.flushes, 1);
    assert_single_output(&sink.bytes, &schema, &[batch]);
}

#[test]
fn output_real_sink_errors_and_zero_writes_are_redacted_not_retried_as_success() {
    let schema = output_schema(&primitive());
    for kind in [
        io::ErrorKind::Other,
        io::ErrorKind::Interrupted,
        io::ErrorKind::FileTooLarge,
    ] {
        let mut sink = OutputTestSink {
            fail_after: Some(0),
            failure_kind: kind,
            ..OutputTestSink::default()
        };
        let error = NativeIpcOutput::new(&mut sink, schema.clone(), OUTPUT_LIMITS).unwrap_err();
        assert_eq!(error, ArrowError::SinkFailed);
        assert!(!format!("{error:?} {error}").contains("top-secret"));
        assert_eq!(sink.writes, 1);
        assert!(sink.bytes.is_empty());
    }
    let mut sink = OutputTestSink {
        max_write: 0,
        ..OutputTestSink::default()
    };
    assert_eq!(
        NativeIpcOutput::new(&mut sink, schema.clone(), OUTPUT_LIMITS).unwrap_err(),
        ArrowError::SinkFailed,
    );
    assert_eq!(sink.writes, 1);
    let mut sink = OutputTestSink {
        max_write: 2,
        fail_after: Some(5),
        ..OutputTestSink::default()
    };
    assert_eq!(
        NativeIpcOutput::new(&mut sink, schema, OUTPUT_LIMITS).unwrap_err(),
        ArrowError::SinkFailed,
    );
    assert_eq!(sink.bytes.len(), 5);
    assert_eq!(sink.flushes, 0);
}

#[test]
fn output_partial_sink_failure_permanently_poisoned_without_finalization() {
    let batch = primitive();
    let schema = output_schema(&batch);
    let prefix = output_schema_bytes(&schema);
    let mut sink = OutputTestSink {
        max_write: 2,
        fail_after: Some(prefix + 5),
        ..OutputTestSink::default()
    };
    let mut output = NativeIpcOutput::new(&mut sink, schema, OUTPUT_LIMITS).unwrap();
    assert_eq!(
        output.write_batch(batch.clone()),
        Err(ArrowError::SinkFailed)
    );
    assert_eq!(output.bytes_written(), prefix + 5);
    assert_eq!(output.write_batch(batch), Err(ArrowError::InvalidState));
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    assert_eq!(output.abort(), Err(ArrowError::InvalidState));
    assert_eq!(output.bytes_written(), prefix + 5);
    drop(output);
    assert_eq!(sink.bytes.len(), prefix + 5);
    assert_eq!(sink.flushes, 0);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(sink.bytes), LIMITS),
        Err(ArrowError::Truncated),
    );
}

#[test]
fn output_flush_failure_is_not_local_completion_even_after_valid_eos() {
    let batch = primitive();
    let schema = output_schema(&batch);
    let mut sink = OutputTestSink {
        fail_flush: true,
        ..OutputTestSink::default()
    };
    let mut output = NativeIpcOutput::new(&mut sink, schema.clone(), OUTPUT_LIMITS).unwrap();
    output.write_batch(batch.clone()).unwrap();
    assert_eq!(output.finish(), Err(ArrowError::SinkFailed));
    let before = output.bytes_written();
    assert_eq!(output.finish(), Err(ArrowError::InvalidState));
    assert_eq!(output.bytes_written(), before);
    drop(output);
    assert_eq!(sink.flushes, 1);
    assert_single_output(&sink.bytes, &schema, &[batch]);
}

#[test]
fn output_abort_or_drop_without_finish_leaves_partial_bytes_and_redacted_debug() {
    for abort in [false, true] {
        let batch = fixture();
        let mut sink = OutputTestSink::default();
        let mut output =
            NativeIpcOutput::new(&mut sink, output_schema(&batch), OUTPUT_LIMITS).unwrap();
        output.write_batch(batch.clone()).unwrap();
        let debug = format!("{output:?}");
        for secret in [
            "offline-native-writer",
            "dictionary",
            "top-secret",
            "data.arrow",
        ] {
            assert!(!debug.contains(secret));
        }

        let before = output.bytes_written();
        if abort {
            output.abort().unwrap();
            assert_eq!(output.write_batch(batch), Err(ArrowError::InvalidState));
            assert_eq!(output.finish(), Err(ArrowError::InvalidState));
        }
        assert_eq!(output.bytes_written(), before);
        drop(output);
        assert_eq!(sink.bytes.len(), before);
        assert_eq!(sink.flushes, 0);
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&chunk(sink.bytes), LIMITS),
            Err(ArrowError::Truncated),
        );
    }
}

#[tokio::test]
async fn result_partial_sink_and_constructor_failure_preserve_bytes_without_replay() {
    let batch = primitive();
    let schema_bytes = output_schema_bytes(&output_schema(&batch));
    for boundary in [1, schema_bytes + 1] {
        let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(result_wire([write(
            std::slice::from_ref(&batch),
            None,
        )]))]]);
        let mut stream = result_stream(&executor);
        let mut sink = OutputTestSink {
            fail_after: Some(boundary),
            max_write: 1,
            ..OutputTestSink::default()
        };
        let error = write_ipc_result(&mut stream, &mut sink, LIMITS, OUTPUT_LIMITS)
            .await
            .unwrap_err();
        if boundary == 1 {
            assert!(matches!(
                error.cause(),
                IpcResultCause::Arrow(ArrowError::SinkFailed)
            ));
        } else {
            let IpcResultCause::Stream(original) = error.cause() else {
                panic!("missing sink failure")
            };
            assert_eq!(
                original.cause(),
                FramedStreamCause::Arrow(ArrowError::SinkFailed)
            );
            assert!(original.progress().sink_may_be_partial);
        }
        assert_eq!(error.output_bytes(), boundary);
        assert_eq!(error.output_bytes(), sink.bytes.len());
        assert_eq!(sink.flushes, 0);
        assert_eq!(error.progress().delivered_chunks, 0);
        assert!(error.cursor().is_none());
        assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 1);
        assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
        assert!(!format!("{error}").contains("top-secret"));
    }
}

#[tokio::test]
async fn result_flush_failure_keeps_acknowledged_progress_without_claiming_completion() {
    let batch = primitive();
    let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(result_wire([write(
        std::slice::from_ref(&batch),
        None,
    )]))]]);
    let mut stream = result_stream(&executor);
    let mut sink = OutputTestSink {
        fail_flush: true,
        ..OutputTestSink::default()
    };
    let error = write_ipc_result(&mut stream, &mut sink, LIMITS, OUTPUT_LIMITS)
        .await
        .unwrap_err();
    assert!(matches!(
        error.cause(),
        IpcResultCause::Arrow(ArrowError::SinkFailed)
    ));
    assert_eq!(error.progress().state, FramedStreamState::Complete);
    assert_eq!(error.progress().delivered_chunks, 1);
    assert!(error.cursor().is_some());
    assert_eq!(error.output_bytes(), sink.bytes.len());
    assert_eq!(sink.flushes, 1);
    assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
    assert_single_output(&sink.bytes, &output_schema(&batch), &[batch]);
}

#[tokio::test]
async fn result_missing_terminal_and_output_bounds_never_finalize_partial_bytes() {
    for missing_terminal in [false, true] {
        let batch = primitive();
        let payload = write(std::slice::from_ref(&batch), None);
        let wire = if missing_terminal {
            result_frame(payload, 0)
        } else {
            result_wire([payload])
        };
        let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(wire)]]);
        let mut stream = result_stream(&executor);
        // This scenario deliberately disables truncation recovery, not framing validation.
        if missing_terminal {
            stream = FramedResultStream::new(
                &executor,
                stream.handle().clone(),
                ResultSelectors::default(),
                FramedStreamOptions::default(),
                NativeDeadline::new(Duration::from_secs(30)).unwrap(),
            )
            .unwrap();
        }
        let mut sink = OutputTestSink::default();
        let error = write_ipc_result(
            &mut stream,
            &mut sink,
            LIMITS,
            IpcOutputLimits {
                max_rows: if missing_terminal { 1000 } else { 1 },
                ..OUTPUT_LIMITS
            },
        )
        .await
        .unwrap_err();
        let IpcResultCause::Stream(original) = error.cause() else {
            panic!("missing structured failure")
        };
        assert_eq!(
            original.cause(),
            FramedStreamCause::Arrow(if missing_terminal {
                ArrowError::Truncated
            } else {
                ArrowError::Limit
            }),
        );
        assert_eq!(error.output_bytes(), sink.bytes.len());
        assert_eq!(sink.flushes, 0);
        assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 1);
        assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            NativeIpcDecoder::new().inspect_schema(&chunk(sink.bytes), LIMITS),
            Err(ArrowError::Truncated)
        );
    }
}

#[tokio::test]
async fn result_preserves_local_dictionary_id_remapping_across_chunks() {
    let batch = dictionary_ordering_fixture(&[false, true], false);
    let first = write(std::slice::from_ref(&batch), None);
    let remapped = remap_second_dictionary(&first);
    let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(result_wire([
        first, remapped,
    ]))]]);
    let mut stream = result_stream(&executor);
    let mut bytes = Vec::new();
    let work = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS);
    fn assert_send<T: Send>(value: T) -> T {
        value
    }
    let receipt = assert_send(work).await.unwrap();
    assert_eq!(receipt.output.rows, 6);
    assert_eq!(receipt.stream.delivered_chunks, 2);
    assert_single_output(&bytes, &output_schema(&batch), &[batch.clone(), batch]);
}

enum ResultStep {
    Bytes(Vec<u8>),
    Failure(NativeError),
    Pending,
}

#[derive(Default)]
struct ResultTrace {
    opens: AtomicUsize,
    reads: AtomicUsize,
    drops: AtomicUsize,
}

struct ResultBody {
    steps: VecDeque<ResultStep>,
    trace: Arc<ResultTrace>,
}

impl Drop for ResultBody {
    fn drop(&mut self) {
        self.trace.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl NativeStreamBody for ResultBody {
    fn read<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            self.trace.reads.fetch_add(1, Ordering::SeqCst);
            match self.steps.pop_front() {
                Some(ResultStep::Bytes(mut bytes)) => {
                    if bytes.len() > max_bytes {
                        let tail = bytes.split_off(max_bytes);
                        self.steps.push_front(ResultStep::Bytes(tail));
                    }
                    Ok(Some(bytes))
                }
                Some(ResultStep::Failure(error)) => Err(error),
                Some(ResultStep::Pending) => pending().await,
                None => Ok(None),
            }
        })
    }
}

struct ResultExecutor {
    responses: Mutex<VecDeque<Vec<ResultStep>>>,
    trace: Arc<ResultTrace>,
}

impl ResultExecutor {
    fn new(responses: Vec<Vec<ResultStep>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            trace: Arc::new(ResultTrace::default()),
        }
    }
}

impl NativeStreamExecutor for ResultExecutor {
    fn open<'a>(
        &'a self,
        request: &'a NativeRequest,
        _: NativeStreamLimits,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<NativeStreamResponse, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            assert_eq!(request.method(), fabric_sdk::runtime::NativeMethod::Get);
            self.trace.opens.fetch_add(1, Ordering::SeqCst);
            Ok(NativeStreamResponse {
                status: 200,
                content_type: Some("application/vnd.livy.arrow-chunks;v=2".into()),
                headers: BTreeMap::new(),
                body: Box::new(ResultBody {
                    steps: self
                        .responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected reopen")
                        .into(),
                    trace: self.trace.clone(),
                }),
            })
        })
    }
}

fn result_stream(executor: &ResultExecutor) -> FramedResultStream<'_> {
    let owner = StatementOwner::session(
        NativeEndpoint::new("https://example.test/livy/".parse().unwrap()).unwrap(),
        "private-session",
        None,
    )
    .unwrap();
    FramedResultStream::new(
        executor,
        StatementHandle::new(owner, 7),
        ResultSelectors::default(),
        FramedStreamOptions {
            max_responses: 3,
            transport: NativeStreamLimits {
                max_fragment_bytes: 97,
                max_body_bytes: 8 * 1024 * 1024,
            },
            resume: FramedResume::Recoverable {
                max_reconnects: 2,
                retry_truncated_eof: true,
            },
            ..FramedStreamOptions::default()
        },
        NativeDeadline::new(Duration::from_secs(30)).unwrap(),
    )
    .unwrap()
}

fn result_frame(payload: Vec<u8>, ordinal: i32) -> Vec<u8> {
    let chunk = RawChunk::new(
        0,
        ordinal,
        format!("part-0-{ordinal}"),
        payload,
        StreamLimits::default(),
    )
    .unwrap();
    let mut wire = Vec::new();
    write_frame(&mut wire, &chunk).unwrap();
    wire
}

fn result_wire(payloads: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut wire = Vec::new();
    for (index, payload) in payloads.into_iter().enumerate() {
        wire.extend(result_frame(payload, i32::try_from(index).unwrap()));
    }
    write_terminal(&mut wire).unwrap();
    wire
}

#[tokio::test]
async fn result_composes_fragmented_http_into_one_dictionary_preserving_ipc_stream() {
    let first = fixture();
    let mut columns = first.columns().to_vec();
    columns[4] = Arc::new(
        [Some("green"), None, Some("gold")]
            .into_iter()
            .collect::<DictionaryArray<Int32Type>>(),
    );
    let second = RecordBatch::try_new(first.schema(), columns).unwrap();
    let wire = result_wire([
        write(std::slice::from_ref(&first), Some(CompressionType::ZSTD)),
        write(
            std::slice::from_ref(&second),
            Some(CompressionType::LZ4_FRAME),
        ),
    ]);
    let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(wire)]]);
    let mut stream = result_stream(&executor);
    let mut bytes = Vec::new();
    let receipt = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS)
        .await
        .unwrap();
    assert_eq!(receipt.output.rows, 6);
    assert_eq!(receipt.output.batches, 2);
    assert_eq!(receipt.output.output_bytes, bytes.len());
    assert_eq!(receipt.stream.state, FramedStreamState::Complete);
    assert_eq!(receipt.stream.delivered_chunks, 2);
    assert!(receipt.stream.eof_received);
    assert!(executor.trace.reads.load(Ordering::SeqCst) > 2);
    assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
    assert_single_output(&bytes, &output_schema(&first), &[first, second]);
}

#[tokio::test]
async fn result_recovery_deduplicates_before_decode_and_output_acknowledgement() {
    let batch = primitive();
    let payload = write(std::slice::from_ref(&batch), None);
    let executor = ResultExecutor::new(vec![
        vec![
            ResultStep::Bytes(result_frame(payload.clone(), 0)),
            ResultStep::Failure(NativeError::Transport),
        ],
        vec![ResultStep::Bytes(result_wire([payload.clone(), payload]))],
    ]);
    let mut stream = result_stream(&executor);
    let mut bytes = Vec::new();
    let receipt = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS)
        .await
        .unwrap();
    assert_eq!(receipt.output.rows, 6);
    assert_eq!(receipt.stream.delivered_chunks, 2);
    assert_eq!(receipt.stream.replayed_chunks, 1);
    assert_eq!(receipt.stream.reconnects, 1);
    assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 2);
    assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 2);
    assert_single_output(&bytes, &output_schema(&batch), &[batch.clone(), batch]);
}

#[tokio::test]
async fn result_requires_observed_schema_and_preserves_known_empty_results() {
    let schema = primitive().schema();
    for payloads in [vec![], vec![schema_only(schema.as_ref())]] {
        let empty = payloads.is_empty();
        let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(result_wire(payloads))]]);
        let mut stream = result_stream(&executor);
        let mut bytes = Vec::new();
        let result = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS).await;
        if empty {
            let error = result.unwrap_err();
            assert!(matches!(error.cause(), IpcResultCause::MissingSchema));
            assert_eq!(error.output_bytes(), 0);
            assert!(bytes.is_empty());
        } else {
            let receipt = result.unwrap();
            assert_eq!(receipt.output.rows, 0);
            assert_eq!(receipt.output.batches, 0);
            assert_single_output(&bytes, &output_schema(&primitive()), &[]);
        }
        assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn result_never_finishes_after_bad_http_framing_or_ipc() {
    for bad_ipc in [false, true] {
        for trailing in [false, true] {
            let payload = if bad_ipc {
                b"invalid-ipc".to_vec()
            } else {
                write(&[primitive()], None)
            };
            let mut wire = result_frame(payload, 0);
            if trailing {
                write_terminal(&mut wire).unwrap();
                wire.push(1);
            } else {
                wire.extend_from_slice(&(-1_i64).to_be_bytes());
            }
            let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(wire)]]);
            let mut stream = result_stream(&executor);
            let mut bytes = Vec::new();
            let error = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS)
                .await
                .unwrap_err();
            assert_eq!(error.output_bytes(), bytes.len());
            assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 1);
            assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
            if !bad_ipc {
                assert!(matches!(error.cause(), IpcResultCause::Stream(_)));
                assert_eq!(
                    NativeIpcDecoder::new().inspect_schema(&chunk(bytes), LIMITS),
                    Err(ArrowError::Truncated),
                );
            } else {
                assert!(bytes.is_empty());
                assert!(matches!(error.cause(), IpcResultCause::Arrow(_)));
            }
        }
    }
}

#[tokio::test]
async fn result_schema_conflict_does_not_acknowledge_or_finish_second_chunk() {
    let first = dictionary_ordering_fixture(&[false], true);
    let second = dictionary_ordering_fixture(&[true], true);
    let executor = ResultExecutor::new(vec![vec![ResultStep::Bytes(result_wire([
        write(&[first], None),
        write(&[second], None),
    ]))]]);
    let mut stream = result_stream(&executor);
    let mut bytes = Vec::new();
    let error = write_ipc_result(&mut stream, &mut bytes, LIMITS, OUTPUT_LIMITS)
        .await
        .unwrap_err();
    let IpcResultCause::Stream(cause) = error.cause() else {
        panic!("missing original cause")
    };
    assert_eq!(
        cause.cause(),
        FramedStreamCause::Arrow(ArrowError::SchemaConflict)
    );
    assert_eq!(error.progress().delivered_chunks, 1);
    assert!(error.cursor().is_some());
    assert_eq!(error.output_bytes(), bytes.len());
    assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 1);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(bytes), LIMITS),
        Err(ArrowError::Truncated)
    );
    let debug = format!("{error:?}");
    for secret in ["private-session", "example.test", "dictionary"] {
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
async fn result_invalid_limits_reject_before_network_and_schema_writes() {
    for invalid_decode in [false, true] {
        let executor = ResultExecutor::new(vec![]);
        let mut stream = result_stream(&executor);
        let mut bytes = Vec::new();
        let error = write_ipc_result(
            &mut stream,
            &mut bytes,
            DecodeLimits {
                max_message_bytes: if invalid_decode {
                    0
                } else {
                    LIMITS.max_message_bytes
                },
                ..LIMITS
            },
            IpcOutputLimits {
                max_output_bytes: if invalid_decode {
                    OUTPUT_LIMITS.max_output_bytes
                } else {
                    0
                },
                ..OUTPUT_LIMITS
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.cause(),
            IpcResultCause::Arrow(ArrowError::InvalidInput)
        ));
        assert!(bytes.is_empty());
        assert_eq!(executor.trace.opens.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn result_cancelled_future_closes_active_response_and_never_finishes_sink() {
    let executor = ResultExecutor::new(vec![vec![
        ResultStep::Bytes(result_frame(write(&[primitive()], None), 0)),
        ResultStep::Pending,
    ]]);
    let mut stream = result_stream(&executor);
    let mut sink = OutputTestSink::default();
    let mut work = Box::pin(write_ipc_result(
        &mut stream,
        &mut sink,
        LIMITS,
        OUTPUT_LIMITS,
    ));
    assert!(matches!(
        Pin::as_mut(&mut work).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(work);
    assert_eq!(stream.progress().state, FramedStreamState::Closed);
    assert_eq!(stream.progress().delivered_chunks, 1);
    assert_eq!(executor.trace.drops.load(Ordering::SeqCst), 1);
    assert_eq!(sink.flushes, 0);
    assert_eq!(
        NativeIpcDecoder::new().inspect_schema(&chunk(sink.bytes), LIMITS),
        Err(ArrowError::Truncated)
    );
}
