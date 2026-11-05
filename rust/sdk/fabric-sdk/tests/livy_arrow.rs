// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public contracts. Golden bytes copied unchanged from the source pin
//! bded4919a647b3f5f24b723abada69d45f23449c, conformance/vectors/framing-v2.json.
//! Native IPC decoding is not implemented or simulated by these tests.

#![cfg(feature = "livy-arrow")]
#![deny(unnameable_test_items)]

use azure_core::http::Url;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fabric_sdk::livy_arrow::{
    ARROW_CHUNKS_MEDIA_TYPE, ARROW_STREAM_MEDIA_TYPE, ArrowError, DecodeLimits, Delivery,
    DeliveryState, Failure, FramedDecoder, RawChunk, RawInline, ResponseHeaders,
    ResponseRepresentation, ResultSelectors, RetryDecision, RetryPolicy, StatementHandle,
    StatementOwner, StopReason, StreamLimits, SubmitMode, SubmitOptions, TypedDecoder, TypedStream,
    UnsupportedDecoder, validate_response_headers, write_frame, write_terminal,
};
use fabric_sdk::runtime::{NativeEffect, NativeEndpoint, NativeMethod};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const GOLDENS: &str = r#"{
  "schemaVersion": 1,
  "mediaType": "application/vnd.livy.arrow-chunks;v=2",
  "vectors": [
    {"name":"empty","bodyBase64":"AAAAAAAAAAA=","expectedChunks":[]},
    {"name":"single","bodyBase64":"AAAAAAAAAAkAAAAAAAAAAAAAAA5wYXJ0LTAwMDAwLTAwMGFycm93LWlwYwAAAAAAAAAA","expectedChunks":[{"partition":0,"ordinal":0,"chunkId":"part-00000-000","payloadBase64":"YXJyb3ctaXBj"}]},
    {"name":"two-partitions","bodyBase64":"AAAAAAAAAAQAAAAAAAAAAAAAAA5wYXJ0LTAwMDAwLTAwMHplcm8AAAAAAAAAAwAAAAIAAAAAAAAADnBhcnQtMDAwMDItMDAwdHdvAAAAAAAAAAA=","expectedChunks":[{"partition":0,"ordinal":0,"chunkId":"part-00000-000","payloadBase64":"emVybw=="},{"partition":2,"ordinal":0,"chunkId":"part-00002-000","payloadBase64":"dHdv"}]},
    {"name":"nonzero-ordinal","bodyBase64":"AAAAAAAAAAUAAAACAAAABwAAAA5wYXJ0LTAwMDAyLTAwN3NldmVuAAAAAAAAAAA=","expectedChunks":[{"partition":2,"ordinal":7,"chunkId":"part-00002-007","payloadBase64":"c2V2ZW4="}]},
    {"name":"failure-marker","bodyBase64":"//////////8=","expectedError":"LivyStreamError"},
    {"name":"negative-length","bodyBase64":"//////////4=","expectedError":"LivyProtocolError"},
    {"name":"truncated-header","bodyBase64":"MTIzNA==","expectedError":"LivyTruncatedStreamError"},
    {"name":"zero-id-length","bodyBase64":"AAAAAAAAAAEAAAAAAAAAAAAAAAB4","expectedError":"LivyProtocolError"},
    {"name":"partition-mismatch","bodyBase64":"AAAAAAAAAAEAAAAAAAAAAAAAAA5wYXJ0LTAwMDAxLTAwMHgAAAAAAAAAAA==","expectedError":"LivyProtocolError"},
    {"name":"duplicate-id","bodyBase64":"AAAAAAAAAAEAAAAAAAAAAAAAAA5wYXJ0LTAwMDAwLTAwMGEAAAAAAAAAAQAAAAAAAAAAAAAADnBhcnQtMDAwMDAtMDAwYgAAAAAAAAAA","expectedError":"LivyProtocolError"},
    {"name":"negative-ordinal","bodyBase64":"AAAAAAAAAAEAAAAA/////wAAAA5wYXJ0LTAwMDAwLTAwMHgAAAAAAAAAAA==","expectedError":"LivyProtocolError"},
    {"name":"duplicate-ordinal","bodyBase64":"AAAAAAAAAAEAAAAAAAAAAAAAAA5wYXJ0LTAwMDAwLTAwMGEAAAAAAAAAAQAAAAAAAAAAAAAADnBhcnQtMDAwMDAtMDAxYgAAAAAAAAAA","expectedError":"LivyProtocolError"},
    {"name":"truncated-metadata","bodyBase64":"AAAAAAAAAAEAAAAAAAAAAA==","expectedError":"LivyTruncatedStreamError"}
  ]
}"#;

const RESOURCE_ID: &str = "90a713da-abd0-47cc-a8ce-be2e3948eefa";

fn endpoint(base: &str) -> NativeEndpoint {
    NativeEndpoint::new(Url::parse(base).unwrap()).unwrap()
}

fn owner() -> StatementOwner {
    StatementOwner::session(endpoint("https://example.test/livy/"), "3", None).unwrap()
}

fn handle() -> StatementHandle {
    StatementHandle::new(owner(), 7)
}

fn chunk(partition: i32, ordinal: i32, id: &str, payload: &[u8]) -> RawChunk {
    RawChunk::new(
        partition,
        ordinal,
        id.into(),
        payload.to_vec(),
        StreamLimits::default(),
    )
    .unwrap()
}

fn frame(partition: i32, ordinal: i32, id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(payload.len() as i64).to_be_bytes());
    bytes.extend_from_slice(&partition.to_be_bytes());
    bytes.extend_from_slice(&ordinal.to_be_bytes());
    bytes.extend_from_slice(&(id.len() as i32).to_be_bytes());
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(payload);
    bytes
}

