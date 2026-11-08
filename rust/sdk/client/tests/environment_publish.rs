// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native Environment publish and protected receipt contract.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    environment_publish::{
        self, ObservationPolicy, PublishData, PublishSource, PublishState, PublishVersion,
    },
    v2::{
        ApprovalPolicy, ChildIdentity, Effect, ItemIdentity, Outcome, Precondition,
        PreparedOperation, Registry, ResolvedTarget, Target, TargetKind, Verb,
    },
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse,
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

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const VERSION: &str = "33333333-3333-3333-3333-333333333333";
const OPERATION: &str = "44444444-4444-4444-4444-444444444444";
const ROOT: &str = "https://dailyapi.fabric.microsoft.com";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-offline-publish",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::with_context(RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    ))
}

#[derive(Debug)]
struct Request {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    requests: Mutex<Vec<Request>>,
    waits: Mutex<Vec<Duration>>,
    wait_error: Option<OperationExecutionError>,
    block_post: Option<Arc<AtomicBool>>,
    dropped_posts: AtomicUsize,
}

impl Executor {
    fn with(replies: Vec<Result<OperationResponse, OperationExecutionError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        }
    }

    fn posts(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == ResourceMethod::Post)
            .count()
    }
}

impl OperationExecutor for &Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.token().token.secret(), "synthetic-offline-publish");
            assert_eq!(request.max_response_bytes().get(), 4096);
            self.requests.lock().unwrap().push(Request {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            if request.method() == ResourceMethod::Post
                && let Some(flag) = &self.block_post
            {
                let _guard = Dropped(&self.dropped_posts);
                poll_fn(|cx| {
                    flag.store(true, Ordering::SeqCst);
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            self.waits.lock().unwrap().push(delay);
            self.wait_error.map_or(Ok(()), Err)
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn native(state: &str) -> OperationResponse {
    response(
        200,
        json!({"publishDetails": {
            "state": state, "targetVersion": VERSION,
            "componentPublishInfo": {
                "sparkLibraries": {"state": state},
                "sparkSettings": {"state": state}
            },
            "future": "retained"
        }}),
    )
}

fn accepted() -> OperationResponse {
    OperationResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OPERATION.into()),
            (
                "Location".into(),
                format!("{ROOT}/v1/operations/{OPERATION}"),
            ),
            ("Retry-After".into(), "120".into()),
        ]),
    }
}

fn target() -> ResolvedTarget {
    ResolvedTarget::new(
        "/Workspace.Workspace/Runtime.Environment/staging",
        ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
        Target {
            kind: TargetKind::Item,
            owner_type: "Environment".into(),
            facet: "staging".into(),
            representation: "environment".into(),
            generation: None,
        },
        None,
    )
    .unwrap()
}

fn prepared(observation: ObservationPolicy) -> PreparedOperation {
    environment_publish::prepare(target(), PublishSource::ExistingStaging, observation).unwrap()
}

struct Approval(bool);

impl ApprovalPolicy for Approval {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(
            operation.effects(),
            [Effect::RemoteWrite, Effect::Publication]
        );
        assert_eq!(
            operation.preconditions(),
            [
                Precondition::ExistingOnly,
                Precondition::UnguardedConcurrency
            ]
        );
        if self.0 { Ok(()) } else { Err("host denied") }
    }
}

fn limits() -> OperationLimits {
    OperationLimits {
        max_requests: NonZeroUsize::new(6).unwrap(),
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_poll_delay: Duration::from_secs(120),
    }
}

#[test]
fn only_environment_staging_is_registered_without_auto_save_or_report_support() {
    let mut registry = Registry::default();
    environment_publish::register(&mut registry).unwrap();
    assert_eq!(registry.operations().count(), 1);
    assert_eq!(registry.bindings().len(), 1);
    assert_eq!(serde_json::to_value(Verb::Publish).unwrap(), "publish");
    assert!(
        registry
            .resolve(Verb::Publish, target().selection(), None)
            .is_ok()
    );
    for (owner_type, facet, representation) in [
        ("Report", "staging", "environment"),
        ("Environment", "src", "environment"),
        ("Environment", "staging", "definition"),
    ] {
        let mut selection = target().selection().clone();
        selection.owner_type = owner_type.into();
        selection.facet = facet.into();
        selection.representation = representation.into();
        assert!(registry.resolve(Verb::Publish, &selection, None).is_err());
    }
    assert!(
        prepared(ObservationPolicy::ReturnAccepted)
            .authorize(&mut Approval(false))
            .is_err()
    );
    assert!(ItemIdentity::new(WORKSPACE, "../item").is_err());
    let child = ResolvedTarget::new(
        "/environment/child",
        ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
        target().selection().clone(),
        Some(ChildIdentity {
            kind: "library".into(),
            id: "file".into(),
        }),
    )
    .unwrap();
    assert!(
        environment_publish::prepare(
            child,
            PublishSource::ExistingStaging,
            ObservationPolicy::ReturnAccepted
        )
        .is_err()
    );
}

