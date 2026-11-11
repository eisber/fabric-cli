// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native HTTP/approval composition, including the unmodified SDK.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use super::*;
use azure_core::http::{AsyncRawResponse, Body, Method};
use fabric_sdk::{Audience, CredentialSelection, RequestContext};
use std::{
    future::{pending, poll_fn},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const VERSION: &str = "33333333-3333-3333-3333-333333333333";
const OPERATION: &str = "44444444-4444-4444-4444-444444444444";
const PATH: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Environment";
const LOCATION: &str =
    "https://api.fabric.microsoft.com/v1/operations/44444444-4444-4444-4444-444444444444";

fn parsed(wait: bool) -> Command {
    let mut args = vec![OsString::from("publish"), OsString::from(PATH)];
    if wait {
        args.push("--wait".into());
    }
    parse(args).unwrap()
}

fn metadata(version: &str, state: &str) -> AsyncRawResponse {
    response(
        200,
        &json!({
            "id":ITEM,"workspaceId":WORKSPACE,"type":"Environment",
            "properties":{"publishDetails":{"state":state,"targetVersion":version}}
        })
        .to_string(),
        &[],
    )
}

fn native(state: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    response(
        200,
        &json!({"publishDetails":{"state":state,"targetVersion":VERSION}}).to_string(),
        headers,
    )
}

fn accepted(body: &str, location: &str) -> AsyncRawResponse {
    response(
        202,
        body,
        &[
            ("x-ms-operation-id", OPERATION),
            ("Location", location),
            ("Retry-After", "120"),
        ],
    )
}

fn clients(http: Arc<ScriptedHttp>, timer: Arc<TestTimer>) -> (Client, HttpExecutor) {
    let client = Client::with_context(RequestContext::with_endpoint(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
        EndpointBinding::public(ServiceOrigin::Fabric),
    ));
    let executor = HttpExecutor::new(
        http,
        timer,
        ExecutionOptions {
            timeout: Duration::from_secs(240),
            max_delay: Duration::from_secs(240),
        },
    )
    .unwrap();
    (client, executor)
}

struct Approval {
    allow: bool,
    calls: AtomicUsize,
}

impl Approval {
    fn new(allow: bool) -> Self {
        Self {
            allow,
            calls: AtomicUsize::new(0),
        }
    }
}

impl ApprovalHost for Approval {
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedPublish,
    ) -> NativeFuture<'a, Result<bool, Failure>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            prepared.operation().operation_id(),
            environment_publish::OPERATION_ID
        );
        assert_eq!(prepared.operation().input()["source"], "existingStaging");
        assert_eq!(
            prepared.operation().target().identity().item_id(),
            Some(ITEM)
        );
        assert_eq!(prepared.context().selection().account(), "offline-account");
        assert_eq!(prepared.context().selection().tenant(), "offline-tenant");
        assert_eq!(prepared.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            prepared.context().endpoint().host(),
            "api.fabric.microsoft.com"
        );
        Box::pin(async { Ok(self.allow) })
    }
}

async fn invoke(http: Arc<ScriptedHttp>, timer: Arc<TestTimer>, wait: bool) -> Delivery {
    let (client, executor) = clients(http, timer);
    dispatch(
        parsed(wait),
        &client,
        &executor,
        &mut Approval::new(true),
        &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
        pending(),
    )
    .await
    .unwrap()
}

fn posts(http: &ScriptedHttp) -> usize {
    http.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.method() == Method::Post)
        .count()
}

#[test]
fn selected_parser_accepts_only_existing_typed_staging_and_wait() {
    assert!(!parsed(false).wait);
    assert!(parsed(true).wait);
    for path in [
        "/W.Workspace/E.Environment/staging",
        "/W.Workspace/E.Environment/staging/",
        "/W.Workspace/E.Notebook/staging",
        "/W.Workspace/E.Environment/staging/publish",
        "/W.Workspace/E.Environment/src/definition/staging",
        "E.Environment/staging",
        "/W.Workspace/E.Environment/staging?x=1",
        "/W.Workspace/E.Environment/../staging",
        "/W.Workspace/E.Environment/%2e/staging",
        "/W.Workspace/*/staging",
    ] {
        assert!(
            parse(vec!["publish".into(), path.into()]).is_err(),
            "{path}"
        );
    }
    for flag in [
        "--input",
        "--approve",
        "--plan",
        "--preview",
        "--force",
        "--timeout",
        "--output",
    ] {
        assert!(
            parse(vec!["publish".into(), PATH.into(), flag.into(), "x".into()]).is_err(),
            "{flag}"
        );
    }
}

