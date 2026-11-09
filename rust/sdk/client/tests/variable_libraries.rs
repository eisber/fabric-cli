// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public consumers; no environment credentials, live values or mutations.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    representations::RepresentationLimits,
    variable_libraries::{
        Error, ReadScope, SourceDisclosure, SourceLimits, SourceRead, Target, read_metadata,
        read_source,
    },
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "aaaaaaaa-0000-1111-2222-bbbbbbbbbbbb";
const ITEM: &str = "bbbbbbbb-1111-2222-3333-cccccccccccc";
const OPERATION: &str = "cccccccc-2222-3333-4444-dddddddddddd";
const PRIVATE: &str = "private-value-never-in-incidental-output";
const DISCLOSURE: SourceDisclosure = SourceDisclosure::ExplicitlyRequested;

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
    }
}

struct Executor {
    response: Mutex<Option<OperationResponse>>,
    requests: Mutex<Vec<(ResourceMethod, String)>>,
}

impl Executor {
    fn response(status: u16, body: Value) -> Self {
        Self {
            response: Mutex::new(Some(OperationResponse {
                status,
                body: serde_json::to_vec(&body).unwrap(),
                headers: BTreeMap::new(),
            })),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn take(&self, method: ResourceMethod, url: &str) -> OperationResponse {
        self.requests.lock().unwrap().push((method, url.into()));
        self.response
            .lock()
            .unwrap()
            .take()
            .expect("no fallback, replay or second request is permitted")
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            let response = self.take(ResourceMethod::Get, request.url().as_str());
            Ok(ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), ResourceMethod::Post);
            assert!(request.body().is_empty());
            Ok(self.take(request.method(), request.url().as_str()))
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("source acceptance must not start another poller")
    }
}

fn client(audience: Audience) -> (Client, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    (
        Client::new(
            credential.clone(),
            CredentialSelection::new("offline", "tenant", audience).unwrap(),
        ),
        credential,
    )
}

fn target() -> Target {
    Target::new(WORKSPACE, ITEM, "VariableLibrary").unwrap()
}

fn limits() -> SourceLimits {
    SourceLimits {
        representation: RepresentationLimits {
            max_response_bytes: NonZeroUsize::new(32_768).unwrap(),
            max_result_bytes: NonZeroUsize::new(32_768).unwrap(),
        },
        max_parts: NonZeroUsize::new(4).unwrap(),
        max_decoded_bytes: NonZeroUsize::new(16_384).unwrap(),
    }
}

fn metadata() -> Value {
    json!({
        "id": ITEM, "workspaceId": WORKSPACE, "type": "VariableLibrary",
        "displayName": PRIVATE, "description": null,
        "properties": {"activeValueSetName": "ExampleSet"},
        "future": {"preserved": true}
    })
}

fn part(path: &str, value: Value) -> Value {
    json!({
        "path": path,
        "payloadType": "InlineBase64",
        "payload": STANDARD.encode(serde_json::to_vec(&value).unwrap())
    })
}

fn source() -> Value {
    json!({
        "futureEnvelope": true,
        "definition": {
            "format": "VariableLibraryV1",
            "parts": [
                part("variables.json", json!({
                    "variables": [
                        {"name":"Text", "type":"String", "value":PRIVATE, "note":PRIVATE},
                        {"name":"Flag", "type":"Boolean", "value":true},
                        {"name":"Count", "type":"Integer", "value":7},
                        {"name":"Future", "type":"FutureReference", "value":{"inert": PRIVATE}}
                    ]
                })),
                part("settings.json", json!({"valueSetsOrder":["ExampleSet"], "future":PRIVATE})),
                part("valueSets/ExampleSet.json", json!({
                    "name":"ExampleSet", "description":PRIVATE,
                    "variableOverrides":[
                        {"name":"Text", "value":PRIVATE},
                        {"name":"Flag", "value":false},
                        {"name":"Count", "value":0}
                    ]
                })),
                part(".platform", json!({"future":{"inert": PRIVATE}}))
            ]
        }
    })
}

fn decode(value: &Value, limits: SourceLimits) -> Result<SourceRead, Error> {
    SourceRead::from_completed_result(&serde_json::to_vec(value).unwrap(), DISCLOSURE, limits)
}

fn edit_part(source: &mut Value, index: usize, edit: impl FnOnce(&mut Value)) {
    let payload = source["definition"]["parts"][index]["payload"]
        .as_str()
        .unwrap();
    let mut value: Value = serde_json::from_slice(&STANDARD.decode(payload).unwrap()).unwrap();
    edit(&mut value);
    source["definition"]["parts"][index]["payload"] =
        json!(STANDARD.encode(serde_json::to_vec(&value).unwrap()));
}

#[test]
fn target_and_default_scope_are_strict_and_do_not_discover_names_or_stages() {
    assert_eq!(ReadScope::default(), ReadScope::Metadata);
    assert!(!ReadScope::Metadata.requires_item_write());
    assert!(ReadScope::NativeSource.requires_item_write());
    for bad in [
        "",
        "name",
        "../item",
        "https://host/item",
        ITEM.trim_end_matches('c'),
    ] {
        assert!(Target::new(bad, ITEM, "VariableLibrary").is_err());
        assert!(Target::new(WORKSPACE, bad, "VariableLibrary").is_err());
    }
    for owner in [
        "variableLibrary",
        "Notebook",
        "VariableLibrary/src",
        "stage/prod",
    ] {
        assert!(Target::new(WORKSPACE, ITEM, owner).is_err());
    }
    assert_eq!(target().workspace_id(), WORKSPACE);
    assert_eq!(target().item_id(), ITEM);
    assert!(!format!("{:?}", target()).contains(ITEM));
}

#[tokio::test]
async fn metadata_default_is_one_get_without_source_or_values() {
    let (client, credential) = client(Audience::Fabric);
    let executor = Executor::response(200, metadata());
    let result = read_metadata(
        &client,
        &executor,
        &target(),
        limits().representation,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.active_value_set_name(), Some("ExampleSet"));
    assert_eq!(result.data().as_value(), &metadata());
    assert!(!format!("{result:?}").contains(PRIVATE));
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        *executor.requests.lock().unwrap(),
        vec![(
            ResourceMethod::Get,
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}")
        )]
    );
}