fn terminated(mut body: Vec<u8>) -> Vec<u8> {
    body.extend_from_slice(&0_i64.to_be_bytes());
    body
}

fn parse_pieces(pieces: &[&[u8]], limits: StreamLimits) -> Result<Vec<RawChunk>, ArrowError> {
    let mut decoder = FramedDecoder::new(limits)?;
    let mut result = Vec::new();
    for mut bytes in pieces.iter().copied() {
        while !bytes.is_empty() {
            let consumed = decoder.push(bytes)?;
            bytes = &bytes[consumed..];
            let ready = decoder.drain();
            assert!(consumed != 0 || ready.is_some(), "parser made no progress");
            if let Some(chunk) = ready {
                result.push(chunk);
            }
        }
    }
    decoder.finish()?;
    Ok(result)
}

fn parse(body: &[u8]) -> Result<Vec<RawChunk>, ArrowError> {
    parse_pieces(&[body], StreamLimits::default())
}

fn assert_golden(vector: &Value, result: Result<Vec<RawChunk>, ArrowError>) {
    if let Some(expected_error) = vector.get("expectedError") {
        let error = result.unwrap_err();
        match expected_error.as_str().unwrap() {
            "LivyStreamError" => assert_eq!(error, ArrowError::ServiceFailure),
            "LivyTruncatedStreamError" => assert_eq!(error, ArrowError::Truncated),
            "LivyProtocolError" => assert!(
                matches!(
                    error,
                    ArrowError::Protocol | ArrowError::DuplicateId | ArrowError::OrdinalConflict
                ),
                "{error:?}"
            ),
            _ => panic!("unknown source error class"),
        }
    } else {
        let chunks = result.unwrap();
        let expected = vector["expectedChunks"].as_array().unwrap();
        assert_eq!(chunks.len(), expected.len());
        for (actual, expected) in chunks.iter().zip(expected) {
            assert_eq!(
                i64::from(actual.partition()),
                expected["partition"].as_i64().unwrap()
            );
            assert_eq!(
                i64::from(actual.ordinal()),
                expected["ordinal"].as_i64().unwrap()
            );
            assert_eq!(actual.id(), expected["chunkId"].as_str().unwrap());
            assert_eq!(
                actual.payload(),
                STANDARD
                    .decode(expected["payloadBase64"].as_str().unwrap())
                    .unwrap()
            );
        }
    }
}

#[test]
fn all_source_goldens_at_every_fragment_boundary() {
    let catalog: Value = serde_json::from_str(GOLDENS).unwrap();
    for vector in catalog["vectors"].as_array().unwrap() {
        let bytes = STANDARD
            .decode(vector["bodyBase64"].as_str().unwrap())
            .unwrap();
        for split in 0..=bytes.len() {
            assert_golden(
                vector,
                parse_pieces(&[&bytes[..split], &bytes[split..]], StreamLimits::default()),
            );
        }
        for width in 1..=bytes.len() {
            let pieces: Vec<_> = bytes.chunks(width).collect();
            assert_golden(vector, parse_pieces(&pieces, StreamLimits::default()));
        }
    }
}

#[test]
fn every_truncated_prefix_including_missing_terminal_fails() {
    let bytes = terminated(frame(0, 0, b"part-0-0", b"payload"));
    for end in 0..bytes.len() {
        assert_eq!(
            parse(&bytes[..end]),
            Err(ArrowError::Truncated),
            "prefix {end}"
        );
    }
    assert_eq!(parse(&bytes).unwrap().len(), 1);
}

#[test]
fn terminal_is_eight_bytes_and_requires_eof_without_trailing_data() {
    assert!(parse(&0_i64.to_be_bytes()).unwrap().is_empty());
    for trailer in [vec![0], 0_i64.to_be_bytes().to_vec(), b"x".to_vec()] {
        let mut bytes = 0_i64.to_be_bytes().to_vec();
        bytes.extend(trailer);
        assert_eq!(parse(&bytes), Err(ArrowError::TrailingBytes));
    }
    assert_eq!(
        parse(&(-1_i64).to_be_bytes()),
        Err(ArrowError::ServiceFailure)
    );
    for length in [-2_i64, i64::MIN] {
        assert_eq!(parse(&length.to_be_bytes()), Err(ArrowError::Protocol));
    }
}

#[test]
fn invalid_utf8_shape_partition_and_signed_metadata_reject_before_payload() {
    for id in [
        &b"part-0-\xff"[..],
        b"bad",
        b"part-1-0",
        b"part--0",
        b"part-0-",
        b"part-0-1-extra",
        b"part-0-0\n",
    ] {
        assert_eq!(parse(&frame(0, 0, id, b"x")), Err(ArrowError::Protocol));
    }
    assert_eq!(
        parse(&frame(-1, 0, b"part-0-0", b"x")),
        Err(ArrowError::Protocol)
    );
    assert_eq!(
        parse(&frame(0, -1, b"part-0-0", b"x")),
        Err(ArrowError::Protocol)
    );
    for length in [0_i32, -1, 65_537, i32::MAX] {
        let mut bytes = frame(0, 0, b"part-0-0", b"x");
        bytes[16..20].copy_from_slice(&length.to_be_bytes());
        assert_eq!(parse(&bytes[..20]), Err(ArrowError::Protocol));
    }
}

