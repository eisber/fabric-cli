// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real argv-to-registry-to-injected-SDK-to-stream contracts, entirely offline.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_ontology::{
    AdapterError, AdapterHost, AdapterLimits, HostError, InputSource, OntologyVerb,
    OutputDestination, bind_public_operations, execute, parse_common_args, parse_facade_args,
};
use fabric_sdk::{Audience, CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_items::{ItemClient, ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse};
use fabric_sdk_ontology::{
    DefinitionLimits,
    v2::{
        ApprovalPolicy, Effect, ItemIdentity, OperationDescriptor, Outcome, PreparedOperation,
        Registry, ResolvedTarget, Target, TargetKind, Verb,
    },
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    io::{self, Write},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OPERATION: &str = "99999999-8888-7777-6666-555555555555";
const PATH: &str = "/w.Workspace/o.Ontology";

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
        Ok(AccessToken {
            token: "offline-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
type Seen = (String, String, Vec<u8>);
impl Executor {
    fn new(responses: Vec<OperationResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            ..Self::default()
        }
    }
    fn take(&self, method: String, url: String, body: Vec<u8>) -> OperationResponse {
        self.seen.lock().unwrap().push((method, url, body));
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("expected SDK dispatch")
    }
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            Ok(self.take(
                format!("{:?}", request.method()),
                request.url().to_string(),
                request.body().to_vec(),
            ))
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(ready(Ok(())))
    }
}
impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.take(request.method().into(), request.url().to_string(), vec![]);
            Ok(ItemResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            })
        })
    }
}

struct Writer {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail: bool,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail {
            self.bytes
                .lock()
                .unwrap()
                .extend_from_slice(&bytes[..bytes.len().min(7)]);
            return Err(io::Error::other("fixture writer failure"));
        }
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Host {
    target: ResolvedTarget,
    input: Vec<u8>,
    output: Arc<Mutex<Vec<u8>>>,
    stderr: Vec<u8>,
    approved: Vec<PreparedOperation>,
    events: Vec<&'static str>,
    refuse: bool,
    exists: bool,
    fail_writer: bool,
}
impl Host {
    fn new(facet: &str, representation: &str) -> Self {
        Self {
            target: ResolvedTarget::new(
                PATH,
                ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
                Target {
                    kind: TargetKind::Item,
                    owner_type: "Ontology".into(),
                    facet: facet.into(),
                    representation: representation.into(),
                    generation: Some(2),
                },
                None,
            )
            .unwrap(),
            input: vec![],
            output: Arc::new(Mutex::new(vec![])),
            stderr: vec![],
            approved: vec![],
            events: vec![],
            refuse: false,
            exists: false,
            fail_writer: false,
        }
    }
}
impl ApprovalPolicy for Host {
    type Error = HostError;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), HostError> {
        assert!(self.events.contains(&"reserve"));
        self.events.push("approve");
        if self.refuse {
            return Err(HostError("approval refused".into()));
        }
        self.approved.push(operation.clone());
        Ok(())
    }
}
impl AdapterHost for Host {
    fn resolve(&mut self, _: &str) -> Result<ResolvedTarget, HostError> {
        self.events.push("resolve");
        Ok(self.target.clone())
    }
    fn read_input(&mut self, _: &InputSource, maximum: NonZeroUsize) -> Result<Vec<u8>, HostError> {
        self.events.push("input");
        if self.input.len() > maximum.get() {
            return Err(HostError("input exceeds bound".into()));
        }
        Ok(self.input.clone())
    }
    fn reserve_output(&mut self, _: &OutputDestination) -> Result<Box<dyn Write>, HostError> {
        self.events.push("reserve");
        if self.exists {
            return Err(HostError("OutputExists".into()));
        }
        Ok(Box::new(Writer {
            bytes: self.output.clone(),
            fail: self.fail_writer,
        }))
    }
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn deadline(&mut self, _: Option<Duration>) -> Result<(), HostError> {
        self.events.push("deadline");
        Ok(())
    }
}

