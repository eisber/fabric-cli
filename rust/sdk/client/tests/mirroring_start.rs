// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic, offline MirroredDatabase START wire and lifecycle contracts.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    mirroring::{self, PreflightState, StartError, StartEvidence, StartExecution, StartPhase},
    v2::{
        ApprovalPolicy, ChildIdentity, Coverage, Effect, ItemIdentity, OperationHandle, Outcome,
        Precondition, PreparedOperation, ReceiptError, Registry, ResolvedTarget, SourceEvidence,
        Verb,
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
    task::{Context, Poll},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OTHER: &str = "33333333-3333-3333-3333-333333333333";
const ROOT: &str = "https://dailyapi.fabric.microsoft.com";
const PRIVATE_MESSAGE: &str = "provider message must not enter the receipt";

fn path() -> String {
    format!("/{WORKSPACE}.Workspace/{ITEM}.MirroredDatabase/runtime/mirroring")
}

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    block_call: Option<usize>,
    fail_call: Option<usize>,
    active: Arc<AtomicBool>,
    fail: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        if self.block_call == Some(call) {
            poll_fn(|cx| {
                self.active.store(true, Ordering::SeqCst);
                cx.waker().wake_by_ref();
                Poll::<()>::Pending
            })
            .await;
        }
        if self.fail || self.fail_call == Some(call) {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Credential,
                "synthetic credential failure",
            ));
        }
        Ok(AccessToken::new(
            "synthetic-offline-mirroring",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn client(credential: Arc<Credential>) -> Client {
    configured(
        credential,
        "offline-account",
        "offline-tenant",
        Audience::Fabric,
        "dailyapi.fabric.microsoft.com",
    )
}

fn configured(
    credential: Arc<Credential>,
    account: &str,
    tenant: &str,
    audience: Audience,
    host: &str,
) -> Client {
    Client::with_context(RequestContext::with_endpoint(
        credential,
        CredentialSelection::new(account, tenant, audience).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap(),
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
    calls: AtomicUsize,
    blocked_call: Option<usize>,
    active: Arc<AtomicBool>,
    cancel_after_reply: Option<usize>,
    dropped: AtomicUsize,
}

impl Executor {
    fn with(replies: Vec<Result<OperationResponse, OperationExecutionError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        }
    }

    fn starts(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.ends_with("/startMirroring"))
            .count()
    }
}

struct DropGuard<'a>(&'a AtomicUsize);

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
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
                request.token().token.secret(),
                "synthetic-offline-mirroring"
            );
            assert_eq!(request.max_response_bytes().get(), 4096);
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.requests.lock().unwrap().push(Request {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            if self.blocked_call == Some(call) {
                let _guard = DropGuard(&self.dropped);
                poll_fn(|cx| {
                    self.active.store(true, Ordering::SeqCst);
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            let response = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request or implicit retry");
            if self.cancel_after_reply == Some(call) {
                self.active.store(true, Ordering::SeqCst);
            }
            response
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("START must never poll or wait");
    }
}

struct CancelOnFlag {
    flag: Arc<AtomicBool>,
    completed: bool,
}

impl Future for CancelOnFlag {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        assert!(
            !self.completed,
            "completed cancellation must remain latched"
        );
        if self.flag.load(Ordering::SeqCst) {
            self.completed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

fn cancellation(flag: &Arc<AtomicBool>) -> CancelOnFlag {
    CancelOnFlag {
        flag: flag.clone(),
        completed: false,
    }
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn empty(status: u16) -> OperationResponse {
    OperationResponse {
        status,
        body: Vec::new(),
        headers: BTreeMap::new(),
    }
}

fn state(status: &str) -> OperationResponse {
    response(200, json!({"status": status}))
}

fn service_error(status: u16) -> OperationResponse {
    response(
        status,
        json!({
            "errorCode": "SyntheticRejection",
            "message": PRIVATE_MESSAGE,
            "requestId": "synthetic-request",
            "severity": "Error"
        }),
    )
}

fn accepted() -> OperationResponse {
    OperationResponse {
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OTHER.into()),
            ("Location".into(), format!("{ROOT}/v1/operations/{OTHER}")),
            ("Retry-After".into(), "1".into()),
        ]),
        ..empty(202)
    }
}

fn limits() -> OperationLimits {
    OperationLimits {
        max_requests: NonZeroUsize::new(2).unwrap(),
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_poll_delay: Duration::ZERO,
    }
}

struct Approval(bool);

impl ApprovalPolicy for Approval {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(operation.operation_id(), mirroring::OPERATION_ID);
        assert_eq!(
            operation.effects(),
            [Effect::RemoteRead, Effect::RemoteWrite]
        );
        assert_eq!(operation.input(), &json!({}));
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

async fn run(
    client: &Client,
    executor: &Executor,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> StartExecution {
    let prepared = mirroring::prepare(client, mirroring::target(&path()).unwrap()).unwrap();
    let approved = prepared.authorize(&mut Approval(true)).unwrap();
    mirroring::execute(client, approved, executor, limits, cancelled)
        .await
        .unwrap()
}

fn assert_no_start(result: &StartExecution, executor: &Executor) {
    assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
    assert!(!result.receipt.evidence().mutation_may_have_occurred);
    assert!(result.receipt.evidence().handles.is_empty());
    assert_eq!(executor.starts(), 0);
    assert!(
        !result
            .operation_error
            .as_ref()
            .unwrap()
            .mutation_may_have_occurred
    );
}

#[test]
fn metadata_is_local_and_bound_to_only_runtime_mirroring() {
    let mut registry = Registry::default();
    mirroring::register(&mut registry).unwrap();
    assert_eq!(registry.operations().count(), 1);
    assert_eq!(registry.bindings().len(), 1);
    assert_eq!(serde_json::to_value(Verb::Start).unwrap(), "start");
    assert!(
        registry
            .resolve(
                Verb::Start,
                mirroring::target(&path()).unwrap().selection(),
                None
            )
            .is_ok()
    );
    assert!(mirroring::register(&mut registry).is_err());
    let schemas = mirroring::schemas().unwrap();
    assert_eq!(schemas["operations"][0]["id"], mirroring::OPERATION_ID);
    assert_eq!(schemas["operations"][0]["inputSchema"]["type"], "object");
    assert_eq!(
        schemas["operations"][0]["inputSchema"]["properties"],
        json!({})
    );
    assert_eq!(
        schemas["operations"][0]["inputSchema"]["additionalProperties"],
        false
    );
    assert_eq!(schemas["bindings"][0]["ownerType"], "MirroredDatabase");
    assert_eq!(schemas["bindings"][0]["facet"], "runtime");
    assert_eq!(schemas["bindings"][0]["representation"], "mirroring");
    assert_eq!(
        mirroring::EFFECTS.to_vec(),
        [Effect::RemoteRead, Effect::RemoteWrite]
    );
    for (owner, facet, representation) in [
        ("Environment", "runtime", "mirroring"),
        ("MirroredDatabase", "job", "mirroring"),
        ("MirroredDatabase", "runtime", "default"),
    ] {
        let mut selection = mirroring::target(&path()).unwrap().selection().clone();
        selection.owner_type = owner.into();
        selection.facet = facet.into();
        selection.representation = representation.into();
        assert!(registry.resolve(Verb::Start, &selection, None).is_err());
    }
}

#[test]
fn target_grammar_rejects_names_aliases_traversal_queries_and_bulk() {
    for bad in [
        String::new(),
        "/".into(),
        path().trim_start_matches('/').into(),
        format!("//{}", path()),
        format!("{}/", path()),
        format!("{}/extra", path()),
        format!("{}?force=true", path()),
        format!("{}#fragment", path()),
        format!("/{WORKSPACE}.Workspace/{ITEM}.MirroredDatabase"),
        format!("/name.Workspace/{ITEM}.MirroredDatabase/runtime/mirroring"),
        format!("/{WORKSPACE}.Workspace/name.MirroredDatabase/runtime/mirroring"),
        format!("/{WORKSPACE}.Workspace/*.MirroredDatabase/runtime/mirroring"),
        format!("/{WORKSPACE}.Workspace/{ITEM}.MirroredDatabase/runtime/../mirroring"),
        format!("/{WORKSPACE}.Workspace/{ITEM}.MirroredDatabase/runtime/%6dirroring"),
        path().replace("/runtime/", "\\runtime\\"),
        path().replace(".Workspace", ".workspace"),
        path().replace(".MirroredDatabase", ".MirroredWarehouse"),
        path().replace("runtime", "Runtime"),
        path().replace("mirroring", "Mirroring"),
        format!("{},{}", path(), path()),
    ] {
        assert!(mirroring::target(&bad).is_err(), "accepted invalid path");
    }
}

#[test]
fn resolver_identity_selection_child_and_generation_cannot_disagree_with_path() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let good = mirroring::target(&path()).unwrap();
    let mut targets = vec![
        ResolvedTarget::new(
            &path(),
            ItemIdentity::new(WORKSPACE, OTHER).unwrap(),
            good.selection().clone(),
            None,
        )
        .unwrap(),
        ResolvedTarget::new(
            &path(),
            ItemIdentity::new(OTHER, ITEM).unwrap(),
            good.selection().clone(),
            None,
        )
        .unwrap(),
        ResolvedTarget::new(
            &path(),
            ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
            good.selection().clone(),
            Some(ChildIdentity {
                kind: "table".into(),
                id: "child".into(),
            }),
        )
        .unwrap(),
    ];
    for (owner, facet, representation, generation) in [
        ("Report", "runtime", "mirroring", None),
        ("MirroredDatabase", "metadata", "mirroring", None),
        ("MirroredDatabase", "runtime", "default", None),
        ("MirroredDatabase", "runtime", "mirroring", Some(1)),
    ] {
        let mut selection = good.selection().clone();
        selection.owner_type = owner.into();
        selection.facet = facet.into();
        selection.representation = representation.into();
        selection.generation = generation;
        targets.push(
            ResolvedTarget::new(
                &path(),
                ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
                selection,
                None,
            )
            .unwrap(),
        );
    }
    for target in targets {
        assert!(mirroring::prepare(&client, target).is_err());
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refusal_wrong_audience_and_swapped_context_have_zero_effects() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::default();
    let prepared = mirroring::prepare(&client, mirroring::target(&path()).unwrap()).unwrap();
    assert_eq!(prepared.context().selection(), client.selection());
    assert!(prepared.authorize(&mut Approval(false)).is_err());
    let invalid = configured(
        credential.clone(),
        "offline-account",
        "offline-tenant",
        Audience::Arm,
        "dailyapi.fabric.microsoft.com",
    );
    assert!(mirroring::prepare(&invalid, mirroring::target(&path()).unwrap()).is_err());
    let wrong_origin = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        client.selection().clone(),
        EndpointBinding::public(ServiceOrigin::Arm),
    ));
    assert!(mirroring::prepare(&wrong_origin, mirroring::target(&path()).unwrap()).is_err());
    for swapped in [
        configured(
            credential.clone(),
            "other-account",
            "offline-tenant",
            Audience::Fabric,
            "dailyapi.fabric.microsoft.com",
        ),
        configured(
            credential.clone(),
            "offline-account",
            "other-tenant",
            Audience::Fabric,
            "dailyapi.fabric.microsoft.com",
        ),
        configured(
            credential.clone(),
            "offline-account",
            "offline-tenant",
            Audience::Fabric,
            "api.fabric.microsoft.com",
        ),
        configured(
            Arc::default(),
            "offline-account",
            "offline-tenant",
            Audience::Fabric,
            "dailyapi.fabric.microsoft.com",
        ),
        invalid,
        wrong_origin,
    ] {
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let error = mirroring::execute(&swapped, approved, &executor, limits(), pending())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StartError::Input(ReceiptError::InvalidEvidence)
        ));
        assert!(error.execution_evidence().is_none());
        let projected = error.evidence();
        assert_eq!(projected["kind"], "input");
        assert!(projected["evidence"].is_null());
        assert!(projected["outcome"].is_null());
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn only_stopped_and_initialized_send_exact_bodyless_post_requests() {
    for (name, expected) in [
        ("Stopped", PreflightState::Stopped),
        ("Initialized", PreflightState::Initialized),
    ] {
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone());
        let executor = Executor::with(vec![Ok(state(name)), Ok(empty(200))]);
        let result = run(&client, &executor, limits(), pending()).await;
        let Outcome::Succeeded { data } = result.receipt.outcome() else {
            panic!("documented action must complete")
        };
        assert_eq!(data.preflight_state, expected);
        assert!(data.action_completed);
        assert!(data.replication_state.is_none());
        assert_eq!(
            serde_json::to_value(data).unwrap(),
            json!({"preflightState":name,"actionCompleted":true,"replicationState":null})
        );
        assert_eq!(result.receipt.evidence().http_status, Some(200));
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert!(result.receipt.evidence().handles.is_empty());
        assert!(result.operation_error.is_none());
        assert_eq!(result.receipt.sources().len(), 2);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 2);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for (request, action) in requests
            .iter()
            .zip(["getMirroringStatus", "startMirroring"])
        {
            assert_eq!(request.method, ResourceMethod::Post);
            assert_eq!(
                request.url,
                format!("{ROOT}/v1/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/{action}")
            );
            assert!(request.body.is_empty(), "empty bytes are not JSON {{}}");
        }
    }
}

#[tokio::test]
async fn prohibited_unknown_and_malformed_statuses_always_block_start() {
    let mut replies: Vec<_> = [
        "Paused",
        "Initializing",
        "Starting",
        "Running",
        "Stopping",
        "Failed",
        "FutureState",
        "stopped",
        "",
        "provider-private-state",
    ]
    .into_iter()
    .map(state)
    .collect();
    replies.extend([
        response(200, json!({})),
        response(200, json!({"status": null})),
        response(200, json!({"status": 12})),
        response(200, json!({"status": ["Stopped"]})),
        response(200, json!([])),
        OperationResponse {
            body: b"not-json".to_vec(),
            ..empty(200)
        },
        OperationResponse {
            body: br#"{"status":"Running","status":"Stopped"}"#.to_vec(),
            ..empty(200)
        },
        empty(200),
        empty(204),
        response(201, json!({"status": "Stopped"})),
        response(202, json!({"status": "Stopped"})),
        accepted(),
    ]);
    for reply in replies {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(reply)]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert_no_start(&result, &executor);
        assert_eq!(result.error_phase, Some(StartPhase::Preflight));
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert!(result.original_preflight_error.is_some());
        let serialized = serde_json::to_string(&result.receipt).unwrap();
        assert!(!serialized.contains("provider-private-state"));
    }
}