#[test]
fn variable_width_and_unicode_decimal_ids_do_not_imply_ordinal_contiguity() {
    let mut bytes = frame(2, 9, "part-００２-٧".as_bytes(), b"x");
    bytes.extend(frame(2, 1, b"part-0002-123", b"y"));
    let chunks = parse(&terminated(bytes)).unwrap();
    assert_eq!(
        chunks.iter().map(RawChunk::ordinal).collect::<Vec<_>>(),
        [9, 1]
    );
}

#[test]
fn protocol_maximum_id_and_partition_boundaries_are_checked_without_integer_wrap() {
    let id = format!("part-0-{}", "0".repeat(65_536 - 7));
    assert_eq!(id.len(), 65_536);
    let bytes = terminated(frame(0, i32::MAX, id.as_bytes(), b"x"));
    let pieces: Vec<_> = bytes.chunks(127).collect();
    let decoded = parse_pieces(&pieces, StreamLimits::default()).unwrap();
    assert_eq!(decoded[0].id().len(), 65_536);
    assert_eq!(decoded[0].ordinal(), i32::MAX);
    assert_eq!(
        parse(&terminated(frame(i32::MAX, 0, b"part-2147483647-0", b"x")))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        parse(&frame(0, 0, b"part-2147483648-0", b"x")),
        Err(ArrowError::Protocol)
    );
}

#[test]
fn duplicate_id_rejects_even_when_payload_is_identical_in_one_response() {
    let first = frame(0, 0, b"part-0-0", b"x");
    let mut bytes = first.clone();
    bytes.extend(first);
    assert_eq!(parse(&terminated(bytes)), Err(ArrowError::DuplicateId));
    let mut bytes = frame(0, 0, b"part-0-0", b"x");
    bytes.extend(frame(0, 0, b"part-0-1", b"x"));
    assert_eq!(parse(&terminated(bytes)), Err(ArrowError::OrdinalConflict));
}

#[test]
fn one_pending_frame_applies_backpressure_without_reading_next_frame() {
    let first = frame(0, 0, b"part-0-0", b"x");
    let mut bytes = first.clone();
    bytes.extend(frame(1, 4, b"part-1-0", b"y"));
    bytes = terminated(bytes);
    let mut decoder = FramedDecoder::new(StreamLimits::default()).unwrap();
    let consumed = decoder.push(&bytes).unwrap();
    assert_eq!(consumed, first.len());
    assert_eq!(decoder.wire_bytes(), first.len() as u64);
    assert_eq!(decoder.push(&bytes[consumed..]), Ok(0));
    assert_eq!(decoder.finish(), Err(ArrowError::InvalidState));
    assert_eq!(decoder.drain().unwrap().payload(), b"x");
    let next = decoder.push(&bytes[consumed..]).unwrap();
    assert_eq!(decoder.drain().unwrap().payload(), b"y");
    decoder.push(&bytes[consumed + next..]).unwrap();
    decoder.finish().unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.push(b"x"), Err(ArrowError::InvalidState));
}

#[test]
fn finite_limits_reject_declared_lengths_before_body_allocation() {
    let bytes = terminated(frame(0, 0, b"part-0-0", b"abc"));
    for limits in [
        StreamLimits {
            max_frame_bytes: 2,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_total_bytes: 2,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_id_bytes: 7,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_identity_bytes: 7,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_wire_bytes: 22,
            ..StreamLimits::default()
        },
    ] {
        assert_eq!(
            parse_pieces(&[&bytes[..20]], limits),
            Err(ArrowError::Limit)
        );
    }
    let mut decoder = FramedDecoder::new(StreamLimits::default()).unwrap();
    assert_eq!(
        decoder.push(&i64::MAX.to_be_bytes()),
        Err(ArrowError::Limit)
    );
    assert_eq!(decoder.push(b""), Err(ArrowError::Limit));
    assert_eq!(decoder.finish(), Err(ArrowError::Limit));
}

#[test]
fn total_chunk_and_identity_budgets_span_frames_and_accept_exact_bounds() {
    let first = frame(0, 0, b"part-0-0", b"x");
    let mut bytes = first.clone();
    bytes.extend(frame(0, 1, b"part-0-1", b"y"));
    bytes = terminated(bytes);
    for limits in [
        StreamLimits {
            max_chunks: 1,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_total_bytes: 1,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_identity_bytes: 15,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_wire_bytes: bytes.len() as u64 - 1,
            ..StreamLimits::default()
        },
    ] {
        assert_eq!(parse_pieces(&[&bytes], limits), Err(ArrowError::Limit));
    }
    let limits = StreamLimits {
        max_frame_bytes: 1,
        max_total_bytes: 2,
        max_chunks: 2,
        max_id_bytes: 8,
        max_identity_bytes: 16,
        max_wire_bytes: bytes.len() as u64,
    };
    assert_eq!(parse_pieces(&[&bytes], limits).unwrap().len(), 2);
}

#[test]
fn zero_and_protocol_exceeding_limit_options_are_invalid() {
    for limits in [
        StreamLimits {
            max_frame_bytes: 0,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_total_bytes: 0,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_chunks: 0,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_id_bytes: 0,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_id_bytes: 65_537,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_identity_bytes: 0,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_wire_bytes: 7,
            ..StreamLimits::default()
        },
    ] {
        assert_eq!(limits.validate(), Err(ArrowError::InvalidInput));
    }
}