fn registry() -> Registry {
    let mut registry = Registry::default();
    for (id, verb, effects) in [
        ("item.metadata.get", Verb::Get, vec![Effect::RemoteRead]),
        ("item.definition.get", Verb::Get, vec![Effect::RemoteRead]),
        (
            "item.definition.replace",
            Verb::Update,
            vec![Effect::RemoteRead, Effect::RemoteWrite],
        ),
        (
            "ontology.model.get",
            Verb::Get,
            vec![Effect::RemoteRead, Effect::MeteredConsumption],
        ),
        (
            "ontology.model.replace",
            Verb::Update,
            vec![
                Effect::RemoteRead,
                Effect::RemoteWrite,
                Effect::MeteredConsumption,
            ],
        ),
    ] {
        registry
            .register(
                OperationDescriptor::new(
                    id,
                    verb,
                    json!({"type":"object"}),
                    json!({"type":"object"}),
                    effects,
                )
                .unwrap()
                .with_optional_effects(vec![Effect::LocalWrite])
                .unwrap(),
            )
            .unwrap();
    }
    bind_public_operations(&mut registry).unwrap();
    registry
}
fn limits() -> AdapterLimits {
    AdapterLimits {
        input_bytes: NonZeroUsize::new(4096).unwrap(),
        data_bytes: NonZeroUsize::new(4096).unwrap(),
        definition: DefinitionLimits {
            max_parts: NonZeroUsize::new(8).unwrap(),
            max_decoded_bytes: NonZeroUsize::new(4096).unwrap(),
        },
        operation: OperationLimits {
            max_requests: NonZeroUsize::new(10).unwrap(),
            max_response_bytes: NonZeroUsize::new(8192).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        },
    }
}
fn client(credential: Arc<Credential>) -> ItemClient {
    ItemClient::with_context(RequestContext::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    ))
}
fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn accepted() -> OperationResponse {
    let mut response = response(202, Value::Null);
    response.headers.insert(
        "Location".into(),
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    response
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    response
}
fn metadata() -> Value {
    json!({"id":ITEM,"displayName":"o","type":"Ontology","properties":{"generation":2}})
}
fn definition() -> Value {
    json!({"definition":{"parts":[{"path":"database.tmdl","payload":"YQ==","payloadType":"InlineBase64"}]}})
}

#[tokio::test]
async fn actual_generic_and_facade_metadata_share_descriptor_sdk_call_receipt_and_streams() {
    let registry = registry();
    let mut receipts = vec![];
    for facade in [false, true] {
        let args = if facade {
            parse_facade_args(["ontology", "get", PATH]).unwrap()
        } else {
            parse_common_args(OntologyVerb::Get, ["get", PATH]).unwrap()
        };
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone());
        let executor = Executor::new(vec![response(200, metadata())]);
        let mut host = Host::new("metadata", "json");
        let report = execute(
            &args,
            facade,
            &registry,
            &mut host,
            &client,
            executor.clone(),
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(report.command_succeeded());
        assert_eq!(host.approved.len(), 1);
        assert_eq!(host.approved[0].operation_id(), "item.metadata.get");
        assert_eq!(host.approved[0].input()["context"]["account"], "offline");
        assert_eq!(host.approved[0].input()["context"]["tenant"], "tenant");
        assert_eq!(
            host.approved[0].input()["context"]["audience"],
            Audience::Fabric.scope()
        );
        assert_eq!(host.approved[0].input()["bounds"]["maxRequests"], 10);
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        let seen = executor.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "GET");
        assert!(
            seen[0]
                .1
                .ends_with(&format!("/workspaces/{WORKSPACE}/ontologies/{ITEM}"))
        );
        assert!(host.stderr.is_empty());
        let printed: Value = serde_json::from_slice(&host.output.lock().unwrap()).unwrap();
        assert_eq!(printed["data"], metadata());
        receipts.push(printed);
    }
    assert_eq!(receipts[0], receipts[1]);
}

