// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline consumers of exact native definition and file representations.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    observe::{ObserveOptions, ReadHandle, observe},
    representations::{
        DefinitionRead, DefinitionTarget, RepresentationError, RepresentationLimits,
        read_definition, read_file,
    },
};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeMethod,
    OneLakePath, OneLakeRequest, OneLakeResponse,
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OTHER_ID: &str = "01234567-89ab-cdef-0123-456789abcdee";
const FABRIC: &str = "https://api.fabric.microsoft.com";
const POST_URL: &str = "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/getDefinition";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    pause: bool,
    started: tokio::sync::Notify,
    dropped: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        if self.pause {
            let _guard = DropGuard(&self.dropped);
            self.started.notify_one();
            pending::<()>().await;
        }
        Ok(AccessToken::new(
            "offline-representations-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct DropGuard<'a>(&'a AtomicUsize);

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RecordedRequest {
    url: String,
    max_response_bytes: usize,
}

#[derive(Default)]
struct Executor {
    reply: Mutex<VecDeque<OperationResponse>>,
    requests: Mutex<Vec<RecordedRequest>>,
    expected_method: Option<ResourceMethod>,
    pause: bool,
    started: tokio::sync::Notify,
    dropped: AtomicUsize,
    cancelled_after_reply: Option<Arc<AtomicBool>>,
}

impl Executor {
    fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> Self {
        Self {
            reply: Mutex::new(VecDeque::from([OperationResponse {
                status,
                body: body.to_vec(),
                headers: headers
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            }])),
            ..Self::default()
        }
    }

    fn record(&self, url: &str, max_response_bytes: usize) {
        self.requests.lock().unwrap().push(RecordedRequest {
            url: url.into(),
            max_response_bytes,
        });
    }

    async fn reply(&self) -> OperationResponse {
        if self.pause {
            let _guard = DropGuard(&self.dropped);
            self.started.notify_one();
            pending::<()>().await;
        }
        let response = self
            .reply
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra request or metadata fallback");
        if let Some(flag) = &self.cancelled_after_reply {
            flag.store(true, Ordering::SeqCst);
        }
        response
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(
                request.method(),
                self.expected_method.unwrap_or(ResourceMethod::Post)
            );
            assert!(request.body().is_empty());
            self.record(request.url().as_str(), request.max_response_bytes().get());
            Ok(self.reply().await)
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("native read must return acceptance without polling")
    }
}

impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), OneLakeMethod::Get);
            assert!(request.body().is_empty());
            assert!(request.headers().is_empty());
            self.record(request.url().as_str(), request.max_response_bytes().get());
            let response = self.reply().await;
            Ok(OneLakeResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", audience).unwrap()
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::new(credential, selection(audience))
}

fn target() -> DefinitionTarget {
    DefinitionTarget {
        workspace_id: "ws".into(),
        item_id: "item".into(),
        native_format: None,
    }
}

fn limits(response: usize, result: usize) -> RepresentationLimits {
    RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(response).unwrap(),
        max_result_bytes: NonZeroUsize::new(result).unwrap(),
    }
}

fn envelope(payload: &str, payload_type: &str) -> Value {
    json!({
        "definition": {
            "parts": [{
                "path": "source.ipynb",
                "payloadType": payload_type,
                "payload": payload
            }]
        }
    })
}

fn wire(definition: &Value) -> Vec<u8> {
    serde_json::to_vec(definition).unwrap()
}

