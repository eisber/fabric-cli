// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline consumers of the public DataAgent read bindings.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    dataagents::{
        ContinuationToken, DataAgentError, DataAgentRequests, DataAgentTarget, NativeFormat,
        ReadPermission, Stage, register_reads,
    },
    representations::{DefinitionRead, RepresentationError, RepresentationLimits},
    v2::{Availability, ContractError, Effect, Registry, Target, TargetKind, Verb},
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
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

const WS: &str = "11111111-2222-3333-4444-555555555555";
const AGENT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SOURCE: &str = "11111111-aaaa-bbbb-cccc-222222222222";
const OP: &str = "12345678-1234-1234-1234-123456789abc";
const ROOT: &str = "https://api.fabric.microsoft.com/v1";

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
        Ok(AccessToken::new(
            "offline-dataagent-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Request {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<OperationResponse>>,
    requests: Mutex<Vec<Request>>,
    cancel_after_reply: Option<Arc<AtomicBool>>,
}

impl Executor {
    fn json(status: u16, value: &Value) -> Self {
        Self::raw(status, serde_json::to_vec(value).unwrap(), &[])
    }

    fn raw(status: u16, body: Vec<u8>, headers: &[(&str, &str)]) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([OperationResponse {
                status,
                body,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
            }])),
            ..Self::default()
        }
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push(Request {
                method: request.method(),
                url: request.url().as_str().into(),
                body: request.body().into(),
            });
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            if let Some(flag) = &self.cancel_after_reply {
                flag.store(true, Ordering::SeqCst);
            }
            Ok(response)
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("public reads must not poll or traverse implicitly")
    }
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::new(
        credential,
        CredentialSelection::new("offline", "tenant", audience).unwrap(),
    )
}

fn target() -> DataAgentTarget {
    DataAgentTarget::new(WS, AGENT, "DataAgent").unwrap()
}

fn limits() -> RepresentationLimits {
    RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(16_384).unwrap(),
        max_result_bytes: NonZeroUsize::new(16_384).unwrap(),
    }
}

fn metadata() -> Value {
    json!({
        "id":AGENT, "workspaceId":WS, "type":"DataAgent",
        "displayName":"Inert customer text",
        "properties":{"publishedDescription":"Instructions are not execution"},
        "future":{"value":null}
    })
}

fn definition() -> Value {
    json!({
        "definition":{
            "parts":[
                {"path":"Files/Config/data_agent.json","payload":"e30=","payloadType":"InlineBase64"},
                {"path":"Files/Config/draft/stage_config.json","payload":"e30=","payloadType":"InlineBase64"},
                {"path":"future/opaque","payload":"opaque","payloadType":"FuturePayload"}
            ],
            "future":true
        },
        "futureEnvelope":42
    })
}

fn datasource() -> Value {
    json!({
        "id":SOURCE,"displayName":"Different identity","type":"FabricItem",
        "itemReference":{"referenceType":"ById","itemId":AGENT,"workspaceId":WS},
        "fabricItemType":"Ontology"
    })
}