#[test]
fn raw_inline_makes_no_binary_terminal_or_typed_ipc_assumptions() {
    let mut raw = RawInline::new(10).unwrap();
    assert_eq!(raw.consume(&[0; 8]).unwrap(), &[0; 8]);
    assert_eq!(raw.consume(b"xy").unwrap(), b"xy");
    raw.finish().unwrap();
    assert_eq!(raw.consume(b""), Err(ArrowError::InvalidState));
    let mut raw = RawInline::new(1).unwrap();
    assert_eq!(raw.consume(b"xx"), Err(ArrowError::Limit));
    assert_eq!(raw.finish(), Err(ArrowError::InvalidState));
    let mut empty = RawInline::new(1).unwrap();
    empty.finish().unwrap();
}

#[test]
fn explicitly_labeled_raw_framed_output_round_trips_not_one_ipc_file() {
    let mut framed_v2_not_ipc = Vec::new();
    write_frame(
        &mut framed_v2_not_ipc,
        &chunk(0, 4, "part-0-0", b"first independent IPC"),
    )
    .unwrap();
    write_frame(
        &mut framed_v2_not_ipc,
        &chunk(1, 2, "part-1-0", b"second independent IPC"),
    )
    .unwrap();
    assert_eq!(parse(&framed_v2_not_ipc), Err(ArrowError::Truncated));
    write_terminal(&mut framed_v2_not_ipc).unwrap();
    assert_eq!(parse(&framed_v2_not_ipc).unwrap().len(), 2);
    let mut bounded_output = [0_u8; 1];
    assert_eq!(
        write_frame(&mut &mut bounded_output[..], &chunk(0, 0, "part-0-0", b"x")),
        Err(ArrowError::Output)
    );
}

#[test]
fn in_memory_delivery_suppresses_identical_replay_and_preserves_owner_selectors() {
    let handle = handle();
    let initial = ResultSelectors::new(None, Some("0,2".into()), None).unwrap();
    let mut state = DeliveryState::new(handle.clone(), initial, StreamLimits::default()).unwrap();
    assert!(state.cursor().is_none());
    let first = chunk(2, 7, "part-2-000", b"a");
    let mut emitted = 0;
    assert_eq!(
        state.deliver(&handle, &first, |_| {
            emitted += 1;
            Ok(())
        }),
        Ok(Delivery::Delivered)
    );
    assert_eq!(
        state.deliver(&handle, &first, |_| panic!("replay reached sink")),
        Ok(Delivery::Replayed)
    );
    assert_eq!(emitted, 1);
    assert_eq!(state.delivered_chunks(), 1);
    assert_eq!(state.delivered_bytes(), 1);
    let cursor = state.cursor().unwrap();
    assert_eq!(cursor.handle(), &handle);
    assert_eq!(cursor.selectors().after_chunk(), Some("part-2-000"));
    assert_eq!(cursor.selectors().parts(), Some("0,2"));
    assert_eq!(cursor.selectors().from_part(), Some(2));
    assert_eq!(
        cursor
            .handle()
            .result_request(cursor.selectors())
            .unwrap()
            .native()
            .url()
            .path(),
        "/livy/sessions/3/statements/7/result"
    );
}

#[test]
fn disconnected_partial_chunk_never_advances_cursor_then_redownload_is_delivered() {
    let handle = handle();
    let mut state = DeliveryState::new(
        handle.clone(),
        ResultSelectors::default(),
        StreamLimits::default(),
    )
    .unwrap();
    let bytes = frame(0, 0, b"part-0-0", b"abcdef");
    let mut response = FramedDecoder::new(StreamLimits::default()).unwrap();
    response.push(&bytes[..bytes.len() - 1]).unwrap();
    assert!(response.drain().is_none());
    assert_eq!(response.finish(), Err(ArrowError::Truncated));
    assert!(state.cursor().is_none());
    for next_response in 0..2 {
        for chunk in parse(&terminated(bytes.clone())).unwrap() {
            let delivery = state.deliver(&handle, &chunk, |_| Ok(())).unwrap();
            assert_eq!(
                delivery,
                if next_response == 0 {
                    Delivery::Delivered
                } else {
                    Delivery::Replayed
                }
            );
        }
    }
}

#[test]
fn changed_replay_payload_partition_or_ordinal_and_new_id_position_conflict_fail_closed() {
    let first = chunk(0, 7, "part-0-0", b"same length");
    for conflict in [
        chunk(0, 7, "part-0-0", b"other bytes"),
        chunk(0, 8, "part-0-0", b"same length"),
    ] {
        let handle = handle();
        let mut state = DeliveryState::new(
            handle.clone(),
            ResultSelectors::default(),
            StreamLimits::default(),
        )
        .unwrap();
        state.deliver(&handle, &first, |_| Ok(())).unwrap();
        assert_eq!(
            state.deliver(&handle, &conflict, |_| panic!("conflict delivered")),
            Err(ArrowError::ReplayConflict)
        );
        assert_eq!(
            state.deliver(&handle, &first, |_| Ok(())),
            Err(ArrowError::InvalidState)
        );
    }
    // A changed partition with the same ID cannot even form a valid RawChunk.
    assert_eq!(
        RawChunk::new(
            1,
            7,
            "part-0-0".into(),
            b"x".to_vec(),
            StreamLimits::default()
        ),
        Err(ArrowError::Protocol)
    );
    let handle = handle();
    let mut state = DeliveryState::new(
        handle.clone(),
        ResultSelectors::default(),
        StreamLimits::default(),
    )
    .unwrap();
    state.deliver(&handle, &first, |_| Ok(())).unwrap();
    assert_eq!(
        state.deliver(&handle, &chunk(0, 7, "part-0-1", b"x"), |_| Ok(())),
        Err(ArrowError::OrdinalConflict)
    );
}

