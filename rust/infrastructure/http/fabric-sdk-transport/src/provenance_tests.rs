// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline decoder, raw metadata and real native-client counterexamples.

#![deny(unnameable_test_items)]

use super::{client, gzip, request, test_tls};
use crate::{
    decode::decoded,
    native::{response_headers, transport_error_kind},
    test_server::{Reply, Server},
};
use azure_core::{
    Bytes,
    error::ErrorKind,
    http::{HttpClient, Method, headers::Headers},
};
use fabric_sdk::{
    Audience, CredentialSelection, RequestContext,
    runtime::{
        NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeMethod,
        NativeStreamExecutor, NativeStreamLimits,
    },
};
use fabric_sdk_http::native_stream::NativeFabricStreamExecutor;
use futures_util::{StreamExt, stream};
use std::{io, sync::Arc, time::Duration};

#[tokio::test]
async fn decoder_preserves_upstream_kinds_for_identity_gzip_and_deflate_without_diagnostics() {
    for encoding in ["identity", "gzip", "deflate"] {
        for kind in [ErrorKind::Io, ErrorKind::DataConversion, ErrorKind::Other] {
            let source = stream::iter([Err(azure_core::Error::with_message(
                kind.clone(),
                "private-upstream-body-token",
            ))]);
            let mut headers = Headers::new();
            headers.insert("content-encoding", encoding.to_owned());
            let mut body = decoded(Box::pin(source), &headers, true).unwrap();
            let error = body.next().await.unwrap().unwrap_err();
            assert_eq!(error.kind(), &kind);
            assert!(!format!("{error:?}: {error}").contains("private"));
        }
    }
}

#[tokio::test]
async fn compression_corruption_and_clean_truncation_are_not_transport_evidence() {
    let valid = gzip(b"synthetic result");
    for (encoding, input) in [
        ("gzip", b"not-gzip".to_vec()),
        ("deflate", b"not-zlib".to_vec()),
        ("gzip", valid[..valid.len() - 3].to_vec()),
    ] {
        let mut headers = Headers::new();
        headers.insert("content-encoding", encoding);
        let source = stream::iter([Ok(Bytes::from(input))]);
        let mut body = decoded(Box::pin(source), &headers, true).unwrap();
        let error = loop {
            match body.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("corrupt compression was accepted"),
            }
        };
        assert_eq!(error.kind(), &ErrorKind::DataConversion);
    }
}

#[tokio::test]
async fn a_real_upstream_io_error_mid_compressed_header_is_not_decoder_corruption() {
    let encoded = gzip(b"synthetic result");
    let source = stream::iter([
        Ok(Bytes::copy_from_slice(&encoded[..5])),
        Err(azure_core::Error::with_message(
            ErrorKind::Io,
            "private-socket-error",
        )),
    ]);
    let mut headers = Headers::new();
    headers.insert("content-encoding", "gzip");
    let mut body = decoded(Box::pin(source), &headers, true).unwrap();
    let error = body.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::Io);
    assert!(!format!("{error:?}: {error}").contains("private"));
}

#[test]
fn error_category_is_typed_evidence_not_every_failure_or_message_text() {
    for (kind, expected) in [
        (io::ErrorKind::ConnectionReset, ErrorKind::Io),
        (io::ErrorKind::UnexpectedEof, ErrorKind::Io),
        (io::ErrorKind::TimedOut, ErrorKind::Io),
        (io::ErrorKind::InvalidData, ErrorKind::DataConversion),
        (io::ErrorKind::InvalidInput, ErrorKind::DataConversion),
        (io::ErrorKind::Other, ErrorKind::Other),
    ] {
        assert_eq!(
            transport_error_kind(&io::Error::new(kind, "private Io")),
            expected
        );
    }
    for kind in [ErrorKind::Io, ErrorKind::DataConversion, ErrorKind::Other] {
        let error = io::Error::other(azure_core::Error::with_message(kind.clone(), "private"));
        assert_eq!(transport_error_kind(&error), kind);
    }
    let tls = io::Error::new(
        io::ErrorKind::InvalidData,
        rustls::Error::General("private TLS".into()),
    );
    assert_eq!(transport_error_kind(&tls), ErrorKind::Other);
}

#[test]
fn livy_singleton_duplicates_are_rejected_before_case_insensitive_azure_erasure() {
    for route in [
        "/v1/workspaces/ws/lakehouses/l/livyApi/versions/v/sessions",
        "/v1/native",
    ] {
        for name in [
            "content-type",
            "content-encoding",
            "content-length",
            "retry-after",
            "location",
            "etag",
            "statementid",
            "id",
        ] {
            let mut request = request(Method::Get, route);
            if route == "/v1/native" {
                request.insert_header(
                    "accept",
                    "application/vnd.apache.arrow.stream, application/json;q=0.9",
                );
            }
            let mut raw = http::HeaderMap::new();
            let lower = http::header::HeaderName::from_bytes(name.as_bytes()).unwrap();
            let upper =
                http::header::HeaderName::from_bytes(name.to_ascii_uppercase().as_bytes()).unwrap();
            raw.append(lower, http::HeaderValue::from_static("first"));
            raw.append(upper, http::HeaderValue::from_static("second"));
            let error = response_headers(&request, &raw).unwrap_err();
            assert_eq!(error.kind(), &ErrorKind::DataConversion);
            assert!(!format!("{error:?} {error}").contains("first"));
        }
    }
}