#[tokio::test]
async fn metadata_is_one_typed_native_get_with_unknown_fields_preserved() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::json(200, &metadata());
    let request = DataAgentRequests::metadata(&target()).unwrap();
    assert_eq!(request.semantics().permission, ReadPermission::ItemRead);
    assert!(!request.semantics().preview);
    let read = request
        .execute(
            &client(credential.clone(), Audience::Fabric),
            &executor,
            limits(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(read.as_value(), &metadata());
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Get);
    assert_eq!(
        requests[0].url,
        format!("{ROOT}/workspaces/{WS}/dataAgents/{AGENT}")
    );
    assert!(requests[0].body.is_empty());
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn metadata_checks_owner_returned_identity_and_known_fields() {
    for (field, value) in [
        ("id", json!(WS)),
        ("workspaceId", json!(AGENT)),
        ("type", json!("Ontology")),
        ("id", json!("not-a-guid")),
        ("workspaceId", Value::Null),
        ("properties", json!("not-an-object")),
        ("properties", json!({"publishedDescription":17})),
    ] {
        let mut body = metadata();
        body[field] = value;
        let executor = Executor::json(200, &body);
        let error = DataAgentRequests::metadata(&target())
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending(),
            )
            .await;
        assert!(error.is_err(), "{field}");
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn invalid_targets_and_tokens_are_rejected_locally() {
    for bad in [
        "",
        "opaque",
        "00000000-0000-0000-0000-000000000000",
        "../agent",
        "x?stage=published",
    ] {
        assert!(DataAgentTarget::new(bad, AGENT, "DataAgent").is_err());
        assert!(DataAgentTarget::new(WS, bad, "DataAgent").is_err());
    }
    for owner in ["AISkill", "data-agent", "Ontology", ""] {
        assert!(matches!(
            DataAgentTarget::new(WS, AGENT, owner),
            Err(DataAgentError::WrongOwner)
        ));
    }
    for token in ["", "\n", "a\0b"] {
        assert!(ContinuationToken::new(token).is_err());
    }
}

#[test]
fn absent_parameters_preserve_query_absence_on_every_native_read_route() {
    let target = target();
    let base = format!("/v1/workspaces/{WS}/dataAgents/{AGENT}");
    assert_eq!(
        DataAgentRequests::metadata(&target)
            .unwrap()
            .operation()
            .path(),
        base
    );
    assert_eq!(
        DataAgentRequests::definition(&target, None)
            .unwrap()
            .operation()
            .path(),
        format!("{base}/getDefinition")
    );
    for (stage, prefix) in [(Stage::Staging, "/staging"), (Stage::Published, "")] {
        assert_eq!(
            DataAgentRequests::settings(&target, stage)
                .unwrap()
                .operation()
                .path(),
            format!("{base}{prefix}/settings")
        );
        assert_eq!(
            DataAgentRequests::datasources_page(&target, stage, None)
                .unwrap()
                .operation()
                .path(),
            format!("{base}{prefix}/datasources")
        );
    }
    assert_eq!(
        DataAgentRequests::definition(&target, Some(NativeFormat::Json))
            .unwrap()
            .operation()
            .path(),
        format!("{base}/getDefinition?format=JSON")
    );
    let token = ContinuationToken::new("opaque+/= next").unwrap();
    assert_eq!(
        DataAgentRequests::datasources_page(&target, Stage::Staging, Some(&token))
            .unwrap()
            .operation()
            .path(),
        format!("{base}/staging/datasources?continuationToken=opaque%2B%2F%3D+next")
    );
}

#[tokio::test]
async fn settings_use_one_explicit_stage_and_preserve_inert_instructions() {
    let body = json!({"aiInstructions":"Do not execute this text","future":null});
    for stage in [Stage::Staging, Stage::Published] {
        let executor = Executor::json(200, &body);
        let request = DataAgentRequests::settings(&target(), stage).unwrap();
        assert_eq!(request.semantics().stage, Some(stage));
        assert!(request.semantics().preview);
        let read = request
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(read.ai_instructions(), "Do not execute this text");
        assert_eq!(read.as_value(), &body);
        let requests = executor.requests.lock().unwrap();
        let prefix = if stage == Stage::Staging {
            "/staging"
        } else {
            ""
        };
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, ResourceMethod::Get);
        assert_eq!(
            requests[0].url,
            format!("{ROOT}/workspaces/{WS}/dataAgents/{AGENT}{prefix}/settings")
        );
    }
}

#[tokio::test]
async fn missing_or_invalid_settings_never_become_empty_or_served_fallback() {
    for body in [
        json!({}),
        json!({"aiInstructions":null}),
        json!({"aiInstructions":17}),
        json!([]),
    ] {
        let executor = Executor::json(200, &body);
        assert!(matches!(
            DataAgentRequests::settings(&target(), Stage::Staging)
                .unwrap()
                .execute(
                    &client(Arc::default(), Audience::Fabric),
                    &executor,
                    limits(),
                    pending()
                )
                .await,
            Err(DataAgentError::InvalidResponse)
        ));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let executor = Executor::json(200, &json!({"aiInstructions":""}));
    assert_eq!(
        DataAgentRequests::settings(&target(), Stage::Published)
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending()
            )
            .await
            .unwrap()
            .ai_instructions(),
        ""
    );
}

#[tokio::test]
async fn exact_native_definition_preserves_parts_and_real_format_choice() {
    for format in [None, Some(NativeFormat::Json)] {
        let executor = Executor::json(200, &definition());
        let request = DataAgentRequests::definition(&target(), format).unwrap();
        assert_eq!(
            request.semantics().permission,
            ReadPermission::DefinitionReadWrite
        );
        let read = request
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            read,
            DefinitionRead::Completed {
                definition: definition()
            }
        );
        assert_eq!(
            read.part_bytes("Files/Config/draft/stage_config.json")
                .unwrap(),
            b"{}"
        );
        assert!(matches!(
            read.part_bytes("future/opaque"),
            Err(RepresentationError::Unsupported)
        ));
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, ResourceMethod::Post);
        let suffix = if format.is_some() { "?format=JSON" } else { "" };
        assert_eq!(
            requests[0].url,
            format!("{ROOT}/workspaces/{WS}/dataAgents/{AGENT}/getDefinition{suffix}")
        );
        assert!(requests[0].body.is_empty());
    }
}

