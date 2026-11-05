// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native IPC fixtures, written by Apache Arrow rather than a schema mock.

#![cfg(feature = "livy-arrow-ipc")]
#![deny(unnameable_test_items)]

use arrow_array::{
    ArrayRef, BooleanArray, DictionaryArray, Int32Array, ListArray, RecordBatch, StringArray,
    StringViewArray, StructArray, types::Int32Type,
};
use arrow_ipc::{
    CompressionType, MessageHeader,
    writer::{DictionaryHandling, IpcWriteOptions, StreamWriter},
};
use arrow_schema::{DataType, Field, Schema};
use fabric_sdk::livy_arrow::{
    ArrowError, DecodeLimits, NativeIpcDecoder, RawChunk, StreamLimits, TypedDecoder, TypedStream,
    ipc::NativeIpcSchema,
};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

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
    let mut second = first.clone();
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
