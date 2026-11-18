// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline definition-only discovery consumers; evaluation is never dispatched.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    dataflow_queries::{from_definition, list_queries},
    representations::{DefinitionRead, DefinitionTarget, RepresentationLimits},
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const METADATA: &str = r#"{"formatVersion":"202502","queriesMetadata":{"Orders":{"queryId":"id1","queryName":"Orders","isHidden":true,"loadEnabled":false},"Other":{"queryId":"id2","queryName":"A/雪"}},"connections":[{"secret":"never projected"}]}"#;

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-discovery-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Wire {
    replies: Mutex<VecDeque<OperationResponse>>,
    requests: Mutex<Vec<(ResourceMethod, String)>>,
    waits: AtomicUsize,
}

impl OperationExecutor for Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert!(request.body().is_empty());
            assert!(!request.url().path().contains("executeQuery"));
            self.requests
                .lock()
                .unwrap()
                .push((request.method(), request.url().path().into()));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected replay or discovery request"))
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        assert_eq!(delay, Duration::from_secs(1));
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Ok(())))
    }
}

fn limits() -> RepresentationLimits {
    RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(2 * 1024 * 1024).unwrap(),
        max_result_bytes: NonZeroUsize::new(2 * 1024 * 1024).unwrap(),
    }
}

fn target() -> DefinitionTarget {
    DefinitionTarget {
        workspace_id: WS.into(),
        item_id: ITEM.into(),
        native_format: None,
    }
}

fn envelope(metadata: &str) -> Value {
    json!({"definition":{"parts":[
        {"path":"queryMetadata.json","payloadType":"InlineBase64","payload":STANDARD.encode(metadata)},
        {"path":"mashup.pq","payloadType":"InlineBase64","payload":STANDARD.encode("private Mashup code")}
    ]}})
}

fn read(value: Value) -> DefinitionRead {
    DefinitionRead::from_completed_result(&serde_json::to_vec(&value).unwrap(), limits()).unwrap()
}

fn response(status: u16, body: Value, headers: &[(&str, String)]) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).into(), value.clone()))
            .collect(),
    }
}

fn setup(replies: Vec<OperationResponse>) -> (Client, Arc<Credential>, Wire) {
    let credential = Arc::new(Credential::default());
    let client = Client::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let wire = Wire {
        replies: Mutex::new(replies.into()),
        ..Wire::default()
    };
    (client, credential, wire)
}

#[test]
fn metadata_projects_exact_names_flags_and_no_connections_or_mashup() {
    let inventory = from_definition(&read(envelope(METADATA))).unwrap();
    assert!(inventory.query_metadata_present);
    assert_eq!(inventory.queries.len(), 2);
    assert_eq!(inventory.queries[0].query_name, "Orders");
    assert!(inventory.queries[0].is_hidden);
    assert!(!inventory.queries[0].load_enabled);
    assert_eq!(inventory.queries[1].query_name, "A/雪");
    assert!(!inventory.queries[1].is_hidden);
    assert!(inventory.queries[1].load_enabled);
    let json = serde_json::to_string(&inventory).unwrap();
    assert!(!json.contains("secret"));
    assert!(!json.contains("private"));
    assert!(inventory.operation_id.is_none());
}