#[tokio::test]
async fn definition_acceptance_keeps_the_shared_observer_handle_without_polling() {
    let location = format!("{ROOT}/operations/{OP}");
    let executor = Executor::raw(
        202,
        vec![],
        &[
            ("Location", &location),
            ("x-ms-operation-id", OP),
            ("Retry-After", "30"),
        ],
    );
    let read = DataAgentRequests::definition(&target(), None)
        .unwrap()
        .execute(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            limits(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        read,
        DefinitionRead::Accepted {
            operation_id: OP.into()
        }
    );
    assert!(read.accepted_handle().is_some());
    assert!(matches!(
        read.part_bytes("anything"),
        Err(RepresentationError::NotCompleted)
    ));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn definition_rejects_missing_handles_foreign_links_and_invalid_envelopes() {
    let client = client(Arc::default(), Audience::Fabric);
    for executor in [
        Executor::raw(202, vec![], &[]),
        Executor::raw(
            202,
            vec![],
            &[
                ("x-ms-operation-id", OP),
                ("Location", "https://other.invalid/v1/operations/x"),
            ],
        ),
        Executor::json(200, &metadata()),
        Executor::json(200, &json!({"definition":{"parts":[{"path":17}]}})),
    ] {
        assert!(
            DataAgentRequests::definition(&target(), None)
                .unwrap()
                .execute(&client, &executor, limits(), pending())
                .await
                .is_err()
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn permission_service_and_undocumented_status_errors_do_not_fall_back() {
    for status in [401, 403, 404, 429, 500, 201, 204] {
        let executor = Executor::json(
            status,
            &json!({"errorCode":"Denied","message":"inert service data"}),
        );
        let error = DataAgentRequests::metadata(&target())
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending(),
            )
            .await;
        assert!(error.is_err());
        if matches!(status, 401 | 403 | 404 | 429 | 500) {
            assert!(matches!(error, Err(DataAgentError::Operation(ref error))
                if matches!(error.kind, OperationErrorKind::Http { status:s, .. } if s == status)));
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let executor = Executor::json(
        403,
        &json!({"errorCode":"Forbidden","message":"source denied"}),
    );
    assert!(matches!(
        DataAgentRequests::definition(&target(), None)
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending()
            )
            .await,
        Err(DataAgentError::Operation(_))
    ));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stage_datasource_pages_keep_management_and_underlying_item_identity_separate() {
    let body =
        json!({"value":[datasource()],"continuationToken":"opaque+/= next","continuationUri":null});
    let first = Executor::json(200, &body);
    let client = client(Arc::default(), Audience::Fabric);
    let page = DataAgentRequests::datasources_page(&target(), Stage::Published, None)
        .unwrap()
        .execute(&client, &first, limits(), pending())
        .await
        .unwrap();
    assert!(!page.is_complete());
    assert_eq!(page.page.value[0].id(), SOURCE);
    assert_eq!(page.page.value[0].as_value(), &datasource());
    assert_eq!(first.requests.lock().unwrap().len(), 1);
    let next = Executor::json(200, &json!({"value":[]}));
    let last =
        DataAgentRequests::datasources_page(&target(), Stage::Published, page.next_token.as_ref())
            .unwrap()
            .execute(&client, &next, limits(), pending())
            .await
            .unwrap();
    assert!(last.is_complete());
    assert_eq!(
        next.requests.lock().unwrap()[0].url,
        format!(
            "{ROOT}/workspaces/{WS}/dataAgents/{AGENT}/datasources?continuationToken=opaque%2B%2F%3D+next"
        )
    );
}

#[tokio::test]
async fn datasource_invalid_pages_and_unexpected_acceptance_are_not_complete_empty() {
    for body in [
        json!({"value":{}}),
        json!({"value":[],"continuationToken":""}),
        json!({"value":[],"continuationUri":""}),
        json!({"value":[{}]}),
        json!({"value":[],"continuationToken":true}),
    ] {
        let executor = Executor::json(200, &body);
        assert!(
            DataAgentRequests::datasources_page(&target(), Stage::Staging, None)
                .unwrap()
                .execute(
                    &client(Arc::default(), Audience::Fabric),
                    &executor,
                    limits(),
                    pending()
                )
                .await
                .is_err()
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let executor = Executor::raw(202, vec![], &[("x-ms-operation-id", OP)]);
    assert!(
        DataAgentRequests::datasources_page(&target(), Stage::Published, None)
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits(),
                pending()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn datasource_pages_require_native_object_envelopes_not_positional_arrays() {
    let client = client(Arc::default(), Audience::Fabric);
    for stage in [Stage::Staging, Stage::Published] {
        for body in [
            json!([[], null, null]),
            json!([[datasource()], null, null]),
            json!([[], "next", null]),
            json!(null),
        ] {
            let executor = Executor::json(200, &body);
            assert!(
                matches!(
                    DataAgentRequests::datasources_page(&target(), stage, None)
                        .unwrap()
                        .execute(&client, &executor, limits(), pending())
                        .await,
                    Err(DataAgentError::InvalidResponse)
                ),
                "nonobject page was accepted: {body}"
            );
            assert_eq!(executor.requests.lock().unwrap().len(), 1);
        }
        for body in [
            json!({"value":[]}),
            json!({"value":[],"continuationToken":null,"continuationUri":null,"future":42}),
        ] {
            let executor = Executor::json(200, &body);
            assert!(
                DataAgentRequests::datasources_page(&target(), stage, None)
                    .unwrap()
                    .execute(&client, &executor, limits(), pending())
                    .await
                    .unwrap()
                    .is_complete()
            );
        }
    }
}

fn decode_checkpoint_signal(
    response_received: Arc<AtomicBool>,
    after_response_polls: Arc<AtomicUsize>,
) -> impl Future<Output = ()> + Send {
    poll_fn(move |_| {
        if response_received.load(Ordering::SeqCst)
            && after_response_polls.fetch_add(1, Ordering::SeqCst) >= 2
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn original_signal_is_observed_after_decode_for_every_public_read() {
    let client = client(Arc::default(), Audience::Fabric);
    for kind in [
        "metadata",
        "settings-staging",
        "settings-published",
        "datasources",
        "definition",
    ] {
        let response_received = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let body = match kind {
            "metadata" => metadata(),
            "definition" => definition(),
            "datasources" => json!({"value":[datasource()]}),
            _ => json!({"aiInstructions":"inert stored source"}),
        };
        let mut executor = Executor::json(200, &body);
        executor.cancel_after_reply = Some(response_received.clone());
        let cancelled = decode_checkpoint_signal(response_received, polls.clone());
        // The first two post-response checkpoints cover transport and the
        // existing pre-decode check. The same signal becomes ready at the
        // final decode/accounting checkpoint, without sleeping or a new timer.
        let result = match kind {
            "metadata" => DataAgentRequests::metadata(&target())
                .unwrap()
                .execute(&client, &executor, limits(), cancelled)
                .await
                .map(|_| ()),
            "definition" => DataAgentRequests::definition(&target(), None)
                .unwrap()
                .execute(&client, &executor, limits(), cancelled)
                .await
                .map(|_| ()),
            "datasources" => DataAgentRequests::datasources_page(&target(), Stage::Staging, None)
                .unwrap()
                .execute(&client, &executor, limits(), cancelled)
                .await
                .map(|_| ()),
            _ => DataAgentRequests::settings(
                &target(),
                if kind == "settings-staging" {
                    Stage::Staging
                } else {
                    Stage::Published
                },
            )
            .unwrap()
            .execute(&client, &executor, limits(), cancelled)
            .await
            .map(|_| ()),
        };
        let Err(DataAgentError::Response(error)) = result else {
            panic!("{kind} returned no final-checkpoint response evidence");
        };
        assert_eq!(error.error.kind, OperationErrorKind::Cancelled, "{kind}");
        assert_eq!(error.response.status, 200);
        assert_eq!(error.response.body, serde_json::to_vec(&body).unwrap());
        assert_eq!(error.error.operation_id, None);
        assert_eq!(error.error.mutation_may_have_occurred, kind == "definition");
        assert!(!format!("{error:?}").contains("inert"));
        assert_eq!(polls.load(Ordering::SeqCst), 3, "{kind}");
        assert_eq!(executor.requests.lock().unwrap().len(), 1, "{kind}");
    }
}

#[tokio::test]
async fn final_signal_retains_real_accepted_definition_handle_and_response() {
    let response_received = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let location = format!("{ROOT}/operations/{OP}");
    let mut executor = Executor::raw(
        202,
        vec![],
        &[
            ("Location", &location),
            ("x-ms-operation-id", OP),
            ("Retry-After", "30"),
        ],
    );
    executor.cancel_after_reply = Some(response_received.clone());
    let error = DataAgentRequests::definition(&target(), None)
        .unwrap()
        .execute(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            limits(),
            decode_checkpoint_signal(response_received, polls.clone()),
        )
        .await
        .unwrap_err();
    let DataAgentError::Response(error) = error else {
        panic!("accepted definition lost post-response evidence");
    };
    assert_eq!(error.error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.error.operation_id.as_deref(), Some(OP));
    assert!(error.error.mutation_may_have_occurred);
    assert_eq!(error.response.status, 202);
    assert_eq!(error.response.headers["Location"], location);
    assert_eq!(error.response.headers["x-ms-operation-id"], OP);
    assert_eq!(polls.load(Ordering::SeqCst), 3);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn result_accounting_failure_preserves_the_bounded_observed_response() {
    let body = serde_json::to_vec(&metadata()).unwrap();
    let executor = Executor::raw(
        200,
        body.clone(),
        &[("x-ms-request-id", "observed-request")],
    );
    let mut bounds = limits();
    bounds.max_result_bytes = NonZeroUsize::new(body.len() - 1).unwrap();
    let error = DataAgentRequests::metadata(&target())
        .unwrap()
        .execute(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            bounds,
            pending(),
        )
        .await
        .unwrap_err();
    let DataAgentError::Response(error) = error else {
        panic!("result budget failure lost bounded response evidence");
    };
    assert_eq!(error.error.kind, OperationErrorKind::Limit);
    assert_eq!(error.response.status, 200);
    assert_eq!(error.response.body, body);
    assert_eq!(
        error.response.headers["x-ms-request-id"],
        "observed-request"
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn finite_byte_bounds_fail_at_n_plus_one_without_truncating_metadata() {
    let body = serde_json::to_vec(&metadata()).unwrap();
    for bound in [body.len(), body.len() - 1] {
        let executor = Executor::raw(200, body.clone(), &[]);
        let mut limits = limits();
        limits.max_response_bytes = NonZeroUsize::new(bound).unwrap();
        limits.max_result_bytes = NonZeroUsize::new(bound).unwrap();
        let read = DataAgentRequests::metadata(&target())
            .unwrap()
            .execute(
                &client(Arc::default(), Audience::Fabric),
                &executor,
                limits,
                pending(),
            )
            .await;
        assert_eq!(read.is_ok(), bound == body.len());
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_audience_and_prior_cancellation_have_zero_effects() {
    for audience in [Audience::OneLake, Audience::Arm] {
        let credential = Arc::new(Credential::default());
        let executor = Executor::default();
        assert!(
            DataAgentRequests::metadata(&target())
                .unwrap()
                .execute(
                    &client(credential.clone(), audience),
                    &executor,
                    limits(),
                    pending()
                )
                .await
                .is_err()
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    assert!(
        DataAgentRequests::settings(&target(), Stage::Staging)
            .unwrap()
            .execute(
                &client(credential.clone(), Audience::Fabric),
                &executor,
                limits(),
                ready(())
            )
            .await
            .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn post_dispatch_cancellation_never_delivers_a_completed_result_or_replays() {
    let flag = Arc::new(AtomicBool::new(false));
    let mut executor = Executor::json(200, &metadata());
    executor.cancel_after_reply = Some(flag.clone());
    let cancelled = poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    assert!(matches!(DataAgentRequests::metadata(&target()).unwrap()
        .execute(&client(Arc::default(), Audience::Fabric), &executor, limits(), cancelled).await,
        Err(DataAgentError::Operation(ref error)) if matches!(error.kind, OperationErrorKind::Cancelled)));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn selected_fabric_origin_is_preserved_for_reads_and_definition_acceptance() {
    let credential = Arc::new(Credential::default());
    let context = RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    );
    let client = Client::with_context(context);
    let executor = Executor::json(200, &metadata());
    DataAgentRequests::metadata(&target())
        .unwrap()
        .execute(&client, &executor, limits(), pending())
        .await
        .unwrap();
    assert!(
        executor.requests.lock().unwrap()[0]
            .url
            .starts_with("https://dailyapi.fabric.microsoft.com/v1/")
    );
}

#[test]
fn registry_facade_resolves_the_same_operation_and_keeps_unsupported_views_absent() {
    let mut registry = Registry::default();
    register_reads(&mut registry).unwrap();
    for (facet, verb) in [
        ("metadata", Verb::Get),
        ("definition", Verb::Get),
        ("settings-staging", Verb::Get),
        ("settings-published", Verb::Get),
        ("datasources-staging", Verb::List),
        ("datasources-published", Verb::List),
    ] {
        let target = Target {
            kind: TargetKind::Item,
            owner_type: "DataAgent".into(),
            facet: facet.into(),
            representation: "json".into(),
            generation: None,
        };
        let generic = registry.resolve(verb, &target, None).unwrap();
        let facade = registry.resolve(verb, &target, Some("data-agent")).unwrap();
        assert_eq!(generic.operation.id(), facade.operation.id());
        assert_eq!(generic.operation.effects(), &[Effect::RemoteRead]);
        assert!(matches!(
            generic.binding.availability(),
            Availability::Conditional(_)
        ));
    }
    for facet in [
        "docs",
        "context",
        "ontologies",
        "answer",
        "publish",
        "reset",
        "unlink",
    ] {
        let target = Target {
            kind: TargetKind::Item,
            owner_type: "DataAgent".into(),
            facet: facet.into(),
            representation: "json".into(),
            generation: None,
        };
        assert!(matches!(
            registry.resolve(Verb::Get, &target, Some("data-agent")),
            Err(ContractError::UnsupportedTarget)
        ));
    }
    let count = registry.operations().count();
    assert!(register_reads(&mut registry).is_err());
    assert_eq!(registry.operations().count(), count);
}

#[test]
fn debug_and_display_do_not_echo_stored_text_selectors_or_provider_errors() {
    let target = target();
    let token = ContinuationToken::new("private-token").unwrap();
    assert!(!format!("{target:?}").contains(AGENT));
    assert!(!format!("{token:?}").contains("private-token"));
    assert!(
        !DataAgentError::InvalidResponse
            .to_string()
            .contains("inert")
    );
}