#[tokio::test]
async fn accepted_definition_output_failure_retains_original_handle_and_never_replays() {
    let args = parse_facade_args(["ontology", "get", PATH, "--output", "receipt.json"]).unwrap();
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::new(vec![accepted()]);
    let mut host = Host::new("definition", "native");
    host.fail_writer = true;
    let error = execute(
        &args,
        true,
        &registry(),
        &mut host,
        &client,
        executor.clone(),
        limits(),
        pending(),
    )
    .await
    .unwrap_err();
    let AdapterError::Output { report, .. } = error else {
        panic!("expected output evidence")
    };
    assert!(matches!(report.receipt.outcome(), Outcome::Accepted));
    assert_eq!(report.receipt.evidence().handles.len(), 1);
    let receipt = serde_json::to_value(&report.receipt).unwrap();
    assert_eq!(receipt["meta"]["evidence"]["handles"][0]["id"], OPERATION);
    assert!(host.approved[0].effects().contains(&Effect::LocalWrite));
    assert_eq!(host.approved[0].input()["output"]["path"], "receipt.json");
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_input_target_generation_output_collision_and_denied_approval_precede_credentials()
{
    for case in 0..7 {
        let args = parse_facade_args(["ontology", "get", PATH]).unwrap();
        let mut host = Host::new("metadata", "json");
        if case == 0 {
            host.target = ResolvedTarget::new(
                PATH,
                ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
                Target {
                    kind: TargetKind::Item,
                    owner_type: "SemanticModel".into(),
                    facet: "metadata".into(),
                    representation: "json".into(),
                    generation: Some(2),
                },
                None,
            )
            .unwrap();
        }
        if case == 1 {
            let mut target = host.target.selection().clone();
            target.generation = Some(99);
            host.target = ResolvedTarget::new(
                PATH,
                ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
                target,
                None,
            )
            .unwrap();
        }
        if case == 2 {
            host = Host::new("docs", "json");
        }
        if case == 3 {
            host.exists = true;
        }
        if case == 4 {
            host.refuse = true;
        }
        let mut args = args;
        if case == 5 {
            args.project = Some("broken[".into());
        }
        if case == 6 {
            args.permanent = true;
        }
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone());
        let executor = Executor::default();
        assert!(
            execute(
                &args,
                true,
                &registry(),
                &mut host,
                &client,
                executor.clone(),
                limits(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(executor.seen.lock().unwrap().is_empty());
        assert!(host.output.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn definition_reads_preserve_native_envelope_and_model_forms_are_not_interchanged() {
    for (facet, repr, body, suffix) in [
        ("definition", "native", definition(), "/getDefinition"),
        (
            "model",
            "minimized",
            json!({"name":"o","model":{}}),
            "/model?form=Minimized",
        ),
        (
            "model",
            "expanded",
            json!({"name":"o","model":{}}),
            "/model?form=Expanded",
        ),
    ] {
        let args = parse_facade_args(["ontology", "get", PATH]).unwrap();
        let executor = Executor::new(vec![response(200, body.clone())]);
        let mut host = Host::new(facet, repr);
        let report = execute(
            &args,
            true,
            &registry(),
            &mut host,
            &client(Arc::new(Credential::default())),
            executor.clone(),
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(report.command_succeeded());
        let output: Value = serde_json::from_slice(&host.output.lock().unwrap()).unwrap();
        assert_eq!(output["data"], body);
        assert!(executor.seen.lock().unwrap()[0].1.ends_with(suffix));
    }
}

#[tokio::test]
async fn replacement_body_and_output_are_approved_before_exactly_one_sdk_mutation() {
    for (facet, repr, body, response_body) in [
        ("definition", "native", definition(), Value::Null),
        (
            "model",
            "minimized",
            json!({"model":{},"description":"changed"}),
            json!({"name":"o","model":{}}),
        ),
    ] {
        let args = parse_facade_args([
            "ontology",
            "update",
            PATH,
            "--input",
            "-",
            "--mode",
            "replace",
            "--output",
            "receipt.json",
        ])
        .unwrap();
        let mut host = Host::new(facet, repr);
        host.input = serde_json::to_vec(&body).unwrap();
        let executor = Executor::new(vec![if facet == "definition" {
            OperationResponse {
                status: 200,
                body: vec![],
                headers: BTreeMap::new(),
            }
        } else {
            response(200, response_body)
        }]);
        let report = execute(
            &args,
            true,
            &registry(),
            &mut host,
            &client(Arc::new(Credential::default())),
            executor.clone(),
            limits(),
            pending(),
        )
        .await
        .unwrap();
        assert!(report.command_succeeded());
        assert_eq!(host.approved[0].input()["body"], body);
        assert!(host.approved[0].effects().contains(&Effect::RemoteWrite));
        assert!(host.approved[0].effects().contains(&Effect::LocalWrite));
        let seen = executor.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "Post");
        assert_eq!(serde_json::from_slice::<Value>(&seen[0].2).unwrap(), body);
        assert_eq!(
            host.events,
            ["resolve", "input", "deadline", "reserve", "approve"]
        );
    }
}

#[tokio::test]
async fn malformed_unknown_and_duplicate_replacement_fields_never_reach_approval_or_sdk() {
    for input in [
        br#"{"model":{},"unexpected":1}"#.as_slice(),
        br#"{"model":{},"model":{}}"#.as_slice(),
        br#"{"model":{"unknownCollection":[]}}"#.as_slice(),
        b"\xff",
    ] {
        let args = parse_facade_args([
            "ontology", "update", PATH, "--input", "-", "--mode", "replace",
        ])
        .unwrap();
        let mut host = Host::new("model", "minimized");
        host.input = input.to_vec();
        let credential = Arc::new(Credential::default());
        let executor = Executor::default();
        assert!(
            execute(
                &args,
                true,
                &registry(),
                &mut host,
                &client(credential.clone()),
                executor.clone(),
                limits(),
                pending()
            )
            .await
            .is_err()
        );
        assert!(host.approved.is_empty());
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(executor.seen.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn provider_denial_and_budget_failure_are_stderr_only_with_original_evidence() {
    for budget in [false, true] {
        let args = parse_facade_args(["ontology", "get", PATH]).unwrap();
        let mut host = Host::new("definition", "native");
        let executor = Executor::new(vec![if budget {
            response(200, definition())
        } else {
            response(403, json!({"error":{"code":"Forbidden"}}))
        }]);
        let mut limits = limits();
        if budget {
            limits.data_bytes = NonZeroUsize::new(1).unwrap();
        }
        let report = execute(
            &args,
            true,
            &registry(),
            &mut host,
            &client(Arc::new(Credential::default())),
            executor.clone(),
            limits,
            pending(),
        )
        .await
        .unwrap();
        assert!(!report.command_succeeded());
        assert!(host.output.lock().unwrap().is_empty());
        let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
        assert_eq!(receipt["status"], "failed");
        if budget {
            assert_eq!(receipt["error"]["code"], "BudgetExceeded");
        } else {
            assert!(report.sdk_error.is_some());
            assert_eq!(report.receipt.evidence().http_status, Some(403));
        }
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
    }
}

#[test]
fn owner_binding_is_transactional_and_does_not_duplicate_caller_descriptors() {
    let mut empty = Registry::default();
    assert!(bind_public_operations(&mut empty).is_err());
    assert!(empty.bindings().is_empty());
    assert_eq!(empty.operations().count(), 0);
    let mut registry = registry();
    let before = serde_json::to_value(registry.bindings()).unwrap();
    assert!(bind_public_operations(&mut registry).is_err());
    assert_eq!(serde_json::to_value(registry.bindings()).unwrap(), before);
    assert_eq!(registry.operations().count(), 5);
}

#[tokio::test]
async fn accepted_observation_budget_exhaustion_is_unknown_and_keeps_original_operation() {
    let args = parse_facade_args(["ontology", "get", PATH, "--wait"]).unwrap();
    let mut host = Host::new("definition", "native");
    let executor = Executor::new(vec![accepted()]);
    let mut limits = limits();
    limits.operation.max_requests = NonZeroUsize::new(1).unwrap();
    let report = execute(
        &args,
        true,
        &registry(),
        &mut host,
        &client(Arc::new(Credential::default())),
        executor.clone(),
        limits,
        pending(),
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(!report.command_succeeded());
    assert!(report.sdk_error.is_some());
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
    assert_eq!(receipt["meta"]["evidence"]["handles"][0]["id"], OPERATION);
    assert!(host.output.lock().unwrap().is_empty());
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn resultless_definition_wait_retains_handle_without_fetching_result_or_replaying_post() {
    let args = parse_facade_args([
        "ontology", "update", PATH, "--input", "-", "--mode", "replace", "--wait",
    ])
    .unwrap();
    let mut host = Host::new("definition", "native");
    host.input = serde_json::to_vec(&definition()).unwrap();
    let executor = Executor::new(vec![
        accepted(),
        response(200, json!({"status":"Succeeded"})),
    ]);
    let report = execute(
        &args,
        true,
        &registry(),
        &mut host,
        &client(Arc::new(Credential::default())),
        executor.clone(),
        limits(),
        pending(),
    )
    .await
    .unwrap();
    assert!(report.command_succeeded());
    let receipt: Value = serde_json::from_slice(&host.output.lock().unwrap()).unwrap();
    assert_eq!(receipt["status"], "succeeded");
    assert_eq!(receipt["meta"]["evidence"]["handles"][0]["id"], OPERATION);
    let seen = executor.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "Post");
    assert_eq!(seen[1].0, "Get");
    assert!(seen[1].1.ends_with(&format!("/operations/{OPERATION}")));
    assert!(seen.iter().all(|(_, url, _)| !url.ends_with("/result")));
}

#[tokio::test]
async fn serialized_source_and_decoded_native_bounds_are_measured_exactly_without_clipping() {
    let source = definition();
    let bytes = serde_json::to_vec(&source).unwrap().len();
    for (maximum, succeeded) in [(bytes, true), (bytes - 1, false)] {
        let args = parse_facade_args(["ontology", "get", PATH]).unwrap();
        let mut host = Host::new("definition", "native");
        let executor = Executor::new(vec![response(200, source.clone())]);
        let mut limits = limits();
        limits.data_bytes = NonZeroUsize::new(maximum).unwrap();
        limits.definition.max_decoded_bytes = NonZeroUsize::new(1).unwrap();
        let report = execute(
            &args,
            true,
            &registry(),
            &mut host,
            &client(Arc::new(Credential::default())),
            executor.clone(),
            limits,
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(report.command_succeeded(), succeeded);
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
        if succeeded {
            let receipt: Value = serde_json::from_slice(&host.output.lock().unwrap()).unwrap();
            assert_eq!(receipt["data"], source);
        } else {
            assert!(host.output.lock().unwrap().is_empty());
            let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
            assert_eq!(receipt["error"]["code"], "BudgetExceeded");
        }
    }
    let mut source = source;
    source["definition"]["parts"][0]["payload"] = json!("YWI=");
    let args = parse_facade_args(["ontology", "get", PATH]).unwrap();
    let mut host = Host::new("definition", "native");
    let mut limits = limits();
    limits.definition.max_decoded_bytes = NonZeroUsize::new(1).unwrap();
    let report = execute(
        &args,
        true,
        &registry(),
        &mut host,
        &client(Arc::new(Credential::default())),
        Executor::new(vec![response(200, source)]),
        limits,
        pending(),
    )
    .await
    .unwrap();
    assert!(!report.command_succeeded());
    assert!(report.sdk_error.is_some());
    assert!(host.output.lock().unwrap().is_empty());
    let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
    assert_eq!(receipt["error"]["code"], "BudgetExceeded");
}