#[tokio::test]
async fn native_success_has_exact_bodyless_release_request_and_source_evidence() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::with(vec![Ok(native("Success"))]);
    let prepared = prepared(ObservationPolicy::ReturnAccepted);
    let approved = prepared.authorize(&mut Approval(true)).unwrap();
    let result = environment_publish::publish(
        &client(credential, Audience::Fabric),
        &executor,
        &approved,
        limits(),
        pending(),
    )
    .await
    .unwrap();
    let Outcome::Succeeded {
        data: PublishData::Native(data),
    } = result.receipt.outcome()
    else {
        panic!("native success required")
    };
    assert_eq!(data.state, PublishState::Success);
    assert_eq!(data.details["future"], "retained");
    assert_eq!(data.target_version.as_ref().unwrap().as_str(), VERSION);
    assert!(result.receipt.evidence().handles.is_empty());
    assert!(result.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(
        result.receipt.sources()[0].details.as_ref().unwrap()["targetVersion"],
        VERSION
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Post);
    assert_eq!(
        requests[0].url,
        format!("{ROOT}/v1/workspaces/{WORKSPACE}/environments/{ITEM}/staging/publish?beta=false")
    );
    assert!(
        requests[0].body.is_empty(),
        "absent is not an empty JSON object"
    );
}