#[tokio::test]
async fn immediate_native_success_uses_one_bodyless_release_post_and_verified_target() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success"), native("Success", &[])]);
    let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), false).await;
    assert_eq!(delivery.exit, 0);
    assert_eq!(delivery.value["status"], "succeeded");
    assert_eq!(delivery.value["meta"]["target"]["identity"]["itemId"], ITEM);
    assert_eq!(delivery.value["meta"]["submission"]["status"], "succeeded");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(requests[1].method(), Method::Post);
    assert_eq!(
        requests[1].url().as_str(),
        format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/environments/{ITEM}/staging/publish?beta=false"
        )
    );
    let Body::Bytes(body) = requests[1].body() else {
        panic!("publication body must be empty bytes, never JSON")
    };
    assert!(body.is_empty(), "absent payload is not {{}}");
}

#[tokio::test]
async fn accepted_without_wait_retains_real_handle_and_never_claims_publication() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success"), accepted("", LOCATION)]);
    let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), false).await;
    assert_eq!(delivery.exit, 0);
    assert_eq!(delivery.value["status"], "accepted");
    assert_eq!(
        delivery.value["meta"]["evidence"]["handles"][0]["id"],
        OPERATION
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert_eq!(posts(&http), 1);
}

#[tokio::test]
async fn accepted_wait_honors_120_seconds_and_never_gets_a_result() {
    let http = ScriptedHttp::new([
        metadata(VERSION, "Success"),
        accepted("", LOCATION),
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[(
                "Location",
                "https://api.fabric.microsoft.com/v1/operations/44444444-4444-4444-4444-444444444444/result",
            )],
        ),
    ]);
    let timer = Arc::new(TestTimer::default());
    let delivery = invoke(http.clone(), timer.clone(), true).await;
    assert_eq!(delivery.exit, 0);
    assert_eq!(delivery.value["status"], "succeeded");
    assert_eq!(delivery.value["meta"]["initialHttpStatus"], 202);
    assert_eq!(*timer.waits.lock().unwrap(), [120]);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].url().as_str(), LOCATION);
    drop(requests);
    assert_eq!(posts(&http), 1);
}

