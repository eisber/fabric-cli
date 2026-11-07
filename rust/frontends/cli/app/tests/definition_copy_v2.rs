// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root parser -> central session -> raw definition read -> create-only mutation.

#![cfg(all(feature = "native-host", feature = "v2-copy"))]
#![deny(unnameable_test_items)]

#[path = "family_support/shared.rs"]
mod family_support;

use azure_core::http::{AsyncRawResponse, HttpClient, Request};
use fabric_cli::{
    families::{
        FamilyEvidence, FamilyOutput, FamilySession,
        copy_v2::{DefinitionCopyFailure, DefinitionCopyResult},
    },
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_mutations::{CommandFuture, MutationError, MutationUi, NoticeKind};
use fabric_cli_output::Timestamp;
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use family_support::{Credential, Effects, Http, TransferUiCapture};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Poll, Waker},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const SOURCE: &str = "22222222-2222-2222-2222-222222222222";
const CREATED: &str = "33333333-3333-3333-3333-333333333333";
const OPERATION: &str = "44444444-4444-4444-4444-444444444444";

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay.is_zero() {
            Box::pin(async {})
        } else {
            Box::pin(pending())
        }
    }
}

#[derive(Debug, Default)]
struct CancelSignal {
    ready: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl CancelSignal {
    fn trigger(&self) {
        self.ready.store(true, Ordering::SeqCst);
        let waker = self.waker.lock().unwrap().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    async fn wait(self: Arc<Self>) {
        poll_fn(move |context| {
            let mut waker = self.waker.lock().unwrap();
            if self.ready.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                *waker = Some(context.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

#[derive(Debug)]
struct SignallingHttp {
    inner: Arc<Http>,
    cancel_after_response: Option<(usize, Arc<CancelSignal>)>,
}

#[async_trait::async_trait]
impl HttpClient for SignallingHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let response = self.inner.execute_request(request).await?;
        if let Some((count, signal)) = &self.cancel_after_response
            && self.inner.requests.lock().unwrap().len() == *count
        {
            signal.trigger();
        }
        Ok(response)
    }
}

struct Ui {
    confirmation: Result<bool, MutationError>,
    prompts: usize,
    warnings: Vec<String>,
    cancel_confirmation: Option<Arc<CancelSignal>>,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            confirmation: Ok(true),
            prompts: 0,
            warnings: Vec::new(),
            cancel_confirmation: None,
        }
    }
}

impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        if kind == NoticeKind::Warning {
            self.warnings.push(message.into());
        }
    }

    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        panic!("definition copy must use the fallible confirmation method")
    }

    fn confirm_result<'a>(
        &'a mut self,
        _: &'a str,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.prompts += 1;
        let result = self.confirmation.clone();
        let cancel = self.cancel_confirmation.clone();
        Box::pin(async move {
            if let Some(signal) = cancel {
                signal.trigger();
                pending::<()>().await;
            }
            result
        })
    }
}

struct Host {
    root: PathBuf,
    http: Arc<Http>,
    credential: Arc<Credential>,
    endpoint: EndpointBinding,
    ui: Ui,
    cancel_after_response: Option<(usize, Arc<CancelSignal>)>,
}