fn cancellation(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    let mut completed = false;
    poll_fn(move |_| {
        assert!(!completed, "completed cancellation was polled again");
        if flag.load(Ordering::SeqCst) {
            completed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn completed_definition_preserves_notebook_outputs_paths_order_media_and_unknown_fields() {
    let notebook = br#"{"cells":[{"outputs":[{"text":["keep me"],"execution_count":7}]}],"metadata":{"keep":true}}"#;
    let payload = STANDARD.encode(notebook);
    let data = json!({
        "futureEnvelope": {"untouched": [null, 1.0, "Unicode \u{2603}"]},
        "definition": {
            "format": "ipynb",
            "parts": [
                {"path": "source.ipynb", "payloadType": "InlineBase64", "payload": payload,
                    "contentType": "application/x-ipynb+json; charset=utf-8"},
                {"path": "../data\\odd.BIN", "payloadType": "InlineBase64", "payload": "/wAB",
                    "mediaType": "application/octet-stream"},
                {"path": "source.ipynb", "payloadType": "FuturePayload", "payload": "opaque"},
                {"path": ".platform", "payloadType": "InlineBase64", "payload": "AA=="}
            ],
            "futureDefinition": {"present": true}
        }
    });
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let executor = Executor::response(200, &wire(&data), &[]);
    let read = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
        .await
        .unwrap();
    let DefinitionRead::Completed { definition } = &read else {
        panic!("unexpected acceptance");
    };
    assert_eq!(
        read,
        DefinitionRead::from_completed_result(&wire(&data), limits(4096, 4096)).unwrap()
    );
    assert_eq!(definition, &data);
    assert_eq!(wire(definition), wire(&data));
    assert_eq!(
        STANDARD
            .decode(
                definition["definition"]["parts"][0]["payload"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
        notebook
    );
    assert_eq!(read.part_bytes("../data\\odd.BIN").unwrap(), [255, 0, 1]);
    assert!(matches!(
        read.part_bytes("source.ipynb"),
        Err(RepresentationError::Ambiguous)
    ));
    assert_eq!(
        executor.requests.lock().unwrap().as_slice(),
        &[RecordedRequest {
            url: POST_URL.into(),
            max_response_bytes: 4096
        }]
    );
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        &[vec![Audience::Fabric.scope().to_owned()]]
    );
    let debug = format!("{read:?}");
    assert!(!debug.contains("keep me") && !debug.contains(&payload));
}

#[tokio::test]
async fn explicit_native_format_uses_only_generic_action_and_preserves_selected_endpoint() {
    let credential = Arc::new(Credential::default());
    let client = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        selection(Audience::Fabric),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    ));
    for format in ["ipynb", "TMDL", "Native-v2.source_1"] {
        let target = DefinitionTarget {
            native_format: Some(format.into()),
            ..target()
        };
        let data = envelope("YWJj", "InlineBase64");
        let executor = Executor::response(200, &wire(&data), &[]);
        let read = read_definition(&client, &executor, &target, limits(4096, 4096), pending())
            .await
            .unwrap();
        assert_eq!(read.part_bytes("source.ipynb").unwrap(), b"abc");
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            format!(
                "https://dailyapi.fabric.microsoft.com/v1/workspaces/ws/items/item/getDefinition?format={format}"
            )
        );
    }
}

#[test]
fn part_selection_is_exact_and_never_uses_suffix_normalization_or_first_duplicate() {
    let read = DefinitionRead::Completed {
        definition: json!({"definition":{"parts":[
            {"path":"A/source.ipynb","payloadType":"InlineBase64","payload":"/wA="},
            {"path":"a/source.ipynb","payloadType":"InlineBase64","payload":"AAE="},
            {"path":"../source.ipynb","payloadType":"InlineBase64","payload":""},
            {"path":"duplicate","payloadType":"FuturePayload","payload":"anything"},
            {"path":"duplicate","payloadType":"InlineBase64","payload":"AA=="}
        ]}}),
    };
    assert_eq!(read.part_bytes("A/source.ipynb").unwrap(), [255, 0]);
    assert_eq!(read.part_bytes("a/source.ipynb").unwrap(), [0, 1]);
    assert_eq!(read.part_bytes("../source.ipynb").unwrap(), b"");
    for path in [
        "source.ipynb",
        "A\\source.ipynb",
        "A/./source.ipynb",
        "A/source.IPYNB",
    ] {
        assert!(matches!(
            read.part_bytes(path),
            Err(RepresentationError::NotFound)
        ));
    }
    assert!(matches!(
        read.part_bytes("duplicate"),
        Err(RepresentationError::Ambiguous)
    ));
    assert!(matches!(
        DefinitionRead::Accepted {
            operation_id: ID.into()
        }
        .part_bytes("anything"),
        Err(RepresentationError::NotCompleted)
    ));
}

#[tokio::test]
async fn inline_bytes_require_canonical_standard_base64_without_mutating_the_payload() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    for payload in [
        "Zg", "Zg=", "Zg==\n", " Zg==", "Zh==", "-_8=", "Zg===", "%%%%", "Zg==AA==",
    ] {
        let data = envelope(payload, "InlineBase64");
        let executor = Executor::response(200, &wire(&data), &[]);
        let read = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .unwrap();
        assert!(
            matches!(
                read.part_bytes("source.ipynb"),
                Err(RepresentationError::InvalidBase64)
            ),
            "{payload:?}"
        );
        assert_eq!(read, DefinitionRead::Completed { definition: data });
    }
    for (payload_type, expected) in [
        ("InlineBase64", true),
        ("inlinebase64", false),
        ("External", false),
        ("", false),
    ] {
        let data = envelope("Zg==", payload_type);
        let executor = Executor::response(200, &wire(&data), &[]);
        let read = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .unwrap();
        if expected {
            assert_eq!(read.part_bytes("source.ipynb").unwrap(), b"f");
        } else {
            assert!(matches!(
                read.part_bytes("source.ipynb"),
                Err(RepresentationError::Unsupported)
            ));
        }
    }
}

#[tokio::test]
async fn completed_requires_a_real_multipart_envelope_not_metadata_or_success_shaped_defaults() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    for data in [
        json!(null),
        json!([]),
        json!({}),
        json!({"id":"item","displayName":"metadata"}),
        json!({"definition":null}),
        json!({"definition":{}}),
        json!({"definition":{"parts":null}}),
        json!({"definition":{"parts":{}}}),
        json!({"definition":{"parts":[null]}}),
        json!({"definition":{"parts":[{}]}}),
        json!({"definition":{"format":null,"parts":[]}}),
        json!({"definition":{"parts":[{"path":null,"payloadType":"InlineBase64","payload":""}]}}),
        json!({"definition":{"parts":[{"path":"p","payloadType":1,"payload":""}]}}),
        json!({"definition":{"parts":[{"path":"p","payloadType":"InlineBase64","payload":null}]}}),
    ] {
        let executor = Executor::response(200, &wire(&data), &[]);
        assert!(
            matches!(
                read_definition(&client, &executor, &target(), limits(4096, 4096), pending()).await,
                Err(RepresentationError::InvalidResponse)
            ),
            "{data}"
        );
    }
    for (status, body) in [(200, b"not-json".as_slice()), (201, b"{}"), (204, b"")] {
        let executor = Executor::response(status, body, &[]);
        assert!(matches!(
            read_definition(&client, &executor, &target(), limits(4096, 4096), pending()).await,
            Err(RepresentationError::InvalidResponse)
        ));
    }
}