#[tokio::test]
async fn error_field_even_with_allowed_state_or_warning_severity_blocks_start() {
    for error in [
        json!({"errorCode":"StateError","message":PRIVATE_MESSAGE,"severity":"Error"}),
        json!({"errorCode":"StateWarning","message":PRIVATE_MESSAGE,"severity":"Warning"}),
        json!({"errorCode":"StateError","message":PRIVATE_MESSAGE,"severity":"FutureSeverity"}),
        json!({}),
        json!("malformed"),
        json!([]),
        json!(false),
        json!(42),
        json!({"errorCode": "MissingMessage"}),
    ] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(response(
            200,
            json!({"status":"Stopped","error":error}),
        ))]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert_no_start(&result, &executor);
        assert!(result.preflight_response.is_some());
        assert_eq!(
            result
                .operation_error
                .as_ref()
                .unwrap()
                .raw_service_error
                .as_deref(),
            Some(&error)
        );
        assert!(
            !serde_json::to_string(&result.receipt)
                .unwrap()
                .contains(PRIVATE_MESSAGE)
        );
    }
    let client = client(Arc::default());
    let executor = Executor::with(vec![
        Ok(response(200, json!({"status":"Stopped","error":null}))),
        Ok(empty(200)),
    ]);
    assert!(matches!(
        run(&client, &executor, limits(), pending())
            .await
            .receipt
            .outcome(),
        Outcome::Succeeded { .. }
    ));
}