impl Host {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::current_dir().unwrap().join(format!(
            ".copy-v2-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let store = ConfigStore::new(&root);
        let mut settings = store.initialize().unwrap();
        settings.insert("output_format".into(), json!("json"));
        store.write(&settings).unwrap();
        Self {
            root,
            http: Arc::new(Http {
                replies: Mutex::default(),
                requests: Mutex::default(),
            }),
            credential: Arc::default(),
            endpoint: EndpointBinding::public(ServiceOrigin::Fabric),
            ui: Ui::default(),
            cancel_after_response: None,
        }
    }

    fn reply(&self, status: u16, body: Value) {
        self.headers(status, body, &[]);
    }

    fn headers(&self, status: u16, body: Value, headers: &[(&str, &str)]) {
        self.http.replies.lock().unwrap().push_back((
            status,
            body,
            headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
        ));
    }

    fn metadata(&self, item_type: &str, existing: bool) {
        self.reply(
            200,
            json!({"value":[{"id":WORKSPACE,"displayName":"Demo","type":"Workspace"}]}),
        );
        let mut items = vec![json!({
            "id":SOURCE,"displayName":"Book","type":item_type,
        })];
        if existing {
            items.push(json!({"id":CREATED,"displayName":"Copied","type":item_type}));
        }
        self.reply(200, json!({"value":items}));
    }

    fn definition(&self, item_type: &str) {
        self.reply(
            200,
            json!({"id":SOURCE,"type":item_type,"displayName":"Book"}),
        );
        self.reply(200, definition());
    }

    async fn execute(&mut self, args: &[&str]) -> FamilyOutput {
        self.execute_cancelled(args, pending()).await
    }

    async fn execute_cancelled(
        &mut self,
        args: &[&str],
        cancelled: impl Future<Output = ()> + Send,
    ) -> FamilyOutput {
        let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
        let sdk = Client::with_context(RequestContext::with_endpoint(
            self.credential.clone(),
            CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
            self.endpoint.clone(),
        ));
        let http = HttpExecutor::new(
            Arc::new(SignallingHttp {
                inner: self.http.clone(),
                cancel_after_response: self.cancel_after_response.clone(),
            }),
            Arc::new(Timer),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(self.endpoint.clone());
        let tenant = Element::tenant("Tenant", "tenant");
        let reads = ReadSession {
            sdk: &sdk,
            executor: &http,
            tenant: tenant.clone(),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2026-11-05T12:00:00Z").unwrap(),
            config: Default::default(),
        };
        let mut navigation = NavigationState::new(
            PathElement::from_fabric(tenant),
            HomeContext::Unspecified,
            self.root.to_string_lossy().into_owned(),
            37,
            true,
        )
        .unwrap();
        let store = ConfigStore::new(&self.root);
        let mut effects = Effects::default();
        let transfer_ui = TransferUiCapture::default();
        let result = FamilySession {
            reads: &reads,
            http: &http,
            store: &store,
            navigation: &mut navigation,
            local: &mut effects,
            mutations: &mut self.ui,
            confirmation: None,
            transfer_ui: &transfer_ui,
            transfer_files: None,
            resource_files: None,
        }
        .dispatch(&invocation, cancelled)
        .await
        .unwrap();
        assert!(self.http.replies.lock().unwrap().is_empty());
        assert_eq!(effects.cache_clears, 0);
        result
    }

    fn requests(&self) -> Vec<Value> {
        self.http.requests.lock().unwrap().clone()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn definition() -> Value {
    json!({"definition":{"parts":[
        {"path":".platform","payload":"eyJtZXRhZGF0YSI6eyJkaXNwbGF5TmFtZSI6IkJvb2sifSwiY29uZmlnIjp7ImxvZ2ljYWxJZCI6IjIyMjIyMjIyLTIyMjItMjIyMi0yMjIyLTIyMjIyMjIyMjIyMiJ9fQ==","payloadType":"InlineBase64"},
        {"path":"notebook-content.ipynb","payload":"eyJvdXRwdXRzIjpbImtlZXAiXX0=","payloadType":"InlineBase64"},
    ]}})
}

fn stdout(output: &FamilyOutput) -> String {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 0);
    String::from_utf8(stdout).unwrap()
}

#[tokio::test]
async fn notebook_and_model_copy_omit_source_platform_preserving_other_parts_and_created_id() {
    for (item_type, alias) in [("Notebook", "cp"), ("SemanticModel", "copy")] {
        let mut host = Host::new();
        host.metadata(item_type, false);
        host.definition(item_type);
        host.reply(201, json!({"id":CREATED}));
        let output = host
            .execute(&[
                alias,
                &format!("Demo.Workspace/Book.{item_type}"),
                &format!("Demo.Workspace/Copied.{item_type}"),
            ])
            .await;
        let text = stdout(&output);
        assert!(text.contains("Definition created"));
        let rendered: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rendered["result"]["data"][0]["platformOmitted"], true);
        assert!(matches!(
            &output.evidence,
            FamilyEvidence::DefinitionCopy(Ok(DefinitionCopyResult::Submitted(receipt)))
                if receipt.item_id.as_deref() == Some(CREATED)
                    && receipt.response.status == 201
                    && receipt.platform_omitted
        ));
        assert_eq!(host.ui.prompts, 1);
        assert_eq!(host.ui.warnings.len(), 1);
        assert!(host.ui.warnings[0].contains("sensitivity labels"));
        let requests = host.requests();
        assert_eq!(requests.len(), 5);
        assert!(
            requests[3]["url"]
                .as_str()
                .unwrap()
                .ends_with("/getDefinition")
        );
        assert_eq!(requests[4]["method"], "POST");
        assert_eq!(
            requests[4]["body"]["definition"]["parts"],
            json!([definition()["definition"]["parts"][1]])
        );
        assert!(
            requests[4]["body"]["definition"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|part| part["path"] != ".platform")
        );
        assert_eq!(requests[4]["body"]["displayName"], "Copied");
        assert_eq!(requests[4]["body"]["type"], item_type);
        assert!(requests[4]["url"].as_str().unwrap().ends_with("/items"));
    }
}

#[tokio::test]
async fn force_bypasses_only_confirmation_not_existing_destination_validation() {
    let mut host = Host::new();
    host.metadata("Notebook", true);
    let output = host
        .execute(&[
            "cp",
            "Demo.Workspace/Book.Notebook",
            "Demo.Workspace/Copied.Notebook",
            "-f",
        ])
        .await;
    assert_eq!(output.plan.exit_code, 1);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Validation(error)))
            if error.code == "AlreadyExists"
    ));
    assert_eq!(host.requests().len(), 2);
    assert_eq!(host.ui.prompts, 0);
}