#[test]
fn owner_mismatch_and_sink_failure_do_not_advance_cursor_or_claim_durability() {
    let handle = handle();
    let mut state = DeliveryState::new(
        handle.clone(),
        ResultSelectors::default(),
        StreamLimits::default(),
    )
    .unwrap();
    let first = chunk(0, 0, "part-0-0", b"x");
    let other_owner = StatementOwner::session(endpoint("https://other.test/"), "3", None).unwrap();
    assert_eq!(
        state.deliver(&StatementHandle::new(other_owner, 7), &first, |_| Ok(())),
        Err(ArrowError::OwnerMismatch)
    );
    assert_eq!(
        state.deliver(&StatementHandle::new(owner(), 8), &first, |_| Ok(())),
        Err(ArrowError::OwnerMismatch)
    );
    assert!(state.cursor().is_none());
    assert_eq!(
        state.deliver(&handle, &first, |_| Err(ArrowError::Output)),
        Err(ArrowError::SinkFailed)
    );
    assert!(state.cursor().is_none());
    assert_eq!(
        state.deliver(&handle, &first, |_| Ok(())),
        Err(ArrowError::InvalidState)
    );
}

#[test]
fn logical_unique_budgets_span_responses_but_replays_do_not_consume_them() {
    let handle = handle();
    let first = chunk(0, 4, "part-0-0", b"x");
    for limits in [
        StreamLimits {
            max_chunks: 1,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_total_bytes: 1,
            ..StreamLimits::default()
        },
        StreamLimits {
            max_identity_bytes: 8,
            ..StreamLimits::default()
        },
    ] {
        let mut state =
            DeliveryState::new(handle.clone(), ResultSelectors::default(), limits).unwrap();
        state.deliver(&handle, &first, |_| Ok(())).unwrap();
        assert_eq!(
            state.deliver(&handle, &first, |_| Ok(())),
            Ok(Delivery::Replayed)
        );
        assert_eq!(
            state.deliver(&handle, &chunk(1, 0, "part-1-0", b"x"), |_| Ok(())),
            Err(ArrowError::Limit)
        );
    }
}

#[test]
fn cross_process_resume_is_not_inferred_from_an_external_cursor() {
    let handle = handle();
    let first = chunk(0, 0, "part-0-0", b"x");
    let external = ResultSelectors::new(Some(first.id().into()), None, Some(0)).unwrap();
    let mut new_iteration =
        DeliveryState::new(handle.clone(), external, StreamLimits::default()).unwrap();
    // No serialized identity history exists: this is new delivery, not deduped
    // output from a previous process or a transactional-sink acknowledgment.
    assert_eq!(
        new_iteration.deliver(&handle, &first, |_| Ok(())),
        Ok(Delivery::Delivered)
    );
}

#[test]
fn retry_plan_is_finite_and_404_protocol_failure_and_all_posts_are_nonretryable() {
    let now = Instant::now();
    let get = handle()
        .result_request(&ResultSelectors::default())
        .unwrap();
    for failure in [
        Failure::Transport,
        Failure::Truncated,
        Failure::Http(408),
        Failure::Http(429),
        Failure::Http(500),
        Failure::Http(502),
        Failure::Http(503),
        Failure::Http(504),
    ] {
        let mut policy = RetryPolicy::new(now, Duration::from_secs(60), 1).unwrap();
        assert_eq!(
            policy.on_failure(now, failure, &get),
            RetryDecision::Reconnect { attempt: 1 }
        );
        assert_eq!(
            policy.on_failure(now, failure, &get),
            RetryDecision::Stop(StopReason::Exhausted)
        );
        assert_eq!(
            policy.on_failure(now, failure, &get),
            RetryDecision::Stop(StopReason::AlreadyStopped)
        );
    }
    for failure in [
        Failure::Http(400),
        Failure::Http(401),
        Failure::Http(403),
        Failure::Http(404),
        Failure::Http(501),
        Failure::Protocol,
        Failure::ServiceFailure,
        Failure::Decode,
        Failure::Cancelled,
        Failure::TimedOut,
        Failure::OutcomeUnknown,
        Failure::Sink,
    ] {
        let mut policy = RetryPolicy::new(now, Duration::from_secs(60), 2).unwrap();
        assert_eq!(
            policy.on_failure(now, failure, &get),
            RetryDecision::Stop(StopReason::Permanent(failure))
        );
    }
    for mode in [
        SubmitMode::Handle { sync_wait_ms: None },
        SubmitMode::Inline {
            sync_wait_ms: 1,
            allow_json_fallback: true,
        },
    ] {
        let post = SubmitOptions {
            code: "select 1",
            kind: "sql",
            codec: "none",
            mode,
        }
        .request(&owner(), RESOURCE_ID, 100)
        .unwrap();
        for failure in [Failure::Transport, Failure::Http(503), Failure::Truncated] {
            let mut policy = RetryPolicy::new(now, Duration::from_secs(60), 10).unwrap();
            assert_eq!(
                policy.on_failure(now, failure, &post),
                RetryDecision::Stop(StopReason::Mutation)
            );
        }
    }
}