#[tokio::test]
async fn probe_http_failures_keep_original_error_and_do_not_claim_start_mutation() {
    for status in [400, 401, 403, 404, 409, 429, 500] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(service_error(status))]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert_no_start(&result, &executor);
        assert_eq!(result.receipt.evidence().http_status, Some(status));
        let error = result.operation_error.as_ref().unwrap();
        assert!(
            matches!(error.kind, OperationErrorKind::Http { status: actual, .. } if actual == status)
        );
        assert_eq!(
            error.raw_service_error.as_ref().unwrap()["message"],
            PRIVATE_MESSAGE
        );
        assert!(
            result
                .original_preflight_error
                .as_ref()
                .unwrap()
                .mutation_may_have_occurred
        );
        assert!(
            !serde_json::to_string(&result.receipt)
                .unwrap()
                .contains(PRIVATE_MESSAGE)
        );
    }
}

#[tokio::test]
async fn concurrent_stop_start_deletion_denial_and_rate_limit_keep_actual_rejection() {
    for (status, code) in [
        (400, "mirroring_bad_request"),
        (401, "mirroring_unauthorized"),
        (403, "mirroring_forbidden"),
        (404, "mirroring_not_found"),
        (409, "mirroring_conflict"),
        (429, "mirroring_rate_limited"),
    ] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(state("Stopped")), Ok(service_error(status))]);
        let result = run(&client, &executor, limits(), pending()).await;
        let Outcome::Failed { error } = result.receipt.outcome() else {
            panic!("observed rejection is not successful START")
        };
        assert_eq!(error.code, code);
        assert_eq!(result.error_phase, Some(StartPhase::Start));
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert_eq!(result.receipt.evidence().http_status, Some(status));
        assert!(matches!(
            result.operation_error.as_ref().unwrap().kind,
            OperationErrorKind::Http { status: actual, .. } if actual == status
        ));
        assert_eq!(executor.starts(), 1);
        assert!(
            !serde_json::to_string(&result.receipt)
                .unwrap()
                .contains(PRIVATE_MESSAGE)
        );
    }
}

