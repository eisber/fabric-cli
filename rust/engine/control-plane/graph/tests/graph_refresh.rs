// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline exact-route, acceptance and lifecycle tests. No tenant operations.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    observe::{ObserveErrorKind, ObserveOptions},
    v2::{ApprovalPolicy, PreparedOperation, Verb},
};
use fabric_sdk_graph::{self as refresh, GraphRefreshErrorKind};
use fabric_sdk_jobs::{
    JobClient, JobExecutionError, JobExecutor, JobFuture, JobLimits, JobRequest, JobResponse,
    graph_refresh::{self, GraphTarget},
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

const W: &str = "11111111-1111-1111-1111-111111111111";
const I: &str = "22222222-2222-2222-2222-222222222222";
const J: &str = "33333333-3333-3333-3333-333333333333";
const HOST: &str = "dailyapi.fabric.microsoft.com";

fn path() -> String {
    format!("/{W}.Workspace/{I}.GraphModel/runtime/graph")
}
fn location() -> String {
    format!("https://{HOST}/v1/workspaces/{W}/items/{I}/jobs/instances/{J}")
}

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
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
            .push(scopes.iter().map(|scope| (*scope).into()).collect());
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "synthetic private credential detail",
            ));
        }
        Ok(AccessToken::new(
            "offline-graph-test-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor {
    jobs: Mutex<VecDeque<Result<JobResponse, JobExecutionError>>>,
    states: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    requests: Mutex<Vec<(String, String, Vec<u8>)>>,
    waits: Mutex<Vec<Duration>>,
    cancel_at_wait: Option<Arc<AtomicBool>>,
    cancel_at_submit: Option<Arc<AtomicBool>>,
}

impl JobExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            assert_eq!(request.method(), Method::Post);
            assert_eq!(request.token().token.secret(), "offline-graph-test-token");
            self.requests.lock().unwrap().push((
                "POST".into(),
                request.url().to_string(),
                request.body().to_vec(),
            ));
            if let Some(flag) = &self.cancel_at_submit {
                poll_fn(|cx| {
                    flag.store(true, Ordering::SeqCst);
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            self.jobs
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected refresh POST")
        })
    }
    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        Box::pin(async move {
            self.waits.lock().unwrap().push(delay);
            Ok(())
        })
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<OperationResponse, OperationExecutionError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            assert_eq!(request.method(), ResourceMethod::Get);
            assert!(request.body().is_empty());
            self.requests.lock().unwrap().push((
                "GET".into(),
                request.url().to_string(),
                request.body().to_vec(),
            ));
            self.states
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected observer GET")
        })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), OperationExecutionError>> + Send + '_>>
    {
        Box::pin(async move {
            self.waits.lock().unwrap().push(delay);
            if let Some(flag) = &self.cancel_at_wait {
                poll_fn(|cx| {
                    flag.store(true, Ordering::SeqCst);
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            Ok(())
        })
    }
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> JobResponse {
    JobResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    }
}
fn accepted() -> JobResponse {
    response(202, "", &[("Location", &location()), ("Retry-After", "60")])
}
fn state(status: &str, delay: &str) -> OperationResponse {
    OperationResponse {
        status: 200,
        body: serde_json::to_vec(
            &json!({"status":status,"failureReason":{"errorCode":"Synthetic"},"futureField":true}),
        )
        .unwrap(),
        headers: BTreeMap::from([("retry-after".into(), delay.into())]),
    }
}
fn fixture(
    executor: Arc<Executor>,
    credential: Arc<Credential>,
    limits: JobLimits,
) -> (Client, JobClient) {
    let context = RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap(),
    );
    (
        Client::with_context(context.clone()),
        JobClient::with_context(context, executor, limits),
    )
}
fn options() -> ObserveOptions {
    ObserveOptions {
        wait: true,
        result: false,
        max_requests: NonZeroUsize::new(4).unwrap(),
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_delay: Duration::from_secs(60),
    }
}
struct Allow;
impl ApprovalPolicy for Allow {
    type Error = &'static str;
    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(prepared.operation_id(), refresh::OPERATION_ID);
        assert_eq!(prepared.effects(), refresh::EFFECTS);
        Ok(())
    }
}
fn cancelled(flag: Arc<AtomicBool>) -> impl std::future::Future<Output = ()> + Send {
    poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[test]
fn registration_is_graph_only_and_input_is_empty() {
    let prepared = refresh::prepare(&path()).unwrap();
    let registry = refresh::registry().unwrap();
    assert_eq!(registry.operations().count(), 1);
    assert_eq!(registry.bindings().len(), 1);
    let selected = registry
        .resolve(Verb::Refresh, prepared.target().selection(), None)
        .unwrap();
    assert_eq!(
        selected.operation.input_schema()["additionalProperties"],
        false
    );
    assert_eq!(prepared.input(), &json!({}));
    assert!(refresh::schemas().unwrap()["bindings"].is_array());
    for invalid in [
        path().replace("GraphModel", "SemanticModel"),
        path().replace("GraphModel", "Lakehouse"),
        path().replace("runtime/graph", "docs/schema"),
        path().replace("runtime/graph", "schema"),
        path().replace(W, "display-name"),
        format!("{}?type=Full", path()),
        format!("{}/", path()),
        path().replace(W, "11111111-1111-1111-1111-11111111111%"),
    ] {
        assert!(refresh::prepare(&invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn exact_native_post_has_no_body_and_returns_scoped_acceptance_without_get() {
    let executor = Arc::new(Executor::default());
    executor.jobs.lock().unwrap().push_back(Ok(accepted()));
    let credential = Arc::new(Credential::default());
    let (client, jobs) = fixture(executor.clone(), credential.clone(), JobLimits::default());
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let result = refresh::execute(
        &approved,
        &client,
        &jobs,
        executor.as_ref(),
        None,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.submission.run().unwrap().id(), J);
    assert_eq!(
        result.submission.run().unwrap().endpoint(),
        client.context().endpoint()
    );
    assert!(result.observation.is_none());
    assert_eq!(
        *executor.requests.lock().unwrap(),
        vec![(
            "POST".into(),
            format!("https://{HOST}/v1/workspaces/{W}/graphModels/{I}/jobs/refreshGraph/instances"),
            vec![]
        )]
    );
    assert!(executor.waits.lock().unwrap().is_empty());
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![Audience::Fabric.scope().to_owned()]]
    );
}

#[tokio::test]
async fn synchronous_200_does_not_invent_job_or_query_readiness() {
    let executor = Arc::new(Executor::default());
    executor
        .jobs
        .lock()
        .unwrap()
        .push_back(Ok(response(200, "", &[])));
    let (client, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let result = refresh::execute(
        &approved,
        &client,
        &jobs,
        executor.as_ref(),
        Some(options()),
        pending(),
    )
    .await
    .unwrap();
    assert!(result.submission.run().is_none());
    assert_eq!(result.submission.response().status, 200);
    assert!(result.observation.is_none());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn initial_retry_after_and_neutral_observer_retain_actual_job_completion() {
    let executor = Arc::new(Executor::default());
    executor.jobs.lock().unwrap().push_back(Ok(accepted()));
    executor
        .states
        .lock()
        .unwrap()
        .extend([Ok(state("InProgress", "7")), Ok(state("Completed", "7"))]);
    let (client, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let result = refresh::execute(
        &approved,
        &client,
        &jobs,
        executor.as_ref(),
        Some(options()),
        pending(),
    )
    .await
    .unwrap();
    let observation = result.observation.unwrap();
    assert!(observation.terminal && observation.work_succeeded);
    assert_eq!(observation.raw_status["futureField"], true);
    assert_eq!(observation.requests, 2);
    assert_eq!(
        *executor.waits.lock().unwrap(),
        [Duration::from_secs(60), Duration::from_secs(7)]
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _, _)| method == "POST")
            .count(),
        1
    );
    assert!(
        requests
            .iter()
            .skip(1)
            .all(|(_, url, _)| url == &location())
    );
}

#[tokio::test]
async fn failed_cancelled_deduped_unknown_and_partial_statuses_never_report_refreshed() {
    for status in ["Failed", "Cancelled", "Deduped", "FutureState", "Partial"] {
        let executor = Arc::new(Executor::default());
        executor.jobs.lock().unwrap().push_back(Ok(accepted()));
        executor
            .states
            .lock()
            .unwrap()
            .push_back(Ok(state(status, "7")));
        let (client, jobs) = fixture(
            executor.clone(),
            Arc::new(Credential::default()),
            JobLimits::default(),
        );
        let prepared = refresh::prepare(&path()).unwrap();
        let approved = prepared.authorize(&mut Allow).unwrap();
        let error = refresh::execute(
            &approved,
            &client,
            &jobs,
            executor.as_ref(),
            Some(options()),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.submission.as_ref().unwrap().run().unwrap().id(), J);
        let GraphRefreshErrorKind::Observation(error) = error.source else {
            panic!("expected observation failure")
        };
        assert_eq!(
            error.last_observation.as_ref().unwrap().raw_status["status"],
            status
        );
        assert!(!error.last_observation.as_ref().unwrap().work_succeeded);
        assert_eq!(error.requests, 1);
        if matches!(status, "FutureState" | "Partial") {
            assert_eq!(error.kind, ObserveErrorKind::UnknownState)
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn unsafe_locations_missing_ids_and_invalid_delay_keep_one_post_evidence() {
    let good = location();
    for bad in [
        good.replace(HOST, "api.powerbi.com"),
        good.replace(I, W),
        format!("{good}?query=x"),
        format!("{good}#fragment"),
        good.replace("/jobs/instances/", "/operations/"),
        good.replace(J, "not-a-guid"),
        good.replace("https://", "https://user@"),
    ] {
        let executor = Arc::new(Executor::default());
        executor.jobs.lock().unwrap().push_back(Ok(response(
            202,
            "",
            &[("location", &bad), ("retry-after", "60")],
        )));
        let (_, jobs) = fixture(
            executor.clone(),
            Arc::new(Credential::default()),
            JobLimits::default(),
        );
        let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), pending())
            .await
            .unwrap_err();
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.response.as_ref().unwrap().status, 202);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    for delay in ["", "0", "-1", " 60", "60 ", "18446744073709551616"] {
        let executor = Arc::new(Executor::default());
        executor.jobs.lock().unwrap().push_back(Ok(response(
            202,
            "",
            &[("location", &good), ("retry-after", delay)],
        )));
        let (_, jobs) = fixture(
            executor.clone(),
            Arc::new(Credential::default()),
            JobLimits::default(),
        );
        assert!(
            graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), pending())
                .await
                .is_err()
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_before_dispatch_does_not_acquire_credential_or_submit() {
    let executor = Arc::new(Executor::default());
    let credential = Arc::new(Credential::default());
    let (_, jobs) = fixture(executor.clone(), credential.clone(), JobLimits::default());
    let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), ready(()))
        .await
        .unwrap_err();
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn credential_failure_is_pre_dispatch_while_response_loss_and_rejection_never_replay() {
    let executor = Arc::new(Executor::default());
    let (_, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential {
            fail: true,
            ..Default::default()
        }),
        JobLimits::default(),
    );
    let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), pending())
        .await
        .unwrap_err();
    assert!(!error.mutation_may_have_occurred);
    assert!(executor.requests.lock().unwrap().is_empty());
    for reply in [
        Err(JobExecutionError::Failed),
        Err(JobExecutionError::TimedOut),
        Ok(response(
            429,
            "{\"errorCode\":\"TooManyRequestsForJobs\"}",
            &[("retry-after", "60")],
        )),
        Ok(response(
            403,
            "{\"errorCode\":\"InsufficientPrivileges\"}",
            &[],
        )),
        Ok(response(201, "", &[])),
    ] {
        let executor = Arc::new(Executor::default());
        executor.jobs.lock().unwrap().push_back(reply);
        let (_, jobs) = fixture(
            executor.clone(),
            Arc::new(Credential::default()),
            JobLimits::default(),
        );
        let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), pending())
            .await
            .unwrap_err();
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert!(executor.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn response_bounds_omit_oversized_bytes_but_keep_status_and_uncertainty() {
    let executor = Arc::new(Executor::default());
    executor.jobs.lock().unwrap().push_back(Ok(response(
        202,
        "oversized",
        &[("location", &location()), ("retry-after", "60")],
    )));
    let limits = JobLimits {
        max_response_bytes: NonZeroUsize::new(4).unwrap(),
        ..JobLimits::default()
    };
    let (_, jobs) = fixture(executor, Arc::new(Credential::default()), limits);
    let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), pending())
        .await
        .unwrap_err();
    assert!(error.response_body_omitted && error.mutation_may_have_occurred);
    assert!(error.response.unwrap().body.is_empty());
}

#[tokio::test]
async fn local_interruption_during_initial_wait_keeps_handle_without_cancel_or_early_get() {
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Arc::new(Executor {
        cancel_at_wait: Some(flag.clone()),
        ..Default::default()
    });
    executor.jobs.lock().unwrap().push_back(Ok(accepted()));
    let (client, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let error = refresh::execute(
        &approved,
        &client,
        &jobs,
        executor.as_ref(),
        Some(options()),
        cancelled(flag),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.source,
        GraphRefreshErrorKind::InitialWait(_)
    ));
    assert_eq!(error.submission.unwrap().run().unwrap().id(), J);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn interruption_after_submit_dispatch_keeps_uncertainty_without_replay() {
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Arc::new(Executor {
        cancel_at_submit: Some(flag.clone()),
        ..Default::default()
    });
    let (_, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    let error = graph_refresh::submit(&jobs, &GraphTarget::new(W, I).unwrap(), cancelled(flag))
        .await
        .unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert!(error.response.is_none());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_observation_context_and_job_result_options_fail_before_mutation() {
    let executor = Arc::new(Executor::default());
    let credential = Arc::new(Credential::default());
    let (client, jobs) = fixture(executor.clone(), credential.clone(), JobLimits::default());
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let mut bad = options();
    bad.result = true;
    assert!(
        refresh::execute(
            &approved,
            &client,
            &jobs,
            executor.as_ref(),
            Some(bad),
            pending()
        )
        .await
        .is_err()
    );
    let (other, _) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    assert!(
        refresh::execute(&approved, &other, &jobs, executor.as_ref(), None, pending())
            .await
            .is_err()
    );
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn first_delay_limit_retains_acceptance_without_status_get() {
    let executor = Arc::new(Executor::default());
    executor.jobs.lock().unwrap().push_back(Ok(accepted()));
    let (client, jobs) = fixture(
        executor.clone(),
        Arc::new(Credential::default()),
        JobLimits::default(),
    );
    let prepared = refresh::prepare(&path()).unwrap();
    let approved = prepared.authorize(&mut Allow).unwrap();
    let mut opts = options();
    opts.max_delay = Duration::from_secs(59);
    let error = refresh::execute(
        &approved,
        &client,
        &jobs,
        executor.as_ref(),
        Some(opts),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.source, GraphRefreshErrorKind::DelayLimit));
    assert_eq!(error.submission.unwrap().run().unwrap().id(), J);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert!(executor.waits.lock().unwrap().is_empty());
}