#[tokio::test]
async fn exact_response_and_result_bounds_accept_n_and_reject_n_plus_one() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let data = wire(&envelope("YWJj", "InlineBase64"));
    let n = data.len();
    for (response, result, success, result_exceeded) in [
        (n, n, true, false),
        (n - 1, n, false, false),
        (n, n - 1, false, true),
    ] {
        let executor = Executor::response(200, &data, &[]);
        let read = read_definition(
            &client,
            &executor,
            &target(),
            limits(response, result),
            pending(),
        )
        .await;
        match (success, result_exceeded) {
            (true, _) => assert!(read.is_ok()),
            (_, true) => assert!(matches!(read, Err(RepresentationError::BudgetExceeded))),
            _ => assert!(matches!(
                read, Err(RepresentationError::Operation(error)) if error.kind == OperationErrorKind::Limit
            )),
        }
        assert_eq!(
            executor.requests.lock().unwrap()[0].max_response_bytes,
            response
        );
    }
}

#[tokio::test]
async fn result_bound_counts_raw_envelope_whitespace_and_compact_numeric_expansion() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let compact = wire(&envelope("YWJj", "InlineBase64"));
    let mut padded = compact.clone();
    padded.push(b' ');
    let executor = Executor::response(200, &padded, &[]);
    assert!(matches!(
        read_definition(
            &client,
            &executor,
            &target(),
            limits(padded.len(), compact.len()),
            pending()
        )
        .await,
        Err(RepresentationError::BudgetExceeded)
    ));
    assert!(matches!(
        DefinitionRead::from_completed_result(&padded, limits(padded.len(), compact.len())),
        Err(RepresentationError::BudgetExceeded)
    ));
    let raw = br#"{"definition":{"parts":[]},"number":1e-5}"#;
    let parsed: Value = serde_json::from_slice(raw).unwrap();
    let encoded = wire(&parsed);
    assert!(encoded.len() > raw.len());
    let executor = Executor::response(200, raw, &[]);
    assert!(matches!(
        read_definition(
            &client,
            &executor,
            &target(),
            limits(4096, encoded.len() - 1),
            pending()
        )
        .await,
        Err(RepresentationError::BudgetExceeded)
    ));
    assert!(matches!(
        DefinitionRead::from_completed_result(raw, limits(4096, encoded.len() - 1)),
        Err(RepresentationError::BudgetExceeded)
    ));
}

