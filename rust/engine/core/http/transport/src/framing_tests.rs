// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native TLS wire capture with synthetic credentials and a length-requiring peer.

#![deny(unnameable_test_items)]

use super::{ITEM, WORKSPACE, bytes, client, executor, request, test_tls};
use crate::test_server::{Reply, Server};
use azure_core::{
    credentials::AccessToken,
    http::{HttpClient, Method, Url},
    time::OffsetDateTime,
};
use serde_json::json;
use std::{num::NonZeroUsize, time::Duration};

fn header<'a>(head: &'a str, expected: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expected).then(|| value.trim())
    })
}

#[tokio::test]
async fn empty_notebook_post_has_explicit_zero_length_on_native_wire() {
    let (tls, root) = test_tls();
    let server = Server::start(vec![Reply::RequireContentLength], Some(tls)).await;
    let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
    let response = exec
        .execute_fabric(
            Method::Post,
            &Url::parse(&format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/notebooks/{ITEM}/jobs/execute/instances?beta=false"
            ))
            .unwrap(),
            &AccessToken::new("synthetic-framing-only", OffsetDateTime::UNIX_EPOCH),
            &[],
            NonZeroUsize::new(1024).unwrap(),
        )
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "mutation must not replay");
    let wire = std::str::from_utf8(&requests[0]).unwrap();
    let (head, body) = wire.split_once("\r\n\r\n").unwrap();
    let capture = json!({
        "event": "native_http_wire_capture",
        "peer": "loopback_tls",
        "method": "POST",
        "route": "notebook_execute",
        "requestBodyBytes": body.len(),
        "contentLength": header(head, "content-length"),
        "transferEncoding": header(head, "transfer-encoding"),
        "httpStatus": response.status,
        "requestCount": requests.len()
    });
    eprintln!("{capture}");
    server.stop().await;
    assert_eq!(response.status, 202, "{capture}");
    assert_eq!(header(head, "content-length"), Some("0"));
    assert_eq!(header(head, "transfer-encoding"), None);
    assert!(body.is_empty());
}

#[tokio::test]
async fn explicit_json_object_and_empty_read_framing_are_preserved() {
    for (method, body, expected_length) in [
        (Method::Post, b"{}".as_slice(), Some("2")),
        (Method::Get, b"".as_slice(), None),
        (Method::Head, b"".as_slice(), None),
        (Method::Delete, b"".as_slice(), None),
        (Method::Put, b"".as_slice(), None),
        (Method::Patch, b"".as_slice(), None),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![Reply::body(200, Vec::new())], Some(tls)).await;
        let mut pending = request(method, "/synthetic");
        pending.set_body(body.to_vec());
        let response = client(&server, vec![root])
            .execute_request(&pending)
            .await
            .unwrap();
        assert!(bytes(response).await.is_empty());
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let wire = std::str::from_utf8(&requests[0]).unwrap();
        let (head, actual_body) = wire.split_once("\r\n\r\n").unwrap();
        assert_eq!(header(head, "content-length"), expected_length);
        assert_eq!(actual_body.as_bytes(), body);
        server.stop().await;
    }
}

#[tokio::test]
async fn empty_post_does_not_add_framing_over_explicit_caller_headers() {
    for (name, value, expected_length) in [
        ("Content-Length", "0", Some("0")),
        ("Transfer-Encoding", "chunked", None),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![Reply::body(200, Vec::new())], Some(tls)).await;
        let mut pending = request(Method::Post, "/synthetic");
        pending.insert_header(name.to_owned(), value.to_owned());
        let response = client(&server, vec![root])
            .execute_request(&pending)
            .await
            .unwrap();
        assert!(bytes(response).await.is_empty());
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let wire = std::str::from_utf8(&requests[0]).unwrap();
        let (head, body) = wire.split_once("\r\n\r\n").unwrap();
        assert_eq!(header(head, "content-length"), expected_length);
        let lengths = head
            .lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            })
            .count();
        assert_eq!(lengths, usize::from(expected_length.is_some()));
        assert!(body.is_empty());
        server.stop().await;
    }
}

#[tokio::test]
async fn length_rejection_never_replays_an_empty_post() {
    let (tls, root) = test_tls();
    let server = Server::start(vec![Reply::body(411, Vec::new())], Some(tls)).await;
    let response = executor(client(&server, vec![root]), Duration::from_secs(3))
        .execute_fabric(
            Method::Post,
            &Url::parse("https://api.fabric.microsoft.com/synthetic").unwrap(),
            &AccessToken::new("synthetic-framing-only", OffsetDateTime::UNIX_EPOCH),
            &[],
            NonZeroUsize::new(1024).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 411);
    assert_eq!(server.requests().len(), 1);
    server.stop().await;
}
