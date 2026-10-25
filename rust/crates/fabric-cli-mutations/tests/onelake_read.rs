// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CLI OneLake dependency reads with explicit credentials, responses and outputs.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_mutations::{OneLakeReadError, OneLakeReadHost};
use fabric_cli_output::{Indent, OutputEnvelope, Timestamp};
use fabric_sdk_http::PublicOrigin;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeLimits,
    OneLakePath, OneLakeRequest, OneLakeResponse,
};
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, Mutex},
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
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        Ok(AccessToken::new(
            "offline-cli-onelake",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<OneLakeResponse>>,
    requests: Mutex<Vec<String>>,
}
impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        self.requests
            .lock()
            .unwrap()
            .push(request.url().to_string());
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected OneLake request");
        Box::pin(std::future::ready(Ok(response)))
    }
}

fn client() -> OneLakeClient {
    OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience()).unwrap(),
    )
}
fn path() -> OneLakePath {
    OneLakePath::new("workspace", "item", "Files/link.Shortcut").unwrap()
}
fn response(status: u16, data: Value) -> OneLakeResponse {
    OneLakeResponse {
        status,
        body: serde_json::to_vec(&data).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn executor(responses: Vec<OneLakeResponse>) -> Executor {
    Executor {
        responses: Mutex::new(responses.into()),
        ..Executor::default()
    }
}

#[tokio::test]
async fn list_uses_existing_query_on_mutated_complete_rows_not_column_projection() {
    let client = client();
    let executor = executor(vec![response(
        200,
        json!({"paths":[
            {"name":"item/Files/link/file","isDirectory":false,"future":{"n":2}},
            {"name":"item/Files/link/folder","isDirectory":"false","future":{"n":1}},
        ]}),
    )]);
    let host = OneLakeReadHost {
        client: &client,
        executor: &executor,
        limits: OneLakeLimits::default(),
    };
    let result = host
        .list(
            &path(),
            false,
            Some("[?type == 'Directory'].future.n"),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.output.data, Some(json!([1])));
    assert!(!result.show_headers);
    assert!(executor.requests.lock().unwrap()[0].contains("directory=item%2FFiles%2Flink&"));
}

#[tokio::test]
async fn scalar_query_keeps_python_stringification_and_long_headers() {
    let client = client();
    let executor = executor(vec![response(200, json!({"paths":[]}))]);
    let result = OneLakeReadHost {
        client: &client,
        executor: &executor,
        limits: OneLakeLimits::default(),
    }
    .list(&path(), true, Some("length(@)"), pending())
    .await
    .unwrap();
    assert_eq!(result.output.data, Some(json!("0")));
    assert!(result.show_headers);
}

#[tokio::test]
async fn invalid_query_is_not_empty_data_or_a_successful_output_envelope() {
    let client = client();
    let executor = executor(vec![response(200, json!({"paths":[]}))]);
    let error = OneLakeReadHost {
        client: &client,
        executor: &executor,
        limits: OneLakeLimits::default(),
    }
    .list(&path(), false, Some("["), pending())
    .await
    .unwrap_err();
    assert!(matches!(error, OneLakeReadError::Query(_)));
    let plan = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        error.output(),
    )
    .render(Indent::Four)
    .unwrap();
    assert_eq!(plan.exit_code, 1);
    assert!(
        String::from_utf8(plan.payload)
            .unwrap()
            .contains("\"error_code\": \"InvalidInput\"")
    );
}

#[tokio::test]
async fn exists_maps_only_http404_and_empty_success_is_true() {
    let client = client();
    for (status, expected) in [(200, "true"), (404, "false")] {
        let executor = executor(vec![response(status, json!({"paths":[]}))]);
        let result = OneLakeReadHost {
            client: &client,
            executor: &executor,
            limits: OneLakeLimits::default(),
        }
        .exists(&path(), pending())
        .await
        .unwrap();
        assert_eq!(result.output.message.as_deref(), Some(expected));
        assert!(result.output.data.is_none());
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn exists_preserves_permission_and_malformed_failures_instead_of_false() {
    let client = client();
    for reply in [
        response(403, json!({})),
        response(200, json!({})),
        response(200, json!({"paths":[{}]})),
    ] {
        let executor = executor(vec![reply]);
        let error = OneLakeReadHost {
            client: &client,
            executor: &executor,
            limits: OneLakeLimits::default(),
        }
        .exists(&path(), pending())
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            OneLakeReadError::Sdk(
                OneLakeError::Http { status: 403, .. } | OneLakeError::InvalidResponse
            )
        ));
    }
}

#[tokio::test]
async fn cancelled_exists_never_acquires_a_successful_absence_or_dispatches() {
    let client = client();
    let executor = Executor::default();
    let error = OneLakeReadHost {
        client: &client,
        executor: &executor,
        limits: OneLakeLimits::default(),
    }
    .exists(&path(), std::future::ready(()))
    .await
    .unwrap_err();
    assert_eq!(error, OneLakeReadError::Sdk(OneLakeError::Cancelled));
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn list_keeps_page_order_then_applies_truthiness_and_long_column_order() {
    let client = client();
    let mut first = response(
        200,
        json!({"paths":[{"name":"item/Files/link/first","lastModified":"one","permissions":"r","isShortcut":"false"}]}),
    );
    first
        .headers
        .insert("x-ms-continuation".into(), "next".into());
    let executor = executor(vec![
        first,
        response(
            200,
            json!({"paths":[{"name":"item/Files/link/second","isDirectory":true}]}),
        ),
    ]);
    let result = OneLakeReadHost {
        client: &client,
        executor: &executor,
        limits: OneLakeLimits::default(),
    }
    .list(&path(), true, None, pending())
    .await
    .unwrap();
    let data = result.output.data.unwrap();
    assert_eq!(
        data,
        json!([
            {"permissions":"r","lastModified":"one","name":"first.Shortcut","type":"Shortcut"},
            {"name":"second","type":"Directory"},
        ])
    );
    assert_eq!(
        data[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["permissions", "lastModified", "name", "type"]
    );
}