#[tokio::test]
async fn metadata_checks_response_owner_identity_type_and_known_properties() {
    let (client, _) = client(Audience::Fabric);
    for (field, value) in [
        ("id", json!(WORKSPACE)),
        ("workspaceId", json!(ITEM)),
        ("workspaceId", Value::Null),
        ("type", json!("Notebook")),
        ("type", json!("variablelibrary")),
        ("properties", Value::Null),
        ("properties", json!({"activeValueSetName":false})),
    ] {
        let mut data = metadata();
        data[field] = value;
        let executor = Executor::response(200, data);
        let error = read_metadata(
            &client,
            &executor,
            &target(),
            limits().representation,
            pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::InvalidMetadata));
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let mut absent = metadata();
    absent.as_object_mut().unwrap().remove("properties");
    let executor = Executor::response(200, absent);
    let result = read_metadata(
        &client,
        &executor,
        &target(),
        limits().representation,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.active_value_set_name(), None);
}

#[tokio::test]
async fn source_is_one_explicit_native_post_and_shapes_do_not_disclose_values() {
    let (client, _) = client(Audience::Fabric);
    let native = source();
    let executor = Executor::response(200, native.clone());
    let result = read_source(
        &client,
        &executor,
        &target(),
        DISCLOSURE,
        limits(),
        pending(),
    )
    .await
    .unwrap();
    let SourceRead::Completed(definition) = &result else {
        panic!("expected completion")
    };
    assert_eq!(definition.native_definition(), &native);
    let shape = definition.shape();
    assert_eq!(shape.variables.len(), 4);
    assert_eq!(shape.variables[3].variable_type, "FutureReference");
    assert_eq!(shape.value_sets[0].name, "ExampleSet");
    assert_eq!(shape.value_sets[0].override_count, 3);
    assert_eq!(
        shape.value_sets_order.as_deref(),
        Some(&["ExampleSet".into()][..])
    );
    assert!(shape.value_sets[0].has_override_list);
    let json = serde_json::to_string(shape).unwrap();
    assert!(!json.contains(PRIVATE));
    assert!(!json.contains(&STANDARD.encode(PRIVATE)));
    assert!(!json.contains("activeValueSetName"));
    for text in [
        format!("{result:?}"),
        format!("{definition:?}"),
        format!("{shape:?}"),
        format!("{:?}", shape.variables[0]),
        format!("{:?}", shape.value_sets[0]),
    ] {
        assert!(!text.contains(PRIVATE));
        assert!(!text.contains("ExampleSet"));
    }
    assert_eq!(
        *executor.requests.lock().unwrap(),
        vec![(
            ResourceMethod::Post,
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}/getDefinition?format=VariableLibraryV1"
            )
        )]
    );
}