#[test]
fn absolute_deadline_survives_reconnects_and_caller_pauses() {
    let now = Instant::now();
    let get = handle()
        .result_request(&ResultSelectors::default())
        .unwrap();
    let mut policy = RetryPolicy::new(now, Duration::from_secs(10), 2).unwrap();
    assert_eq!(
        policy.on_failure(now + Duration::from_secs(2), Failure::Truncated, &get),
        RetryDecision::Reconnect { attempt: 1 }
    );
    assert_eq!(
        policy.remaining(now + Duration::from_secs(9)),
        Ok(Duration::from_secs(1))
    );
    assert_eq!(
        policy.remaining(now + Duration::from_secs(10)),
        Err(ArrowError::TimedOut)
    );
    assert_eq!(
        policy.on_failure(now + Duration::from_secs(11), Failure::Transport, &get),
        RetryDecision::Stop(StopReason::Deadline)
    );
    let mut no_retries = RetryPolicy::new(now, Duration::from_secs(10), 0).unwrap();
    assert_eq!(
        no_retries.on_failure(now, Failure::Transport, &get),
        RetryDecision::Stop(StopReason::Exhausted)
    );
}

#[test]
fn codec_error_classification_never_retries_protocol_limit_schema_or_sink_failures() {
    assert_eq!(Failure::from(ArrowError::Truncated), Failure::Truncated);
    assert_eq!(Failure::from(ArrowError::Http(404)), Failure::Http(404));
    assert_eq!(
        Failure::from(ArrowError::ServiceFailure),
        Failure::ServiceFailure
    );
    assert_eq!(Failure::from(ArrowError::SchemaConflict), Failure::Decode);
    assert_eq!(Failure::from(ArrowError::SinkFailed), Failure::Sink);
    let now = Instant::now();
    let get = handle()
        .result_request(&ResultSelectors::default())
        .unwrap();
    for error in [
        ArrowError::Limit,
        ArrowError::Allocation,
        ArrowError::Protocol,
        ArrowError::TrailingBytes,
        ArrowError::DuplicateId,
        ArrowError::OrdinalConflict,
        ArrowError::ReplayConflict,
        ArrowError::OwnerMismatch,
        ArrowError::InvalidState,
        ArrowError::SchemaConflict,
        ArrowError::UnsupportedCapability,
        ArrowError::OutcomeUnknown,
        ArrowError::Output,
    ] {
        let mut policy = RetryPolicy::new(now, Duration::from_secs(60), 2).unwrap();
        assert!(matches!(
            policy.on_failure(now, error.into(), &get),
            RetryDecision::Stop(_)
        ));
    }
}

fn headers<'a>(content_type: Option<&'a str>) -> ResponseHeaders<'a> {
    ResponseHeaders {
        status: 200,
        content_type,
        content_encoding: None,
        content_length: None,
    }
}

#[test]
fn media_version_duplicate_parameters_compression_and_status_validation() {
    for content_type in [
        None,
        Some(ARROW_CHUNKS_MEDIA_TYPE),
        Some("application/vnd.livy.arrow-chunks;v=1"),
        Some("application/vnd.livy.arrow-chunks;v=2;V=2"),
        Some("application/vnd.livy.arrow-chunks;v=2;bad"),
        Some("application/vnd.livy.arrow-chunks;v=\"2"),
        Some("application/vnd.livy.arrow-chunks;v=2\n"),
        Some(ARROW_STREAM_MEDIA_TYPE),
        Some("application/vnd.livy.arrow-page;v=1"),
    ] {
        assert_eq!(
            validate_response_headers(
                headers(content_type),
                ResponseRepresentation::FramedV2,
                false,
                100
            ),
            Err(ArrowError::Protocol)
        );
    }
    for content_type in [
        "application/vnd.livy.arrow-chunks;v=2",
        "Application/Vnd.Livy.Arrow-Chunks ; V=\"2\"",
    ] {
        assert_eq!(
            validate_response_headers(
                headers(Some(content_type)),
                ResponseRepresentation::FramedV2,
                false,
                100
            ),
            Ok(ResponseRepresentation::FramedV2)
        );
    }
    assert_eq!(
        validate_response_headers(
            ResponseHeaders {
                status: 404,
                ..headers(None)
            },
            ResponseRepresentation::FramedV2,
            false,
            100
        ),
        Err(ArrowError::Http(404))
    );
    for encoding in ["gzip", "br", "identity,gzip"] {
        assert_eq!(
            validate_response_headers(
                ResponseHeaders {
                    content_encoding: Some(encoding),
                    ..headers(Some(ARROW_STREAM_MEDIA_TYPE))
                },
                ResponseRepresentation::InlineIpc,
                false,
                100
            ),
            Err(ArrowError::Protocol)
        );
    }
    for length in ["-1", "abc", "+1", "1.0", ""] {
        assert_eq!(
            validate_response_headers(
                ResponseHeaders {
                    content_length: Some(length),
                    ..headers(Some(ARROW_STREAM_MEDIA_TYPE))
                },
                ResponseRepresentation::InlineIpc,
                false,
                100
            ),
            Err(ArrowError::Protocol)
        );
    }
    assert_eq!(
        validate_response_headers(
            ResponseHeaders {
                content_length: Some("101"),
                ..headers(Some(ARROW_STREAM_MEDIA_TYPE))
            },
            ResponseRepresentation::InlineIpc,
            false,
            100
        ),
        Err(ArrowError::Limit)
    );
}