#[tokio::test]
async fn no_timeout_reset_or_ambiguous_response_replays_or_fabricates_acceptance() {
    let mut replies = vec![
        Err(OperationExecutionError::TimedOut),
        Err(OperationExecutionError::Failed),
        Err(OperationExecutionError::Cancelled),
        Err(OperationExecutionError::ResponseLimit),
        Err(OperationExecutionError::RetriesExhausted),
        Ok(empty(202)),
        Ok(accepted()),
        Ok(OperationResponse {
            headers: BTreeMap::from([("Location".into(), "https://untrusted.invalid/work".into())]),
            ..accepted()
        }),
        Ok(response(200, json!({}))),
        Ok(response(200, json!(null))),
        Ok(OperationResponse {
            body: b" ".to_vec(),
            ..empty(200)
        }),
        Ok(response(200, json!({"status":"Running"}))),
        Ok(empty(201)),
        Ok(empty(204)),
        Ok(service_error(500)),
    ];
    for reply in replies.drain(..) {
        let expected_handles = if reply.as_ref().is_ok_and(|response| {
            response.status == 202
                && response
                    .headers
                    .get("x-ms-operation-id")
                    .map(String::as_str)
                    == Some(OTHER)
        }) {
            vec![OperationHandle::FabricOperation { id: OTHER.into() }]
        } else {
            Vec::new()
        };
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(state("Initialized")), reply]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert_eq!(result.receipt.evidence().handles, expected_handles);
        assert!(result.operation_error.is_some());
        assert_eq!(executor.starts(), 1);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn extra_action_headers_do_not_install_polling_or_claim_replication_state() {
    let client = client(Arc::default());
    let executor = Executor::with(vec![
        Ok(state("Stopped")),
        Ok(OperationResponse {
            status: 200,
            ..accepted()
        }),
    ]);
    let result = run(&client, &executor, limits(), pending()).await;
    assert!(matches!(
        result.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert!(result.receipt.evidence().handles.is_empty());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unexpected_start_acceptance_keeps_real_ids_without_polling_or_acceptance_claim() {
    for reply in [
        accepted(),
        OperationResponse {
            headers: BTreeMap::from([("Location".into(), format!("{ROOT}/v1/operations/{OTHER}"))]),
            ..empty(202)
        },
        OperationResponse {
            headers: BTreeMap::from([("X-MS-Operation-ID".into(), OTHER.into())]),
            ..empty(202)
        },
        OperationResponse {
            headers: BTreeMap::from([
                ("x-ms-operation-id".into(), OTHER.into()),
                ("Location".into(), "https://untrusted.invalid/work".into()),
            ]),
            ..empty(202)
        },
    ] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Ok(state("Stopped")), Ok(reply)]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert_eq!(
            result.receipt.evidence().handles,
            [OperationHandle::FabricOperation { id: OTHER.into() }]
        );
        assert_eq!(result.receipt.evidence().http_status, Some(202));
        assert_eq!(executor.starts(), 1);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn location_only_wrong_path_or_origin_never_becomes_a_fabric_operation_handle() {
    for location in [
        format!("{ROOT}/v1/items/{OTHER}"),
        format!("https://api.fabric.microsoft.com/v1/items/{OTHER}"),
        format!("https://api.fabric.microsoft.com/v1/operations/{OTHER}"),
        format!("https://untrusted.invalid/v1/operations/{OTHER}"),
        format!("{ROOT}/v1/operations/{OTHER}?unexpected=1"),
        format!("{ROOT}/v1/operations/{OTHER}#fragment"),
    ] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![
            Ok(state("Stopped")),
            Ok(OperationResponse {
                headers: BTreeMap::from([("Location".into(), location.clone())]),
                ..empty(202)
            }),
        ]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(result.receipt.evidence().handles.is_empty());
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert_eq!(result.receipt.evidence().http_status, Some(202));
        let error = result.operation_error.as_ref().unwrap();
        assert!(matches!(error.kind, OperationErrorKind::InvalidLink));
        assert_eq!(error.operation_id.as_deref(), Some(OTHER));
        assert_eq!(
            result.response.as_ref().unwrap().headers["Location"],
            location
        );
        assert_eq!(executor.starts(), 1);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn cancelled_before_probe_has_no_credentials_or_requests() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::default();
    let result = run(&client, &executor, limits(), ready(())).await;
    assert_no_start(&result, &executor);
    assert!(matches!(
        result.operation_error.unwrap().kind,
        OperationErrorKind::Cancelled
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelled_during_either_credential_phase_never_dispatches_start() {
    for block_call in [1, 2] {
        let credential = Arc::new(Credential {
            block_call: Some(block_call),
            ..Credential::default()
        });
        let signal = cancellation(&credential.active);
        let client = client(credential.clone());
        let executor = Executor::with(vec![Ok(state("Stopped"))]);
        let result = run(&client, &executor, limits(), signal).await;
        assert_no_start(&result, &executor);
        assert_eq!(credential.calls.load(Ordering::SeqCst), block_call);
        assert_eq!(executor.calls.load(Ordering::SeqCst), block_call - 1);
    }
}

#[tokio::test]
async fn cancellation_during_probe_and_after_instant_probe_never_dispatches_start() {
    for instant in [false, true] {
        let client = client(Arc::default());
        let executor = Executor {
            blocked_call: (!instant).then_some(1),
            cancel_after_reply: instant.then_some(1),
            ..Executor::with(vec![Ok(state("Stopped"))])
        };
        let result = run(&client, &executor, limits(), cancellation(&executor.active)).await;
        assert_no_start(&result, &executor);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            executor.dropped.load(Ordering::SeqCst),
            usize::from(!instant)
        );
    }
}

#[tokio::test]
async fn cancellation_during_start_retains_unknown_effect_and_drops_inflight_work() {
    let client = client(Arc::default());
    let executor = Executor {
        blocked_call: Some(2),
        ..Executor::with(vec![Ok(state("Stopped"))])
    };
    let result = run(&client, &executor, limits(), cancellation(&executor.active)).await;
    assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
    assert_eq!(result.error_phase, Some(StartPhase::Start));
    assert!(result.receipt.evidence().mutation_may_have_occurred);
    assert!(matches!(
        result.operation_error.unwrap().kind,
        OperationErrorKind::Cancelled
    ));
    assert_eq!(executor.starts(), 1);
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_after_instant_start_retains_response_but_not_completion() {
    let client = client(Arc::default());
    let executor = Executor {
        cancel_after_reply: Some(2),
        ..Executor::with(vec![Ok(state("Stopped")), Ok(empty(200))])
    };
    let result = run(&client, &executor, limits(), cancellation(&executor.active)).await;
    assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(result.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(result.response.as_ref().unwrap().status, 200);
    assert!(result.response.as_ref().unwrap().body.is_empty());
    assert_eq!(executor.starts(), 1);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn contradictory_receipt_evidence_retains_all_executed_native_evidence() {
    for action_attempted in [false, true] {
        let client = client(Arc::default());
        let mut replies = Vec::new();
        if action_attempted {
            replies.push(Ok(state("Stopped")));
        }
        replies.push(Ok(service_error(999)));
        let executor = Executor::with(replies);
        let prepared = mirroring::prepare(&client, mirroring::target(&path()).unwrap()).unwrap();
        let approved = prepared.authorize(&mut Approval(true)).unwrap();
        let error = mirroring::execute(&client, approved, &executor, limits(), pending())
            .await
            .unwrap_err();
        let safe = serde_json::to_value(error.execution_evidence().unwrap()).unwrap();
        assert_eq!(safe["httpStatus"], 999);
        assert_eq!(safe["mutationMayHaveOccurred"], action_attempted);
        assert!(
            !serde_json::to_string(&safe)
                .unwrap()
                .contains(PRIVATE_MESSAGE)
        );
        let projected = error.evidence();
        assert_eq!(projected["kind"], "receipt");
        assert_eq!(projected["operationId"], mirroring::OPERATION_ID);
        assert_eq!(projected["target"]["canonicalPath"], path());
        assert_eq!(projected["evidence"], safe);
        assert_eq!(
            projected["outcome"]["status"],
            if action_attempted {
                "unknown"
            } else {
                "failed"
            }
        );
        assert_eq!(
            projected["phase"],
            if action_attempted {
                "start"
            } else {
                "preflight"
            }
        );
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains(PRIVATE_MESSAGE)
        );
        let StartError::Receipt { cause, evidence } = error else {
            panic!("invalid native HTTP status must reject receipt construction")
        };
        assert_eq!(cause.evidence.http_status, Some(999));
        let response = if action_attempted {
            evidence.response.as_ref()
        } else {
            evidence.preflight_response.as_ref()
        };
        assert_eq!(response.unwrap().status, 999);
        assert!(matches!(
            evidence.operation_error.as_ref().unwrap().kind,
            OperationErrorKind::Http { status: 999, .. }
        ));
        assert_eq!(executor.starts(), usize::from(action_attempted));
    }
}

#[tokio::test]
async fn rejected_source_evidence_has_a_safe_projection_without_losing_mutation_evidence() {
    let client = client(Arc::default());
    let executor = Executor::with(vec![Ok(state("Stopped")), Ok(empty(200))]);
    let result = run(&client, &executor, limits(), pending()).await;
    let target = result.receipt.target().clone();
    let cause = result
        .receipt
        .with_source_evidence(vec![SourceEvidence {
            source: "synthetic.invalid".into(),
            http_status: Some(403),
            coverage: Coverage::Complete,
            error: None,
            details: Some(json!({"untrusted": PRIVATE_MESSAGE})),
        }])
        .unwrap_err();
    let error = StartError::Source {
        cause,
        evidence: Box::new(StartEvidence {
            target,
            preflight_response: result.preflight_response,
            response: result.response,
            operation_error: result.operation_error,
            original_preflight_error: result.original_preflight_error,
            error_phase: result.error_phase,
        }),
    };
    let safe = serde_json::to_value(error.execution_evidence().unwrap()).unwrap();
    assert_eq!(safe["httpStatus"], 200);
    assert_eq!(safe["mutationMayHaveOccurred"], true);
    assert!(
        !serde_json::to_string(&safe)
            .unwrap()
            .contains(PRIVATE_MESSAGE)
    );
    let projected = error.evidence();
    assert_eq!(projected["kind"], "source");
    assert_eq!(projected["target"]["canonicalPath"], path());
    assert_eq!(projected["outcome"]["status"], "succeeded");
    assert_eq!(projected["outcome"]["data"]["actionCompleted"], true);
    assert_eq!(projected["evidence"], safe);
    assert!(
        !serde_json::to_string(&projected)
            .unwrap()
            .contains(PRIVATE_MESSAGE)
    );
    assert_eq!(executor.starts(), 1);
}

#[tokio::test]
async fn shared_request_and_byte_limits_fail_closed_without_extra_start() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::with(vec![Ok(state("Stopped"))]);
    let result = run(
        &client,
        &executor,
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            ..limits()
        },
        pending(),
    )
    .await;
    assert_no_start(&result, &executor);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        result.operation_error.unwrap().kind,
        OperationErrorKind::Limit
    ));
    for oversized_phase in [1, 2] {
        let mut replies = vec![Ok(state("Stopped"))];
        let oversized = response(200, json!({"status":"Stopped","padding":"x".repeat(4096)}));
        if oversized_phase == 1 {
            replies[0] = Ok(oversized);
        } else {
            replies.push(Ok(oversized));
        }
        let executor = Executor::with(replies);
        let result = run(&client, &executor, limits(), pending()).await;
        assert!(matches!(
            result.operation_error.as_ref().unwrap().kind,
            OperationErrorKind::Limit
        ));
        if oversized_phase == 1 {
            assert_no_start(&result, &executor);
            assert!(result.preflight_response.is_none());
        } else {
            assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
            assert!(result.receipt.evidence().mutation_may_have_occurred);
            assert_eq!(executor.starts(), 1);
            assert!(result.response.is_none());
        }
    }
}

#[tokio::test]
async fn start_credential_failure_is_not_probe_post_uncertainty_or_a_start_dispatch() {
    let credential = Arc::new(Credential {
        fail_call: Some(2),
        ..Credential::default()
    });
    let client = client(credential.clone());
    let executor = Executor::with(vec![Ok(state("Stopped"))]);
    let result = run(&client, &executor, limits(), pending()).await;
    assert_no_start(&result, &executor);
    assert_eq!(result.error_phase, Some(StartPhase::Start));
    assert!(matches!(
        result.operation_error.as_ref().unwrap().kind,
        OperationErrorKind::Credential
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 2);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.preflight_response.as_ref().unwrap().status, 200);
    assert!(result.response.is_none());
    assert!(result.original_preflight_error.is_none());
}

#[tokio::test]
async fn failed_probe_transport_and_credentials_do_not_claim_a_start_mutation() {
    for error in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
    ] {
        let client = client(Arc::default());
        let executor = Executor::with(vec![Err(error)]);
        let result = run(&client, &executor, limits(), pending()).await;
        assert_no_start(&result, &executor);
        assert!(result.receipt.evidence().http_status.is_none());
    }
    let client = client(Arc::new(Credential {
        fail: true,
        ..Credential::default()
    }));
    let executor = Executor::default();
    let result = run(&client, &executor, limits(), pending()).await;
    assert_no_start(&result, &executor);
    assert!(matches!(
        result.operation_error.unwrap().kind,
        OperationErrorKind::Credential
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}
