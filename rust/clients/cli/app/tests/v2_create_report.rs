// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Report parser/planner/native adapter fixtures. Shared-host coverage is separate.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_cli_mutations::v2_create::{self, CreateCommand};
use fabric_sdk::{
    Audience, CredentialSelection, TokenCredential,
    items::create::{CreateTarget, LeafState, NativeId, Preflight, TargetScope},
    reports::create::{ReportCreateValidator, capabilities},
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOutcome};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const REPORT: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const PATH: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Report.Report";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-report-create",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Wire {
    replies: Mutex<VecDeque<OperationResponse>>,
    requests: Mutex<Vec<(ResourceMethod, String, Vec<u8>)>>,
    waits: Mutex<Vec<Duration>>,
}

impl Wire {
    fn new(replies: Vec<OperationResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            waits: Mutex::new(Vec::new()),
        }
    }
}

impl OperationExecutor for &Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push((
                request.method(),
                request.url().to_string(),
                request.body().to_vec(),
            ));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("no mutation replay"))
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(std::future::ready(Ok(())))
    }
}

fn command(wait: bool) -> CreateCommand {
    let mut args = vec!["create", PATH, "--input", "-"];
    if wait {
        args.push("--wait");
    }
    v2_create::parse_args(args).unwrap()
}

fn target() -> CreateTarget {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Report",
        "Report",
    )
    .unwrap()
}

fn validator() -> ReportCreateValidator {
    ReportCreateValidator::new(NonZeroUsize::new(64 * 1024).unwrap())
}

fn body() -> Value {
    let part = |path: &str, bytes: &[u8]| {
        json!({
            "path":path,"payloadType":"InlineBase64","payload":STANDARD.encode(bytes),
            "futurePart":{"retained":null}
        })
    };
    json!({"displayName":"Report","definition":{
        "format":"PBIR-Legacy","futureDefinition":true,"parts":[
            part("definition.pbir", br#"{"version":"4.0","datasetReference":{"byConnection":{"connectionString":"semanticmodelid=44444444-4444-4444-4444-444444444444"}}}"#),
            part("report.json", br#"{"futureReport":null}"#),
            part("StaticResources/Exact Image.bin", &[0,255,128,0])
        ]
    }})
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn accepted() -> OperationResponse {
    OperationResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OP.into()),
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
            ("Retry-After".into(), "2".into()),
        ]),
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline-owner", WS, audience).unwrap()
}

#[tokio::test]
async fn parsed_report_body_uses_real_native_sdk_route_without_model_requests() {
    let supplied = body();
    let bytes = serde_json::to_vec(&supplied).unwrap();
    let input = fabric_cli_output::input::strict_json(&bytes).unwrap();
    let command = command(false);
    let plan = command
        .plan(
            target(),
            Preflight::Leaf(LeafState::Missing),
            &capabilities(),
            Some(input),
            &validator(),
        )
        .unwrap();
    let metadata = json!({"id":REPORT,"workspaceId":WS,"displayName":"Report","type":"Report"});
    let wire = Wire::new(vec![response(201, metadata.clone())]);
    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        pending(),
    );
    assert_eq!(
        validator()
            .submit(&mut session, &plan, CompletionPolicy::ReturnAccepted)
            .await
            .unwrap(),
        ResourceOutcome::Completed {
            status: 201,
            data: Some(metadata)
        }
    );
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, ResourceMethod::Post);
    assert_eq!(
        requests[0].1,
        format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/reports")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].2).unwrap(),
        supplied
    );
    assert!(wire.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn accepted_handle_is_not_report_identity_and_poll_failure_keeps_uncertainty() {
    for wait in [false, true] {
        let command = command(wait);
        let plan = command
            .plan(
                target(),
                Preflight::Leaf(LeafState::Missing),
                &capabilities(),
                Some(body()),
                &validator(),
            )
            .unwrap();
        let wire = Wire::new(vec![
            accepted(),
            response(500, json!({"errorCode":"OfflineFailure"})),
        ]);
        let credential = Credential::default();
        let selection = selection(Audience::Fabric);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            pending(),
        );
        let result = validator()
            .submit(
                &mut session,
                &plan,
                if wait {
                    CompletionPolicy::WaitForResult
                } else {
                    CompletionPolicy::ReturnAccepted
                },
            )
            .await;
        if wait {
            let error = result.unwrap_err();
            assert_eq!(error.operation_id.as_deref(), Some(OP));
            assert!(error.mutation_may_have_occurred);
            assert_eq!(
                wire.waits.lock().unwrap().as_slice(),
                [Duration::from_secs(2)]
            );
        } else {
            assert_eq!(
                result.unwrap(),
                ResourceOutcome::Accepted {
                    operation_id: Some(OP.into())
                }
            );
            assert!(wire.waits.lock().unwrap().is_empty());
        }
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), if wait { 2 } else { 1 });
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.0 == ResourceMethod::Post)
                .count(),
            1
        );
        if wait {
            assert_eq!(requests[1].0, ResourceMethod::Get);
            assert_eq!(
                requests[1].1,
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}")
            );
        }
    }
}

#[tokio::test]
async fn wrong_audience_sends_nothing_and_provider_denial_does_not_replay() {
    let plan = command(false)
        .plan(
            target(),
            Preflight::Leaf(LeafState::Missing),
            &capabilities(),
            Some(body()),
            &validator(),
        )
        .unwrap();
    for audience in [Audience::OneLake, Audience::Fabric] {
        let wire = Wire::new(vec![response(
            403,
            json!({"errorCode":"InsufficientPrivileges"}),
        )]);
        let credential = Credential::default();
        let selection = selection(audience);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            pending(),
        );
        let error = validator()
            .submit(&mut session, &plan, CompletionPolicy::ReturnAccepted)
            .await
            .unwrap_err();
        let sent = usize::from(audience == Audience::Fabric);
        assert_eq!(wire.requests.lock().unwrap().len(), sent);
        assert_eq!(credential.0.load(Ordering::SeqCst), sent);
        assert_eq!(error.mutation_may_have_occurred, sent == 1);
    }
}

#[test]
fn strict_input_and_parser_do_not_hide_duplicates_or_turn_force_into_approval() {
    for bytes in [
        br#"{"displayName":"Report","displayName":"Other"}"#.as_slice(),
        br#"{"definition":{"parts":[],"parts":[]}}"#,
    ] {
        assert!(fabric_cli_output::input::strict_json(bytes).is_err());
    }
    let command = v2_create::parse_args(["create", PATH, "--input", "-", "--force"]).unwrap();
    assert!(
        command
            .plan(
                target(),
                Preflight::Leaf(LeafState::Missing),
                &capabilities(),
                Some(body()),
                &validator(),
            )
            .is_err()
    );
}