#[test]
fn legitimate_repeated_non_singletons_and_generic_response_conversion_are_unchanged() {
    let mut raw = http::HeaderMap::new();
    raw.append("vary", http::HeaderValue::from_static("accept"));
    raw.append("vary", http::HeaderValue::from_static("origin"));
    raw.append("x-native", http::HeaderValue::from_static("one"));
    raw.append("x-native", http::HeaderValue::from_static("two"));
    raw.insert(
        "location",
        http::HeaderValue::from_static("/path/with,comma"),
    );
    let livy = request(Method::Get, "/v1/livyApi/versions/v/sessions");
    let fields = response_headers(&livy, &raw).unwrap();
    assert_eq!(fields.get_str(&"vary".into()).unwrap(), "accept, origin");
    assert_eq!(fields.get_str(&"x-native".into()).unwrap(), "one, two");
    assert_eq!(
        fields.get_str(&"location".into()).unwrap(),
        "/path/with,comma"
    );
    raw.append("etag", http::HeaderValue::from_static("first"));
    raw.append("etag", http::HeaderValue::from_static("second"));
    let generic = response_headers(&request(Method::Get, "/v1/workspaces"), &raw).unwrap();
    assert_eq!(generic.get_str(&"etag".into()).unwrap(), "first, second");
}

#[tokio::test]
async fn actual_hyper_parse_error_and_premature_header_eof_have_different_kinds() {
    for (reply, expected) in [
        (
            Reply::Raw(b"NOT-HTTP\r\n\r\n".to_vec()),
            ErrorKind::DataConversion,
        ),
        (Reply::Close, ErrorKind::Io),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![reply], Some(tls)).await;
        let error = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/v1/livyApi/versions/v/sessions"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), &expected);
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn real_raw_duplicate_livy_metadata_is_rejected_before_combining() {
    for (name, value) in [
        ("Content-Type", "one"),
        ("Content-Encoding", "one"),
        ("Content-Length", "0"),
        ("Retry-After", "one"),
        ("Location", "one"),
        ("ETag", "one"),
        ("statementId", "one"),
        ("id", "one"),
    ] {
        let (tls, root) = test_tls();
        let length = if name == "Content-Length" {
            ""
        } else {
            "Content-Length: 0\r\n"
        };
        let lower = name.to_ascii_lowercase();
        let raw = format!("HTTP/1.1 200 OK\r\n{name}: {value}\r\n{lower}: {value}\r\n{length}\r\n");
        let server = Server::start(vec![Reply::Raw(raw.into_bytes())], Some(tls)).await;
        let error = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/v1/livyApi/versions/v/sessions"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), &ErrorKind::DataConversion);
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn actual_chunk_protocol_errors_and_trailers_remain_data_conversion() {
    for raw in [
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\ninvalid\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: x-test\r\n\r\n0\r\nx-test: late\r\n\r\n",
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![Reply::Raw(raw.to_vec())], Some(tls)).await;
        let response = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/v1/livyApi/versions/v/sessions"))
            .await
            .unwrap();
        let error = response.into_body().next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), &ErrorKind::DataConversion);
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn actual_native_compression_failure_is_not_recoverable_stream_transport() {
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::body(200, b"not-gzip".to_vec()).headers(&[("Content-Encoding", "gzip")])],
        Some(tls),
    )
    .await;
    let http = super::executor(client(&server, vec![root]), Duration::from_secs(3));
    let context = RequestContext::new(
        Arc::new(super::SyntheticCredential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = NativeFabricStreamExecutor::new(context, http).unwrap();
    let request = NativeEndpoint::new(
        azure_core::http::Url::parse("https://api.fabric.microsoft.com/v1/livyApi/versions/v")
            .unwrap(),
    )
    .unwrap()
    .request(
        NativeMethod::Get,
        &["sessions"],
        &[],
        None,
        NativeEffect::Read,
    )
    .unwrap();
    let deadline = NativeDeadline::new(Duration::from_secs(3)).unwrap();
    let mut response = executor
        .open(
            &request,
            NativeStreamLimits {
                max_fragment_bytes: 64,
                max_body_bytes: 1024,
            },
            &deadline,
        )
        .await
        .unwrap();
    assert_eq!(
        response.body.read(64, &deadline).await,
        Err(NativeError::InvalidResponse)
    );
    assert_eq!(server.requests().len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn actual_native_mid_body_disconnect_is_transport_not_protocol_or_mutation_replay() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let (tls, root) = test_tls();
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nab".to_vec();
        let server = Server::start(vec![Reply::Raw(raw)], Some(tls)).await;
        let http = super::executor(client(&server, vec![root]), Duration::from_secs(3));
        let context = RequestContext::new(
            Arc::new(super::SyntheticCredential),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        );
        let executor = NativeFabricStreamExecutor::new(context, http).unwrap();
        let request = NativeEndpoint::new(
            azure_core::http::Url::parse("https://api.fabric.microsoft.com/v1/livyApi/versions/v")
                .unwrap(),
        )
        .unwrap()
        .request(
            method,
            &["sessions"],
            &[],
            None,
            if method == NativeMethod::Post {
                NativeEffect::Execute
            } else {
                NativeEffect::Read
            },
        )
        .unwrap();
        let deadline = NativeDeadline::new(Duration::from_secs(3)).unwrap();
        let mut response = executor
            .open(
                &request,
                NativeStreamLimits {
                    max_fragment_bytes: 64,
                    max_body_bytes: 1024,
                },
                &deadline,
            )
            .await
            .unwrap();
        assert_eq!(
            response.body.read(64, &deadline).await.unwrap(),
            Some(b"ab".to_vec())
        );
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
        assert_eq!(
            response.body.read(64, &deadline).await,
            Err(if method == NativeMethod::Post {
                NativeError::OutcomeUnknown
            } else {
                NativeError::Transport
            })
        );
    }
}
