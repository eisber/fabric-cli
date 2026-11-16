// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline tests of the production response-body/decode path after a safe head.
use super::*;
use fabric_core::{Audience, CancellationToken};
use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

enum Mode {
    Fail,
    Stall,
    Cancel(CancellationToken),
}
struct SyntheticBody {
    mode: Mode,
    polls: Arc<AtomicUsize>,
}
impl hyper::body::Body for SyntheticBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        match &self.mode {
            Mode::Fail => Poll::Ready(Some(Err(std::io::Error::other("synthetic body failure")))),
            Mode::Stall => Poll::Pending,
            Mode::Cancel(cancellation) => {
                cancellation.cancel();
                Poll::Pending
            }
        }
    }
}
fn safe_headers(audience: Audience) -> BTreeMap<String, String> {
    let url = if audience == Audience::Fabric {
        "/v1/operations/op-1?secret=synthetic"
    } else {
        "/operation-status?secret=synthetic"
    };
    let name = if audience == Audience::Fabric {
        "location"
    } else {
        "azure-asyncoperation"
    };
    BTreeMap::from([(name.into(), url.into())])
}
fn assert_safe_error(error: CoreError, audience: Audience, expected: ErrorKind) {
    assert_eq!(error.kind, expected);
    assert!(!format!("{error:?}").contains("synthetic"));
    let recovery = error.recovery.unwrap();
    assert!(recovery.outcome_uncertain);
    assert_eq!(
        recovery.operation_id.as_deref(),
        if audience == Audience::Fabric {
            Some("op-1")
        } else {
            None
        }
    );
    assert_eq!(
        recovery.status_url,
        if audience == Audience::Fabric {
            "https://api.fabric.microsoft.com/v1/operations/op-1"
        } else {
            "https://management.azure.com/operation-status"
        }
    );
}

#[tokio::test]
async fn safe_head_survives_real_body_reader_stream_and_decode_errors() {
    for audience in [Audience::Fabric, Audience::Azure] {
        let context = OperationContext::new(Duration::from_secs(240), CancellationToken::new());
        let head = ResponseHeadCapture::new(audience);
        let body = SyntheticBody {
            mode: Mode::Fail,
            polls: Arc::new(AtomicUsize::new(0)),
        };
        let error = receive(202, safe_headers(audience), body, &context, &head)
            .await
            .unwrap_err();
        assert_safe_error(error, audience, ErrorKind::Transport);

        let mut headers = safe_headers(audience);
        headers.insert("content-encoding".into(), "gzip".into());
        let error = receive(
            202,
            headers,
            Full::new(Bytes::from_static(b"not gzip")),
            &context,
            &head,
        )
        .await
        .unwrap_err();
        assert_safe_error(error, audience, ErrorKind::Protocol);
    }
}

#[tokio::test(start_paused = true)]
async fn safe_head_survives_pending_body_timeout_and_cancellation() {
    for audience in [Audience::Fabric, Audience::Azure] {
        for cancel in [false, true] {
            let context = OperationContext::new(Duration::from_secs(1), CancellationToken::new());
            let head = ResponseHeadCapture::new(audience);
            let body = SyntheticBody {
                mode: if cancel {
                    Mode::Cancel(context.cancellation.clone())
                } else {
                    Mode::Stall
                },
                polls: Arc::new(AtomicUsize::new(0)),
            };
            let error = receive(202, safe_headers(audience), body, &context, &head)
                .await
                .unwrap_err();
            assert_safe_error(
                error,
                audience,
                if cancel {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Timeout
                },
            );
        }
    }
}

