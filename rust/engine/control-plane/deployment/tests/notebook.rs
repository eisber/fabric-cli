// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline local-files-to-service contracts; no credential discovery or network.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core::{Audience, CredentialSelection};
use fabric_deploy::{
    config,
    notebook::{
        DefinitionLimits, DeploymentAction, DeploymentError, LocalNotebook, PreparedNotebook,
    },
    transforms::{DEFAULT_GUID, NotebookTransforms},
};
use fabric_sdk::{
    Client,
    notebooks::{
        NotebookErrorKind, NotebookExecutionError, NotebookExecutor, NotebookLimits,
        NotebookRequest, NotebookResponse,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
const TOKEN: &str = "offline-only-not-a-real-token";
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new(ipynb: bool) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fabric-notebook-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join(".platform"),
            json!({
                "metadata": {"type": "Notebook", "displayName": "Daily.Load", "description": "Synthetic"},
                "config": {"logicalId": "logical-a"}
            }).to_string(),
        ).unwrap();
        if ipynb {
            fs::write(
                path.join("notebook-content.ipynb"),
                json!({
                    "cells": [], "nbformat": 4, "nbformat_minor": 5,
                    "metadata": {"workspaceId": DEFAULT_GUID}
                })
                .to_string(),
            )
            .unwrap();
        } else {
            fs::write(path.join("notebook-content.py"),
                format!("# Fabric notebook source\r\nworkspaceId = \"{DEFAULT_GUID}\"\r\nprint('logical-a source')\r\n")
            ).unwrap();
            fs::write(path.join("notebook-settings.json"), "{\"enabled\":true}").unwrap();
        }
        Self(path)
    }

    fn prepare(&self) -> PreparedNotebook {
        LocalNotebook::load(&self.0, DefinitionLimits::default())
            .unwrap()
            .prepare(
                WORKSPACE,
                &NotebookTransforms {
                    logical_ids: vec![("logical-a".to_owned(), ITEM.to_owned())],
                    literals: vec![("source".to_owned(), "target".to_owned())],
                },
            )
            .unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("clean up the exact test-created directory");
    }
}

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    fail: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, &[Audience::Fabric.scope()]);
        if self.fail {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "private provider details must not be displayed",
            ));
        }
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::new(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
    )
}

#[derive(Clone, Debug)]
struct Request {
    method: String,
    url: String,
    body: Vec<u8>,
}

enum Step {
    Reply(NotebookResponse),
    Error(NotebookExecutionError),
    Pending,
}

struct MockService {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<Request>>,
    waits: Mutex<Vec<Duration>>,
    calls: AtomicUsize,
    dropped: AtomicUsize,
}

struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl MockService {
    fn new(responses: impl IntoIterator<Item = NotebookResponse>) -> Self {
        Self::steps(responses.into_iter().map(Step::Reply))
    }

    fn steps(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            waits: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }
}

