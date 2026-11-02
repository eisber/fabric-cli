// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic-only OneLake consumer contracts, including the table dependency seam.

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenRequestOptions};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeLimits,
    OneLakeMethod, OneLakePath, OneLakeRequest, OneLakeResponse,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct SyntheticCredential;

#[async_trait]
impl TokenCredential for SyntheticCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::OneLake.scope()]);
        Ok(AccessToken::new(
            "synthetic-offline-only",
            azure_core::time::OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor {
    requests: Mutex<Vec<(OneLakeMethod, String)>>,
    responses: Mutex<VecDeque<OneLakeResponse>>,
}

impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        assert_eq!(request.token().token.secret(), "synthetic-offline-only");
        assert!(!format!("{request:?}").contains("synthetic-offline-only"));
        self.requests
            .lock()
            .unwrap()
            .push((request.method(), request.url().to_string()));
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(std::future::ready(Ok(response)))
    }
}

fn client(audience: Audience) -> OneLakeClient {
    OneLakeClient::new(
        Arc::new(SyntheticCredential),
        CredentialSelection::new("synthetic-account", "synthetic-tenant", audience).unwrap(),
    )
}

fn response(body: &[u8], status: u16) -> OneLakeResponse {
    OneLakeResponse {
        status,
        body: body.to_vec(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn table_dependency_lists_delta_log_and_reads_exact_json_bytes() {
    let executor = Executor {
        responses: Mutex::new(VecDeque::from([
            response(br#"{"paths":[{"name":"item/Tables/t/_delta_log/00001.json","isDirectory":"false","contentLength":"12","extension":{"arbitrary":true}}]}"#, 200),
            response(br#"{"metaData":{"schemaString":"{\"type\":\"struct\"}"}}"#, 200),
        ])),
        ..Executor::default()
    };
    let path = OneLakePath::new("workspace", "item", "Tables/t/_delta_log").unwrap();
    let rows = client(Audience::OneLake)
        .list(&executor, &path, false, OneLakeLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(rows.pages_received, 1);
    assert_eq!(rows.paths[0]["extension"], json!({"arbitrary": true}));
    let content = client(Audience::OneLake)
        .read(
            &executor,
            &OneLakePath::new("workspace", "item", "Tables/t/_delta_log/00001.json").unwrap(),
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        content,
        br#"{"metaData":{"schemaString":"{\"type\":\"struct\"}"}}"#
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests[0], (OneLakeMethod::Get, "https://onelake.dfs.fabric.microsoft.com/workspace/?recursive=false&resource=filesystem&directory=item%2FTables%2Ft%2F_delta_log&getShortcutMetadata=true".into()));
    assert_eq!(
        requests[1].1,
        "https://onelake.dfs.fabric.microsoft.com/workspace/item/Tables/t/_delta_log/00001.json"
    );
}

#[tokio::test]
async fn properties_uses_head_and_keeps_source_headers() {
    let mut head = response(&[], 200);
    head.headers.insert("Content-Length".into(), "42".into());
    head.headers.insert("ETag".into(), "synthetic".into());
    let executor = Executor {
        responses: Mutex::new(VecDeque::from([head])),
        ..Executor::default()
    };
    let headers = client(Audience::OneLake)
        .properties(
            &executor,
            &OneLakePath::new("ws", "item", "Files/a b.txt").unwrap(),
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(headers["Content-Length"], "42");
    assert_eq!(
        executor.requests.lock().unwrap()[0],
        (
            OneLakeMethod::Head,
            "https://onelake.dfs.fabric.microsoft.com/ws/item/Files/a%20b.txt".into()
        )
    );
}

#[tokio::test]
async fn source_continuation_spelling_and_raw_order_are_preserved() {
    let mut first = response(br#"{"paths":[{"name":"first"}]}"#, 200);
    first
        .headers
        .insert("X-MS-Continuation".into(), "opaque+/=".into());
    let executor = Executor {
        responses: Mutex::new(VecDeque::from([
            first,
            response(br#"{"paths":[{"name":"second"}]}"#, 200),
        ])),
        ..Executor::default()
    };
    let result = client(Audience::OneLake)
        .list(
            &executor,
            &OneLakePath::new("ws", "item", "Files").unwrap(),
            false,
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.paths,
        [json!({"name":"first"}), json!({"name":"second"})]
    );
    assert_eq!(result.pages_received, 2);
    assert!(
        executor.requests.lock().unwrap()[1]
            .1
            .ends_with("&continuationToken=opaque%2B%2F%3D")
    );
}

#[tokio::test]
async fn forbidden_is_not_missing_and_only_real_404_is_missing() {
    for status in [403, 404] {
        let mut failure = response(b"not JSON", status);
        failure
            .headers
            .insert("x-ms-error-code".into(), "StorageError".into());
        let executor = Executor {
            responses: Mutex::new(VecDeque::from([failure])),
            ..Executor::default()
        };
        assert_eq!(
            client(Audience::OneLake)
                .read(
                    &executor,
                    &OneLakePath::new("ws", "item", "Files/file").unwrap(),
                    OneLakeLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err(),
            OneLakeError::Http {
                status,
                code: Some("StorageError".into()),
                raw_service_error: None,
            }
        );
    }
}

#[tokio::test]
async fn cancellation_and_wrong_audience_do_not_dispatch() {
    let executor = Executor::default();
    let path = OneLakePath::new("ws", "item", "Files").unwrap();
    assert_eq!(
        client(Audience::OneLake)
            .read(
                &executor,
                &path,
                OneLakeLimits::default(),
                std::future::ready(())
            )
            .await
            .unwrap_err(),
        OneLakeError::Cancelled
    );
    assert_eq!(
        client(Audience::Fabric)
            .read(&executor, &path, OneLakeLimits::default(), pending())
            .await
            .unwrap_err(),
        OneLakeError::InvalidAudience
    );
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn traversal_bounds_and_malformed_rows_never_become_successful_absence() {
    for body in [
        br#"{}"#.as_slice(),
        br#"{"paths":[{}]}"#,
        br#"{"paths":"invalid"}"#,
    ] {
        let executor = Executor {
            responses: Mutex::new(VecDeque::from([response(body, 200)])),
            ..Executor::default()
        };
        assert_eq!(
            client(Audience::OneLake)
                .list(
                    &executor,
                    &OneLakePath::new("ws", "item", "Tables").unwrap(),
                    false,
                    OneLakeLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err(),
            OneLakeError::InvalidResponse
        );
    }
    let executor = Executor {
        responses: Mutex::new(VecDeque::from([response(b"too long", 200)])),
        ..Executor::default()
    };
    assert_eq!(
        client(Audience::OneLake)
            .read(
                &executor,
                &OneLakePath::new("ws", "item", "Files/file").unwrap(),
                OneLakeLimits {
                    max_response_bytes: NonZeroUsize::new(1).unwrap(),
                    ..OneLakeLimits::default()
                },
                pending(),
            )
            .await
            .unwrap_err(),
        OneLakeError::Limit
    );
}

#[test]
fn origin_and_path_inputs_are_not_arbitrary_urls() {
    for relative in [
        "../secret",
        "/absolute",
        "Files/../secret",
        "Files//file",
        "Files/%2e%2e/file",
        "https://host/file",
        "Files\\file",
        "Files/a?query",
    ] {
        assert_eq!(
            OneLakePath::new("ws", "item", relative).unwrap_err(),
            OneLakeError::InvalidInput
        );
    }
    assert!(OneLakePath::new("ws", "item", "Tables/_delta_log/00000.json").is_ok());
    assert!(OneLakePath::new("ws", "item", "").is_ok());
}