#[test]
fn exact_owner_scoped_routes_and_selector_queries_preserve_execution_identity() {
    let fabric_base =
        "https://example.test/v1/workspaces/w/lakehouses/l/livyApi/versions/2024-04-01";
    for (owner, expected) in [
        (owner(), "/livy/sessions/3/statements/7/result"),
        (
            StatementOwner::session(endpoint("https://example.test"), "s", Some("r")).unwrap(),
            "/sessions/s/repls/r/statements/7/result",
        ),
        (
            StatementOwner::session(endpoint(fabric_base), "s", None).unwrap(),
            "/v1/workspaces/w/lakehouses/l/livyApi/versions/2024-04-01/sessions/s/statements/7/result",
        ),
        (
            StatementOwner::high_concurrency(endpoint(fabric_base), "execution", "owned-repl")
                .unwrap(),
            "/v1/workspaces/w/lakehouses/l/livyApi/versions/2024-04-01/highConcurrencySessions/execution/repls/owned-repl/statements/7/result",
        ),
    ] {
        let selectors =
            ResultSelectors::new(Some("part-2-9".into()), Some("2,3".into()), Some(2)).unwrap();
        let request = StatementHandle::new(owner.clone(), 7)
            .result_request(&selectors)
            .unwrap();
        assert_eq!(request.native().method(), NativeMethod::Get);
        assert_eq!(request.native().effect(), NativeEffect::Read);
        assert_eq!(request.native().url().path(), expected);
        assert_eq!(request.native().accept(), ARROW_CHUNKS_MEDIA_TYPE);
        assert!(request.native().body().is_none());
        assert!(request.native().bytes().is_none());
        assert!(request.native().headers().is_empty());
        assert_eq!(
            request.headers().collect::<Vec<_>>(),
            [("accept", ARROW_CHUNKS_MEDIA_TYPE)]
        );
        let query: Vec<_> = request
            .native()
            .url()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            query,
            [
                ("after_chunk".to_string(), "part-2-9".to_string()),
                ("parts".to_string(), "2,3".to_string()),
                ("from_part".to_string(), "2".to_string()),
            ]
        );
        let post = SubmitOptions {
            code: "select 1",
            kind: "sql",
            codec: "zstd",
            mode: SubmitMode::Handle { sync_wait_ms: None },
        }
        .request(&owner, RESOURCE_ID, 100)
        .unwrap();
        assert_eq!(
            post.native().url().path(),
            expected.strip_suffix("/7/result").unwrap()
        );
    }
}

#[test]
fn route_injection_and_invalid_selectors_fail_before_any_dispatch() {
    for id in ["", "..", "a/b", "a\\b", "a%2fb", "a?b", "a#b", "x\n"] {
        assert!(StatementOwner::session(endpoint("https://example.test"), id, None).is_err());
        assert!(
            StatementOwner::high_concurrency(endpoint("https://example.test"), "s", id).is_err()
        );
    }
    assert!(ResultSelectors::new(None, None, Some(-1)).is_err());
    assert!(ResultSelectors::new(Some("part-1-0".into()), None, Some(0)).is_err());
    assert!(ResultSelectors::new(None, Some("x\n".into()), None).is_err());
    assert!(ResultSelectors::new(None, Some("x".repeat(65_537)), None).is_err());
    let selectors =
        ResultSelectors::new(None, Some("provider-specific-expression".into()), None).unwrap();
    assert_eq!(selectors.parts(), Some("provider-specific-expression"));
}

#[test]
fn submission_body_required_headers_wait_boundaries_and_inline_accept_are_exact() {
    for wait in [None, Some(0), Some(1), Some(10_000)] {
        let request = SubmitOptions {
            code: "select 1",
            kind: "sql",
            codec: "none",
            mode: SubmitMode::Handle { sync_wait_ms: wait },
        }
        .request(&owner(), RESOURCE_ID, 100)
        .unwrap();
        assert_eq!(request.native().method(), NativeMethod::Post);
        assert_eq!(request.native().effect(), NativeEffect::Execute);
        assert_eq!(request.native().accept(), "application/json");
        assert_eq!(request.native().content_type(), "application/json");
        assert!(request.native().bytes().is_none());
        assert_eq!(
            request
                .native()
                .headers()
                .get("x-ms-livy-resource-id")
                .map(String::as_str),
            Some(RESOURCE_ID)
        );
        assert_eq!(request.native().headers().len(), 1);
        assert_eq!(
            request.native().body(),
            Some(&json!({"code":"select 1","kind":"sql","format":"arrow","codec":"none"}))
        );
        assert_eq!(
            request.native().url().query(),
            wait.map(|wait| format!("sync_wait_ms={wait}")).as_deref()
        );
        assert_eq!(
            request.headers().collect::<Vec<_>>(),
            [
                ("accept", "application/json"),
                ("content-type", "application/json"),
                ("x-ms-livy-resource-id", RESOURCE_ID)
            ]
        );
    }
    for (fallback, accept) in [
        (false, ARROW_STREAM_MEDIA_TYPE),
        (
            true,
            "application/vnd.apache.arrow.stream, application/json;q=0.9",
        ),
    ] {
        for wait in [1, 10_000] {
            let request = SubmitOptions {
                code: "select 1",
                kind: "sql",
                codec: "none",
                mode: SubmitMode::Inline {
                    sync_wait_ms: wait,
                    allow_json_fallback: fallback,
                },
            }
            .request(&owner(), RESOURCE_ID, 100)
            .unwrap();
            assert_eq!(request.native().accept(), accept);
            assert_eq!(request.native().content_type(), "application/json");
            assert_eq!(
                request
                    .native()
                    .headers()
                    .get("x-ms-livy-resource-id")
                    .map(String::as_str),
                Some(RESOURCE_ID)
            );
            assert_eq!(
                request
                    .validate_response(headers(Some("application/json")), 100)
                    .is_ok(),
                fallback
            );
            assert_eq!(request.inline_fallback(100).is_ok(), fallback);
            assert_eq!(
                request.validate_response(headers(Some(ARROW_STREAM_MEDIA_TYPE)), 100),
                Ok(ResponseRepresentation::InlineIpc)
            );
        }
    }
    for mode in [
        SubmitMode::Handle {
            sync_wait_ms: Some(10_001),
        },
        SubmitMode::Inline {
            sync_wait_ms: 0,
            allow_json_fallback: true,
        },
        SubmitMode::Inline {
            sync_wait_ms: 10_001,
            allow_json_fallback: false,
        },
    ] {
        assert!(
            SubmitOptions {
                code: "x",
                kind: "sql",
                codec: "none",
                mode
            }
            .request(&owner(), RESOURCE_ID, 100)
            .is_err()
        );
    }
    let options = SubmitOptions {
        code: "long",
        kind: "sql",
        codec: "none",
        mode: SubmitMode::Handle { sync_wait_ms: None },
    };
    assert!(options.request(&owner(), "not-a-uuid", 100).is_err());
    assert!(options.request(&owner(), RESOURCE_ID, 2).is_err());
}