impl NotebookExecutor for MockService {
    fn execute<'a>(
        &'a self,
        request: NotebookRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<NotebookResponse, NotebookExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.token().token.secret(), TOKEN);
            assert_eq!(request.content_type(), "application/json");
            assert!(!format!("{request:?}").contains(TOKEN));
            assert!(!format!("{request:?}").contains("logical-a"));
            assert!(request.max_response_bytes().get() > 0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(Request {
                method: request.method().to_owned(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            match step {
                Step::Reply(response) => Ok(response),
                Step::Error(error) => Err(error),
                Step::Pending => {
                    let _guard = DropCounter(&self.dropped);
                    pending().await
                }
            }
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotebookExecutionError>> + Send + '_>> {
        Box::pin(async move {
            self.waits.lock().unwrap().push(delay);
            Ok(())
        })
    }
}

fn response(status: u16, value: Value) -> NotebookResponse {
    NotebookResponse {
        status,
        body: serde_json::to_vec(&value).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn page(items: Value) -> NotebookResponse {
    response(200, json!({"value": items}))
}

fn item(name: &str, item_type: &str) -> Value {
    json!({"id": ITEM, "displayName": name, "type": item_type})
}

fn accepted() -> NotebookResponse {
    let mut response = response(202, Value::Null);
    response.headers.insert(
        "Location".to_owned(),
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    response
        .headers
        .insert("x-ms-operation-id".to_owned(), OPERATION.to_owned());
    response
        .headers
        .insert("Retry-After".to_owned(), "0".to_owned());
    response
}

fn state(status: &str) -> NotebookResponse {
    let mut response = response(200, json!({"status": status}));
    response
        .headers
        .insert("Retry-After".to_owned(), "0".to_owned());
    response
}

fn service_kind(error: DeploymentError, mutated: bool) -> NotebookErrorKind {
    let DeploymentError::Service(error) = error else {
        panic!("expected service error: {error:?}")
    };
    assert_eq!(error.mutation_may_have_occurred, mutated);
    assert!(!error.to_string().contains("private provider"));
    error.kind
}

#[tokio::test(flavor = "current_thread")]
async fn local_git_notebook_transforms_encodes_creates_polls_and_fetches_actual_result() {
    let directory = Directory::new(false);
    let notebook = directory.prepare();
    let service = MockService::new([
        page(json!([])),
        accepted(),
        state("Running"),
        state("Succeeded"),
        response(200, item("Daily.Load", "Notebook")),
    ]);
    let credential = Arc::new(Credential::default());
    let outcome = notebook
        .deploy(
            &client(credential.clone(), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.action, DeploymentAction::Created);
    assert_eq!(outcome.item_id, ITEM);
    let requests = service.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 5);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].url,
        format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items")
    );
    assert_eq!(
        requests[4].url,
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}/result")
    );
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["displayName"], "Daily.Load");
    assert_eq!(body["type"], "Notebook");
    assert_eq!(body["description"], "Synthetic");
    assert!(body["definition"].get("format").is_none());
    let parts = body["definition"]["parts"].as_array().unwrap();
    assert_eq!(
        parts
            .iter()
            .map(|part| part["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [".platform", "notebook-content.py", "notebook-settings.json"]
    );
    assert_eq!(parts[1]["payloadType"], "InlineBase64");
    let decoded = |index: usize| {
        String::from_utf8(
            STANDARD
                .decode(parts[index]["payload"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    assert!(decoded(0).contains("logical-a")); // .platform is never transformed.
    assert_eq!(
        decoded(1),
        format!(
            "# Fabric notebook target\nworkspaceId = \"{WORKSPACE}\"\nprint('{ITEM} target')\n"
        )
    );
    assert_eq!(
        service.waits.lock().unwrap().as_slice(),
        &[Duration::ZERO, Duration::ZERO]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_follows_pages_then_updates_exact_name_and_type_with_metadata() {
    let directory = Directory::new(true);
    let mut first = page(json!([
        item("Daily.Load", "Report"),
        item("daily.load", "Notebook")
    ]));
    let body: Value = serde_json::from_slice(&first.body).unwrap();
    first.body = serde_json::to_vec(&json!({
        "value": body["value"], "continuationToken": "opaque+/= token",
        "continuationUri": format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?continuationToken=ignored")
    })).unwrap();
    let service = MockService::new([
        first,
        page(json!([item("Daily.Load", "Notebook")])),
        accepted(),
        state("Succeeded"),
    ]);
    let outcome = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.action, DeploymentAction::Updated);
    let requests = service.requests.lock().unwrap();
    assert!(
        requests[1]
            .url
            .ends_with("continuationToken=opaque%2B%2F%3D+token")
    );
    assert_eq!(
        requests[2].url,
        format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}/updateDefinition?updateMetadata=True"
        )
    );
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(body["definition"]["format"], "ipynb");
    assert!(body.get("displayName").is_none());
    assert_eq!(requests.len(), 4); // Update needs terminal success, not a create-result object.
}

#[tokio::test(flavor = "current_thread")]
async fn immediate_create_and_update_require_documented_success_status() {
    for update in [false, true] {
        let directory = Directory::new(false);
        let existing = if update {
            json!([item("Daily.Load", "Notebook")])
        } else {
            json!([])
        };
        let service = MockService::new([
            page(existing),
            if update {
                response(200, Value::Null)
            } else {
                response(201, item("Daily.Load", "Notebook"))
            },
        ]);
        let outcome = directory
            .prepare()
            .deploy(
                &client(Arc::new(Credential::default()), Audience::Fabric),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.action,
            if update {
                DeploymentAction::Updated
            } else {
                DeploymentAction::Created
            }
        );
        assert!(service.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_target_never_mutates() {
    let directory = Directory::new(false);
    let service = MockService::new([page(json!([
        item("Daily.Load", "Notebook"),
        item("Daily.Load", "Notebook")
    ]))]);
    let result = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await;
    assert!(matches!(result, Err(DeploymentError::AmbiguousTarget)));
    assert_eq!(service.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_without_operation_evidence_is_incomplete_not_deployed() {
    let directory = Directory::new(false);
    let service = MockService::new([page(json!([])), response(202, Value::Null)]);
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        service_kind(error, true),
        NotebookErrorKind::InvalidResponse
    );
    assert_eq!(service.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn foreign_operation_link_is_rejected_without_sending_token_to_it() {
    let directory = Directory::new(false);
    let mut reply = accepted();
    reply.headers.insert(
        "Location".to_owned(),
        format!("https://untrusted.example/v1/operations/{OPERATION}"),
    );
    let service = MockService::new([page(json!([])), reply]);
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, true), NotebookErrorKind::InvalidLink);
    assert_eq!(service.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn failed_cancelled_and_unknown_operation_states_never_succeed() {
    for status in ["Failed", "Cancelled", "FutureState", "Undefined"] {
        let directory = Directory::new(false);
        let service = MockService::new([
            page(json!([])),
            accepted(),
            response(
                200,
                json!({"status": status, "error": {"errorCode": "SyntheticFailure", "message": "synthetic"}}),
            ),
        ]);
        let error = directory
            .prepare()
            .deploy(
                &client(Arc::new(Credential::default()), Audience::Fabric),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        let DeploymentError::Service(error) = error else {
            panic!("expected service error")
        };
        assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
        assert!(error.mutation_may_have_occurred);
        match status {
            "Failed" | "Cancelled" => {
                let NotebookErrorKind::OperationFailed {
                    status: actual,
                    error: details,
                } = error.kind
                else {
                    panic!("terminal failure lost")
                };
                assert_eq!(actual, status);
                assert_eq!(details.unwrap().error_code, "SyntheticFailure");
            }
            _ => assert_eq!(error.kind, NotebookErrorKind::InvalidResponse),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lost_post_response_is_not_retried_and_preserves_uncertainty() {
    let directory = Directory::new(false);
    let service = MockService::steps([
        Step::Reply(page(json!([]))),
        Step::Error(NotebookExecutionError::TimedOut),
    ]);
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        service_kind(error, true),
        NotebookErrorKind::Execution(NotebookExecutionError::TimedOut)
    );
    assert_eq!(service.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_start_acquires_no_credential_or_transport() {
    let directory = Directory::new(false);
    let credential = Arc::new(Credential::default());
    let service = MockService::new([]);
    let error = directory
        .prepare()
        .deploy(
            &client(credential.clone(), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, false), NotebookErrorKind::Cancelled);
    assert_eq!(service.calls.load(Ordering::SeqCst), 0);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_drops_inflight_mutation_without_claiming_rollback() {
    let directory = Directory::new(false);
    let service = MockService::steps([Step::Reply(page(json!([]))), Step::Pending]);
    let cancelled = poll_fn(|cx| {
        if service.calls.load(Ordering::SeqCst) >= 2 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            cancelled,
        )
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, true), NotebookErrorKind::Cancelled);
    assert_eq!(service.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn request_bound_stops_running_operation_and_retains_handle() {
    let directory = Directory::new(false);
    let service = MockService::new([page(json!([])), accepted(), state("Running")]);
    let limits = NotebookLimits {
        max_requests: NonZeroUsize::new(3).unwrap(),
        ..NotebookLimits::default()
    };
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            limits,
            pending(),
        )
        .await
        .unwrap_err();
    let DeploymentError::Service(error) = error else {
        panic!("expected service failure")
    };
    assert_eq!(error.kind, NotebookErrorKind::Limit);
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(service.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_http_malformed_or_foreign_continuation_never_becomes_create() {
    for reply in [
        response(403, json!({"errorCode":"Forbidden","message":"synthetic"})),
        response(200, json!({})),
        response(
            200,
            json!({"value": [], "continuationToken": "next",
            "continuationUri": "https://untrusted.example/items"}),
        ),
    ] {
        let directory = Directory::new(false);
        let service = MockService::new([reply]);
        let error = directory
            .prepare()
            .deploy(
                &client(Arc::new(Credential::default()), Audience::Fabric),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            service_kind(error, false),
            NotebookErrorKind::Http { .. }
                | NotebookErrorKind::InvalidResponse
                | NotebookErrorKind::InvalidLink
        ));
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn response_item_and_wait_limits_are_enforced_at_exact_boundaries() {
    let directory = Directory::new(false);
    let notebook = directory.prepare();
    let c = client(Arc::new(Credential::default()), Audience::Fabric);
    let page = page(json!([]));
    let bytes = page.body.len();
    let exact = NotebookLimits {
        max_response_bytes: NonZeroUsize::new(bytes).unwrap(),
        max_requests: NonZeroUsize::new(1).unwrap(),
        ..NotebookLimits::default()
    };
    let service = MockService::new([page.clone()]);
    let error = notebook
        .deploy(&c, &service, exact, pending())
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, false), NotebookErrorKind::Limit); // Discovery decoded, POST denied by request limit.
    let service = MockService::new([page]);
    let error = notebook
        .deploy(
            &c,
            &service,
            NotebookLimits {
                max_response_bytes: NonZeroUsize::new(bytes - 1).unwrap(),
                ..exact
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, false), NotebookErrorKind::Limit);
    let service = MockService::new([response(
        200,
        json!({"value":[item("one","Notebook"),item("two","Notebook")]}),
    )]);
    let error = notebook
        .deploy(
            &c,
            &service,
            NotebookLimits {
                max_items: NonZeroUsize::new(1).unwrap(),
                ..NotebookLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, false), NotebookErrorKind::Limit);
    let mut reply = accepted();
    reply
        .headers
        .insert("Retry-After".to_owned(), "61".to_owned());
    let service = MockService::new([response(200, json!({"value":[]})), reply]);
    let error = notebook
        .deploy(&c, &service, NotebookLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(service_kind(error, true), NotebookErrorKind::Limit);
    assert!(service.waits.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn identity_failures_do_not_fall_back_or_send_requests() {
    let directory = Directory::new(false);
    for (audience, fail, expected) in [
        (Audience::Arm, false, NotebookErrorKind::InvalidAudience),
        (Audience::Fabric, true, NotebookErrorKind::Credential),
    ] {
        let service = MockService::new([]);
        let credential = Arc::new(Credential {
            fail,
            ..Credential::default()
        });
        let error = directory
            .prepare()
            .deploy(
                &client(credential, audience),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(service_kind(error, false), expected);
        assert_eq!(service.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn malformed_unsupported_and_oversized_local_files_fail_explicitly() {
    let directory = Directory::new(false);
    let bytes = fs::read_dir(&directory.0)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len() as usize)
        .sum();
    assert!(
        LocalNotebook::load(
            &directory.0,
            DefinitionLimits {
                max_bytes: NonZeroUsize::new(bytes).unwrap()
            }
        )
        .is_ok()
    );
    assert!(matches!(
        LocalNotebook::load(
            &directory.0,
            DefinitionLimits {
                max_bytes: NonZeroUsize::new(bytes - 1).unwrap()
            }
        ),
        Err(DeploymentError::Limit)
    ));
    fs::write(directory.0.join("parameter.yml"), "find_replace: []").unwrap();
    assert!(matches!(
        LocalNotebook::load(&directory.0, DefinitionLimits::default()),
        Err(DeploymentError::UnsupportedContent)
    ));
    fs::remove_file(directory.0.join("parameter.yml")).unwrap();
    fs::write(directory.0.join(".platform"), "{}").unwrap();
    assert!(matches!(
        LocalNotebook::load(&directory.0, DefinitionLimits::default()),
        Err(DeploymentError::InvalidInput)
    ));
}

#[test]
fn rejects_mixed_notebook_formats_and_invalid_transformed_json() {
    let directory = Directory::new(true);
    let loaded = LocalNotebook::load(&directory.0, DefinitionLimits::default()).unwrap();
    assert!(matches!(
        loaded.prepare(
            WORKSPACE,
            &NotebookTransforms {
                literals: vec![("cells".to_owned(), "\"".to_owned())],
                ..NotebookTransforms::default()
            }
        ),
        Err(DeploymentError::InvalidInput)
    ));
    fs::write(directory.0.join("notebook-content.py"), "print('source')").unwrap();
    assert!(matches!(
        LocalNotebook::load(&directory.0, DefinitionLimits::default()),
        Err(DeploymentError::UnsupportedContent)
    ));
}

#[test]
fn logical_literal_and_workspace_transforms_preserve_reference_cases_and_bounds() {
    let document: Value =
        serde_json::from_str(include_str!("../../../../tests/fixtures/deploy/ids.json")).unwrap();
    let mut checked = 0;
    for case in document["cases"].as_array().unwrap() {
        if case["operation"] != "logical"
            && case["id"] != "workspace-default-only-recognized-case-sensitive-keys"
        {
            continue;
        }
        let transforms = NotebookTransforms {
            logical_ids: case["items"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            (
                                item["logical_id"].as_str().unwrap().to_owned(),
                                item["guid"].as_str().unwrap().to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            literals: Vec::new(),
        };
        let result = transforms.apply(case["input"].as_str().unwrap(), WORKSPACE, 4096);
        if case.get("error").is_some() {
            assert!(matches!(result, Err(DeploymentError::UndeployedLogicalId)));
        } else {
            assert_eq!(
                result.unwrap(),
                case["expected"].as_str().unwrap(),
                "{}",
                case["id"]
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 3);
    let literal = NotebookTransforms {
        literals: vec![("a".to_owned(), "xyz".to_owned())],
        ..NotebookTransforms::default()
    };
    assert_eq!(literal.apply("aa", WORKSPACE, 6).unwrap(), "xyzxyz");
    assert!(matches!(
        literal.apply("aa", WORKSPACE, 5),
        Err(DeploymentError::Limit)
    ));
}

#[test]
fn consumes_all_five_immutable_config_cases_without_changing_types() {
    let document: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/deploy/config.json"
    ))
    .unwrap();
    let cases = document["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for case in cases {
        let environment = case["environment"].as_str().unwrap_or("dev");
        let actual = match case["operation"].as_str().unwrap() {
            "config_value" => config::get_config_value(
                &case["section"],
                case["key"].as_str().unwrap(),
                environment,
            )
            .unwrap()
            .cloned()
            .unwrap_or(Value::Null),
            "workspace_settings" => Value::Object(
                config::extract_workspace_settings(&case["config"], environment).unwrap(),
            ),
            "environment_key" => {
                let mut input = case["input"].as_object().unwrap().clone();
                config::process_environment_key(environment, &mut input);
                Value::Object(input)
            }
            _ => panic!("unexpected immutable case"),
        };
        assert_eq!(actual, case["expected"], "{}", case["id"]);
    }
    assert!(config::extract_workspace_settings(&Value::Null, "dev").is_err());
    assert!(config::get_config_value(&Value::Null, "workspace", "dev").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_pagination_token_never_reaches_a_mutation() {
    let directory = Directory::new(false);
    let service = MockService::new([
        response(200, json!({"value": [], "continuationToken": "repeat"})),
        response(200, json!({"value": [], "continuationToken": "repeat"})),
    ]);
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        service_kind(error, false),
        NotebookErrorKind::RepeatedContinuation
    );
    assert_eq!(service.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_create_result_cannot_be_reported_as_a_deployment() {
    for body in [
        json!({}),
        item("Daily.Load", "Report"),
        item("Other", "Notebook"),
    ] {
        let directory = Directory::new(false);
        let service = MockService::new([page(json!([])), response(201, body)]);
        let error = directory
            .prepare()
            .deploy(
                &client(Arc::new(Credential::default()), Audience::Fabric),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            service_kind(error, true),
            NotebookErrorKind::InvalidResponse
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_service_error_fields_do_not_erase_the_error_code() {
    let directory = Directory::new(false);
    let service = MockService::new([response(403, json!({"errorCode": "NotebookDenied"}))]);
    let error = directory
        .prepare()
        .deploy(
            &client(Arc::new(Credential::default()), Audience::Fabric),
            &service,
            NotebookLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        service_kind(error, false),
        NotebookErrorKind::Http {
            status: 403,
            code: Some("NotebookDenied".to_owned()),
            error: None,
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn operation_links_cannot_change_resource_origin_or_id() {
    for uri in [
        format!("http://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
        format!("https://api.fabric.microsoft.com:444/v1/operations/{OPERATION}"),
        format!("https://user@api.fabric.microsoft.com/v1/operations/{OPERATION}"),
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}?unexpected=true"),
        format!("https://api.fabric.microsoft.com/v1/operations/{ITEM}"),
    ] {
        let directory = Directory::new(false);
        let mut reply = accepted();
        reply.headers.insert("Location".to_owned(), uri);
        let service = MockService::new([page(json!([])), reply]);
        let error = directory
            .prepare()
            .deploy(
                &client(Arc::new(Credential::default()), Audience::Fabric),
                &service,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(service_kind(error, true), NotebookErrorKind::InvalidLink);
        assert_eq!(service.calls.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn unsafe_part_paths_are_rejected_and_deployment_future_is_send() {
    for path in ["", "/absolute", "../parent", "a\\b", "C:drive", "a//b"] {
        assert!(fabric_sdk::notebooks::DefinitionPart::inline(path, b"data").is_err());
    }
    fn assert_send(_: impl Future + Send) {}
    let directory = Directory::new(false);
    let notebook = directory.prepare();
    let c = client(Arc::new(Credential::default()), Audience::Fabric);
    let service = MockService::new([]);
    assert_send(notebook.deploy(&c, &service, NotebookLimits::default(), pending()));
}

#[tokio::test(flavor = "current_thread")]
async fn completed_cancellation_future_is_not_polled_again_on_session_reuse() {
    let c = client(Arc::new(Credential::default()), Audience::Fabric);
    let service = MockService::new([]);
    let mut session = c.notebook_session(&service, NotebookLimits::default(), ready(()));
    for _ in 0..2 {
        assert_eq!(
            session.list_items(WORKSPACE).await.unwrap_err().kind,
            NotebookErrorKind::Cancelled
        );
    }
}