#[tokio::test]
async fn malformed_and_foreign_acceptance_never_poll_or_replay() {
    for (body, location) in [
        ("{}", LOCATION),
        (
            "",
            "https://foreign.invalid/v1/operations/44444444-4444-4444-4444-444444444444",
        ),
    ] {
        for wait in [false, true] {
            let http = ScriptedHttp::new([metadata(VERSION, "Success"), accepted(body, location)]);
            let timer = Arc::new(TestTimer::default());
            let delivery = invoke(http.clone(), timer.clone(), wait).await;
            assert_eq!(delivery.exit, 1);
            assert_eq!(delivery.value["status"], "unknown");
            assert_eq!(posts(&http), 1);
            assert_eq!(http.requests.lock().unwrap().len(), 2);
            assert!(timer.waits.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn host_refusal_does_not_acquire_mutation_authority_or_post() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success")]);
    let (client, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let mut approval = Approval::new(false);
    let error = dispatch(
        parsed(false),
        &client,
        &executor,
        &mut approval,
        &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
        pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.0, "ApprovalDenied");
    assert_eq!(approval.calls.load(Ordering::SeqCst), 1);
    assert_eq!(posts(&http), 0);
}

#[tokio::test]
async fn names_resolve_to_one_typed_id_before_verification_and_approval() {
    let http = ScriptedHttp::new([
        response(
            200,
            &json!({"value":[
                {"id":VERSION,"displayName":"Runtime","type":"Notebook"},
                {"id":ITEM,"displayName":"Runtime","type":"Environment"}
            ]})
            .to_string(),
            &[],
        ),
        metadata(VERSION, "Success"),
        native("Success", &[]),
    ]);
    let (client, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let command = parse(vec![
        "publish".into(),
        format!("/{WORKSPACE}.Workspace/Runtime.Environment").into(),
    ])
    .unwrap();
    let delivery = dispatch(
        command,
        &client,
        &executor,
        &mut Approval::new(true),
        &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.exit, 0);
    assert_eq!(delivery.value["meta"]["target"]["identity"]["itemId"], ITEM);
    assert_eq!(http.requests.lock().unwrap().len(), 3);
    assert_eq!(posts(&http), 1);
}

#[tokio::test]
async fn ambiguous_names_never_reach_approval_or_publish() {
    let http = ScriptedHttp::new([response(
        200,
        &json!({"value":[
            {"id":VERSION,"displayName":"Runtime","type":"Environment"},
            {"id":ITEM,"displayName":"Runtime","type":"Environment"}
        ]})
        .to_string(),
        &[],
    )]);
    let (client, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let command = parse(vec![
        "publish".into(),
        format!("/{WORKSPACE}.Workspace/Runtime.Environment").into(),
    ])
    .unwrap();
    let mut approval = Approval::new(true);
    let error = dispatch(
        command,
        &client,
        &executor,
        &mut approval,
        &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
        pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.0, "TargetResolutionFailed");
    assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
    assert_eq!(posts(&http), 0);
}

#[tokio::test]
async fn expired_total_budget_prevents_discovery_verification_and_approval() {
    let http = ScriptedHttp::new([]);
    let (client, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let deadline = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let mut approval = Approval::new(true);
    let error = dispatch(
        parsed(false),
        &client,
        &executor,
        &mut approval,
        &deadline,
        pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.0, "TimedOut");
    assert!(http.requests.lock().unwrap().is_empty());
    assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_write_denial_is_separate_from_host_approval() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success"), response(403, "", &[])]);
    let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), true).await;
    assert_eq!(delivery.exit, 1);
    assert_eq!(delivery.value["status"], "failed");
    assert_eq!(delivery.value["meta"]["evidence"]["httpStatus"], 403);
    assert_eq!(posts(&http), 1);
}

#[tokio::test]
async fn denied_or_wrong_owner_preflight_never_reaches_approval() {
    for reply in [
        response(403, "", &[]),
        response(
            200,
            &json!({"id":VERSION,"workspaceId":WORKSPACE,"type":"Environment"}).to_string(),
            &[],
        ),
        response(
            200,
            &json!({"id":ITEM,"workspaceId":WORKSPACE,"type":"Notebook"}).to_string(),
            &[],
        ),
    ] {
        let http = ScriptedHttp::new([reply]);
        let (client, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
        let mut approval = Approval::new(true);
        assert!(
            dispatch(
                parsed(false),
                &client,
                &executor,
                &mut approval,
                &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
        assert_eq!(posts(&http), 0);
    }
}

#[test]
fn approval_cannot_be_substituted_or_reused() {
    let (client, _) = clients(ScriptedHttp::new([]), Arc::new(TestTimer::default()));
    let target = ResolvedTarget::new(
        PATH,
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
    .unwrap();
    let first = environment_publish::prepare(
        &client,
        target.clone(),
        PublishSource::ExistingStaging,
        ObservationPolicy::ReturnAccepted,
    )
    .unwrap();
    let other = environment_publish::prepare(
        &client,
        target,
        PublishSource::ExistingStaging,
        ObservationPolicy::WaitForOperation,
    )
    .unwrap();
    let mut policy = ImmediatePolicy {
        operation: first.operation(),
        approved: true,
    };
    assert!(other.authorize(&mut policy).is_err());
    assert!(first.authorize(&mut policy).is_ok());
    assert!(first.authorize(&mut policy).is_err());
}

#[tokio::test]
async fn native_running_no_wait_is_incomplete_not_a_fabric_handle() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success"), native("Running", &[])]);
    let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), false).await;
    assert_eq!(delivery.exit, 1);
    assert_eq!(delivery.value["status"], "partial");
    assert_eq!(delivery.value["meta"]["evidence"]["handles"], json!([]));
    assert_eq!(delivery.value["data"]["data"]["targetVersion"], VERSION);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn native_running_wait_observes_only_its_original_version_using_shared_driver() {
    let http = ScriptedHttp::new([
        metadata(VERSION, "Success"),
        native("Running", &[("Retry-After", "120")]),
        metadata(VERSION, "Running"),
        metadata(VERSION, "Success"),
    ]);
    let timer = Arc::new(TestTimer::default());
    let delivery = invoke(http.clone(), timer.clone(), true).await;
    assert_eq!(delivery.exit, 0);
    assert_eq!(delivery.value["status"], "succeeded");
    assert_eq!(delivery.value["meta"]["submission"]["status"], "partial");
    assert_eq!(
        delivery.value["meta"]["evidence"]["coverage"]["kind"],
        "complete"
    );
    assert_eq!(delivery.value["meta"]["sources"][0]["error"], Value::Null);
    assert_eq!(
        delivery.value["meta"]["submission"]["meta"]["evidence"]["coverage"]["kind"],
        "incomplete"
    );
    assert_eq!(
        delivery.value["meta"]["lastNativeObservation"]["targetVersion"],
        VERSION
    );
    assert_eq!(*timer.waits.lock().unwrap(), [120, 1]);
    assert_eq!(posts(&http), 1);
    assert_eq!(http.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn changed_native_version_or_denied_read_cannot_certify_completion() {
    for reply in [metadata(OPERATION, "Success"), response(403, "", &[])] {
        let http = ScriptedHttp::new([metadata(VERSION, "Success"), native("Running", &[]), reply]);
        let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), true).await;
        assert_eq!(delivery.exit, 1);
        assert_eq!(delivery.value["status"], "unknown");
        assert_eq!(
            delivery.value["meta"]["submission"]["data"]["data"]["targetVersion"],
            VERSION
        );
        assert_eq!(posts(&http), 1);
        assert_eq!(http.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn failed_and_malformed_native_states_do_not_become_http_success() {
    for (reply, status) in [
        (native("Failed", &[]), "failed"),
        (native("Cancelled", &[]), "failed"),
        (native("Mystery", &[]), "unknown"),
        (response(200, "{}", &[]), "unknown"),
        (
            response(
                200,
                r#"{"publishDetails":{"state":"Success","componentPublishInfo":{"sparkSettings":{"state":"Running"}}}}"#,
                &[],
            ),
            "partial",
        ),
    ] {
        let http = ScriptedHttp::new([metadata(VERSION, "Success"), reply]);
        let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), true).await;
        assert_eq!(delivery.exit, 1);
        assert_eq!(delivery.value["status"], status);
        assert_eq!(posts(&http), 1);
    }
}

#[tokio::test]
async fn total_deadline_after_acceptance_keeps_handle_without_early_poll_or_remote_cancel() {
    for native_wait in [false, true] {
        let reply = if native_wait {
            native("Running", &[("Retry-After", "120")])
        } else {
            accepted("", LOCATION)
        };
        let http = ScriptedHttp::new([metadata(VERSION, "Success"), reply]);
        let timer = Arc::new(TestTimer::default());
        timer.pending_wait.store(true, Ordering::SeqCst);
        let (client, executor) = clients(http.clone(), timer.clone());
        let delivery = dispatch(
            parsed(true),
            &client,
            &executor,
            &mut Approval::new(true),
            &NativeDeadline::new(Duration::from_millis(20)).unwrap(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(delivery.exit, 1);
        assert_eq!(delivery.value["status"], "unknown");
        assert_eq!(delivery.value["meta"]["remoteCancelled"], false);
        assert_eq!(delivery.value["meta"]["localInterruption"], "TimedOut");
        assert_eq!(delivery.value["meta"]["remoteReplayed"], false);
        if !native_wait {
            assert_eq!(
                delivery.value["meta"]["evidence"]["handles"][0]["id"],
                OPERATION
            );
        }
        assert_eq!(*timer.waits.lock().unwrap(), [120]);
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn interruption_after_acceptance_never_cancels_or_replays_publication() {
    let http = ScriptedHttp::new([metadata(VERSION, "Success"), accepted("", LOCATION)]);
    let timer = Arc::new(TestTimer::default());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let (client, executor) = clients(http.clone(), timer.clone());
    let cancelled = poll_fn(|cx| {
        if timer.waits.lock().unwrap().is_empty() {
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let delivery = dispatch(
        parsed(true),
        &client,
        &executor,
        &mut Approval::new(true),
        &NativeDeadline::new(Duration::from_secs(240)).unwrap(),
        cancelled,
    )
    .await
    .unwrap();
    assert_eq!(delivery.exit, 1);
    assert_eq!(
        delivery.value["meta"]["evidence"]["handles"][0]["id"],
        OPERATION
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert_eq!(posts(&http), 1);
}

struct FailOnce {
    fail_write: bool,
    fail_flush: bool,
    bytes: Vec<u8>,
}

impl Write for FailOnce {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            self.fail_write = false;
            return Err(io::Error::other("synthetic delivery failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            self.fail_flush = false;
            return Err(io::Error::other("synthetic flush failure"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn receipt_writer_and_flush_failures_preserve_actual_outcome_handle_and_one_post() {
    for fail_flush in [false, true] {
        let http = ScriptedHttp::new([metadata(VERSION, "Success"), accepted("", LOCATION)]);
        let delivery = invoke(http.clone(), Arc::new(TestTimer::default()), false).await;
        let mut stdout = FailOnce {
            fail_write: !fail_flush,
            fail_flush,
            bytes: Vec::new(),
        };
        let mut stderr = Vec::new();
        assert_eq!(delivery.write_to(&mut stdout, &mut stderr), 1);
        let value: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(value["status"], "accepted");
        assert_eq!(value["meta"]["delivery"]["code"], "OutputFailure");
        assert_eq!(value["meta"]["evidence"]["handles"][0]["id"], OPERATION);
        assert_eq!(posts(&http), 1);
    }
}
