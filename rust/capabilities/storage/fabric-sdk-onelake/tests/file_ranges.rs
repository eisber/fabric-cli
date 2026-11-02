// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline incremental OneLake consumer contracts; no ambient identity or network.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenRequestOptions};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeLimits,
    OneLakeMethod, OneLakePath, OneLakeRequest, OneLakeResponse, OneLakeWriteLimits,
    OneLakeWriteOutcome,
    file_ranges::{
        OneLakeChunkLimits, OneLakeFileCreateCondition, OneLakeFileUpload, OneLakeFileVersion,
    },
};
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::OneLake.scope()]);
        Ok(AccessToken::new(
            "offline-range-credential",
            azure_core::time::OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Recorded {
    method: OneLakeMethod,
    url: String,
    bytes: usize,
    headers: Vec<(String, String)>,
    response_bound: usize,
}

enum Reply {
    Response(OneLakeResponse),
    Error(OneLakeExecutionError),
    Pending,
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Recorded>>,
}

impl Executor {
    fn new(responses: impl IntoIterator<Item = OneLakeResponse>) -> Self {
        Self {
            replies: Mutex::new(responses.into_iter().map(Reply::Response).collect()),
            requests: Mutex::default(),
        }
    }

    fn push(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }
}

impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        assert_eq!(request.token().token.secret(), "offline-range-credential");
        assert!(!format!("{request:?}").contains("offline-range-credential"));
        self.requests.lock().unwrap().push(Recorded {
            method: request.method(),
            url: request.url().to_string(),
            bytes: request.body().len(),
            headers: request.headers().to_vec(),
            response_bound: request.max_response_bytes().get(),
        });
        match self.replies.lock().unwrap().pop_front().unwrap() {
            Reply::Response(response) => Box::pin(ready(Ok(response))),
            Reply::Error(error) => Box::pin(ready(Err(error))),
            Reply::Pending => Box::pin(pending()),
        }
    }
}

fn client() -> OneLakeClient {
    OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::OneLake).unwrap(),
    )
}

fn source() -> OneLakePath {
    OneLakePath::new("workspace", "item", "Files/source.bin").unwrap()
}

fn target() -> OneLakePath {
    OneLakePath::new("workspace", "item", "Files/target.bin").unwrap()
}

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn limits(bytes: usize) -> OneLakeChunkLimits {
    OneLakeChunkLimits {
        max_chunk_bytes: nz(bytes),
        max_response_bytes: nz(bytes),
    }
}