#[test]
fn completed_operation_results_require_the_same_full_multipart_envelope() {
    for data in [
        json!({"status":"Succeeded"}),
        json!({"id":"item","displayName":"metadata"}),
        json!({"definition":{"parts":[{"path":"p","payload":"AA=="}]}}),
        json!({"definition":{"parts":[{"path":"p","payloadType":"InlineBase64","payload":null}]}}),
        json!({"definition":{"parts":null}}),
    ] {
        assert!(matches!(
            DefinitionRead::from_completed_result(&wire(&data), limits(4096, 4096)),
            Err(RepresentationError::InvalidResponse)
        ));
    }
    assert!(matches!(
        DefinitionRead::from_completed_result(b"not-json", limits(4096, 4096)),
        Err(RepresentationError::InvalidResponse)
    ));
    let data = envelope("YWJj", "InlineBase64");
    let read = DefinitionRead::from_completed_result(&wire(&data), limits(4096, 4096)).unwrap();
    assert_eq!(read.part_bytes("source.ipynb").unwrap(), b"abc");
    assert_eq!(read, DefinitionRead::Completed { definition: data });
}

#[test]
fn completed_operation_result_response_and_result_bounds_are_both_exact() {
    let body = wire(&envelope("YWJj", "InlineBase64"));
    let n = body.len();
    assert!(DefinitionRead::from_completed_result(&body, limits(n, n)).is_ok());
    for bound in [limits(n - 1, n), limits(n, n - 1)] {
        assert!(matches!(
            DefinitionRead::from_completed_result(&body, bound),
            Err(RepresentationError::BudgetExceeded)
        ));
    }
}

