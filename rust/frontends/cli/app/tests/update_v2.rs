// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Injected root host: same SDK request/receipt and preflight-before-mutation.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::update::{self, PreparedUpdateCommand, UpdateCommandError};
use fabric_sdk::{
    Audience, CredentialSelection, RequestContext, TokenCredential,
    v2::{ApprovalPolicy, Outcome, PreparedOperation},
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    io::{self, Cursor, Write},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
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
            "synthetic-root",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
fn context() -> RequestContext {
    RequestContext::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    )
}
struct Policy(bool);
impl ApprovalPolicy for Policy {
    type Error = &'static str;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(operation.target().identity().item_id(), Some(ITEM));
        assert_eq!(operation.input()["body"], json!({"displayName":"new"}));
        if self.0 { Ok(()) } else { Err("denied") }
    }
}
#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<String>>>);
impl OperationExecutor for Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.lock().unwrap().push(request.url().to_string());
        assert_eq!(
            serde_json::from_slice::<Value>(request.body()).unwrap(),
            json!({"displayName":"new"})
        );
        Box::pin(std::future::ready(Ok(OperationResponse { status:200, headers:BTreeMap::new(), body:serde_json::to_vec(&json!({"id":ITEM,"workspaceId":WS,"type":"Notebook","displayName":"new","privateExtra":"retained"})).unwrap() })))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("unexpected poll")
    }
}
fn command(extra: &[&str]) -> update::UpdateCommand {
    update::parse(
        [
            "fab-cli-v2",
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
    .unwrap()
}
fn preflight(command: update::UpdateCommand, cwd: &std::path::Path) -> PreparedUpdateCommand {
    PreparedUpdateCommand::preflight(command, cwd, &mut Cursor::new(br#"{"displayName":"new"}"#))
        .unwrap()
}

#[test]
fn local_plan_and_no_clobber_input_preflight_need_no_context() {
    let dir = tempfile::tempdir().unwrap();
    let plan = preflight(command(&["--plan"]), dir.path());
    let mut output = Vec::new();
    plan.deliver_plan(&mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["meta"]["executionMode"], "plan");
    let existing = dir.path().join("receipt.json");
    std::fs::write(&existing, b"keep").unwrap();
    let args = command(&["--output", existing.to_str().unwrap()]);
    assert!(matches!(
        PreparedUpdateCommand::preflight(
            args,
            dir.path(),
            &mut Cursor::new(br#"{"displayName":"new"}"#)
        ),
        Err(UpdateCommandError::File(_))
    ));
    assert_eq!(std::fs::read(existing).unwrap(), b"keep");
    assert!(
        PreparedUpdateCommand::preflight(
            command(&[]),
            dir.path(),
            &mut Cursor::new(br#"{"displayName":null}"#)
        )
        .is_err()
    );
}

#[tokio::test]
async fn root_executes_one_sdk_patch_and_projection_cannot_hide_meta() {
    let dir = tempfile::tempdir().unwrap();
    let prepared = preflight(command(&["--project", "displayName"]), dir.path());
    let wire = Wire::default();
    let delivery = prepared
        .execute(
            context(),
            wire.clone(),
            OperationLimits::default(),
            &mut Policy(true),
            pending(),
        )
        .await
        .unwrap();
    assert!(matches!(
        delivery.report().receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["data"], "new");
    assert_eq!(result["meta"]["target"]["identity"]["itemId"], ITEM);
    assert_eq!(
        result["meta"]["preconditions"][1]["kind"],
        "unguardedConcurrency"
    );
    assert_eq!(
        wire.0.lock().unwrap().as_slice(),
        [format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WS}/notebooks/{ITEM}"
        )]
    );
}

#[tokio::test]
async fn root_decline_and_plan_intent_dispatch_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let wire = Wire::default();
    assert!(matches!(
        preflight(command(&[]), dir.path())
            .execute(
                context(),
                wire.clone(),
                OperationLimits::default(),
                &mut Policy(false),
                pending()
            )
            .await,
        Err(UpdateCommandError::Approval)
    ));
    assert!(
        preflight(command(&["--plan"]), dir.path())
            .execute(
                context(),
                wire.clone(),
                OperationLimits::default(),
                &mut Policy(true),
                pending()
            )
            .await
            .is_err()
    );
    assert!(wire.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn racing_output_or_writer_failure_retains_remote_success_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("race.json");
    let prepared = preflight(command(&["--output", output.to_str().unwrap()]), dir.path());
    let wire = Wire::default();
    let delivery = prepared
        .execute(
            context(),
            wire.clone(),
            OperationLimits::default(),
            &mut Policy(true),
            pending(),
        )
        .await
        .unwrap();
    std::fs::write(&output, b"racing owner").unwrap();
    let failure = delivery.deliver(&mut Vec::new()).unwrap_err();
    let UpdateCommandError::Delivery { report, .. } = failure else {
        panic!("lost remote evidence")
    };
    assert!(matches!(
        report.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert_eq!(std::fs::read(&output).unwrap(), b"racing owner");
    assert_eq!(wire.0.lock().unwrap().len(), 1);

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "offline writer"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let delivery = preflight(command(&[]), dir.path())
        .execute(
            context(),
            wire.clone(),
            OperationLimits::default(),
            &mut Policy(true),
            pending(),
        )
        .await
        .unwrap();
    let UpdateCommandError::Delivery { report, .. } = delivery.deliver(&mut Broken).unwrap_err()
    else {
        panic!("lost remote evidence")
    };
    assert!(matches!(
        report.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert_eq!(wire.0.lock().unwrap().len(), 2);
}

#[cfg(feature = "headless")]
#[test]
fn standalone_head_plans_locally_and_refuses_unapproved_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("metadata.json");
    std::fs::write(&input, br#"{"displayName":"new"}"#).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .args(["update", TARGET, "--mode", "patch", "--input"])
        .arg(&input)
        .arg("--plan")
        .env_remove("FAB_RUST_AUTH_MODE");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["meta"]["executionMode"], "plan");
    assert_eq!(receipt["meta"]["target"]["identity"]["itemId"], ITEM);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["update", TARGET, "--mode", "patch", "--input"])
        .arg(&input)
        .env_remove("FAB_RUST_AUTH_MODE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
        "ApprovalHostRequired"
    );
}

#[tokio::test]
async fn typed_scalar_identity_and_null_projection_preserve_json_and_protected_meta() {
    let dir = tempfile::tempdir().unwrap();
    for (expression, expected) in [
        ("length(displayName)", json!(3)),
        ("displayName == 'new'", json!(true)),
        ("missing", Value::Null),
    ] {
        let delivery = preflight(command(&["--project", expression]), dir.path())
            .execute(
                context(),
                Wire::default(),
                OperationLimits::default(),
                &mut Policy(true),
                pending(),
            )
            .await
            .unwrap();
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(receipt["data"], expected);
        assert_eq!(receipt["meta"]["target"]["identity"]["itemId"], ITEM);
        assert_eq!(
            receipt["meta"]["preconditions"][1]["kind"],
            "unguardedConcurrency"
        );
    }
    let delivery = preflight(command(&["--project", "."]), dir.path())
        .execute(
            context(),
            Wire::default(),
            OperationLimits::default(),
            &mut Policy(true),
            pending(),
        )
        .await
        .unwrap();
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let receipt: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(receipt["data"]["displayName"], "new");
    assert_eq!(receipt["meta"]["target"]["identity"]["itemId"], ITEM);
}