#[tokio::test]
async fn acceptance_is_not_an_empty_source_and_never_polls() {
    let (client, _) = client(Audience::Fabric);
    let executor = Executor::response(202, Value::Null);
    executor
        .response
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    let result = read_source(
        &client,
        &executor,
        &target(),
        DISCLOSURE,
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(matches!(&result, SourceRead::Accepted { operation_id } if operation_id == OPERATION));
    assert!(result.accepted_handle().is_some());
    assert!(!format!("{result:?}").contains(OPERATION));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn source_denial_or_bad_acceptance_never_falls_back_to_metadata() {
    let (client, _) = client(Audience::Fabric);
    for status in [401, 403, 202, 204] {
        let executor = Executor::response(
            status,
            json!({
                "errorCode":"Denied", "message":PRIVATE, "parameters":[{"value":PRIVATE}]
            }),
        );
        let error = read_source(
            &client,
            &executor,
            &target(),
            DISCLOSURE,
            limits(),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn ready_cancellation_and_wrong_audience_perform_zero_requests() {
    let (client, credential) = client(Audience::Fabric);
    let executor = Executor::response(200, source());
    assert!(
        read_source(
            &client,
            &executor,
            &target(),
            DISCLOSURE,
            limits(),
            ready(()),
        )
        .await
        .is_err()
    );
    assert!(
        read_metadata(
            &client,
            &executor,
            &target(),
            limits().representation,
            ready(()),
        )
        .await
        .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
    let (wrong_client, wrong_credential) = self::client(Audience::OneLake);
    assert!(
        read_source(
            &wrong_client,
            &executor,
            &target(),
            DISCLOSURE,
            limits(),
            pending(),
        )
        .await
        .is_err()
    );
    assert!(
        read_metadata(
            &wrong_client,
            &executor,
            &target(),
            limits().representation,
            pending(),
        )
        .await
        .is_err()
    );
    assert_eq!(wrong_credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[test]
fn missing_duplicate_or_unsupported_native_parts_and_formats_fail_closed() {
    for index in [0, 1] {
        let mut native = source();
        native["definition"]["parts"]
            .as_array_mut()
            .unwrap()
            .remove(index);
        assert!(decode(&native, limits()).is_err());
    }
    for path in [
        "variables.json",
        "settings.json",
        "../variables.json",
        "/variables.json",
        "valueSet/ExampleSet.json",
        "valueSets/../ExampleSet.json",
        "valueSets\\ExampleSet.json",
        "C:\\variables.json",
        "docs/context.md",
    ] {
        let mut native = source();
        native["definition"]["parts"][2]["path"] = json!(path);
        assert!(decode(&native, limits()).is_err(), "{path}");
    }
    for format in ["json", "VariableLibraryV2", "", "variablelibraryv1"] {
        let mut native = source();
        native["definition"]["format"] = json!(format);
        assert!(decode(&native, limits()).is_err());
    }
    let mut native = source();
    native["definition"]
        .as_object_mut()
        .unwrap()
        .remove("format");
    assert!(decode(&native, limits()).is_ok());
}

#[test]
fn null_malformed_and_ambiguous_shape_data_is_not_a_success_shaped_empty_set() {
    let edits: Vec<(usize, Value)> = vec![
        (0, json!({"variables":null})),
        (0, json!({"variables":[{"name":"x","type":null}]})),
        (
            0,
            json!({"variables":[{"name":"x","type":"String","note":null}]}),
        ),
        (
            0,
            json!({"variables":[{"name":"x","type":"String"},{"name":"X","type":"String"}]}),
        ),
        (1, json!({"valueSetsOrder":null})),
        (1, json!({"valueSetsOrder":["Missing"]})),
        (1, json!({"valueSetsOrder":["ExampleSet","ExampleSet"]})),
        (2, json!({"name":"WrongStem"})),
        (2, json!({"name":"ExampleSet","variableOverrides":null})),
        (
            2,
            json!({"name":"ExampleSet","variableOverrides":[{"name":"Missing","value":1}]}),
        ),
        (
            2,
            json!({"name":"ExampleSet","variableOverrides":[{"name":"Text"}]}),
        ),
        (
            2,
            json!({"name":"ExampleSet","variableOverrides":[{"name":"Text","value":1},{"name":"text","value":2}]}),
        ),
    ];
    for (index, replacement) in edits {
        let mut native = source();
        edit_part(&mut native, index, |value| *value = replacement);
        assert!(decode(&native, limits()).is_err());
    }
    for (field, value) in [
        ("payload", json!("not-base64")),
        ("payload", json!(STANDARD.encode(PRIVATE))),
        ("payloadType", json!("SecretReference")),
    ] {
        let mut native = source();
        native["definition"]["parts"][2][field] = value;
        let error = decode(&native, limits()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
    }
}

#[test]
fn source_retains_absent_null_and_false_defaults_without_resolving_values() {
    let mut native = source();
    edit_part(&mut native, 0, |value| {
        value["variables"][0]
            .as_object_mut()
            .unwrap()
            .remove("value");
        value["variables"][1]["value"] = Value::Null;
        value["variables"][2]["value"] = json!(false);
    });
    let SourceRead::Completed(result) = decode(&native, limits()).unwrap() else {
        panic!()
    };
    assert!(!result.shape().variables[0].has_default_value);
    assert!(result.shape().variables[1].has_default_value);
    assert!(result.shape().variables[2].has_default_value);
    assert_eq!(result.native_definition(), &native);
    assert_eq!(result.shape().value_sets[0].override_count, 3);
}

#[test]
fn empty_library_and_sparse_sets_are_native_shapes_not_synthesized_default_names() {
    let mut native = source();
    edit_part(&mut native, 0, |value| *value = json!({"variables":[]}));
    edit_part(&mut native, 1, |value| *value = json!({}));
    edit_part(&mut native, 2, |value| {
        *value = json!({"name":"ExampleSet"})
    });
    let SourceRead::Completed(result) = decode(&native, limits()).unwrap() else {
        panic!()
    };
    assert!(result.shape().variables.is_empty());
    assert_eq!(result.shape().value_sets[0].override_count, 0);
    assert_eq!(result.shape().value_sets_order, None);
    assert!(!result.shape().value_sets[0].has_override_list);
    assert!(
        !serde_json::to_string(result.shape())
            .unwrap()
            .contains("Default")
    );
}

#[test]
fn omitted_and_empty_source_lists_remain_distinct() {
    let mut native = source();
    edit_part(&mut native, 1, |value| {
        *value = json!({"valueSetsOrder":[]})
    });
    edit_part(&mut native, 2, |value| {
        *value = json!({"name":"ExampleSet","variableOverrides":[]})
    });
    let SourceRead::Completed(result) = decode(&native, limits()).unwrap() else {
        panic!()
    };
    assert_eq!(result.shape().value_sets_order, Some(vec![]));
    assert!(result.shape().value_sets[0].has_override_list);
    assert_eq!(result.shape().value_sets[0].override_count, 0);
}

#[tokio::test]
async fn cancellation_at_delivery_preserves_an_already_observed_acceptance() {
    let (client, _) = client(Audience::Fabric);
    let make_executor = || {
        let executor = Executor::response(202, Value::Null);
        executor
            .response
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .headers
            .insert("x-ms-operation-id".into(), OPERATION.into());
        executor
    };
    let polls = AtomicUsize::new(0);
    let count = poll_fn(|_| {
        polls.fetch_add(1, Ordering::SeqCst);
        Poll::<()>::Pending
    });
    assert!(
        read_source(
            &client,
            &make_executor(),
            &target(),
            DISCLOSURE,
            limits(),
            count,
        )
        .await
        .unwrap()
        .accepted_handle()
        .is_some()
    );
    let final_poll = polls.load(Ordering::SeqCst);
    assert!(final_poll > 1);
    let polls = AtomicUsize::new(0);
    let cancel_at_delivery = poll_fn(|_| {
        if polls.fetch_add(1, Ordering::SeqCst) + 1 == final_poll {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let executor = make_executor();
    let error = read_source(
        &client,
        &executor,
        &target(),
        DISCLOSURE,
        limits(),
        cancel_at_delivery,
    )
    .await
    .unwrap_err();
    assert!(matches!(&error, Error::Cancelled { operation_id: Some(id) } if id == OPERATION));
    assert!(!format!("{error:?} {error}").contains(OPERATION));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn metadata_bounds_accept_exact_result_and_reject_one_byte_less() {
    let (client, _) = client(Audience::Fabric);
    let bytes = serde_json::to_vec(&metadata()).unwrap().len();
    let exact = RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(bytes).unwrap(),
        max_result_bytes: NonZeroUsize::new(bytes).unwrap(),
    };
    assert!(
        read_metadata(
            &client,
            &Executor::response(200, metadata()),
            &target(),
            exact,
            pending(),
        )
        .await
        .is_ok()
    );
    for bound in [
        RepresentationLimits {
            max_response_bytes: NonZeroUsize::new(bytes - 1).unwrap(),
            ..exact
        },
        RepresentationLimits {
            max_result_bytes: NonZeroUsize::new(bytes - 1).unwrap(),
            ..exact
        },
    ] {
        let executor = Executor::response(200, metadata());
        assert!(
            read_metadata(&client, &executor, &target(), bound, pending(),)
                .await
                .is_err()
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn budgets_cover_complete_unprojected_source_and_all_decoded_parts() {
    let native = source();
    let wire_len = serde_json::to_vec(&native).unwrap().len();
    let decoded_len: usize = native["definition"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            STANDARD
                .decode(part["payload"].as_str().unwrap())
                .unwrap()
                .len()
        })
        .sum();
    let exact = SourceLimits {
        representation: RepresentationLimits {
            max_response_bytes: NonZeroUsize::new(wire_len).unwrap(),
            max_result_bytes: NonZeroUsize::new(wire_len).unwrap(),
        },
        max_parts: NonZeroUsize::new(4).unwrap(),
        max_decoded_bytes: NonZeroUsize::new(decoded_len).unwrap(),
    };
    assert!(decode(&native, exact).is_ok());
    assert!(
        decode(
            &native,
            SourceLimits {
                max_parts: NonZeroUsize::new(3).unwrap(),
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        decode(
            &native,
            SourceLimits {
                max_decoded_bytes: NonZeroUsize::new(decoded_len - 1).unwrap(),
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        decode(
            &native,
            SourceLimits {
                representation: RepresentationLimits {
                    max_result_bytes: NonZeroUsize::new(wire_len - 1).unwrap(),
                    ..exact.representation
                },
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        decode(
            &native,
            SourceLimits {
                representation: RepresentationLimits {
                    max_response_bytes: NonZeroUsize::new(wire_len - 1).unwrap(),
                    ..exact.representation
                },
                ..exact
            }
        )
        .is_err()
    );
}