#[tokio::test]
async fn acceptance_returns_only_a_real_handle_without_polling_or_parsing_a_definition() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let location = format!("{FABRIC}/v1/operations/{ID}");
    for headers in [
        vec![("x-ms-operation-id", ID)],
        vec![("Location", location.as_str())],
        vec![
            ("X-MS-OPERATION-ID", ID),
            ("location", location.as_str()),
            ("Retry-After", "90"),
        ],
    ] {
        let executor = Executor::response(202, b"not a completed definition", &headers);
        let read = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .unwrap();
        assert_eq!(
            read,
            DefinitionRead::Accepted {
                operation_id: ID.into()
            }
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn accepted_definition_handle_roundtrips_through_the_shared_observer_and_result_validator() {
    let credential = Arc::new(Credential::default());
    let client = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        selection(Audience::Fabric),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    ));
    let operation_url = format!("https://dailyapi.fabric.microsoft.com/v1/operations/{ID}");
    let result_url = format!("{operation_url}/result");
    let submit = Executor::response(
        202,
        b"",
        &[("x-ms-operation-id", ID), ("Location", &operation_url)],
    );
    let accepted = read_definition(&client, &submit, &target(), limits(4096, 4096), pending())
        .await
        .unwrap();
    let handle = accepted.accepted_handle().unwrap();
    assert_eq!(
        handle,
        ReadHandle::Operation {
            operation_id: ID.into()
        }
    );
    assert_eq!(submit.requests.lock().unwrap().len(), 1);
    let definition = envelope("YWJj", "InlineBase64");
    let body = wire(&definition);
    let observer = Executor {
        expected_method: Some(ResourceMethod::Get),
        ..Executor::response(
            200,
            br#"{"status":"Succeeded","future":"preserved"}"#,
            &[("Location", &result_url)],
        )
    };
    observer.reply.lock().unwrap().push_back(OperationResponse {
        status: 200,
        body: body.clone(),
        headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
    });
    let observed = observe(
        &client,
        &observer,
        &handle,
        ObserveOptions {
            wait: true,
            result: true,
            max_requests: NonZeroUsize::new(2).unwrap(),
            max_response_bytes: NonZeroUsize::new(4096).unwrap(),
            max_delay: Duration::from_secs(1),
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(observed.handle, handle);
    assert!(observed.terminal && observed.work_succeeded);
    assert_eq!(observed.requests, 2);
    let result = observed.result.unwrap();
    assert_eq!(result.bytes, body);
    assert_eq!(result.media_type.as_deref(), Some("application/json"));
    let completed =
        DefinitionRead::from_completed_result(&result.bytes, limits(4096, 4096)).unwrap();
    assert_eq!(completed, DefinitionRead::Completed { definition });
    assert_eq!(completed.part_bytes("source.ipynb").unwrap(), b"abc");
    assert_eq!(completed.accepted_handle(), None);
    assert_eq!(
        observer.requests.lock().unwrap().as_slice(),
        &[
            RecordedRequest {
                url: operation_url,
                max_response_bytes: 4096,
            },
            RecordedRequest {
                url: result_url,
                max_response_bytes: 4096,
            },
        ]
    );
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        &[
            vec![Audience::Fabric.scope().to_owned()],
            vec![Audience::Fabric.scope().to_owned()],
            vec![Audience::Fabric.scope().to_owned()],
        ]
    );
}

#[tokio::test]
async fn acceptance_rejects_missing_invalid_or_duplicate_ids_and_untrusted_locations() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let foreign = format!("https://foreign.invalid/v1/operations/{ID}");
    let mismatched = format!("{FABRIC}/v1/operations/{OTHER_ID}");
    let query = format!("{FABRIC}/v1/operations/{ID}?ignored=true");
    let traversal = format!("{FABRIC}/v1/operations/bogus/../{ID}");
    let result = format!("{FABRIC}/v1/operations/{ID}/result");
    for headers in [
        vec![],
        vec![("x-ms-operation-id", "")],
        vec![("x-ms-operation-id", "opaque-not-guid")],
        vec![("x-ms-operation-id", ID), ("X-MS-OPERATION-ID", ID)],
        vec![("Location", foreign.as_str())],
        vec![("x-ms-operation-id", ID), ("Location", foreign.as_str())],
        vec![("x-ms-operation-id", ID), ("Location", mismatched.as_str())],
        vec![("Location", query.as_str())],
        vec![("Location", traversal.as_str())],
        vec![("x-ms-operation-id", ID), ("Location", result.as_str())],
        vec![(
            "Location",
            "/v1/operations/01234567-89ab-cdef-0123-456789abcdef",
        )],
    ] {
        let executor = Executor::response(202, b"", &headers);
        let error = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .unwrap_err();
        let RepresentationError::Operation(error) = error else {
            panic!("expected operation evidence");
        };
        assert!(matches!(
            error.kind,
            OperationErrorKind::InvalidResponse | OperationErrorKind::InvalidLink
        ));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn selected_origin_is_required_for_an_accepted_location() {
    let client = Client::with_context(RequestContext::with_endpoint(
        Arc::new(Credential::default()),
        selection(Audience::Fabric),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    ));
    for (origin, success) in [
        ("https://dailyapi.fabric.microsoft.com", true),
        (FABRIC, false),
    ] {
        let location = format!("{origin}/v1/operations/{ID}");
        let executor = Executor::response(202, b"", &[("Location", &location)]);
        let read =
            read_definition(&client, &executor, &target(), limits(4096, 4096), pending()).await;
        if success {
            assert_eq!(
                read.unwrap(),
                DefinitionRead::Accepted {
                    operation_id: ID.into()
                }
            );
        } else {
            assert!(matches!(
                read, Err(RepresentationError::Operation(error)) if error.kind == OperationErrorKind::InvalidLink
            ));
        }
    }
}

#[tokio::test]
async fn denied_definition_retains_failure_data_and_never_falls_back_to_metadata() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let data =
        json!({"errorCode":"Denied","message":"untrusted-provider-text","future":{"keep":true}});
    for status in [401, 403, 404, 429, 500] {
        let executor = Executor::response(status, &wire(&data), &[]);
        let error = read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("untrusted-provider-text"));
        let RepresentationError::Operation(error) = error else {
            panic!("expected service failure");
        };
        assert!(
            matches!(error.kind, OperationErrorKind::Http { status: actual, .. } if actual == status)
        );
        assert_eq!(error.raw_service_error.as_deref(), Some(&data));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap()[0].url, POST_URL);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_identifiers_and_native_formats_are_rejected_before_credentials() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let executor = Executor::default();
    for id in [
        "", ".", "..", "a/b", "a\\b", "a?x=y", "a#frag", "%2e", " space", "w\u{0}",
    ] {
        for invalid in [
            DefinitionTarget {
                workspace_id: id.into(),
                ..target()
            },
            DefinitionTarget {
                item_id: id.into(),
                ..target()
            },
        ] {
            assert!(matches!(
                read_definition(&client, &executor, &invalid, limits(4096, 4096), pending()).await,
                Err(RepresentationError::InvalidInput)
            ));
        }
    }
    let long_format = "x".repeat(129);
    for format in [
        "",
        ".",
        "-ipynb",
        "ipynb&format=other",
        "ipynb?x=1",
        "json#part",
        "ipynb%26x",
        "ipynb source",
        "ipynb\n",
        "\u{2603}",
        long_format.as_str(),
    ] {
        let invalid = DefinitionTarget {
            native_format: Some(format.into()),
            ..target()
        };
        assert!(matches!(
            read_definition(&client, &executor, &invalid, limits(4096, 4096), pending()).await,
            Err(RepresentationError::InvalidInput)
        ));
    }
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_definition_audience_or_endpoint_never_acquires_a_token() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let client = client(credential.clone(), Audience::OneLake);
    assert!(matches!(
        read_definition(&client, &executor, &target(), limits(4096, 4096), pending()).await,
        Err(RepresentationError::Operation(error)) if error.kind == OperationErrorKind::InvalidAudience
    ));
    let client = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        selection(Audience::Fabric),
        EndpointBinding::public(ServiceOrigin::OneLake),
    ));
    assert!(
        read_definition(&client, &executor, &target(), limits(4096, 4096), pending())
            .await
            .is_err()
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn definition_cancellation_before_credentials_during_credentials_and_during_execution() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let executor = Executor::default();
    assert!(matches!(
        read_definition(&client, &executor, &target(), limits(4096, 4096), ready(())).await,
        Err(RepresentationError::Operation(error)) if error.kind == OperationErrorKind::Cancelled
    ));
    assert!(credential.scopes.lock().unwrap().is_empty());
    let paused = Arc::new(Credential {
        pause: true,
        ..Credential::default()
    });
    let paused_client = crate::client(paused.clone(), Audience::Fabric);
    let resolved = target();
    let read = read_definition(
        &paused_client,
        &executor,
        &resolved,
        limits(4096, 4096),
        paused.started.notified(),
    );
    assert!(
        matches!(read.await, Err(RepresentationError::Operation(error)) if error.kind == OperationErrorKind::Cancelled)
    );
    assert_eq!(paused.dropped.load(Ordering::SeqCst), 1);
    assert!(executor.requests.lock().unwrap().is_empty());
    let paused_executor = Executor {
        pause: true,
        ..Executor::default()
    };
    let error = read_definition(
        &client,
        &paused_executor,
        &resolved,
        limits(4096, 4096),
        paused_executor.started.notified(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, RepresentationError::Operation(error) if error.kind == OperationErrorKind::Cancelled && error.mutation_may_have_occurred)
    );
    assert_eq!(paused_executor.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_after_acceptance_retains_handle_evidence_but_returns_no_completed_result() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Executor {
        cancelled_after_reply: Some(flag.clone()),
        ..Executor::response(202, b"", &[("x-ms-operation-id", ID)])
    };
    let error = read_definition(
        &client,
        &executor,
        &target(),
        limits(4096, 4096),
        cancellation(flag),
    )
    .await
    .unwrap_err();
    let RepresentationError::Operation(error) = error else {
        panic!("expected cancellation evidence");
    };
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(ID));
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn file_read_preserves_binary_empty_and_original_headers_using_only_storage_scope() {
    let credential = Arc::new(Credential::default());
    let client = OneLakeClient::new(credential.clone(), selection(Audience::OneLake));
    let path = OneLakePath::new("ws", "item", "Files/odd \u{2603}.ipynb").unwrap();
    let headers = [
        ("Content-Type", "application/octet-stream; custom=keep"),
        ("ETag", "\"opaque\""),
        ("X-Future", "unchanged"),
    ];
    for body in [vec![255, 0, 13, 10, 128], Vec::new()] {
        let executor = Executor::response(200, &body, &headers);
        let read = read_file(&client, &executor, &path, limits(4096, 4096), pending())
            .await
            .unwrap();
        assert_eq!(read.bytes, body);
        assert_eq!(
            read.headers,
            headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect::<BTreeMap<_, _>>()
        );
        assert_eq!(
            executor.requests.lock().unwrap().as_slice(),
            &[RecordedRequest {
                url: "https://onelake.dfs.fabric.microsoft.com/ws/item/Files/odd%20%E2%98%83.ipynb"
                    .into(),
                max_response_bytes: 4096,
            }]
        );
        let debug = format!("{read:?}");
        assert!(!debug.contains("opaque") && !debug.contains("custom"));
    }
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        &[
            vec![Audience::OneLake.scope().to_owned()],
            vec![Audience::OneLake.scope().to_owned()]
        ]
    );
}

