// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::update;
use fabric_sdk::{Audience, CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    io::{Cursor, Write},
    pin::Pin,
    sync::{Arc, Mutex},
};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const TARGET: &str =
    "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.Notebook";

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "synthetic-offline",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn bound(cwd: &std::path::Path, extra: &[&str]) -> PreparedUpdateCommand {
    bound_with_credential(cwd, extra, Arc::new(Credential))
}

fn bound_with_credential(
    cwd: &std::path::Path,
    extra: &[&str],
    credential: Arc<dyn TokenCredential>,
) -> PreparedUpdateCommand {
    let command = update::parse(
        [
            "fab-cli-v2",
            "artifact",
            "notebook",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
        ]
        .into_iter()
        .chain(extra.iter().copied()),
    )
    .unwrap();
    PreparedUpdateCommand::preflight(
        command,
        cwd,
        &mut Cursor::new(br#"{"displayName":"new","description":""}"#),
    )
    .unwrap()
    .with_context(
        RequestContext::new(
            credential,
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        ),
        OperationLimits::default(),
    )
    .unwrap()
}

#[derive(Debug)]
struct PendingCredential;

#[async_trait]
impl TokenCredential for PendingCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        pending().await
    }
}

#[tokio::test]
async fn cancellation_during_credentials_retains_failed_no_send_receipt_across_publication() {
    let dir = tempfile::tempdir().unwrap();
    let prepared = bound_with_credential(dir.path(), &[], Arc::new(PendingCredential));
    let mut policy = ExactApproval(Some(prepared.update().prepared().clone()));
    let backend = wire(200, metadata(), false);
    let error = execute_and_publish(
        prepared,
        backend.clone(),
        &mut policy,
        tokio::time::sleep(Duration::from_millis(10)),
        tokio::time::Instant::now() + Duration::from_secs(5),
        io::sink(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "InterruptedAfterDispatch");
    assert_eq!(error.2["status"], "failed");
    assert_eq!(
        error.2["meta"]["evidence"]["mutationMayHaveOccurred"],
        false
    );
    assert_eq!(*backend.calls.lock().unwrap(), 0);
}

#[derive(Clone)]
struct Wire {
    calls: Arc<Mutex<usize>>,
    response: Arc<Mutex<Option<Result<OperationResponse, OperationExecutionError>>>>,
}

impl OperationExecutor for Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        *self.calls.lock().unwrap() += 1;
        assert_eq!(format!("{:?}", request.method()), "Patch");
        assert_eq!(
            request.url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/notebooks/{ITEM}")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(request.body()).unwrap(),
            json!({"displayName":"new","description":""})
        );
        Box::pin(std::future::ready(
            self.response.lock().unwrap().take().expect("replay"),
        ))
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("no polling or retry")
    }
}

fn wire(status: u16, body: Value, accepted: bool) -> Wire {
    let headers = if accepted {
        BTreeMap::from([(
            "x-ms-operation-id".into(),
            "cccccccc-dddd-eeee-ffff-111111111111".into(),
        )])
    } else {
        BTreeMap::new()
    };
    Wire {
        calls: Arc::default(),
        response: Arc::new(Mutex::new(Some(Ok(OperationResponse {
            status,
            headers,
            body: serde_json::to_vec(&body).unwrap(),
        })))),
    }
}

async fn execute(bound: PreparedUpdateCommand, wire: Wire) -> UpdateDelivery {
    let mut policy = ExactApproval(Some(bound.update().prepared().clone()));
    bound
        .execute_bound(wire, &mut policy, pending())
        .await
        .unwrap()
}

fn metadata() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":"Notebook","displayName":"new","description":""})
}

#[test]
fn notebook_host_rejects_other_owners_before_input_or_credentials() {
    let target = TARGET.replace(".Notebook", ".Lakehouse");
    let command = update::parse([
        "fab-cli-v2",
        "update",
        &target,
        "--input",
        "not-a-local-file.json",
        "--mode",
        "patch",
    ])
    .unwrap();
    assert_eq!(run(command).unwrap_err().0, "UnsupportedCapability");
}

