// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser -> mounted KQL host -> shared native HTTP operation contracts.
#![cfg(feature = "v2-link-kql")]
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli::v2_link_kql_host::{self as host, Delivery, KqlDestination};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{LinkApproval, LinkEffects, LinkFiles, LinkResolver, ResolvedLinkTarget},
};
use fabric_sdk::{Audience, CredentialSelection, RequestContext};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_operations::{OperationLimits, OperationSession};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::{pending, poll_fn},
    io::{self, Write},
    num::NonZeroUsize,
    sync::{Arc, atomic::Ordering},
    task::Poll,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const DB: &str = "22222222-2222-4222-8222-222222222222";
const OP: &str = "55555555-5555-4555-8555-555555555555";
const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.KQLDatabase/Tables/sales_ref.Shortcut";
const LOCATION: &str =
    "https://api.fabric.microsoft.com/v1/operations/55555555-5555-4555-8555-555555555555";

fn input() -> Value {
    json!({
        "name":"sales_ref", "enableQueryAcceleration":true,
        "target":{"oneLake":{"workspaceId":WS,"itemId":DB,"path":"Tables/sales"}}
    })
}

fn command(facade: bool, wait: bool) -> fabric_cli_mutations::v2_link::LinkCommand {
    let mut args = if facade {
        vec!["artifact", "kql-database", "link", TARGET, "--input", "-"]
    } else {
        vec!["link", TARGET, "--input", "-"]
    };
    if wait {
        args.push("--wait");
    }
    host::parse(args.into_iter().map(OsString::from)).unwrap()
}

struct Resolver;
impl LinkResolver for Resolver {
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        Box::pin(async move { KqlDestination::parse(path)?.resolved(WS, DB) })
    }
}

struct Files(Vec<u8>);
impl LinkFiles for Files {
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        assert_eq!(path, "-");
        assert_eq!(limit.get(), host::MAX_BYTES);
        Box::pin(std::future::ready(Ok(self.0.clone())))
    }
}

struct Approval {
    allowed: bool,
    calls: Vec<(ResolvedLinkTarget, LinkEffects)>,
}
impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.calls.push((target.clone(), effects));
        Box::pin(std::future::ready(Ok(self.allowed)))
    }
}

fn approval(allowed: bool) -> Approval {
    Approval {
        allowed,
        calls: Vec::new(),
    }
}

async fn prepared(facade: bool, wait: bool) -> fabric_cli_mutations::v2_link::PreparedLink {
    host::prepare(
        &command(facade, wait),
        &mut Resolver,
        &mut Files(serde_json::to_vec(&input()).unwrap()),
    )
    .await
    .unwrap()
}

fn context(credential: Arc<TestCredential>) -> RequestContext {
    RequestContext::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    )
}

fn executor(http: Arc<ScriptedHttp>, timer: Arc<TestTimer>) -> HttpExecutor {
    HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap()
}

#[tokio::test]
async fn root_and_facade_send_exact_native_route_body_and_visible_effects_once() {
    for facade in [false, true] {
        let http = ScriptedHttp::new([response(201, r#"{"name":"sales_ref"}"#, &[])]);
        let credential = Arc::new(TestCredential::default());
        let context = context(credential.clone());
        let mut operations = OperationSession::with_context(
            &context,
            executor(http.clone(), Arc::new(TestTimer::default())),
            OperationLimits::default(),
            pending(),
        );
        let mut approval = approval(true);
        let delivery = Delivery::execute(
            prepared(facade, false).await,
            &mut operations,
            Some(&mut approval),
        )
        .await;
        assert!(delivery.result().is_ok());
        assert_eq!(approval.calls.len(), 1);
        assert!(approval.calls[0].1.query_acceleration);
        let question = host::approval_question(&approval.calls[0].0, approval.calls[0].1);
        for required in [
            "sales_ref",
            WS,
            DB,
            "cache",
            "indexing",
            "storage",
            "compute costs",
            "no copy",
            "ACL",
        ] {
            assert!(
                question.contains(required),
                "missing disclosure: {required}"
            );
        }
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), azure_core::http::Method::Post);
        assert_eq!(
            requests[0].url().as_str(),
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases/{DB}/shortcuts"
            )
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&azure_core::Bytes::from(requests[0].body())).unwrap(),
            input()
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        let value = delivery.metadata();
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["target"]["name"], "sales_ref");
        assert_eq!(value["httpStatus"], 201);
        assert_eq!(value["remoteReplayed"], false);
        assert!(!value.to_string().contains("oneLake"));
    }
}