#[tokio::test]
async fn file_response_and_result_bounds_are_exact_without_partial_success() {
    let client = OneLakeClient::new(
        Arc::new(Credential::default()),
        selection(Audience::OneLake),
    );
    let path = OneLakePath::new("ws", "item", "Files/a.bin").unwrap();
    let body = [0, 255, 1, 128];
    for (response, result, success, result_exceeded) in [
        (4, 4, true, false),
        (3, 4, false, false),
        (4, 3, false, true),
    ] {
        let executor = Executor::response(200, &body, &[]);
        let read = read_file(
            &client,
            &executor,
            &path,
            limits(response, result),
            pending(),
        )
        .await;
        match (success, result_exceeded) {
            (true, _) => assert_eq!(read.unwrap().bytes, body),
            (_, true) => assert!(matches!(read, Err(RepresentationError::BudgetExceeded))),
            _ => assert!(matches!(
                read,
                Err(RepresentationError::OneLake(OneLakeError::Limit))
            )),
        }
        assert_eq!(
            executor.requests.lock().unwrap()[0].max_response_bytes,
            response
        );
    }
}

#[tokio::test]
async fn denied_files_do_not_become_empty_files_or_metadata_fallbacks() {
    let client = OneLakeClient::new(
        Arc::new(Credential::default()),
        selection(Audience::OneLake),
    );
    let path = OneLakePath::new("ws", "item", "Files/a.bin").unwrap();
    for status in [401, 403, 404, 500] {
        let executor = Executor::response(status, b"denied", &[("x-ms-error-code", "Denied")]);
        let error = read_file(&client, &executor, &path, limits(4096, 4096), pending())
            .await
            .unwrap_err();
        assert!(matches!(
            error, RepresentationError::OneLake(OneLakeError::Http { status: actual, code: Some(code), .. })
                if actual == status && code == "Denied"
        ));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn file_input_audience_and_cancellation_fail_before_any_fallback_or_token() {
    let credential = Arc::new(Credential::default());
    let wrong = OneLakeClient::new(credential.clone(), selection(Audience::Fabric));
    let path = OneLakePath::new("ws", "item", "Files/a").unwrap();
    let executor = Executor::default();
    assert!(matches!(
        read_file(&wrong, &executor, &path, limits(4096, 4096), pending()).await,
        Err(RepresentationError::OneLake(OneLakeError::InvalidAudience))
    ));
    for invalid in [
        "../Files/a",
        "Files/../a",
        "Files\\a",
        "/Files/a",
        "Files/a?x=1",
    ] {
        assert!(OneLakePath::new("ws", "item", invalid).is_err());
    }
    let client = OneLakeClient::new(credential.clone(), selection(Audience::OneLake));
    assert!(matches!(
        read_file(&client, &executor, &path, limits(4096, 4096), ready(())).await,
        Err(RepresentationError::OneLake(OneLakeError::Cancelled))
    ));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
    let paused_executor = Executor {
        pause: true,
        ..Executor::default()
    };
    assert!(matches!(
        read_file(
            &client,
            &paused_executor,
            &path,
            limits(4096, 4096),
            paused_executor.started.notified()
        )
        .await,
        Err(RepresentationError::OneLake(OneLakeError::Cancelled))
    ));
    assert_eq!(paused_executor.dropped.load(Ordering::SeqCst), 1);
}