#[tokio::test]
async fn unsafe_or_conflicting_handle_is_not_followed_or_leaked_but_raw_response_survives() {
    for fields in [
        vec![("location", "http://evil.invalid/status?secret=synthetic")],
        vec![
            ("location", "/v1/operations/location-id?secret=synthetic"),
            ("x-ms-operation-id", "different-id"),
        ],
    ] {
        let context = OperationContext::new(Duration::from_secs(240), CancellationToken::new());
        let head = ResponseHeadCapture::new(Audience::Fabric);
        let headers: BTreeMap<String, String> = fields
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let response = receive(
            202,
            headers.clone(),
            Full::new(Bytes::from_static(b"raw initial")),
            &context,
            &head,
        )
        .await
        .unwrap();
        assert_eq!(response.status, 202);
        assert_eq!(response.body, b"raw initial");
        let error =
            fabric_core::operation_handle(Audience::Fabric, 202, &response.headers).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!format!("{error:?}").contains("synthetic"));

        let body = SyntheticBody {
            mode: Mode::Fail,
            polls: Arc::new(AtomicUsize::new(0)),
        };
        let error = receive(202, headers, body, &context, &head)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Transport);
        assert!(!format!("{error:?}").contains("synthetic"));
        assert!(!format!("{error:?}").contains("evil.invalid"));
    }
}

#[tokio::test]
async fn ambiguous_http_control_headers_fail_before_body_read() {
    let context = OperationContext::new(Duration::from_secs(240), CancellationToken::new());
    let head = ResponseHeadCapture::new(Audience::Fabric);
    let polls = Arc::new(AtomicUsize::new(0));
    let body = SyntheticBody {
        mode: Mode::Fail,
        polls: polls.clone(),
    };
    let headers = BTreeMap::from([
        ("Location".into(), "/v1/operations/first".into()),
        ("location".into(), "/v1/operations/second".into()),
    ]);
    let error = receive(202, headers, body, &context, &head)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(head.recovery().is_none());
}

fn raw_headers(fields: &[(&str, &str)]) -> http::HeaderMap {
    let mut raw = http::HeaderMap::new();
    for (name, value) in fields {
        raw.append(
            http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse::<http::HeaderValue>().unwrap(),
        );
    }
    raw
}

#[test]
fn native_ingestion_rejects_duplicate_metadata_but_retains_unique_safe_id() {
    for duplicate in ["retry-after", "content-encoding"] {
        for id_first in [false, true] {
            let mut fields = vec![
                (duplicate, "1"),
                (duplicate, "2"),
                ("location", "https://evil.invalid/status?secret=synthetic"),
            ];
            if id_first {
                fields.insert(0, ("x-ms-operation-id", "op-1"));
            } else {
                fields.push(("x-ms-operation-id", "op-1"));
            }
            let raw = raw_headers(&fields);
            let head = ResponseHeadCapture::new(Audience::Fabric);
            let error = ingest_headers(202, &raw, &head).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Protocol);
            assert!(!format!("{error:?}").contains("synthetic"));
            assert!(!format!("{error:?}").contains("evil.invalid"));
            let recovery = error.recovery.unwrap();
            assert_eq!(recovery.operation_id.as_deref(), Some("op-1"));
            assert_eq!(
                recovery.status_url,
                "https://api.fabric.microsoft.com/v1/operations/op-1"
            );
            assert!(recovery.outcome_uncertain);
            assert_eq!(
                head.recovery().unwrap().operation_id.as_deref(),
                Some("op-1")
            );
        }
    }
}

#[test]
fn native_ingestion_counts_every_raw_id_and_never_salvages_location() {
    for ids in [
        vec![("x-ms-operation-id", "op-1"), ("x-ms-operation-id", "op-1")],
        vec![
            ("x-ms-operation-id", "op-1"),
            ("x-ms-operation-id", "other"),
        ],
        vec![
            ("x-ms-operation-id", "bad/id"),
            ("x-ms-operation-id", "op-1"),
        ],
        vec![("x-ms-operation-id", "bad/id")],
        vec![],
    ] {
        let mut fields = vec![("retry-after", "1"), ("retry-after", "2")];
        fields.extend(ids);
        fields.push(("location", "/v1/operations/location-only?secret=synthetic"));
        let head = ResponseHeadCapture::new(Audience::Fabric);
        let error = ingest_headers(202, &raw_headers(&fields), &head).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
        assert!(head.recovery().is_none());
    }
}