#[test]
fn unsupported_owners_children_flags_and_nonliteral_paths_fail_before_host_io() {
    for path in [
        "/w.Workspace/db.Lakehouse/Tables/ref.Shortcut",
        "/w.Workspace/db.KQLDatabase/Tables/ref",
        "/w.Workspace/db.KQLDatabase/Files/ref.Shortcut",
        "/w.Workspace/db.KQLDatabase/Tables/ref.Shortcut/child",
        "/w.Workspace/db.KQLDatabase/Tables/../ref.Shortcut",
        "/w.Workspace/db.KQLDatabase/Tables/%72ef.Shortcut",
        "/w.Workspace/db.KQLDatabase",
    ] {
        assert!(
            host::parse(
                ["link", path, "--input", "-"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
    }
    for flag in ["--force", "--overwrite"] {
        assert!(
            host::parse(
                ["link", TARGET, "--input", "-", flag]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
    }
    assert!(
        host::parse(
            ["artifact", "lakehouse", "link", TARGET, "--input", "-"]
                .into_iter()
                .map(OsString::from)
        )
        .is_err()
    );
}

#[tokio::test]
async fn unsupported_provider_duplicate_unknown_conflict_and_oversize_input_send_nothing() {
    let mut unsupported = input();
    unsupported["target"] = json!({"oneDriveSharePoint":{"connectionId":DB,"location":"https://example.sharepoint.com","subpath":"/sales","updateFabricItemSensitivity":true}});
    let mut share = input();
    share["target"] = json!({"externalDataShare":{"id":DB}});
    let mut dataverse = input();
    dataverse["target"] = json!({"dataverse":{"connectionId":DB,"environmentDomain":"https://org.crm4.dynamics.com","tableName":"account","deltaLakeFolder":"folder/account"}});
    let mut path = input();
    path["path"] = json!("Tables");
    let mut conflict = input();
    conflict["name"] = json!("different");
    let mut unknown = input();
    unknown["unexpected"] = json!(true);
    let mut cases: Vec<Vec<u8>> = [unsupported, share, dataverse, path, conflict, unknown]
        .iter()
        .map(|value| serde_json::to_vec(value).unwrap())
        .collect();
    cases.extend([
        br#"{"name":"sales_ref","name":"sales_ref","enableQueryAcceleration":true,"target":{}}"#.to_vec(),
        format!(r#"{{"name":"sales_ref","enableQueryAcceleration":true,"target":{{"oneLake":{{"workspaceId":"{WS}","workspaceId":"{WS}","itemId":"{DB}","path":"Tables/sales"}}}}}}"#).into_bytes(),
        vec![b' '; host::MAX_BYTES + 1], vec![0xff],
    ]);
    for bytes in cases {
        // No ResourceOperations, token or HTTP client exists during preflight.
        assert!(
            host::prepare(&command(false, false), &mut Resolver, &mut Files(bytes))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn six_native_kql_provider_kinds_preserve_exact_payloads() {
    let targets = [
        input()["target"].clone(),
        json!({"adlsGen2":{"connectionId":DB,"location":"https://storage.dfs.core.windows.net","subpath":"/container/folder"}}),
        json!({"amazonS3":{"connectionId":DB,"location":"https://bucket.s3.us-west-2.amazonaws.com","subpath":"/folder"}}),
        json!({"googleCloudStorage":{"connectionId":DB,"location":"https://bucket.storage.googleapis.com","subpath":"/folder"}}),
        json!({"s3Compatible":{"connectionId":DB,"location":"https://s3.example.invalid","bucket":"bucket","subpath":"/folder"}}),
        json!({"azureBlobStorage":{"connectionId":DB,"location":"https://storage.blob.core.windows.net","subpath":"container/folder"}}),
    ];
    for target in targets {
        let body = json!({"name":"sales_ref","enableQueryAcceleration":false,"target":target});
        let prepared = host::prepare(
            &command(true, false),
            &mut Resolver,
            &mut Files(serde_json::to_vec(&body).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(prepared.operation().body(), Some(&body));
        assert!(!prepared.effects().query_acceleration);
    }
}

#[cfg(feature = "headless")]
#[test]
fn executable_routes_both_spellings_without_auth_or_update_dispatch() {
    for prefix in [vec!["link"], vec!["artifact", "kql-database", "link"]] {
        let help = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
            .args(&prefix)
            .arg("--help")
            .env_remove("FAB_RUST_AUTH_MODE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(help.status.success());
        assert!(help.stderr.is_empty());
        assert!(String::from_utf8(help.stdout).unwrap().contains("--input"));
        let unconfigured = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
            .args(&prefix)
            .args([TARGET, "--input", "-"])
            .env_remove("FAB_RUST_AUTH_MODE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(unconfigured.status.code(), Some(1));
        assert!(unconfigured.stdout.is_empty());
        let error: Value = serde_json::from_slice(&unconfigured.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
}

#[tokio::test]
async fn missing_or_declined_approval_and_identity_failure_send_no_request() {
    for mode in [0, 1, 2] {
        let http = ScriptedHttp::new([]);
        let credential = Arc::new(TestCredential::default());
        credential.failed.store(mode == 2, Ordering::SeqCst);
        let context = context(credential.clone());
        let mut operations = OperationSession::with_context(
            &context,
            executor(http.clone(), Arc::new(TestTimer::default())),
            OperationLimits::default(),
            pending(),
        );
        let mut approval = approval(mode == 2);
        let delivery = Delivery::execute(
            prepared(false, false).await,
            &mut operations,
            if mode == 0 { None } else { Some(&mut approval) },
        )
        .await;
        assert!(delivery.result().is_err());
        assert!(http.requests.lock().unwrap().is_empty());
        assert_eq!(delivery.metadata()["mutationMayHaveOccurred"], false);
        assert_eq!(
            credential.calls.load(Ordering::SeqCst),
            usize::from(mode == 2)
        );
    }
}

#[tokio::test]
async fn acceptance_wait_completion_failed_poll_and_malformed_response_retain_evidence() {
    for (wait, terminal) in [
        (false, ""),
        (true, "Succeeded"),
        (true, "Failed"),
        (true, "Cancelled"),
    ] {
        let mut replies = vec![response(
            202,
            "",
            &[("location", LOCATION), ("retry-after", "1")],
        )];
        if wait {
            replies.push(response(200, &json!({"status":terminal}).to_string(), &[]));
            if terminal == "Succeeded" {
                replies.push(response(200, r#"{"name":"sales_ref"}"#, &[]));
            }
        }
        let http = ScriptedHttp::new(replies);
        let context = context(Arc::new(TestCredential::default()));
        let mut operations = OperationSession::with_context(
            &context,
            executor(http.clone(), Arc::new(TestTimer::default())),
            OperationLimits::default(),
            pending(),
        );
        let delivery = Delivery::execute(
            prepared(true, wait).await,
            &mut operations,
            Some(&mut approval(true)),
        )
        .await;
        if !wait {
            assert_eq!(delivery.metadata()["status"], "accepted");
            assert_eq!(delivery.metadata()["operationId"], OP);
        } else if terminal == "Succeeded" {
            assert!(delivery.result().is_ok());
            assert_eq!(delivery.metadata()["operationId"], OP);
        } else {
            assert!(delivery.result().is_err());
            assert_eq!(delivery.metadata()["operationId"], OP);
            assert_eq!(delivery.metadata()["status"], "failed");
        }
        assert_eq!(
            http.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.method() == azure_core::http::Method::Post)
                .count(),
            1
        );
    }
    for (status, body) in [
        (403, r#"{"errorCode":"Forbidden","message":"synthetic"}"#),
        (201, r#"{"name":"wrong"}"#),
        (201, "not-json"),
    ] {
        let http = ScriptedHttp::new([response(status, body, &[])]);
        let context = context(Arc::new(TestCredential::default()));
        let mut operations = OperationSession::with_context(
            &context,
            executor(http.clone(), Arc::new(TestTimer::default())),
            OperationLimits::default(),
            pending(),
        );
        let delivery = Delivery::execute(
            prepared(false, false).await,
            &mut operations,
            Some(&mut approval(true)),
        )
        .await;
        assert!(delivery.result().is_err());
        assert_eq!(delivery.metadata()["mutationMayHaveOccurred"], true);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_waited_result_keeps_the_accepted_handle_and_observation() {
    let http = ScriptedHttp::new([
        response(202, "", &[("location", LOCATION), ("retry-after", "1")]),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
        response(200, r#"{"name":"wrong"}"#, &[]),
    ]);
    let context = context(Arc::new(TestCredential::default()));
    let mut operations = OperationSession::with_context(
        &context,
        executor(http.clone(), Arc::new(TestTimer::default())),
        OperationLimits::default(),
        pending(),
    );
    let delivery = Delivery::execute(
        prepared(false, true).await,
        &mut operations,
        Some(&mut approval(true)),
    )
    .await;
    assert_eq!(delivery.metadata()["status"], "unknown");
    assert_eq!(delivery.metadata()["operationId"], OP);
    assert_eq!(delivery.metadata()["observationRetained"], true);
    assert!(delivery.result().as_ref().unwrap_err().observed.is_some());
    assert_eq!(http.requests.lock().unwrap().len(), 3);
}

struct BrokenWriter {
    flush: bool,
}
impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("synthetic write failure"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[tokio::test]
async fn cancelled_after_acceptance_and_writer_failure_never_replay() {
    let http = ScriptedHttp::new([response(
        202,
        "",
        &[("location", LOCATION), ("retry-after", "1")],
    )]);
    let timer = Arc::new(TestTimer::default());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let context = context(Arc::new(TestCredential::default()));
    let cancelled = poll_fn(|cx| {
        if timer.waits.lock().unwrap().is_empty() {
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let mut operations = OperationSession::with_context(
        &context,
        executor(http.clone(), timer.clone()),
        OperationLimits::default(),
        cancelled,
    );
    let delivery = Delivery::execute(
        prepared(false, true).await,
        &mut operations,
        Some(&mut approval(true)),
    )
    .await;
    assert_eq!(delivery.metadata()["status"], "unknown");
    assert_eq!(delivery.metadata()["operationId"], OP);
    assert_eq!(delivery.metadata()["remoteCancelled"], false);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    for flush in [false, true] {
        let mut stderr = Vec::new();
        assert_eq!(
            delivery.write_to(&mut BrokenWriter { flush }, &mut stderr),
            1
        );
        // Error delivery uses stderr, so a failed stdout is not involved here.
        assert_eq!(
            serde_json::from_slice::<Value>(&stderr).unwrap()["operationId"],
            OP
        );
    }
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn successful_receipt_survives_write_and_flush_failures_without_resubmission() {
    let http = ScriptedHttp::new([response(202, "", &[("location", LOCATION)])]);
    let context = context(Arc::new(TestCredential::default()));
    let mut operations = OperationSession::with_context(
        &context,
        executor(http.clone(), Arc::new(TestTimer::default())),
        OperationLimits::default(),
        pending(),
    );
    let delivery = Delivery::execute(
        prepared(false, false).await,
        &mut operations,
        Some(&mut approval(true)),
    )
    .await;
    for flush in [false, true] {
        let mut stderr = Vec::new();
        assert_eq!(
            delivery.write_to(&mut BrokenWriter { flush }, &mut stderr),
            1
        );
        let fallback: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(fallback["error"]["code"], "OutputFailed");
        assert_eq!(fallback["evidence"]["operationId"], OP);
        assert_eq!(fallback["evidence"]["status"], "accepted");
        assert!(delivery.result().is_ok());
    }
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}