#[tokio::test]
async fn decline_and_prompt_error_never_read_definition_or_create() {
    for confirmation in [
        Ok(false),
        Err(MutationError::new(
            "PromptFailed",
            "Injected prompt failure",
        )),
    ] {
        let mut host = Host::new();
        host.metadata("Notebook", false);
        host.ui.confirmation = confirmation;
        let output = host
            .execute(&[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
            ])
            .await;
        assert_eq!(host.requests().len(), 2);
        match host.ui.confirmation {
            Ok(false) => assert!(matches!(
                output.evidence,
                FamilyEvidence::DefinitionCopy(Ok(DefinitionCopyResult::Declined))
            )),
            _ => assert!(matches!(
                output.evidence,
                FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Confirmation(_)))
            )),
        }
    }
}

#[tokio::test]
async fn unsupported_types_destinations_and_options_stop_before_definition_reads() {
    for (item_type, destination, options) in [
        ("Report", "Demo.Workspace/Copied.Report", vec!["-f"]),
        ("Notebook", "Demo.Workspace", vec!["-f"]),
        (
            "Notebook",
            "Demo.Workspace/Copied.SemanticModel",
            vec!["-f"],
        ),
        (
            "Notebook",
            "Demo.Workspace/Copied.Notebook",
            vec!["-r", "-f"],
        ),
        (
            "Notebook",
            "Demo.Workspace/Copied.Notebook",
            vec!["--block_on_path_collision", "-f"],
        ),
        (
            "Notebook",
            "Demo.Workspace/Copied.Notebook",
            vec!["--skill", "copy-test", "-f"],
        ),
    ] {
        let mut host = Host::new();
        host.metadata(item_type, false);
        let source = format!("Demo.Workspace/Book.{item_type}");
        let mut args = vec!["cp", &source, destination];
        args.extend(options);
        let output = host.execute(&args).await;
        assert_eq!(output.plan.exit_code, 1);
        assert!(matches!(
            output.evidence,
            FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Validation(_)))
        ));
        assert_eq!(host.requests().len(), 2);
    }
}

#[tokio::test]
async fn denied_and_malformed_source_definitions_cannot_create() {
    for (status, body) in [
        (
            403,
            json!({"error":{"code":"AccessDenied","message":"denied"}}),
        ),
        (200, json!({"definition":{"parts":"not-an-array"}})),
    ] {
        let mut host = Host::new();
        host.metadata("Notebook", false);
        host.reply(
            200,
            json!({"id":SOURCE,"displayName":"Book","type":"Notebook"}),
        );
        host.reply(status, body);
        let output = host
            .execute(&[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
                "-f",
            ])
            .await;
        assert_eq!(output.plan.exit_code, 1);
        assert!(matches!(
            output.evidence,
            FamilyEvidence::DefinitionCopy(Err(_))
        ));
        assert_eq!(host.requests().len(), 4);
    }
}