fn response(status: u16, body: Vec<u8>, headers: &[(&str, &str)]) -> OneLakeResponse {
    OneLakeResponse {
        status,
        body,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

fn properties(length: u64) -> OneLakeResponse {
    response(
        200,
        Vec::new(),
        &[
            ("Content-Length", &length.to_string()),
            ("ETag", "\"source-v1\""),
            ("Content-Type", "application/octet-stream"),
            ("x-ms-resource-type", "file"),
        ],
    )
}

fn created() -> OneLakeResponse {
    response(201, Vec::new(), &[("ETag", "\"target-v1\"")])
}

fn accepted() -> OneLakeResponse {
    response(202, Vec::new(), &[])
}

fn flushed() -> OneLakeResponse {
    response(200, Vec::new(), &[("ETag", "\"target-final\"")])
}

fn range(offset: u64, body: Vec<u8>, total: u64) -> OneLakeResponse {
    let count = body.len();
    response(
        206,
        body,
        &[
            ("Content-Length", &count.to_string()),
            (
                "Content-Range",
                &format!("bytes {offset}-{}/{total}", offset + count as u64 - 1),
            ),
            ("ETag", "\"source-v1\""),
        ],
    )
}

async fn version(executor: &Executor, length: u64) -> OneLakeFileVersion {
    executor.push(Reply::Response(properties(length)));
    client()
        .file_version(
            executor,
            &source(),
            OneLakeChunkLimits::default(),
            pending(),
        )
        .await
        .unwrap()
}

async fn upload(executor: &Executor, length: u64) -> OneLakeFileUpload {
    executor.push(Reply::Response(created()));
    client()
        .start_file_upload(
            executor,
            &target(),
            length,
            "application/octet-stream",
            OneLakeFileCreateCondition::IfAbsent,
            OneLakeChunkLimits::default(),
            pending(),
        )
        .await
        .unwrap()
}

fn header<'a>(request: &'a Recorded, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn file_above_four_mib_uses_bounded_ranges_and_positioned_appends_before_one_flush() {
    let chunk = 2 * 1024 * 1024;
    let total = (2 * chunk + 17) as u64;
    let executor = Executor::new([properties(total), created()]);
    let client = client();
    let limits = limits(chunk);
    let version = client
        .file_version(&executor, &source(), limits, pending())
        .await
        .unwrap();
    assert_eq!(version.len(), total);
    assert_eq!(version.etag(), "\"source-v1\"");
    assert_eq!(version.content_type(), Some("application/octet-stream"));
    let mut upload = client
        .start_file_upload(
            &executor,
            &target(),
            total,
            version.content_type().unwrap(),
            OneLakeFileCreateCondition::IfAbsent,
            limits,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(upload.len(), total);
    assert!(!upload.is_empty());
    for (offset, count, byte) in [
        (0, chunk, 1),
        (chunk as u64, chunk, 2),
        ((2 * chunk) as u64, 17, 3),
    ] {
        executor.push(Reply::Response(range(offset, vec![byte; count], total)));
        let part = client
            .read_range(
                &executor,
                &source(),
                &version,
                offset,
                nz(chunk),
                limits,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(part.offset, offset);
        assert_eq!(part.body.len(), count);
        assert!(part.body.iter().all(|value| *value == byte));
        assert_eq!(part.eof, offset + count as u64 == total);
        executor.push(Reply::Response(accepted()));
        assert_eq!(
            upload
                .append_at(&executor, offset, &part.body, pending())
                .await
                .unwrap(),
            OneLakeWriteOutcome::Accepted
        );
        assert_eq!(upload.position(), offset + count as u64);
        assert!(!upload.is_complete());
        assert!(
            executor
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| !request.url.contains("action=flush"))
        );
    }
    executor.push(Reply::Response(flushed()));
    assert_eq!(
        upload.flush(&executor, pending()).await.unwrap(),
        OneLakeWriteOutcome::Completed { status: 200 }
    );
    assert!(upload.is_complete());
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    assert_eq!(requests[0].method, OneLakeMethod::Head);
    assert_eq!(requests[1].method, OneLakeMethod::Put);
    assert_eq!(header(&requests[1], "If-None-Match"), Some("*"));
    for (index, offset, count) in [(2, 0, chunk), (4, chunk, chunk), (6, 2 * chunk, 17)] {
        let read = &requests[index];
        let append = &requests[index + 1];
        assert_eq!(read.method, OneLakeMethod::Get);
        assert_eq!(read.bytes, 0);
        assert_eq!(header(read, "If-Match"), Some("\"source-v1\""));
        assert_eq!(header(read, "Accept-Encoding"), Some("identity"));
        assert_eq!(
            header(read, "Range"),
            Some(format!("bytes={offset}-{}", offset + count - 1).as_str())
        );
        assert_eq!(append.method, OneLakeMethod::Patch);
        assert!(
            append
                .url
                .ends_with(&format!("action=append&position={offset}"))
        );
        assert_eq!(append.bytes, count);
        assert_eq!(header(append, "If-Match"), Some("\"target-v1\""));
    }
    assert!(
        requests[8]
            .url
            .ends_with(&format!("action=flush&position={total}&close=true"))
    );
    assert_eq!(requests[8].bytes, 0);
    assert!(
        requests
            .iter()
            .all(|request| request.response_bound == chunk)
    );
    assert!(requests.iter().all(|request| request.bytes <= chunk));
}

#[tokio::test]
async fn malformed_properties_never_produce_a_snapshot() {
    let cases: Vec<(&str, Option<&str>)> = vec![
        ("Content-Length", None),
        ("Content-Length", Some("-1")),
        ("Content-Length", Some("1 ")),
        ("Content-Length", Some("18446744073709551616")),
        ("ETag", None),
        ("ETag", Some("unquoted")),
        ("ETag", Some("W/\"weak\"")),
        ("ETag", Some("\"contains\rnewline\"")),
        ("Content-Type", Some("bad\nvalue")),
        ("x-ms-resource-type", Some("directory")),
        ("Content-Encoding", Some("gzip")),
    ];
    for (name, value) in cases {
        let mut reply = properties(10);
        if let Some(value) = value {
            reply.headers.insert(name.into(), value.into());
        } else {
            reply.headers.remove(name);
        }
        let executor = Executor::new([reply]);
        assert_eq!(
            client()
                .file_version(
                    &executor,
                    &source(),
                    OneLakeChunkLimits::default(),
                    pending()
                )
                .await,
            Err(OneLakeError::InvalidResponse),
            "{name}: {value:?}"
        );
    }
    for (name, value) in [
        ("content-length", "10"),
        ("etag", "\"source-v1\""),
        ("CONTENT-TYPE", "application/octet-stream"),
    ] {
        let mut reply = properties(10);
        reply.headers.insert(name.into(), value.into());
        let executor = Executor::new([reply]);
        assert!(
            client()
                .file_version(
                    &executor,
                    &source(),
                    OneLakeChunkLimits::default(),
                    pending()
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn nonempty_or_non_200_head_is_invalid() {
    let mut with_body = properties(1);
    with_body.body.push(1);
    let mut accepted_head = properties(0);
    accepted_head.status = 202;
    for reply in [with_body, accepted_head] {
        assert_eq!(
            client()
                .file_version(
                    &Executor::new([reply]),
                    &source(),
                    OneLakeChunkLimits::default(),
                    pending()
                )
                .await,
            Err(OneLakeError::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn malformed_ranges_changed_etags_and_truncated_bodies_never_report_bytes() {
    let executor = Executor::default();
    let version = version(&executor, 8).await;
    let valid = || range(2, vec![1; 3], 8);
    let mut cases = Vec::new();
    for (name, value) in [
        ("Content-Range", "bytes 1-3/8"),
        ("Content-Range", "bytes 2-5/8"),
        ("Content-Range", "bytes 2-4/9"),
        ("Content-Range", "bytes */8"),
        ("Content-Range", "bytes 2-4/*"),
        ("Content-Range", "bytes 4-2/8"),
        ("Content-Range", "bytes 2-4/4"),
        ("Content-Range", "bytes 2-4/8 trailing"),
        ("Content-Range", "bytes 2-4/18446744073709551616"),
        ("Content-Length", "2"),
        ("Content-Length", "+3"),
        ("ETag", "\"changed\""),
        ("ETag", "W/\"source-v1\""),
        ("Content-Encoding", "gzip"),
        ("content-range", "bytes 2-4/8"),
    ] {
        let mut reply = valid();
        reply.headers.insert(name.into(), value.into());
        cases.push(reply);
    }
    for name in ["Content-Length", "Content-Range", "ETag"] {
        let mut reply = valid();
        reply.headers.remove(name);
        cases.push(reply);
    }
    for body in [vec![1; 2], vec![1; 4]] {
        let mut reply = valid();
        reply.body = body;
        cases.push(reply);
    }
    let mut ignored = valid();
    ignored.status = 200;
    cases.push(ignored);
    for reply in cases {
        executor.push(Reply::Response(reply));
        assert_eq!(
            client()
                .read_range(
                    &executor,
                    &source(),
                    &version,
                    2,
                    nz(3),
                    limits(8),
                    pending()
                )
                .await,
            Err(OneLakeError::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn zero_bytes_and_exact_eof_need_no_get_but_empty_upload_still_flushes() {
    let executor = Executor::default();
    let version = version(&executor, 0).await;
    assert!(version.is_empty());
    let empty = client()
        .read_range(
            &executor,
            &source(),
            &version,
            0,
            nz(4),
            limits(4),
            pending(),
        )
        .await
        .unwrap();
    assert!(empty.body.is_empty());
    assert!(empty.eof);
    let mut upload = upload(&executor, 0).await;
    assert!(upload.is_empty());
    assert!(!upload.is_complete());
    assert_eq!(
        upload
            .append_at(&executor, 0, &[], pending())
            .await
            .unwrap_err()
            .kind,
        OneLakeError::InvalidInput
    );
    executor.push(Reply::Response(flushed()));
    upload.flush(&executor, pending()).await.unwrap();
    assert!(upload.is_complete());
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
    assert_eq!(
        upload.flush(&executor, pending()).await.unwrap_err().kind,
        OneLakeError::InvalidInput
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn invalid_bounds_offsets_or_version_path_never_dispatch_reads() {
    let executor = Executor::default();
    let version = version(&executor, 8).await;
    let client = client();
    for (path, offset, count, bounds, expected) in [
        (source(), 9, 1, limits(4), OneLakeError::InvalidInput),
        (target(), 0, 1, limits(4), OneLakeError::InvalidInput),
        (source(), 0, 5, limits(4), OneLakeError::Limit),
        (
            source(),
            0,
            1,
            limits(4 * 1024 * 1024 + 1),
            OneLakeError::Limit,
        ),
        (
            source(),
            0,
            3,
            OneLakeChunkLimits {
                max_chunk_bytes: nz(4),
                max_response_bytes: nz(2),
            },
            OneLakeError::Limit,
        ),
    ] {
        assert_eq!(
            client
                .read_range(
                    &executor,
                    &path,
                    &version,
                    offset,
                    nz(count),
                    bounds,
                    pending(),
                )
                .await,
            Err(expected)
        );
    }
    let eof = client
        .read_range(
            &executor,
            &source(),
            &version,
            8,
            nz(4),
            limits(4),
            pending(),
        )
        .await
        .unwrap();
    assert!(eof.eof && eof.body.is_empty());
    assert_eq!(
        client
            .read_range(
                &executor,
                &source(),
                &version,
                8,
                nz(4),
                limits(4),
                ready(()),
            )
            .await,
        Err(OneLakeError::Cancelled)
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn last_byte_of_u64_length_does_not_overflow_or_truncate() {
    let executor = Executor::default();
    let version = version(&executor, u64::MAX).await;
    executor.push(Reply::Response(range(u64::MAX - 1, vec![7], u64::MAX)));
    let part = client()
        .read_range(
            &executor,
            &source(),
            &version,
            u64::MAX - 1,
            nz(4),
            limits(4),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(part.body, [7]);
    assert!(part.eof);
}

#[tokio::test]
async fn eof_still_requires_explicit_onelake_selection() {
    let executor = Executor::default();
    let version = version(&executor, 0).await;
    let wrong_audience = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
    );
    assert_eq!(
        wrong_audience
            .read_range(
                &executor,
                &source(),
                &version,
                0,
                nz(4),
                limits(4),
                pending(),
            )
            .await,
        Err(OneLakeError::InvalidAudience)
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn positioned_writes_validate_locally_and_propagate_destination_conditions() {
    let executor = Executor::new([created()]);
    let mut upload = client()
        .start_file_upload(
            &executor,
            &target(),
            3,
            "application/octet-stream",
            OneLakeFileCreateCondition::IfMatch("\"previous-target\"".into()),
            limits(2),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        header(&executor.requests.lock().unwrap()[0], "If-Match"),
        Some("\"previous-target\"")
    );
    assert!(upload.flush(&executor, pending()).await.is_err());
    for (position, bytes, expected) in [
        (1, &[1][..], OneLakeError::InvalidInput),
        (u64::MAX, &[1][..], OneLakeError::InvalidInput),
        (0, &[][..], OneLakeError::InvalidInput),
        (0, &[1, 2, 3][..], OneLakeError::Limit),
    ] {
        assert_eq!(
            upload
                .append_at(&executor, position, bytes, pending())
                .await
                .unwrap_err()
                .kind,
            expected
        );
    }
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    executor.push(Reply::Response(response(
        202,
        Vec::new(),
        &[("ETag", "\"target-v2\"")],
    )));
    upload
        .append_at(&executor, 0, &[1, 2], pending())
        .await
        .unwrap();
    assert!(!upload.is_complete());
    assert!(
        upload
            .append_at(&executor, 1, &[3], pending())
            .await
            .is_err()
    );
    assert!(
        upload
            .append_at(&executor, 2, &[3, 4], pending())
            .await
            .is_err()
    );
    executor.push(Reply::Response(accepted()));
    upload
        .append_at(&executor, 2, &[3], pending())
        .await
        .unwrap();
    executor.push(Reply::Response(flushed()));
    upload.flush(&executor, pending()).await.unwrap();
    assert!(upload.is_complete());
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(header(&requests[2], "If-Match"), Some("\"target-v2\""));
    assert_eq!(header(&requests[3], "If-Match"), Some("\"target-v2\""));
}

#[tokio::test]
async fn invalid_create_policy_and_cancellation_have_no_side_effects() {
    let executor = Executor::default();
    for condition in [
        OneLakeFileCreateCondition::IfMatch("*".into()),
        OneLakeFileCreateCondition::IfMatch("W/\"weak\"".into()),
        OneLakeFileCreateCondition::IfMatch("\"injected\r\nheader\"".into()),
    ] {
        let failure = client()
            .start_file_upload(
                &executor,
                &target(),
                u64::MAX,
                "application/octet-stream",
                condition,
                limits(4),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(failure.kind, OneLakeError::InvalidInput);
        assert!(!failure.mutation_may_have_occurred);
    }
    let failure = client()
        .start_file_upload(
            &executor,
            &target(),
            1,
            "application/octet-stream",
            OneLakeFileCreateCondition::IfAbsent,
            limits(4),
            ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.kind, OneLakeError::Cancelled);
    assert!(!failure.mutation_may_have_occurred);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn accepted_or_malformed_create_is_not_a_usable_upload() {
    for reply in [
        accepted(),
        response(200, Vec::new(), &[("ETag", "\"target-v1\"")]),
        response(201, Vec::new(), &[]),
        response(201, Vec::new(), &[("ETag", "W/\"weak\"")]),
        response(201, vec![1], &[("ETag", "\"target-v1\"")]),
    ] {
        let status = reply.status;
        let executor = Executor::new([reply]);
        let error = client()
            .start_file_upload(
                &executor,
                &target(),
                1,
                "application/octet-stream",
                OneLakeFileCreateCondition::IfAbsent,
                limits(4),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, OneLakeError::InvalidResponse);
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.response.unwrap().status, status);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn accepted_final_flush_is_incomplete_and_cannot_be_replayed() {
    let executor = Executor::default();
    let mut upload = upload(&executor, 1).await;
    executor.push(Reply::Response(accepted()));
    upload
        .append_at(&executor, 0, &[1], pending())
        .await
        .unwrap();
    assert!(!upload.is_complete());
    executor.push(Reply::Response(accepted()));
    let failure = upload.flush(&executor, pending()).await.unwrap_err();
    assert_eq!(failure.kind, OneLakeError::InvalidResponse);
    assert_eq!(failure.acknowledged_position, 1);
    assert!(failure.mutation_may_have_occurred);
    assert_eq!(failure.response.unwrap().status, 202);
    assert!(!upload.is_complete());
    assert!(upload.flush(&executor, pending()).await.is_err());
    assert!(
        upload
            .append_at(&executor, 1, &[2], pending())
            .await
            .is_err()
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn lost_create_or_flush_response_retains_uncertainty_without_replay() {
    let executor = Executor::default();
    executor.push(Reply::Error(OneLakeExecutionError::TimedOut));
    let failure = client()
        .start_file_upload(
            &executor,
            &target(),
            0,
            "application/octet-stream",
            OneLakeFileCreateCondition::IfAbsent,
            limits(4),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        failure.kind,
        OneLakeError::Execution(OneLakeExecutionError::TimedOut)
    );
    assert!(failure.mutation_may_have_occurred);
    assert!(failure.response.is_none());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);

    let mut upload = upload(&executor, 0).await;
    executor.push(Reply::Error(OneLakeExecutionError::TimedOut));
    let failure = upload.flush(&executor, pending()).await.unwrap_err();
    assert_eq!(
        failure.kind,
        OneLakeError::Execution(OneLakeExecutionError::TimedOut)
    );
    assert!(failure.mutation_may_have_occurred);
    assert!(failure.response.is_none());
    assert!(!upload.is_complete());
    assert!(upload.flush(&executor, pending()).await.is_err());
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn failed_append_retains_acknowledged_position_service_evidence_and_never_flushes() {
    let executor = Executor::default();
    let mut upload = upload(&executor, 3).await;
    executor.push(Reply::Response(accepted()));
    upload
        .append_at(&executor, 0, &[1], pending())
        .await
        .unwrap();
    executor.push(Reply::Response(response(
        412,
        br#"{"error":{"code":"ConditionNotMet","message":"untrusted service detail"}}"#.to_vec(),
        &[("x-ms-error-code", "ConditionNotMet")],
    )));
    let failure = upload
        .append_at(&executor, 1, &[2], pending())
        .await
        .unwrap_err();
    assert_eq!(failure.acknowledged_position, 1);
    assert_eq!(upload.position(), 1);
    assert!(failure.mutation_may_have_occurred);
    assert!(failure.response.is_none());
    match &failure.kind {
        OneLakeError::Http {
            status,
            code,
            raw_service_error,
        } => {
            assert_eq!(*status, 412);
            assert_eq!(code.as_deref(), Some("ConditionNotMet"));
            assert_eq!(
                raw_service_error.as_ref().unwrap()["error"]["code"],
                json!("ConditionNotMet")
            );
        }
        error => panic!("unexpected failure: {error:?}"),
    }
    assert!(!failure.to_string().contains("untrusted"));
    assert!(
        upload
            .append_at(&executor, 1, &[2], pending())
            .await
            .is_err()
    );
    assert!(upload.flush(&executor, pending()).await.is_err());
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn malformed_append_response_poisons_upload_without_advancing_position() {
    for reply in [
        response(200, Vec::new(), &[]),
        response(202, vec![1], &[]),
        response(202, Vec::new(), &[("Content-Length", "1")]),
        response(202, Vec::new(), &[("ETag", "invalid")]),
    ] {
        let executor = Executor::default();
        let mut upload = upload(&executor, 1).await;
        executor.push(Reply::Response(reply));
        let failure = upload
            .append_at(&executor, 0, &[1], pending())
            .await
            .unwrap_err();
        assert_eq!(failure.kind, OneLakeError::InvalidResponse);
        assert!(failure.response.is_some());
        assert_eq!(failure.acknowledged_position, 0);
        assert_eq!(upload.position(), 0);
        assert!(upload.flush(&executor, pending()).await.is_err());
        assert_eq!(executor.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn execution_failures_remain_typed_for_reads_and_uncertain_writes_without_retries() {
    for error in [
        OneLakeExecutionError::Failed,
        OneLakeExecutionError::TimedOut,
        OneLakeExecutionError::Cancelled,
        OneLakeExecutionError::RetriesExhausted,
        OneLakeExecutionError::ResponseLimit,
    ] {
        let executor = Executor::default();
        let version = version(&executor, 1).await;
        executor.push(Reply::Error(error));
        assert_eq!(
            client()
                .read_range(
                    &executor,
                    &source(),
                    &version,
                    0,
                    nz(1),
                    limits(4),
                    pending()
                )
                .await,
            Err(OneLakeError::Execution(error))
        );
        let mut upload = upload(&executor, 1).await;
        executor.push(Reply::Error(error));
        let failure = upload
            .append_at(&executor, 0, &[1], pending())
            .await
            .unwrap_err();
        assert_eq!(failure.kind, OneLakeError::Execution(error));
        assert!(failure.mutation_may_have_occurred);
        assert_eq!(failure.acknowledged_position, 0);
        assert!(
            upload
                .append_at(&executor, 0, &[1], pending())
                .await
                .is_err()
        );
        assert!(upload.flush(&executor, pending()).await.is_err());
        assert_eq!(executor.requests.lock().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn cancellation_during_append_leaves_uncertainty_and_never_flushes() {
    let executor = Executor::default();
    let mut upload = upload(&executor, 1).await;
    executor.push(Reply::Pending);
    let cancelled = std::future::poll_fn(|context| {
        if executor.requests.lock().unwrap().len() == 2 {
            Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let failure = upload
        .append_at(&executor, 0, &[1], cancelled)
        .await
        .unwrap_err();
    assert_eq!(failure.kind, OneLakeError::Cancelled);
    assert!(failure.mutation_may_have_occurred);
    assert!(!upload.is_complete());
    assert!(upload.flush(&executor, pending()).await.is_err());
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dropping_dispatched_append_future_blocks_same_position_retry_and_flush() {
    let executor = Executor::default();
    let mut upload = upload(&executor, 1).await;
    executor.push(Reply::Pending);
    let content = [1];
    let mut append = Box::pin(upload.append_at(&executor, 0, &content, pending()));
    let mut context = Context::from_waker(Waker::noop());
    assert!(append.as_mut().poll(&mut context).is_pending());
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
    drop(append);

    assert_eq!(upload.position(), 0);
    assert!(!upload.is_complete());
    let failure = upload
        .append_at(&executor, 0, &content, pending())
        .await
        .unwrap_err();
    assert_eq!(failure.kind, OneLakeError::InvalidInput);
    assert!(failure.mutation_may_have_occurred);
    assert_eq!(failure.acknowledged_position, 0);
    assert!(upload.flush(&executor, pending()).await.is_err());
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dropping_dispatched_flush_future_blocks_another_flush_without_dispatch() {
    let executor = Executor::default();
    let mut upload = upload(&executor, 1).await;
    executor.push(Reply::Response(accepted()));
    upload
        .append_at(&executor, 0, &[1], pending())
        .await
        .unwrap();
    executor.push(Reply::Pending);
    let mut flush = Box::pin(upload.flush(&executor, pending()));
    let mut context = Context::from_waker(Waker::noop());
    assert!(flush.as_mut().poll(&mut context).is_pending());
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
    drop(flush);

    assert_eq!(upload.position(), 1);
    assert!(!upload.is_complete());
    let failure = upload.flush(&executor, pending()).await.unwrap_err();
    assert_eq!(failure.kind, OneLakeError::InvalidInput);
    assert!(failure.mutation_may_have_occurred);
    assert_eq!(failure.acknowledged_position, 1);
    assert!(
        upload
            .append_at(&executor, 1, &[2], pending())
            .await
            .is_err()
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn range_source_conflict_retains_typed_remote_error_without_bytes() {
    let executor = Executor::default();
    let version = version(&executor, 1).await;
    executor.push(Reply::Response(response(
        412,
        br#"{"error":{"code":"ConditionNotMet"}}"#.to_vec(),
        &[("x-ms-error-code", "ConditionNotMet")],
    )));
    let error = client()
        .read_range(
            &executor,
            &source(),
            &version,
            0,
            nz(1),
            limits(128),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        OneLakeError::Http {
            status: 412,
            code: Some(ref code),
            raw_service_error: Some(_)
        } if code == "ConditionNotMet"
    ));
}

#[tokio::test]
async fn buffered_api_bounds_and_rejection_of_partial_content_are_unchanged() {
    let executor = Executor::default();
    let failure = client()
        .upload_file(
            &executor,
            &target(),
            &vec![1; 4 * 1024 * 1024 + 1],
            "application/octet-stream",
            OneLakeWriteLimits {
                max_request_bytes: nz(8 * 1024 * 1024),
                max_response_bytes: nz(8 * 1024 * 1024),
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.kind, OneLakeError::Limit);
    assert!(executor.requests.lock().unwrap().is_empty());
    executor.push(Reply::Response(range(0, vec![1], 1)));
    assert_eq!(
        client()
            .read_file(&executor, &source(), OneLakeLimits::default(), pending())
            .await
            .unwrap_err(),
        OneLakeError::InvalidResponse
    );
}