#[tokio::test]
async fn native_active_and_terminal_failure_states_do_not_become_http_success() {
    for state in ["Waiting", "Running", "Cancelling", "Failed", "Cancelled"] {
        let executor = Executor::with(vec![Ok(native(state))]);
        let prepared = prepared(ObservationPolicy::WaitForOperation);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        if ["Failed", "Cancelled"].contains(&state) {
            assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
        } else {
            assert!(matches!(result.receipt.outcome(), Outcome::Partial { .. }));
        }
        assert!(
            result.receipt.evidence().handles.is_empty(),
            "version is not a generic operation handle"
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert!(
            executor.waits.lock().unwrap().is_empty(),
            "native 200 has no generic LRO"
        );
        assert_eq!(
            result.receipt.sources()[0].details.as_ref().unwrap()["nativeState"],
            state
        );
    }
}

#[tokio::test]
async fn absent_or_null_version_is_preserved_without_fabricating_it() {
    for details in [
        json!({"state":"Running"}),
        json!({"state":"Running","targetVersion":null}),
    ] {
        let executor = Executor::with(vec![Ok(response(200, json!({"publishDetails":details})))]);
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        let Outcome::Partial {
            data: PublishData::Native(native),
            ..
        } = result.receipt.outcome()
        else {
            panic!("incomplete native")
        };
        assert_eq!(native.details, details);
        assert!(native.target_version.is_none());
    }
}

#[tokio::test]
async fn malformed_unknown_and_conflicting_component_states_are_not_complete() {
    let mut conflicting = native("Success");
    conflicting.body = serde_json::to_vec(
        &json!({"publishDetails":{"state":"Success","targetVersion":VERSION,
        "componentPublishInfo":{"sparkLibraries":{"state":"Failed"}}}}),
    )
    .unwrap();
    for reply in [
        native("FutureState"),
        response(200, json!({})),
        response(
            200,
            json!({"publishDetails":{"state":"Success","targetVersion":"bad"}}),
        ),
        response(204, Value::Null),
        conflicting,
    ] {
        let executor = Executor::with(vec![Ok(reply)]);
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(matches!(
            result.receipt.outcome(),
            Outcome::Unknown { .. } | Outcome::Partial { .. }
        ));
        assert_eq!(executor.posts(), 1);
        assert!(result.receipt.evidence().mutation_may_have_occurred);
    }
}

#[tokio::test]
async fn return_accepted_retains_real_handle_and_never_polls() {
    let executor = Executor::with(vec![Ok(accepted())]);
    let prepared = prepared(ObservationPolicy::ReturnAccepted);
    let approved = prepared.authorize(&mut Approval(true)).unwrap();
    let result = environment_publish::publish(
        &client(Arc::default(), Audience::Fabric),
        &executor,
        &approved,
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(matches!(result.receipt.outcome(), Outcome::Accepted));
    assert_eq!(result.receipt.evidence().http_status, Some(202));
    assert_eq!(result.receipt.evidence().handles.len(), 1);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert!(executor.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lro_wait_uses_exact_120_second_delay_and_never_fetches_void_result() {
    let executor = Executor::with(vec![
        Ok(accepted()),
        Ok(response(
            200,
            json!({"status":"Succeeded","percentComplete":100}),
        )),
    ]);
    let prepared = prepared(ObservationPolicy::WaitForOperation);
    let approved = prepared.authorize(&mut Approval(true)).unwrap();
    let result = environment_publish::publish(
        &client(Arc::default(), Audience::Fabric),
        &executor,
        &approved,
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(matches!(
        result.receipt.outcome(),
        Outcome::Succeeded {
            data: PublishData::Operation(_)
        }
    ));
    assert_eq!(*executor.waits.lock().unwrap(), [Duration::from_secs(120)]);
    assert_eq!(executor.posts(), 1);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].url, format!("{ROOT}/v1/operations/{OPERATION}"));
    assert!(requests[1].body.is_empty());
}

#[tokio::test]
async fn invalid_accepted_headers_never_turn_into_completed_or_replay() {
    for reply in [
        OperationResponse {
            headers: BTreeMap::new(),
            ..accepted()
        },
        OperationResponse {
            headers: BTreeMap::from([(
                "Location".into(),
                "https://evil.example/v1/operations/x".into(),
            )]),
            ..accepted()
        },
        OperationResponse {
            headers: BTreeMap::from([("x-ms-operation-id".into(), "../other".into())]),
            ..accepted()
        },
        OperationResponse {
            body: b"unexpected acceptance body".to_vec(),
            ..accepted()
        },
    ] {
        let executor = Executor::with(vec![Ok(reply)]);
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert_eq!(executor.posts(), 1);
    }
}

#[tokio::test]
async fn cancellation_drops_inflight_post_and_retains_possible_mutation() {
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Executor {
        block_post: Some(flag.clone()),
        ..Executor::default()
    };
    let prepared = prepared(ObservationPolicy::ReturnAccepted);
    let approved = prepared.authorize(&mut Approval(true)).unwrap();
    let result = environment_publish::publish(
        &client(Arc::default(), Audience::Fabric),
        &executor,
        &approved,
        limits(),
        poll_fn(move |_| {
            if flag.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    )
    .await
    .unwrap();
    assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(result.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(executor.posts(), 1);
    assert_eq!(executor.dropped_posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn debug_does_not_disclose_native_source_content() {
    let reply = response(
        200,
        json!({"id":ITEM,"workspaceId":WORKSPACE,"type":"Environment",
        "properties":{"publishDetails":{"state":"Success","targetVersion":VERSION,"private":"private-source"}}}),
    );
    let executor = Executor::with(vec![Ok(reply)]);
    let result = environment_publish::observe_version(
        &client(Arc::default(), Audience::Fabric),
        &executor,
        &target(),
        &PublishVersion::new(VERSION).unwrap(),
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(!format!("{result:?}").contains("private-source"));
}
#[tokio::test]
async fn rejected_publication_and_another_ongoing_publish_keep_provider_error() {
    for status in [401, 403, 404, 409, 429] {
        let executor = Executor::with(vec![Ok(response(
            status,
            json!({
                "errorCode":"InvalidEnvironmentPublish", "message":"EnvironmentAnotherOngoingPublishProcess"
            }),
        ))]);
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
        assert_eq!(result.receipt.evidence().http_status, Some(status));
        assert!(result.operation_error.unwrap().raw_service_error.is_some());
        assert_eq!(executor.posts(), 1);
    }
}

#[tokio::test]
async fn lost_response_timeout_and_local_cancellation_preserve_unknown_effect() {
    for error in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
    ] {
        let executor = Executor::with(vec![Err(error)]);
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert_eq!(result.receipt.evidence().http_status, None);
        assert_eq!(executor.posts(), 1);
        assert!(result.operation_error.is_some());
    }
}

#[tokio::test]
async fn denied_observation_and_poll_limits_do_not_mean_failed_publication() {
    for (replies, limit) in [
        (
            vec![
                Ok(accepted()),
                Ok(response(403, json!({"errorCode":"Forbidden"}))),
            ],
            limits(),
        ),
        (
            vec![Ok(accepted())],
            OperationLimits {
                max_poll_delay: Duration::from_secs(60),
                ..limits()
            },
        ),
        (
            vec![Ok(accepted())],
            OperationLimits {
                max_requests: NonZeroUsize::new(1).unwrap(),
                ..limits()
            },
        ),
    ] {
        let executor = Executor::with(replies);
        let prepared = prepared(ObservationPolicy::WaitForOperation);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limit,
            pending(),
        )
        .await
        .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert_eq!(result.receipt.evidence().handles.len(), 1);
        assert_eq!(executor.posts(), 1);
    }
}

#[tokio::test]
async fn lro_failure_and_cancelled_wait_preserve_handle_without_remote_cancel() {
    for failed in [false, true] {
        let mut executor = Executor::with(if failed {
            vec![
                Ok(accepted()),
                Ok(response(
                    200,
                    json!({"status":"Failed","error":{"errorCode":"InstallFailed"}}),
                )),
            ]
        } else {
            vec![Ok(accepted())]
        });
        if !failed {
            executor.wait_error = Some(OperationExecutionError::Cancelled);
        }
        let prepared = prepared(ObservationPolicy::WaitForOperation);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let result = environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.receipt.evidence().handles.len(), 1);
        if failed {
            assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
        } else {
            assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        }
        assert_eq!(executor.posts(), 1);
    }
}

#[tokio::test]
async fn predispatch_cancellation_and_wrong_audience_do_not_acquire_or_dispatch() {
    for cancel in [true, false] {
        let credential = Arc::new(Credential::default());
        let executor = Executor::default();
        let prepared = prepared(ObservationPolicy::ReturnAccepted);
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let signal: Pin<Box<dyn Future<Output = ()> + Send>> = if cancel {
            Box::pin(ready(()))
        } else {
            Box::pin(pending())
        };
        let result = environment_publish::publish(
            &client(
                credential.clone(),
                if cancel {
                    Audience::Fabric
                } else {
                    Audience::Arm
                },
            ),
            &executor,
            &approved,
            limits(),
            signal,
        )
        .await
        .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(!result.receipt.evidence().mutation_may_have_occurred);
        assert!(executor.requests.lock().unwrap().is_empty());
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    }
}

fn environment(state: &str, version: &str) -> OperationResponse {
    response(
        200,
        json!({"id":ITEM,"workspaceId":WORKSPACE,"type":"Environment",
        "properties":{"publishDetails":{"state":state,"targetVersion":version}}}),
    )
}

#[tokio::test]
async fn native_observation_is_one_version_correlated_read_without_mutation() {
    let executor = Executor::with(vec![Ok(environment("Success", VERSION))]);
    let result = environment_publish::observe_version(
        &client(Arc::default(), Audience::Fabric),
        &executor,
        &target(),
        &PublishVersion::new(VERSION).unwrap(),
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(result.succeeded);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Get);
    assert_eq!(
        requests[0].url,
        format!("{ROOT}/v1/workspaces/{WORKSPACE}/environments/{ITEM}")
    );
}

#[tokio::test]
async fn concurrent_new_version_wrong_owner_and_unknown_state_never_establish_completion() {
    for reply in [
        environment("Success", OPERATION),
        environment("Future", VERSION),
        response(
            200,
            json!({"id":OPERATION,"workspaceId":WORKSPACE,"type":"Environment","properties":{"publishDetails":{"state":"Success","targetVersion":VERSION}}}),
        ),
    ] {
        let executor = Executor::with(vec![Ok(reply)]);
        let error = environment_publish::observe_version(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &target(),
            &PublishVersion::new(VERSION).unwrap(),
            limits(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.cause.kind, OperationErrorKind::InvalidResponse);
        assert!(error.response.is_some());
        assert!(!error.cause.mutation_may_have_occurred);
        assert_eq!(executor.posts(), 0);
    }
}

#[tokio::test]
async fn fabricated_approval_input_cannot_bypass_typed_staging_intent() {
    let mut registry = Registry::default();
    environment_publish::register(&mut registry).unwrap();
    let target = target();
    let tampered = PreparedOperation::new(
        registry
            .resolve(Verb::Publish, target.selection(), None)
            .unwrap(),
        target,
        json!({"source":"definition","observation":"returnAccepted","save":true}),
    )
    .unwrap()
    .with_preconditions(vec![
        Precondition::ExistingOnly,
        Precondition::UnguardedConcurrency,
    ])
    .unwrap();
    let approved = tampered.authorize(&mut Approval(true)).unwrap();
    let executor = Executor::default();
    assert!(
        environment_publish::publish(
            &client(Arc::default(), Audience::Fabric),
            &executor,
            &approved,
            limits(),
            pending()
        )
        .await
        .is_err()
    );
    assert!(executor.requests.lock().unwrap().is_empty());
}