#[tokio::test]
async fn ready_cancellation_has_no_http_or_confirmation_side_effects() {
    let mut host = Host::new();
    let output = host
        .execute_cancelled(
            &[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
            ],
            async {},
        )
        .await;
    assert_eq!(output.plan.exit_code, 1);
    assert!(host.requests().is_empty());
    assert_eq!(host.ui.prompts, 0);
}

#[tokio::test]
async fn cancellation_during_confirmation_never_retrieves_source_or_creates_destination() {
    let mut host = Host::new();
    host.metadata("Notebook", false);
    let signal = Arc::new(CancelSignal::default());
    host.ui.cancel_confirmation = Some(signal.clone());
    let output = host
        .execute_cancelled(
            &[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
            ],
            signal.wait(),
        )
        .await;
    assert_eq!(output.plan.exit_code, 1);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Interrupted(error)))
            if matches!(error.kind, fabric_sdk_operations::OperationErrorKind::Cancelled)
                && !error.mutation_may_have_occurred
    ));
    assert_eq!(host.ui.prompts, 1);
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn cancellation_at_source_response_boundary_retains_read_error_without_destination_create() {
    let mut host = Host::new();
    host.metadata("Notebook", false);
    host.definition("Notebook");
    let signal = Arc::new(CancelSignal::default());
    host.cancel_after_response = Some((4, signal.clone()));
    let output = host
        .execute_cancelled(
            &[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
                "-f",
            ],
            signal.wait(),
        )
        .await;
    assert_eq!(output.plan.exit_code, 1);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Read(error)))
            if matches!(
                &error.kind,
                fabric_deploy::definitions::DefinitionErrorKind::Operation(operation)
                    if matches!(
                        operation.kind,
                        fabric_sdk_operations::OperationErrorKind::Cancelled
                    )
            )
    ));
    let requests = host.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[3]["url"]
            .as_str()
            .unwrap()
            .ends_with("/getDefinition")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1,
        "only the source getDefinition POST may have been dispatched"
    );
}

#[tokio::test]
async fn source_get_definition_lro_is_observed_without_replaying_source_post() {
    let mut host = Host::new();
    host.metadata("Notebook", false);
    host.reply(
        200,
        json!({"id":SOURCE,"type":"Notebook","displayName":"Book"}),
    );
    let operation_url = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result_url = format!("{operation_url}/result");
    host.headers(
        202,
        json!({}),
        &[
            ("x-ms-operation-id", OPERATION),
            ("Location", &operation_url),
            ("Retry-After", "0"),
        ],
    );
    host.headers(
        200,
        json!({"status":"Succeeded"}),
        &[("Location", &result_url)],
    );
    host.reply(200, definition());
    host.reply(201, json!({"id":CREATED}));
    let output = host
        .execute(&[
            "cp",
            "Demo.Workspace/Book.Notebook",
            "Demo.Workspace/Copied.Notebook",
            "-f",
        ])
        .await;
    assert_eq!(output.plan.exit_code, 0);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::DefinitionCopy(Ok(DefinitionCopyResult::Submitted(receipt)))
            if receipt.response.status == 201
                && receipt.item_id.as_deref() == Some(CREATED)
                && receipt.operation_id.is_none()
                && receipt.platform_omitted
    ));
    let requests = host.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(requests[3]["method"], "POST");
    assert!(
        requests[3]["url"]
            .as_str()
            .unwrap()
            .ends_with("/getDefinition")
    );
    assert_eq!(requests[4]["method"], "GET");
    assert_eq!(requests[4]["url"], operation_url);
    assert_eq!(requests[5]["method"], "GET");
    assert_eq!(requests[5]["url"], result_url);
    assert_eq!(requests[6]["method"], "POST");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["url"].as_str().unwrap().ends_with("/getDefinition"))
            .count(),
        1
    );
    assert_eq!(
        requests[6]["body"]["definition"]["parts"],
        json!([definition()["definition"]["parts"][1]])
    );
}