#[tokio::test]
async fn prompt_and_single_use_policy_bind_exact_body_context_options_and_conditions() {
    let dir = tempfile::tempdir().unwrap();
    let prepared = bound(dir.path(), &["--project", "displayName"]);
    let operation = prepared.update().prepared().clone();
    let question = approval_question(&operation).unwrap();
    assert!(question.contains("unguardedConcurrency"));
    assert!(question.contains("existingOnly"));
    assert!(question.contains(&serde_json::to_string(&operation).unwrap()));
    assert_eq!(
        operation.input()["body"],
        json!({"displayName":"new","description":""})
    );
    assert!(question.contains(r#""account":"offline""#));
    assert!(question.contains("https://api.fabric.microsoft.com"));
    assert!(question.contains("maxResponseBytes"));
    let other = bound(dir.path(), &[]);
    let backend = wire(200, metadata(), false);
    assert!(matches!(
        other
            .execute_bound(
                backend.clone(),
                &mut ExactApproval(Some(operation.clone())),
                pending()
            )
            .await,
        Err(UpdateCommandError::Approval)
    ));
    assert_eq!(*backend.calls.lock().unwrap(), 0);
    let mut policy = ExactApproval(Some(operation.clone()));
    policy.authorize(&operation).unwrap();
    assert!(policy.authorize(&operation).is_err());
    let delivery = execute(prepared, backend.clone()).await;
    let mut output = Vec::new();
    deliver(
        delivery,
        tokio::time::Instant::now() + Duration::from_secs(5),
        &mut output,
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["data"], "new");
    assert_eq!(value["meta"]["target"]["identity"]["itemId"], ITEM);
    assert_eq!(value["meta"]["evidence"]["httpStatus"], 200);
    assert_eq!(*backend.calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn all_remote_classifications_retain_receipts_without_polling_or_replay() {
    let dir = tempfile::tempdir().unwrap();
    for (status, body, accepted, expected) in [
        (200, metadata(), false, "succeeded"),
        (
            200,
            json!({"id":ITEM,"type":"Notebook","displayName":"old","description":""}),
            false,
            "partial",
        ),
        (
            200,
            json!({"id":"bbbbbbbb-cccc-dddd-eeee-ffffffffffff"}),
            false,
            "unknown",
        ),
        (202, Value::Null, true, "accepted"),
        (202, Value::Null, false, "unknown"),
        (403, json!({"error":{"code":"Forbidden"}}), false, "failed"),
        (404, Value::Null, false, "failed"),
        (412, Value::Null, false, "failed"),
        (429, Value::Null, false, "failed"),
        (503, Value::Null, false, "unknown"),
    ] {
        let backend = wire(status, body, accepted);
        let delivery = execute(bound(dir.path(), &[]), backend.clone()).await;
        let mut output = Vec::new();
        let result = deliver(
            delivery,
            tokio::time::Instant::now() + Duration::from_secs(5),
            &mut output,
        );
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["status"], expected, "HTTP {status}");
        assert_eq!(value["meta"]["evidence"]["httpStatus"], status);
        assert_eq!(result.is_ok(), matches!(expected, "succeeded" | "accepted"));
        if expected == "accepted" {
            assert!(
                !value["meta"]["evidence"]["handles"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(*backend.calls.lock().unwrap(), 1);
    }
}

#[derive(Clone)]
struct PendingPatch(Arc<Mutex<usize>>);

impl OperationExecutor for PendingPatch {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        assert_eq!(format!("{:?}", request.method()), "Patch");
        *self.0.lock().unwrap() += 1;
        Box::pin(pending())
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("no polling or retry")
    }
}

#[tokio::test]
async fn cancellation_during_patch_retains_unknown_receipt_across_publication_without_repoll_panic()
{
    let dir = tempfile::tempdir().unwrap();
    let prepared = bound(dir.path(), &[]);
    let mut policy = ExactApproval(Some(prepared.update().prepared().clone()));
    let backend = PendingPatch(Arc::default());
    let error = execute_and_publish(
        prepared,
        backend.clone(),
        &mut policy,
        tokio::time::sleep(Duration::from_millis(10)),
        tokio::time::Instant::now() + Duration::from_secs(5),
        io::sink(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "InterruptedAfterDispatch");
    assert_eq!(error.2["status"], "unknown");
    assert_eq!(error.2["meta"]["evidence"]["mutationMayHaveOccurred"], true);
    assert_eq!(*backend.0.lock().unwrap(), 1);
}

#[tokio::test]
async fn declined_or_cancelled_before_dispatch_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let backend = wire(200, metadata(), false);
    assert!(matches!(
        bound(dir.path(), &[])
            .execute_bound(backend.clone(), &mut ExactApproval(None), pending())
            .await,
        Err(UpdateCommandError::Approval)
    ));
    let prepared = bound(dir.path(), &[]);
    let mut policy = ExactApproval(Some(prepared.update().prepared().clone()));
    let delivery = prepared
        .execute_bound(backend.clone(), &mut policy, std::future::ready(()))
        .await
        .unwrap();
    assert!(matches!(
        delivery.report().receipt.outcome(),
        Outcome::Failed { .. }
    ));
    assert_eq!(*backend.calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn lost_response_and_delivery_failures_keep_evidence_and_never_resubmit() {
    let dir = tempfile::tempdir().unwrap();
    let backend = wire(200, metadata(), false);
    *backend.response.lock().unwrap() = Some(Err(OperationExecutionError::Failed));
    let delivery = execute(bound(dir.path(), &[]), backend.clone()).await;
    assert!(matches!(
        delivery.report().receipt.outcome(),
        Outcome::Unknown { .. }
    ));
    assert_eq!(*backend.calls.lock().unwrap(), 1);

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let backend = wire(200, metadata(), false);
    let delivery = execute(bound(dir.path(), &[]), backend.clone()).await;
    let error = deliver(
        delivery,
        tokio::time::Instant::now() + Duration::from_secs(5),
        &mut Broken,
    )
    .unwrap_err();
    assert_eq!(error.2["status"], "succeeded");
    assert_eq!(*backend.calls.lock().unwrap(), 1);

    let output = dir.path().join("race.json");
    let prepared = bound(dir.path(), &["--output", output.to_str().unwrap()]);
    let backend = wire(200, metadata(), false);
    let delivery = execute(prepared, backend.clone()).await;
    std::fs::write(&output, b"preserve").unwrap();
    let error = deliver(
        delivery,
        tokio::time::Instant::now() + Duration::from_secs(5),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(error.2["status"], "succeeded");
    assert_eq!(std::fs::read(output).unwrap(), b"preserve");
    assert_eq!(*backend.calls.lock().unwrap(), 1);

    let backend = wire(200, metadata(), false);
    let delivery = execute(bound(dir.path(), &[]), backend.clone()).await;
    let mut output = Vec::new();
    let error = deliver(delivery, tokio::time::Instant::now(), &mut output).unwrap_err();
    assert_eq!(error.0, "TimedOut");
    assert_eq!(error.2["status"], "succeeded");
    assert!(output.is_empty());
    assert_eq!(*backend.calls.lock().unwrap(), 1);
}
