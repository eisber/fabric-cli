// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact parser-to-native-SDK execution and delivery contracts.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::stage_deploy::{
    StageDeployCommand, StageDeployError, StageDeployHead, StageDeployInvocation,
    StageDeployOutput, parse_args,
};
use fabric_deploy::stage::{
    DeploymentApproval, DeploymentError, DeploymentOutcome, PreparedDeployment, SelectionIntent,
    StageReader,
};
use fabric_sdk::{Audience, CredentialSelection, RequestContext};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    future::{Future, pending, ready},
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const PIPELINE: &str = "11111111-1111-1111-1111-111111111111";
const SOURCE: &str = "22222222-2222-2222-2222-222222222222";
const TARGET: &str = "33333333-3333-3333-3333-333333333333";
const OPERATION: &str = "77777777-7777-7777-7777-777777777777";
const DEPLOYMENT: &str = "88888888-8888-8888-8888-888888888888";

fn args(selection: &str) -> Vec<OsString> {
    [
        "fab-rs",
        "deploy",
        &format!("/.deployment-pipelines/{PIPELINE}/stages/{TARGET}"),
        "--input",
        "missing-native.json",
        "--max-input-bytes",
        "4096",
        selection,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn command() -> StageDeployCommand {
    match parse_args(args("--selected-items")).unwrap() {
        StageDeployInvocation::Deploy(command) => command,
        _ => panic!("expected deployment"),
    }
}

fn body() -> Vec<u8> {
    serde_json::to_vec(&json!({"sourceStageId":SOURCE,"targetStageId":TARGET,
        "items":[{"sourceItemId":"66666666-6666-6666-6666-666666666666","itemType":"Notebook"}]}))
    .unwrap()
}

#[test]
fn parser_requires_explicit_native_path_selection_and_byte_budget() {
    let command = command();
    assert_eq!(command.max_input_bytes(), 4096);
    assert_eq!(command.input().to_str(), Some("missing-native.json"));
    assert_eq!(
        command.request(&body()).unwrap().intent(),
        SelectionIntent::Selected
    );
    assert!(!command.wait());
    for selection in ["--prune", "--config", "--beta"] {
        assert!(parse_args(args(selection)).is_err());
    }
    let mut both = args("--selected-items");
    both.push("--all-items".into());
    assert!(parse_args(both).is_err());
    let mut absent = args("--selected-items");
    absent.pop();
    assert!(parse_args(absent).is_err());
    for path in [
        "/Workspace.Workspace/src",
        "/.deployment-pipelines/name/stages/next",
        "/.deployment-pipelines/x/stages/y",
    ] {
        let mut input = args("--selected-items");
        input[2] = path.into();
        assert!(parse_args(input).is_err());
    }
}

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "offline-stage-head",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
struct Harness {
    replies: Mutex<VecDeque<OperationResponse>>,
    posts: Mutex<Vec<Value>>,
}
impl Harness {
    fn new() -> Self {
        let mut acceptance = OperationResponse {
            status: 202,
            body: vec![],
            headers: BTreeMap::new(),
        };
        acceptance
            .headers
            .insert("x-ms-operation-id".into(), OPERATION.into());
        acceptance
            .headers
            .insert("deployment-id".into(), DEPLOYMENT.into());
        Self { replies: Mutex::new(VecDeque::from([
            OperationResponse { status: 200, body: serde_json::to_vec(&json!({"id":SOURCE,"workspaceId":"44444444-4444-4444-4444-444444444444","order":0})).unwrap(), headers:BTreeMap::new() },
            OperationResponse { status: 200, body: serde_json::to_vec(&json!({"id":TARGET,"workspaceId":"55555555-5555-5555-5555-555555555555","order":1})).unwrap(), headers:BTreeMap::new() },
            acceptance,
        ])), posts: Mutex::new(Vec::new()) }
    }
}
impl OperationExecutor for &Harness {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if request.method() == ResourceMethod::Post {
                assert_eq!(
                    request.url().path(),
                    format!("/v1/deploymentPipelines/{PIPELINE}/deploy")
                );
                self.posts
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(request.body()).unwrap());
            }
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request"))
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(ready(Ok(())))
    }
}
struct Policy(usize);
impl DeploymentApproval for Policy {
    fn authorize(&mut self, prepared: &PreparedDeployment) -> Result<(), DeploymentError> {
        assert_eq!(prepared.request().pipeline_id(), PIPELINE);
        assert_eq!(
            prepared.request().body(),
            &serde_json::from_slice::<Value>(&body()).unwrap()
        );
        self.0 += 1;
        Ok(())
    }
}
#[derive(Default)]
struct Output {
    fail_preflight: bool,
    fail_write: bool,
    values: Vec<Value>,
}
impl StageDeployOutput for Output {
    fn preflight(&mut self) -> io::Result<()> {
        if self.fail_preflight {
            Err(io::Error::other("offline-preflight"))
        } else {
            Ok(())
        }
    }
    fn write(&mut self, value: &Value) -> io::Result<()> {
        self.values.push(value.clone());
        if self.fail_write {
            Err(io::Error::other("offline-delivery"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn actual_cli_sdk_consumer_preserves_acceptance_and_delivery_failure() {
    for delivery_failure in [false, true] {
        let harness = Harness::new();
        let executor = &harness;
        let context = RequestContext::new(
            Arc::new(Credential),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        );
        let reader = StageReader {
            context: &context,
            executor: &executor,
            limits: OperationLimits::default(),
        };
        let stages = (
            reader
                .read_stage(PIPELINE, SOURCE, pending())
                .await
                .unwrap(),
            reader
                .read_stage(PIPELINE, TARGET, pending())
                .await
                .unwrap(),
        );
        let mut policy = Policy(0);
        let mut output = Output {
            fail_write: delivery_failure,
            ..Default::default()
        };
        let mut head = StageDeployHead {
            context: &context,
            executor: &executor,
            limits: OperationLimits::default(),
            policy: &mut policy,
        };
        let result = head
            .run(&command(), &body(), stages, &mut output, pending())
            .await;
        let receipt = if delivery_failure {
            match result.err().unwrap() {
                StageDeployError::Delivery { execution, .. } => execution.unwrap(),
                _ => panic!("expected delivery evidence"),
            }
        } else {
            result.unwrap()
        };
        assert_eq!(receipt.outcome, DeploymentOutcome::Accepted);
        assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert_eq!(harness.posts.lock().unwrap().len(), 1);
        assert_eq!(output.values.len(), 1);
        assert_eq!(output.values[0]["status"], "accepted");
        assert_eq!(policy.0, 1);
    }
}

#[tokio::test]
async fn output_preflight_stops_approval_and_mutation() {
    let harness = Harness::new();
    let executor = &harness;
    let context = RequestContext::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let reader = StageReader {
        context: &context,
        executor: &executor,
        limits: OperationLimits::default(),
    };
    let stages = (
        reader
            .read_stage(PIPELINE, SOURCE, pending())
            .await
            .unwrap(),
        reader
            .read_stage(PIPELINE, TARGET, pending())
            .await
            .unwrap(),
    );
    let mut policy = Policy(0);
    let mut output = Output {
        fail_preflight: true,
        ..Default::default()
    };
    let mut head = StageDeployHead {
        context: &context,
        executor: &executor,
        limits: OperationLimits::default(),
        policy: &mut policy,
    };
    assert!(matches!(
        head.run(&command(), &body(), stages, &mut output, pending())
            .await,
        Err(StageDeployError::Preflight(_))
    ));
    assert!(harness.posts.lock().unwrap().is_empty());
    assert_eq!(policy.0, 0);
}