#[tokio::test]
async fn accepted_create_is_not_completed_and_failed_delivery_keeps_original_receipt() {
    let mut host = Host::new();
    host.metadata("Notebook", false);
    host.definition("Notebook");
    host.headers(202, json!({}), &[("x-ms-operation-id", OPERATION)]);
    let output = host
        .execute(&[
            "cp",
            "Demo.Workspace/Book.Notebook",
            "Demo.Workspace/Copied.Notebook",
            "-f",
        ])
        .await;
    let text = stdout(&output);
    assert!(text.contains("accepted"));
    assert!(!text.contains("Definition created"));
    let rendered: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(rendered["result"]["data"][0]["platformOmitted"], true);
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::DefinitionCopy(Ok(DefinitionCopyResult::Submitted(receipt)))
            if receipt.response.status == 202
                && receipt.operation_id.as_deref() == Some(OPERATION)
                && receipt.item_id.is_none()
    ));
    assert_eq!(host.ui.prompts, 0);
    assert_eq!(host.ui.warnings.len(), 1);
    assert_eq!(host.requests().len(), 5);
}

#[tokio::test]
async fn malformed_created_or_bare_accepted_response_retains_uncertain_create_evidence() {
    for (status, body) in [(201, json!({"id":"not-a-guid"})), (202, json!({}))] {
        let mut host = Host::new();
        host.metadata("Notebook", false);
        host.definition("Notebook");
        host.reply(status, body);
        let output = host
            .execute(&[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
                "-f",
            ])
            .await;
        assert_eq!(output.plan.exit_code, 1);
        assert!(matches!(
            &output.evidence,
            FamilyEvidence::DefinitionCopy(Err(DefinitionCopyFailure::Create(error)))
                if error.response.as_ref().is_some_and(|response| response.status == status)
                    && error.operation.as_ref().is_some_and(|operation| operation.mutation_may_have_occurred)
        ));
        assert_eq!(host.requests().len(), 5);
    }
}

#[tokio::test]
async fn cross_workspace_copy_uses_resolved_destination_without_rediscovery() {
    const DESTINATION: &str = "66666666-6666-6666-6666-666666666666";
    let mut host = Host::new();
    host.reply(
        200,
        json!({"value":[
            {"id":WORKSPACE,"displayName":"Demo","type":"Workspace"},
            {"id":DESTINATION,"displayName":"Other","type":"Workspace"},
        ]}),
    );
    host.reply(
        200,
        json!({"value":[{"id":SOURCE,"displayName":"Book","type":"Notebook"}]}),
    );
    host.reply(200, json!({"value":[]}));
    host.definition("Notebook");
    host.reply(
        201,
        json!({"id":CREATED,"workspaceId":DESTINATION,"type":"Notebook"}),
    );
    let output = host
        .execute(&[
            "cp",
            "Demo.Workspace/Book.Notebook",
            "Other.Workspace/Copied.Notebook",
            "-f",
        ])
        .await;
    assert_eq!(output.plan.exit_code, 0);
    let requests = host.requests();
    assert_eq!(requests.len(), 6);
    assert!(requests[3]["url"].as_str().unwrap().contains(WORKSPACE));
    assert!(
        requests[5]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/workspaces/{DESTINATION}/items"))
    );
}

#[tokio::test]
async fn selected_endpoint_is_used_for_source_and_destination_sessions() {
    for hostname in [
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        let mut host = Host::new();
        host.endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, hostname).unwrap();
        host.metadata("Notebook", false);
        host.definition("Notebook");
        host.reply(201, json!({"id":CREATED}));
        let output = host
            .execute(&[
                "cp",
                "Demo.Workspace/Book.Notebook",
                "Demo.Workspace/Copied.Notebook",
                "-f",
            ])
            .await;
        assert_eq!(output.plan.exit_code, 0);
        let prefix = format!("https://{hostname}/v1/");
        assert!(
            host.requests()
                .iter()
                .all(|request| { request["url"].as_str().unwrap().starts_with(&prefix) })
        );
        assert!(
            host.credential
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|scopes| { scopes == &[Audience::Fabric.scope()] })
        );
    }
}