#[test]
fn optional_index_absence_is_distinct_from_explicit_empty_index() {
    let absent = from_definition(&read(envelope(r#"{"formatVersion":"202502"}"#))).unwrap();
    assert!(!absent.query_metadata_present);
    assert!(absent.queries.is_empty());
    let empty = from_definition(&read(envelope(
        r#"{"formatVersion":"202502","queriesMetadata":{}}"#,
    )))
    .unwrap();
    assert!(empty.query_metadata_present);
    assert!(empty.queries.is_empty());
}

#[test]
fn unsupported_malformed_duplicate_and_control_metadata_fails_explicitly() {
    for metadata in [
        r#"{"formatVersion":"future","queriesMetadata":{}}"#,
        r#"{"queriesMetadata":{}}"#,
        r#"{"formatVersion":"202502","formatVersion":"202502"}"#,
        r#"{"formatVersion":"202502","queriesMetadata":null}"#,
        r#"{"formatVersion":"202502","queriesMetadata":[]}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{},"queriesMetadata":{}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id","queryName":"X"},"x":{"queryId":"id","queryName":"Y"}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id","queryName":"X"},"y":{"queryId":"id2","queryName":"X"}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id","queryName":"X","isHidden":null}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id","queryName":"X","loadEnabled":"false"}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"","queryName":"X"}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id","queryName":"\u000a"}}}"#,
        r#"{"formatVersion":"202502","queriesMetadata":{"x":{"queryId":"id"}}}"#,
    ] {
        assert!(
            from_definition(&read(envelope(metadata))).is_err(),
            "{metadata}"
        );
    }
}

#[test]
fn missing_duplicate_case_alias_noninline_and_invalid_base64_parts_fail() {
    for kind in 0..6 {
        let mut value = envelope(METADATA);
        let parts = value["definition"]["parts"].as_array_mut().unwrap();
        match kind {
            0 => {
                parts.remove(0);
            }
            1 => {
                parts.push(parts[0].clone());
            }
            2 => {
                let mut alias = parts[0].clone();
                alias["path"] = json!("QueryMetadata.json");
                parts.push(alias);
            }
            3 => {
                parts[0]["payloadType"] = json!("ExternalUrl");
            }
            4 => {
                parts[0]["payload"] = json!("not-base64");
            }
            _ => {
                parts[0]["path"] = json!("QUERYMETADATA.JSON");
            }
        }
        assert!(from_definition(&read(value)).is_err());
    }
    assert!(
        from_definition(&DefinitionRead::Accepted {
            operation_id: OP.into()
        })
        .is_err()
    );
}

#[test]
fn metadata_byte_and_query_count_limits_are_enforced_without_partial_results() {
    let oversized = format!(
        r#"{{"formatVersion":"202502","padding":"{}"}}"#,
        "x".repeat(1024 * 1024)
    );
    assert!(from_definition(&read(envelope(&oversized))).is_err());
    let mut queries = serde_json::Map::new();
    for index in 0..4097 {
        queries.insert(
            index.to_string(),
            json!({"queryId":index.to_string(),"queryName":index.to_string()}),
        );
    }
    let metadata = json!({"formatVersion":"202502","queriesMetadata":queries}).to_string();
    assert!(from_definition(&read(envelope(&metadata))).is_err());
}

#[tokio::test]
async fn completed_definition_uses_one_post_and_never_evaluates_or_discovers_connections() {
    let (client, _, wire) = setup(vec![response(200, envelope(METADATA), &[])]);
    let inventory = list_queries(&client, &wire, &target(), limits(), pending())
        .await
        .unwrap();
    assert_eq!(inventory.queries.len(), 2);
    assert_eq!(
        *wire.requests.lock().unwrap(),
        vec![(
            ResourceMethod::Post,
            format!("/v1/workspaces/{WS}/items/{ITEM}/getDefinition")
        )]
    );
    assert_eq!(wire.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn accepted_definition_uses_existing_operation_observer_without_post_replay() {
    let url = format!("https://api.fabric.microsoft.com/v1/operations/{OP}");
    let (client, _, wire) = setup(vec![
        response(
            202,
            json!({}),
            &[("x-ms-operation-id", OP.into()), ("location", url.clone())],
        ),
        response(
            200,
            json!({"status":"Running"}),
            &[("retry-after", "1".into())],
        ),
        response(
            200,
            json!({"status":"Succeeded"}),
            &[("location", format!("{url}/result"))],
        ),
        response(200, envelope(METADATA), &[]),
    ]);
    let inventory = list_queries(&client, &wire, &target(), limits(), pending())
        .await
        .unwrap();
    assert_eq!(inventory.operation_id.as_deref(), Some(OP));
    assert_eq!(wire.waits.load(Ordering::SeqCst), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _)| *method == ResourceMethod::Post)
            .count(),
        1
    );
    assert!(requests[3].1.ends_with("/result"));
}

#[tokio::test]
async fn invalid_target_and_precancelled_discovery_never_acquire_credentials() {
    let (client, credential, wire) = setup(vec![]);
    let mut invalid = target();
    invalid.item_id = "../escape".into();
    assert!(
        list_queries(&client, &wire, &invalid, limits(), pending())
            .await
            .is_err()
    );
    invalid = target();
    invalid.native_format = Some("unrequested".into());
    assert!(
        list_queries(&client, &wire, &invalid, limits(), pending())
            .await
            .is_err()
    );
    assert!(
        list_queries(&client, &wire, &target(), limits(), ready(()))
            .await
            .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_definition_operation_is_not_an_empty_inventory_and_never_replays() {
    let (client, _, wire) = setup(vec![
        response(202, json!({}), &[("x-ms-operation-id", OP.into())]),
        response(
            200,
            json!({"status":"Failed","error":{"message":"private connection"}}),
            &[],
        ),
    ]);
    let error = list_queries(&client, &wire, &target(), limits(), pending())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private"));
    assert!(!format!("{error:?}").contains("private"));
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
}