#[test]
fn bounded_opt_in_inline_fallback_preserves_hc_owner_and_matches_only_statement_ids() {
    let owner =
        StatementOwner::high_concurrency(endpoint("https://example.test"), "execution", "repl")
            .unwrap();
    let request = SubmitOptions {
        code: "select 1",
        kind: "sql",
        codec: "none",
        mode: SubmitMode::Inline {
            sync_wait_ms: 1,
            allow_json_fallback: true,
        },
    }
    .request(&owner, RESOURCE_ID, 100)
    .unwrap();
    for (body, header, expected) in [
        (r#"{"id":7}"#, None, Some(7)),
        (r#"{"id":7}"#, Some("007"), Some(7)),
        (r#"{}"#, Some("7"), Some(7)),
        (r#"{"id":null}"#, Some("7"), Some(7)),
        (r#"{"id":0}"#, None, Some(0)),
        (r#"{"id":7}"#, Some("8"), None),
        (r#"{"id":true}"#, None, None),
        (r#"{"id":7.0}"#, None, None),
        (r#"{"id":-1}"#, None, None),
        (r#"{"id":"7"}"#, None, None),
        (r#"{}"#, None, None),
        (r#"{}"#, Some("repl"), None),
        (r#"{}"#, Some("+7"), None),
        (r#"{}"#, Some("٧"), None),
        (r#"{"id":7}"#, Some(""), None),
        (r#"[]"#, Some("7"), None),
        ("not json", Some("7"), None),
    ] {
        let mut fallback = request.inline_fallback(100).unwrap();
        for byte in body.as_bytes() {
            fallback.push(&[*byte]).unwrap();
        }
        match expected {
            Some(id) => {
                let handle = fallback.finish(header).unwrap();
                assert_eq!(handle, StatementHandle::new(owner.clone(), id));
                assert!(
                    handle
                        .result_request(&ResultSelectors::default())
                        .unwrap()
                        .native()
                        .url()
                        .path()
                        .contains("/highConcurrencySessions/execution/repls/repl/")
                );
            }
            None => assert_eq!(fallback.finish(header), Err(ArrowError::OutcomeUnknown)),
        }
    }
    let mut fallback = request.inline_fallback(2).unwrap();
    assert_eq!(fallback.push(b"{\"id\":7}"), Err(ArrowError::Limit));
    assert_eq!(fallback.push(b"{}"), Err(ArrowError::InvalidState));
    assert_eq!(fallback.finish(Some("7")), Err(ArrowError::InvalidState));
}

// This is an interface-contract probe, NOT an IPC/schema/decompression implementation.
struct SchemaProbe;

impl TypedDecoder for SchemaProbe {
    type Batch = u8;
    type Schema = Vec<u8>;

    fn inspect_schema(
        &mut self,
        chunk: &RawChunk,
        _limits: DecodeLimits,
    ) -> Result<Vec<u8>, ArrowError> {
        Ok(chunk.payload().to_vec())
    }

    fn decode(
        &mut self,
        _chunk: &RawChunk,
        _limits: DecodeLimits,
        emit: &mut dyn FnMut(u8) -> Result<(), ArrowError>,
    ) -> Result<(), ArrowError> {
        emit(1)
    }
}

#[test]
fn external_decoder_seam_checks_schema_before_delivery_and_default_is_unsupported() {
    let limits = DecodeLimits {
        max_message_bytes: 100,
        max_decoded_bytes: 100,
    };
    let first = chunk(0, 0, "part-0-0", b"schema including metadata");
    let changed = chunk(1, 0, "part-1-0", b"changed metadata");
    let mut typed = TypedStream::new(SchemaProbe, limits).unwrap();
    let mut emitted = 0;
    typed
        .decode(&first, &mut |_| {
            emitted += 1;
            Ok(())
        })
        .unwrap();
    typed
        .decode(&first, &mut |_| {
            emitted += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        typed.decode(&changed, &mut |_| panic!("schema conflict emitted")),
        Err(ArrowError::SchemaConflict)
    );
    assert_eq!(emitted, 2);
    assert_eq!(
        typed.decode(&first, &mut |_| Ok(())),
        Err(ArrowError::InvalidState)
    );
    let mut unsupported = TypedStream::new(UnsupportedDecoder, limits).unwrap();
    assert_eq!(
        unsupported.decode(&first, &mut |_| panic!("unsupported decoder emitted")),
        Err(ArrowError::UnsupportedCapability)
    );
    let mut typed = TypedStream::new(SchemaProbe, limits).unwrap();
    assert_eq!(
        typed.decode(&first, &mut |_| Err(ArrowError::Output)),
        Err(ArrowError::Output)
    );
    assert_eq!(
        typed.decode(&first, &mut |_| Ok(())),
        Err(ArrowError::InvalidState)
    );
}
