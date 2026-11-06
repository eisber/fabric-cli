// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root argv -> existing owner resolver -> adapter -> shared SDK HTTP contracts.
#![cfg(feature = "v2-preview")]
#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod support;

use async_trait::async_trait;
use azure_core::http::{AsyncRawResponse, Body, HttpClient, Request, headers::Headers};
use fabric_cli::{
    ReadConfig,
    session::ReadSession,
    v2_preview::{self, PreviewCommand, PreviewError, PreviewHost, PreviewInvocation},
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_ontology::{
    AdapterError, AdapterLimits, CapabilityError, HostError, InputSource, NativeBinding,
    NativeContext, OutputDestination, bind_metadata_operations, bind_native_operations,
    bind_public_operations,
};
use fabric_cli_output::Timestamp;
use fabric_sdk::{
    Client, EndpointBinding, RequestContext, ServiceOrigin,
    ontology::DefinitionLimits,
    ontology_workload::{
        client::Features,
        workload::{
            WorkloadError, WorkloadExecutor, WorkloadFuture, WorkloadRequest, WorkloadResponse,
        },
    },
    v2::{
        ApprovalPolicy, Availability, Effect, OperationDescriptor, OperationHandle, Outcome,
        Precondition, PreparedOperation, Registry, Verb,
    },
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::OperationLimits;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    future::{Future, pending},
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{Credential, Reply, reply};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OPERATION: &str = "99999999-8888-7777-6666-555555555555";
const OWNER: &str = "/Demo.Workspace/Knowledge.Ontology";
const DEFINITION: &str = "/Demo.Workspace/Knowledge.Ontology/src/definition";
const MODEL: &str = "/Demo.Workspace/Knowledge.Ontology/src/model/minimized";

type Events = Arc<Mutex<Vec<&'static str>>>;

struct Wire {
    host: String,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Value>>,
    events: Events,
    poll_started: tokio::sync::Notify,
    pause_poll: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Wire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Wire").finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().host_str(), Some(self.host.as_str()));
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            concat!("Bearer ", "offline-command-test"),
        );
        let body = match request.body() {
            Body::Bytes(bytes) if bytes.is_empty() => Value::Null,
            Body::Bytes(bytes) => serde_json::from_slice(bytes).unwrap(),
            _ => panic!("unexpected streaming input"),
        };
        let path = request.url().path();
        assert!(!path.contains("onelake"));
        assert!(!path.contains("/folders"));
        if request.method() != azure_core::http::Method::Get
            || path.ends_with("/model")
            || path.ends_with("Definition")
        {
            let events = self.events.lock().unwrap();
            assert!(events.contains(&"reserve"));
            assert!(events.contains(&"approve"));
        }
        self.requests.lock().unwrap().push(json!({
            "method": request.method().to_string(),
            "url": request.url().as_str(),
            "body": body,
        }));
        if path.contains("/operations/") && self.pause_poll.load(Ordering::SeqCst) {
            self.poll_started.notify_one();
            return pending().await;
        }
        let response = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or replay");
        let mut headers = Headers::new();
        for (name, value) in response.headers {
            headers.insert(name, value);
        }
        let body = if response.status == 204 {
            vec![]
        } else {
            serde_json::to_vec(&response.body).unwrap()
        };
        Ok(AsyncRawResponse::from_bytes(
            response.status.into(),
            headers,
            body,
        ))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

struct Fixture {
    sdk: Client,
    executor: HttpExecutor,
    wire: Arc<Wire>,
    credential: Arc<Credential>,
}

impl Fixture {
    fn new(replies: Vec<Reply>, events: Events) -> Self {
        Self::at_host("api.fabric.microsoft.com", replies, events)
    }

    fn at_host(host: &str, replies: Vec<Reply>, events: Events) -> Self {
        let credential = Arc::new(Credential::default());
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let sdk = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            fabric_sdk::CredentialSelection::new(
                "offline-account",
                "offline-tenant",
                fabric_sdk::Audience::Fabric,
            )
            .unwrap(),
            endpoint.clone(),
        ));
        let wire = Arc::new(Wire {
            host: host.into(),
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(vec![]),
            events,
            poll_started: tokio::sync::Notify::new(),
            pause_poll: std::sync::atomic::AtomicBool::new(false),
        });
        let executor =
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
                .unwrap()
                .with_endpoint(endpoint);
        Self {
            sdk,
            executor,
            wire,
            credential,
        }
    }

    fn reads(&self) -> ReadSession<'_> {
        ReadSession {
            sdk: &self.sdk,
            executor: &self.executor,
            tenant: Element::tenant("Tenant", "offline-tenant"),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.wire.requests.lock().unwrap().clone()
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
            return Err(io::Error::other("synthetic output failure"));
        }
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Host {
    allowed_id: &'static str,
    events: Events,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Vec<u8>,
    approved: Vec<PreparedOperation>,
    refuse_discovery: bool,
    refuse_operation: bool,
    fail_writer: bool,
    stdin: Option<Vec<u8>>,
    native: Option<NativeContext>,
}
impl Host {
    fn new(allowed_id: &'static str) -> Self {
        Self {
            allowed_id,
            events: Arc::default(),
            stdout: Arc::default(),
            stderr: vec![],
            approved: vec![],
            refuse_discovery: false,
            refuse_operation: false,
            fail_writer: false,
            stdin: None,
            native: None,
        }
    }
}
impl ApprovalPolicy for Host {
    type Error = HostError;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), HostError> {
        assert!(self.events.lock().unwrap().contains(&"reserve"));
        self.events.lock().unwrap().push("authorize");
        if self.refuse_operation {
            return Err(HostError("Synthetic policy refusal.".into()));
        }
        assert_eq!(operation.operation_id(), self.allowed_id);
        assert_eq!(operation.target().identity().workspace_id(), WORKSPACE);
        if self.allowed_id.starts_with("ontology.entity.") {
            assert_eq!(operation.target().identity().item_id(), Some(ITEM));
            assert_eq!(operation.target().selection().generation, Some(2));
            assert!(
                matches!(operation.availability(), Availability::Conditional(gates)
                if gates.iter().any(|gate| gate.id == "ontology.native.host"))
            );
            self.events.lock().unwrap().push("approve");
            self.approved.push(operation.clone());
            return Ok(());
        }
        let metadata_mutation = matches!(
            self.allowed_id,
            "item.metadata.create" | "item.metadata.patch" | "item.delete"
        );
        assert_eq!(
            operation.target().identity().item_id(),
            if self.allowed_id == "item.metadata.create" {
                None
            } else {
                Some(ITEM)
            },
        );
        let gate = if metadata_mutation {
            "ontology.public.mutation.host"
        } else {
            "ontology.public.host"
        };
        assert!(
            matches!(operation.availability(), Availability::Conditional(gates)
            if gates.iter().any(|prerequisite| prerequisite.id == gate))
        );
        if metadata_mutation {
            assert_eq!(
                operation.input()["request"]["context"]["account"],
                "offline-account"
            );
            assert_eq!(
                operation.input()["request"]["context"]["tenant"],
                "offline-tenant"
            );
            assert_eq!(
                operation.input()["request"]["bounds"]["maxInputBytes"],
                4096
            );
        } else {
            assert_eq!(operation.input()["context"]["account"], "offline-account");
            assert_eq!(operation.input()["context"]["tenant"], "offline-tenant");
            assert_eq!(operation.input()["maxInputBytes"], 4096);
        }
        if self.allowed_id.ends_with("replace") || self.allowed_id == "item.metadata.patch" {
            assert!(
                operation
                    .preconditions()
                    .contains(&Precondition::ExistingOnly)
            );
            assert!(
                operation
                    .preconditions()
                    .contains(&Precondition::UnguardedConcurrency)
            );
            assert!(operation.effects().contains(&Effect::RemoteWrite));
        }
        if self.allowed_id == "item.metadata.create" {
            assert_eq!(operation.target().selection().generation, None);
            assert_eq!(operation.preconditions(), [Precondition::CreateOnly]);
        }
        if self.allowed_id == "item.delete" {
            assert!(operation.effects().contains(&Effect::RemoteDelete));
        }
        if self.allowed_id.starts_with("ontology.model") {
            assert!(operation.effects().contains(&Effect::MeteredConsumption));
        }
        self.events.lock().unwrap().push("approve");
        self.approved.push(operation.clone());
        Ok(())
    }
}
impl PreviewHost for Host {
    fn deadline(&mut self, explicit: Option<Duration>) -> Result<(), HostError> {
        assert!(explicit.is_none_or(|value| value == Duration::from_secs(5)));
        assert!(!self.events.lock().unwrap().contains(&"deadline"));
        self.events.lock().unwrap().push("deadline");
        Ok(())
    }
    fn authorize_discovery(
        &mut self,
        command: &PreviewCommand,
        context: &RequestContext,
    ) -> Result<(), HostError> {
        assert!(
            command
                .arguments()
                .path
                .starts_with("/Demo.Workspace/Knowledge.Ontology")
        );
        assert_eq!(context.selection().account(), "offline-account");
        if self.refuse_discovery {
            return Err(HostError("Synthetic discovery refusal.".into()));
        }
        self.events.lock().unwrap().push("discovery");
        Ok(())
    }
    fn read_input(
        &mut self,
        source: &InputSource,
        maximum: NonZeroUsize,
    ) -> Result<Vec<u8>, HostError> {
        self.events.lock().unwrap().push("input");
        let bytes = match source {
            InputSource::Stdin => self
                .stdin
                .take()
                .ok_or_else(|| HostError("Stdin already consumed.".into()))?,
            InputSource::File(path) => {
                let mut bytes = vec![];
                File::open(path)
                    .and_then(|file| {
                        file.take(u64::try_from(maximum.get()).unwrap() + 1)
                            .read_to_end(&mut bytes)
                    })
                    .map_err(|_| HostError("Input file unavailable.".into()))?;
                bytes
            }
        };
        if bytes.len() > maximum.get() {
            return Err(HostError("Input byte bound exceeded.".into()));
        }
        Ok(bytes)
    }
    fn reserve_output(
        &mut self,
        destination: &OutputDestination,
    ) -> Result<Box<dyn Write>, HostError> {
        self.events.lock().unwrap().push("reserve");
        match destination {
            OutputDestination::Stdout => Ok(Box::new(Writer {
                bytes: self.stdout.clone(),
                fail: self.fail_writer,
            })),
            OutputDestination::File(path) => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(|file| Box::new(file) as Box<dyn Write>)
                .map_err(|_| HostError("Output reservation failed.".into())),
        }
    }
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
}

impl v2_preview::NativePreviewHost for Host {
    fn native_context(&mut self) -> Result<NativeContext, HostError> {
        self.native
            .clone()
            .ok_or_else(|| HostError("Private binding is unavailable.".into()))
    }
}

struct NativeWire {
    events: Events,
    requests: Mutex<Vec<Value>>,
    replies: Mutex<VecDeque<WorkloadResponse>>,
}

impl WorkloadExecutor for NativeWire {
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            assert!(self.events.lock().unwrap().contains(&"approve"));
            self.requests.lock().unwrap().push(json!({
                "method": request.method().as_str(),
                "route": request.route().as_str(),
                "body": if request.body().is_empty() { Value::Null }
                    else { serde_json::from_slice::<Value>(request.body()).unwrap() },
            }));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("No native replay permitted"))
        })
    }
}

fn native_response(status: u16, body: Value) -> WorkloadResponse {
    WorkloadResponse {
        status,
        headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
        body: if status == 204 {
            vec![]
        } else {
            serde_json::to_vec(&body).unwrap()
        },
    }
}

fn native_host(id: &'static str, response: WorkloadResponse) -> (Host, Arc<NativeWire>) {
    let mut host = Host::new(id);
    let wire = Arc::new(NativeWire {
        events: host.events.clone(),
        requests: Mutex::default(),
        replies: Mutex::new(vec![response].into()),
    });
    host.native = Some(NativeContext {
        executor: wire.clone(),
        features: Features {
            tmsl: true,
            ..Features::default()
        },
        binding: NativeBinding::new(
            "offline-account",
            "offline-tenant",
            "offline-native-audience",
            "offline-capacity-routing",
        )
        .unwrap(),
    });
    (host, wire)
}

fn native_registry() -> Registry {
    let mut registry = registry();
    for (id, verb) in [
        ("ontology.entity.get", Verb::Get),
        ("ontology.entity.list", Verb::List),
        ("ontology.entity.create", Verb::Create),
        ("ontology.entity.patch", Verb::Update),
        ("ontology.entity.delete", Verb::Delete),
        ("ontology.entity.bind", Verb::Link),
        ("ontology.entity.rename", Verb::Move),
    ] {
        let effects = match verb {
            Verb::Get | Verb::List => vec![Effect::RemoteRead, Effect::MeteredConsumption],
            Verb::Delete => vec![Effect::RemoteWrite, Effect::RemoteDelete],
            _ => vec![Effect::RemoteWrite],
        };
        registry
            .register(
                OperationDescriptor::new(
                    id,
                    verb,
                    json!({"type": "object"}),
                    json!({"type": "object"}),
                    effects,
                )
                .unwrap()
                .with_optional_effects(vec![Effect::LocalRead, Effect::LocalWrite])
                .unwrap(),
            )
            .unwrap();
    }
    bind_native_operations(&mut registry).unwrap();
    registry
}

fn registry() -> Registry {
    let mut registry = Registry::default();
    for (id, verb, mut effects) in [
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
        (
            "item.metadata.create",
            Verb::Create,
            vec![Effect::RemoteWrite],
        ),
        (
            "item.metadata.patch",
            Verb::Update,
            vec![Effect::RemoteWrite],
        ),
        ("item.delete", Verb::Delete, vec![Effect::RemoteDelete]),
    ] {
        if matches!(verb, Verb::Update | Verb::Create) {
            effects.push(Effect::LocalRead);
        }
        registry.register(OperationDescriptor::new(
            id, verb,
            json!({"type":"object","description":"Offline fixture descriptor; production catalog remains caller-owned."}),
            json!({"type":"object"}), effects,
        ).unwrap().with_optional_effects(vec![Effect::LocalWrite]).unwrap()).unwrap();
    }
    bind_public_operations(&mut registry).unwrap();
    bind_metadata_operations(&mut registry).unwrap();
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

fn parsed(args: &[&str]) -> PreviewCommand {
    let PreviewInvocation::Operation(command) =
        v2_preview::parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("operation required");
    };
    *command
}

fn metadata(generation: Value) -> Value {
    json!({"id":ITEM,"workspaceId":WORKSPACE,"displayName":"Knowledge","type":"Ontology","properties":{"generation":generation}})
}

fn discovery(generation: Value) -> Vec<Reply> {
    vec![
        reply(
            json!({"value":[{"id":WORKSPACE,"displayName":"Demo","type":"Workspace","capacityId":"capacity"}]}),
        ),
        reply(json!({"value":[{"id":ITEM,"displayName":"Knowledge","type":"Ontology"}]})),
        reply(metadata(generation)),
    ]
}

fn definition() -> Value {
    json!({"definition":{"parts":[{"path":"database.tmdl","payload":"YQ==","payloadType":"InlineBase64"}]}})
}

fn model() -> Value {
    json!({"name":"Knowledge","model":{"entities":[]}})
}

fn accepted() -> Reply {
    Reply {
        status: 202,
        body: Value::Null,
        headers: BTreeMap::from([
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
            ("x-ms-operation-id".into(), OPERATION.into()),
            ("Retry-After".into(), "0".into()),
        ]),
    }
}

async fn run(
    fixture: &Fixture,
    command: &PreviewCommand,
    host: &mut Host,
) -> Result<fabric_cli_ontology::ExecutionReport, PreviewError> {
    run_cancelled(
        fixture,
        command,
        host,
        tokio::time::sleep(Duration::from_secs(5)),
    )
    .await
}

async fn run_cancelled(
    fixture: &Fixture,
    command: &PreviewCommand,
    host: &mut Host,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<fabric_cli_ontology::ExecutionReport, PreviewError> {
    v2_preview::dispatch(
        command,
        &fixture.reads(),
        fixture.executor.clone(),
        &registry(),
        host,
        limits(),
        cancelled,
    )
    .await
}

#[tokio::test]
async fn generic_and_facade_metadata_use_the_same_real_root_pipeline() {
    let mut receipts = vec![];
    for args in [
        vec!["get", OWNER],
        vec!["artifact", "ontology", "get", OWNER],
    ] {
        let mut host = Host::new("item.metadata.get");
        let mut replies = discovery(json!(2));
        replies.push(reply(metadata(json!(2))));
        let fixture = Fixture::new(replies, host.events.clone());
        let report = run(&fixture, &parsed(&args), &mut host).await.unwrap();
        assert_eq!(v2_preview::execution_exit_code(&report), 0);
        assert!(
            matches!(report.receipt.outcome(), Outcome::Succeeded { data } if data == &metadata(json!(2)))
        );
        assert_eq!(report.receipt.target().canonical_path(), OWNER);
        assert_eq!(fixture.requests().len(), 4);
        assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 4);
        assert_eq!(host.approved.len(), 1);
        assert!(host.stderr.is_empty());
        let bytes = host.stdout.lock().unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let printed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(printed["meta"]["operationId"], "item.metadata.get");
        assert_eq!(printed["data"], metadata(json!(2)));
        receipts.push(printed);
    }
    assert_eq!(receipts[0], receipts[1]);
}

#[tokio::test]
async fn source_facets_reach_the_real_public_sdk_not_onelake_or_metadata_fallback() {
    for (path, operation, output, tail) in [
        (
            DEFINITION,
            "item.definition.get",
            definition(),
            "/getDefinition",
        ),
        (
            MODEL,
            "ontology.model.get",
            model(),
            "/model?form=Minimized",
        ),
        (
            "/Demo.Workspace/Knowledge.Ontology/src/model",
            "ontology.model.get",
            model(),
            "/model?form=Expanded",
        ),
        (
            "/Demo.Workspace/Knowledge.Ontology/src/model/expanded",
            "ontology.model.get",
            model(),
            "/model?form=Expanded",
        ),
    ] {
        let mut host = Host::new(operation);
        let mut replies = discovery(json!(2));
        replies.push(reply(output.clone()));
        let fixture =
            Fixture::at_host("msitapi.fabric.microsoft.com", replies, host.events.clone());
        let report = run(&fixture, &parsed(&["get", path]), &mut host)
            .await
            .unwrap();
        assert!(matches!(report.receipt.outcome(), Outcome::Succeeded { data } if data == &output));
        let seen = fixture.requests();
        assert_eq!(seen.len(), 4);
        assert!(seen[3]["url"].as_str().unwrap().ends_with(tail));
        assert_eq!(
            seen[3]["method"],
            if operation == "item.definition.get" {
                "POST"
            } else {
                "GET"
            }
        );
        assert_eq!(
            host.approved[0].input()["context"]["endpoint"],
            "msitapi.fabric.microsoft.com"
        );
        assert!(host.stderr.is_empty());
    }
}

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-cli-v2-preview-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn input(&self) -> PathBuf {
        self.0.join("input.json")
    }
    fn output(&self) -> PathBuf {
        self.0.join("output.json")
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        for path in [self.input(), self.output()] {
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[tokio::test]
async fn both_replacement_routes_read_actual_input_reserve_no_clobber_and_retain_conditions() {
    for facade in [false, true] {
        for (path, id, body, result, tail) in [
            (
                DEFINITION,
                "item.definition.replace",
                definition(),
                Value::Null,
                "/updateDefinition",
            ),
            (
                MODEL,
                "ontology.model.replace",
                json!({"model":{"entities":[]},"description":"authored"}),
                model(),
                "/model",
            ),
        ] {
            let directory = TempRoot::new();
            fs::write(directory.input(), serde_json::to_vec(&body).unwrap()).unwrap();
            let input = directory.input();
            let output = directory.output();
            let mut args = if facade {
                vec!["artifact", "ontology"]
            } else {
                vec![]
            };
            args.extend([
                "update",
                path,
                "--mode",
                "replace",
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ]);
            let mut host = Host::new(id);
            let mut replies = discovery(json!(2));
            replies.push(reply(result));
            let fixture = Fixture::new(replies, host.events.clone());
            let report = run(&fixture, &parsed(&args), &mut host).await.unwrap();
            assert!(report.command_succeeded());
            assert!(
                report
                    .receipt
                    .preconditions()
                    .contains(&Precondition::UnguardedConcurrency)
            );
            assert!(host.stdout.lock().unwrap().is_empty());
            assert!(host.stderr.is_empty());
            assert!(host.approved[0].effects().contains(&Effect::LocalWrite));
            let printed: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
            assert_eq!(printed["meta"]["operationId"], id);
            let seen = fixture.requests();
            assert_eq!(seen.len(), 4);
            assert_eq!(seen[3]["method"], "POST");
            assert!(seen[3]["url"].as_str().unwrap().ends_with(tail));
            assert_eq!(seen[3]["body"], body);
            assert_eq!(
                *host.events.lock().unwrap(),
                [
                    "deadline",
                    "discovery",
                    "input",
                    "reserve",
                    "authorize",
                    "approve"
                ]
            );
        }
    }
}

#[tokio::test]
async fn existing_output_and_exact_effect_refusal_prevent_mutation() {
    for refuse in [false, true] {
        let directory = TempRoot::new();
        if !refuse {
            fs::write(directory.output(), b"keep-existing").unwrap();
        }
        let output = directory.output();
        let mut host = Host::new("ontology.model.replace");
        host.stdin = Some(serde_json::to_vec(&json!({"model":{}})).unwrap());
        host.refuse_operation = refuse;
        let fixture = Fixture::new(discovery(json!(2)), host.events.clone());
        let command = parsed(&[
            "update",
            MODEL,
            "--input",
            "-",
            "--mode",
            "replace",
            "-o",
            output.to_str().unwrap(),
        ]);
        let result = run(&fixture, &command, &mut host).await;
        assert!(matches!(
            result,
            Err(PreviewError::Adapter(AdapterError::Host(_)))
        ));
        assert_eq!(fixture.requests().len(), 3);
        assert!(host.approved.is_empty());
        assert!(host.stdout.lock().unwrap().is_empty());
        if !refuse {
            assert_eq!(fs::read(output).unwrap(), b"keep-existing");
        }
    }
}

#[tokio::test]
async fn discovery_refusal_invalid_facets_and_profiles_never_acquire_credentials() {
    for (args, discovery_refusal) in [
        (vec!["get", OWNER], true),
        (
            vec!["get", "/Demo.Workspace/Knowledge.Ontology/docs"],
            false,
        ),
        (
            vec!["get", "/Demo.Workspace/Knowledge.Ontology/src/owl"],
            false,
        ),
        (
            vec!["get", "/Demo.Workspace/Knowledge.Ontology/entities/x"],
            false,
        ),
        (
            vec!["get", "/Demo.Workspace/Knowledge.Ontology/Files/a"],
            false,
        ),
        (
            vec![
                "get",
                "/Demo.Workspace/Knowledge.Ontology/../Other.Ontology",
            ],
            false,
        ),
        (vec!["get", "/Demo.Workspace/Knowledge.Lakehouse"], false),
        (vec!["get", OWNER, "--profile", "unbound"], false),
        (vec!["get", OWNER, "--project", "id"], false),
        (vec!["create", OWNER], false),
        (vec!["delete", OWNER], false),
        (vec!["list", OWNER], false),
    ] {
        let mut host = Host::new("item.metadata.get");
        host.refuse_discovery = discovery_refusal;
        let fixture = Fixture::new(vec![], host.events.clone());
        assert!(run(&fixture, &parsed(&args), &mut host).await.is_err());
        assert!(fixture.requests().is_empty());
        assert!(fixture.credential.scopes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn provider_generation_and_type_are_not_inferred_from_the_path() {
    for generation in [Value::Null, json!(3), json!("2"), json!(1)] {
        let mut host = Host::new("ontology.model.get");
        let fixture = Fixture::new(discovery(generation), host.events.clone());
        assert!(
            run(&fixture, &parsed(&["get", MODEL]), &mut host)
                .await
                .is_err()
        );
        assert_eq!(fixture.requests().len(), 3);
        assert!(host.approved.is_empty());
    }
    let mut host = Host::new("ontology.model.get");
    let mut replies = discovery(json!(2));
    replies[2].body["type"] = json!("Lakehouse");
    let fixture = Fixture::new(replies, host.events.clone());
    assert!(matches!(
        run(&fixture, &parsed(&["get", MODEL]), &mut host).await,
        Err(PreviewError::Metadata(_)),
    ));
    assert_eq!(fixture.requests().len(), 3);
    assert!(host.approved.is_empty());
}

#[tokio::test]
async fn accepted_source_is_not_completion_and_denied_source_is_not_metadata() {
    for (response, accepted_result) in [
        (accepted(), true),
        (
            Reply {
                status: 403,
                body: json!({"error":{"code":"FeatureDenied","message":"private-provider-detail"}}),
                headers: BTreeMap::new(),
            },
            false,
        ),
    ] {
        let mut host = Host::new("item.definition.get");
        let mut replies = discovery(json!(2));
        replies.push(response);
        let fixture = Fixture::new(replies, host.events.clone());
        let report = run(&fixture, &parsed(&["get", DEFINITION]), &mut host)
            .await
            .unwrap();
        assert_eq!(fixture.requests().len(), 4);
        if accepted_result {
            assert!(matches!(report.receipt.outcome(), Outcome::Accepted));
            assert_eq!(v2_preview::execution_exit_code(&report), 0);
            assert_eq!(
                report.receipt.evidence().handles,
                [OperationHandle::FabricOperation {
                    id: OPERATION.into()
                }]
            );
            assert!(!host.stdout.lock().unwrap().is_empty());
            assert!(host.stderr.is_empty());
        } else {
            assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
            assert_eq!(v2_preview::execution_exit_code(&report), 1);
            assert!(host.stdout.lock().unwrap().is_empty());
            let printed: Value = serde_json::from_slice(&host.stderr).unwrap();
            assert_eq!(printed["error"]["code"], "AccessDenied");
            assert!(!String::from_utf8_lossy(&host.stderr).contains("private-provider-detail"));
        }
    }
}

#[tokio::test]
async fn poll_http_failure_after_accepted_replacement_retains_unknown_and_original_handle() {
    let mut host = Host::new("item.definition.replace");
    host.stdin = Some(serde_json::to_vec(&definition()).unwrap());
    let mut replies = discovery(json!(2));
    replies.extend([
        accepted(),
        Reply {
            status: 403,
            body: json!({"error":{"code":"Denied"}}),
            headers: BTreeMap::new(),
        },
    ]);
    let fixture = Fixture::new(replies, host.events.clone());
    let report = run(
        &fixture,
        &parsed(&[
            "update", DEFINITION, "--input", "-", "--mode", "replace", "--wait",
        ]),
        &mut host,
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(
        report.receipt.evidence().handles,
        [OperationHandle::FabricOperation {
            id: OPERATION.into()
        }]
    );
    assert!(!report.command_succeeded());
    assert_eq!(v2_preview::execution_exit_code(&report), 1);
    assert!(host.stdout.lock().unwrap().is_empty());
    assert_eq!(fixture.requests().len(), 5);
}

#[tokio::test]
async fn output_failure_keeps_successful_remote_evidence_and_does_not_replay() {
    let mut host = Host::new("ontology.model.replace");
    host.stdin = Some(serde_json::to_vec(&json!({"model":{}})).unwrap());
    host.fail_writer = true;
    let mut replies = discovery(json!(2));
    replies.push(reply(model()));
    let fixture = Fixture::new(replies, host.events.clone());
    let error = run(
        &fixture,
        &parsed(&["update", MODEL, "--input", "-", "--mode", "replace"]),
        &mut host,
    )
    .await
    .unwrap_err();
    let PreviewError::Adapter(AdapterError::Output { report, .. }) = error else {
        panic!("original execution report must survive output failure");
    };
    assert!(matches!(
        report.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert_eq!(fixture.requests().len(), 4);
    assert_eq!(host.stdout.lock().unwrap().len(), 7);
    assert!(host.stderr.is_empty());
}

#[tokio::test]
async fn cancelled_discovery_never_starts_http_or_outputs_a_success() {
    let mut host = Host::new("item.metadata.get");
    let fixture = Fixture::new(vec![], host.events.clone());
    let error = run_cancelled(
        &fixture,
        &parsed(&["get", OWNER, "--timeout", "5s"]),
        &mut host,
        async {},
    )
    .await
    .unwrap_err();
    assert!(matches!(error, PreviewError::Discovery(_)));
    assert!(fixture.requests().is_empty());
    assert!(host.stdout.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_after_acceptance_keeps_unknown_effects_without_replay() {
    let mut host = Host::new("item.definition.replace");
    host.stdin = Some(serde_json::to_vec(&definition()).unwrap());
    let mut replies = discovery(json!(2));
    replies.push(accepted());
    let fixture = Fixture::new(replies, host.events.clone());
    fixture.wire.pause_poll.store(true, Ordering::SeqCst);
    let cancelled = async {
        tokio::select! {
            _ = fixture.wire.poll_started.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("poll must start"),
        }
    };
    let report = run_cancelled(
        &fixture,
        &parsed(&[
            "update", DEFINITION, "--input", "-", "--mode", "replace", "--wait",
        ]),
        &mut host,
        cancelled,
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert_eq!(
        report.receipt.evidence().handles,
        [OperationHandle::FabricOperation {
            id: OPERATION.into()
        },]
    );
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert!(host.stdout.lock().unwrap().is_empty());
    assert_eq!(fixture.requests().len(), 5);
}

#[tokio::test]
async fn result_byte_bound_never_clips_source_into_successful_output() {
    let mut host = Host::new("ontology.model.get");
    let mut replies = discovery(json!(2));
    replies.push(reply(model()));
    let fixture = Fixture::new(replies, host.events.clone());
    let report = run(
        &fixture,
        &parsed(&["get", MODEL, "--max-bytes", "1"]),
        &mut host,
    )
    .await
    .unwrap();
    assert!(
        matches!(report.receipt.outcome(), Outcome::Failed { error } if error.code == "BudgetExceeded")
    );
    assert!(!report.command_succeeded());
    assert!(host.stdout.lock().unwrap().is_empty());
    assert_eq!(fixture.requests().len(), 4);
    let printed: Value = serde_json::from_slice(&host.stderr).unwrap();
    assert!(printed.get("data").is_none());
}

#[tokio::test]
async fn generation_one_definition_remains_native_and_expanded_model_cannot_be_replaced() {
    let mut host = Host::new("item.definition.get");
    let mut replies = discovery(json!(1));
    replies.push(reply(definition()));
    let fixture = Fixture::new(replies, host.events.clone());
    let report = run(&fixture, &parsed(&["get", DEFINITION]), &mut host)
        .await
        .unwrap();
    assert!(report.command_succeeded());
    assert_eq!(report.receipt.target().selection().generation, Some(1));
    assert_eq!(fixture.requests().len(), 4);

    for (path, flags) in [
        (
            "/Demo.Workspace/Knowledge.Ontology/src/model/expanded",
            vec!["--mode", "replace"],
        ),
        (MODEL, vec!["--mode", "patch"]),
        (
            MODEL,
            vec!["--mode", "replace", "--if-match", "unsupported-token"],
        ),
    ] {
        let mut host = Host::new("ontology.model.replace");
        let fixture = Fixture::new(discovery(json!(2)), host.events.clone());
        let mut args = vec!["update", path, "--input", "-"];
        args.extend(flags);
        assert!(run(&fixture, &parsed(&args), &mut host).await.is_err());
        assert_eq!(fixture.requests().len(), 3);
        assert!(host.approved.is_empty());
        assert!(!host.events.lock().unwrap().contains(&"input"));
    }
}

#[tokio::test]
async fn root_metadata_create_resolves_only_real_workspace_and_generic_facade_share_receipt() {
    let mut receipts = vec![];
    for facade in [false, true] {
        let args = if facade {
            vec!["artifact", "ontology", "create", OWNER, "--input", "-"]
        } else {
            vec!["create", OWNER, "--input", "-"]
        };
        let mut host = Host::new("item.metadata.create");
        host.stdin = Some(serde_json::to_vec(&json!({"displayName":"Knowledge"})).unwrap());
        let mut replies = discovery(json!(2));
        replies.truncate(1);
        let mut created = reply(metadata(json!(2)));
        created.status = 201;
        replies.push(created);
        let fixture = Fixture::new(replies, host.events.clone());
        let report = v2_preview::dispatch_command(
            &parsed(&args),
            &fixture.reads(),
            fixture.executor.clone(),
            &registry(),
            &mut host,
            limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert_eq!(report.exit_code(), 0, "{report:?}");
        assert_eq!(
            report.receipt().target().identity().workspace_id(),
            WORKSPACE
        );
        assert_eq!(report.receipt().target().identity().item_id(), None);
        assert_eq!(report.receipt().target().selection().generation, None);
        let calls = fixture.requests();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["method"], "POST");
        assert!(
            calls[1]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/workspaces/{WORKSPACE}/ontologies"))
        );
        assert!(
            !calls
                .iter()
                .any(|call| call["url"].as_str().unwrap().ends_with("/items"))
        );
        assert_eq!(calls[1]["body"], json!({"displayName":"Knowledge"}));
        assert!(host.stderr.is_empty());
        receipts.push(serde_json::from_slice::<Value>(&host.stdout.lock().unwrap()).unwrap());
    }
    assert_eq!(receipts[0], receipts[1]);
}

#[tokio::test]
async fn root_metadata_patch_and_delete_use_authoritative_owner_and_existing_sdk_routes() {
    for facade in [false, true] {
        for permanent in [false, true] {
            let mut args = if facade {
                vec!["artifact", "ontology"]
            } else {
                vec![]
            };
            args.extend(["delete", OWNER]);
            if permanent {
                args.push("--permanent");
            }
            let mut host = Host::new("item.delete");
            let mut replies = discovery(json!(2));
            let mut removed = reply(Value::Null);
            removed.status = 204;
            replies.push(removed);
            let fixture = Fixture::new(replies, host.events.clone());
            let report = v2_preview::dispatch_command(
                &parsed(&args),
                &fixture.reads(),
                fixture.executor.clone(),
                &registry(),
                &mut host,
                limits(),
                tokio::time::sleep(Duration::from_secs(5)),
            )
            .await
            .unwrap();
            assert_eq!(report.exit_code(), 0);
            assert_eq!(report.receipt().target().identity().item_id(), Some(ITEM));
            assert_eq!(
                report
                    .receipt()
                    .preconditions()
                    .contains(&Precondition::PermanentDeletion),
                permanent
            );
            let calls = fixture.requests();
            assert_eq!(calls.len(), 4);
            assert_eq!(calls[3]["method"], "DELETE");
            assert!(
                calls[3]["url"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("/workspaces/{WORKSPACE}/items/{ITEM}"))
            );
            assert_eq!(
                calls[3]["url"]
                    .as_str()
                    .unwrap()
                    .contains("hardDelete=true"),
                permanent
            );
        }

        let mut args = if facade {
            vec!["artifact", "ontology"]
        } else {
            vec![]
        };
        args.extend(["update", OWNER, "--mode", "patch", "--input", "-"]);
        let mut host = Host::new("item.metadata.patch");
        host.stdin = Some(serde_json::to_vec(&json!({"description":"changed"})).unwrap());
        let mut replies = discovery(json!(2));
        replies.push(reply(metadata(json!(2))));
        let fixture = Fixture::new(replies, host.events.clone());
        let report = v2_preview::dispatch_command(
            &parsed(&args),
            &fixture.reads(),
            fixture.executor.clone(),
            &registry(),
            &mut host,
            limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert_eq!(report.exit_code(), 0);
        let calls = fixture.requests();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3]["method"], "PATCH");
        assert_eq!(calls[3]["body"], json!({"description":"changed"}));
        assert!(
            calls[3]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/workspaces/{WORKSPACE}/ontologies/{ITEM}"))
        );
        assert!(
            report
                .receipt()
                .preconditions()
                .contains(&Precondition::UnguardedConcurrency)
        );
    }
}

#[tokio::test]
async fn root_creation_name_mismatch_and_exact_policy_refusal_never_dispatch_mutation() {
    for mismatch in [false, true] {
        let mut host = Host::new("item.metadata.create");
        host.stdin = Some(
            serde_json::to_vec(&json!({
                "displayName": if mismatch { "Different" } else { "Knowledge" },
            }))
            .unwrap(),
        );
        host.refuse_operation = !mismatch;
        let mut replies = discovery(json!(2));
        replies.truncate(1);
        let fixture = Fixture::new(replies, host.events.clone());
        let result = v2_preview::dispatch_command(
            &parsed(&["create", OWNER, "--input", "-"]),
            &fixture.reads(),
            fixture.executor.clone(),
            &registry(),
            &mut host,
            limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await;
        assert!(matches!(
            result,
            Err(PreviewError::MetadataMutation(CapabilityError::Host(_)))
        ));
        assert_eq!(fixture.requests().len(), 1);
        assert!(host.approved.is_empty());
        assert!(host.stdout.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn root_metadata_existing_output_is_not_truncated_and_prevents_patch() {
    let directory = TempRoot::new();
    let output = directory.output();
    fs::write(&output, b"preserve").unwrap();
    let mut host = Host::new("item.metadata.patch");
    host.stdin = Some(serde_json::to_vec(&json!({"description":"changed"})).unwrap());
    let fixture = Fixture::new(discovery(json!(2)), host.events.clone());
    let result = v2_preview::dispatch_command(
        &parsed(&[
            "update",
            OWNER,
            "--mode",
            "patch",
            "--input",
            "-",
            "-o",
            output.to_str().unwrap(),
        ]),
        &fixture.reads(),
        fixture.executor.clone(),
        &registry(),
        &mut host,
        limits(),
        tokio::time::sleep(Duration::from_secs(5)),
    )
    .await;
    assert!(matches!(
        result,
        Err(PreviewError::MetadataMutation(CapabilityError::Host(_)))
    ));
    assert_eq!(fs::read(output).unwrap(), b"preserve");
    assert_eq!(fixture.requests().len(), 3);
    assert!(host.approved.is_empty());
}

#[tokio::test]
async fn native_root_all_seven_aliases_share_real_owner_sdk_identity_and_streams() {
    let collection = format!("{OWNER}/src/schema/entities");
    let entity = format!("{collection}/stable-lineage");
    let binding = format!("{entity}/binding");
    for (verb, id, path, body, method) in [
        ("get", "ontology.entity.get", entity.as_str(), None, "GET"),
        (
            "list",
            "ontology.entity.list",
            collection.as_str(),
            None,
            "GET",
        ),
        (
            "create",
            "ontology.entity.create",
            collection.as_str(),
            Some(json!({"name":"Machine"})),
            "POST",
        ),
        (
            "update",
            "ontology.entity.patch",
            entity.as_str(),
            Some(json!({"description":null})),
            "PATCH",
        ),
        (
            "delete",
            "ontology.entity.delete",
            entity.as_str(),
            None,
            "DELETE",
        ),
        (
            "link",
            "ontology.entity.bind",
            binding.as_str(),
            Some(json!({"backingTable":"Machines"})),
            "PATCH",
        ),
        (
            "move",
            "ontology.entity.rename",
            entity.as_str(),
            Some(json!({"name":"Renamed"})),
            "PATCH",
        ),
    ] {
        let mut receipts = vec![];
        for facade in [false, true] {
            let data = json!({"name":"Machine","lineageTag":"stable-lineage"});
            let response = native_response(
                if verb == "create" {
                    201
                } else if verb == "delete" {
                    204
                } else {
                    200
                },
                if verb == "list" {
                    json!({"value":[data]})
                } else {
                    data
                },
            );
            let (mut host, native) = native_host(id, response);
            host.stdin = body
                .as_ref()
                .map(|value| serde_json::to_vec(value).unwrap());
            let fixture = Fixture::new(discovery(json!(2)), host.events.clone());
            let mut args = if facade {
                vec!["artifact", "ontology"]
            } else {
                vec![]
            };
            args.extend([verb, path]);
            if body.is_some() {
                args.extend(["--input", "-"]);
            }
            if verb == "update" {
                args.extend(["--mode", "patch"]);
            }
            if verb == "delete" {
                args.push("--permanent");
            }
            let report = v2_preview::dispatch_with_native(
                &parsed(&args),
                &fixture.reads(),
                fixture.executor.clone(),
                &native_registry(),
                &mut host,
                limits(),
                pending(),
            )
            .await
            .unwrap();
            assert_eq!(report.exit_code(), 0);
            assert_eq!(host.approved[0].operation_id(), id);
            assert_eq!(fixture.requests().len(), 3);
            let requests = native.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["method"], method);
            assert!(
                requests[0]["route"]
                    .as_str()
                    .unwrap()
                    .contains("/entityTypes")
            );
            if !matches!(verb, "list" | "create") {
                assert!(
                    requests[0]["route"]
                        .as_str()
                        .unwrap()
                        .ends_with("/stable-lineage")
                );
                assert_eq!(
                    report.receipt().target().child().unwrap().id,
                    "stable-lineage"
                );
            }
            if let Some(body) = &body {
                assert_eq!(requests[0]["body"], *body);
            }
            assert!(host.stderr.is_empty());
            receipts.push(serde_json::from_slice::<Value>(&host.stdout.lock().unwrap()).unwrap());
        }
        assert_eq!(receipts[0], receipts[1]);
    }
}

#[tokio::test]
async fn native_root_requires_known_generation_private_binding_approval_and_no_clobber() {
    let path = format!("{OWNER}/src/schema/entities/stable-lineage");
    for (generation, binding, refusal, collision) in [
        (json!(1), true, false, false),
        (Value::Null, true, false, false),
        (json!(2), false, false, false),
        (json!(2), true, true, false),
        (json!(2), true, false, true),
    ] {
        let (mut host, native) = native_host(
            "ontology.entity.patch",
            native_response(200, json!({"name":"Machine"})),
        );
        host.stdin = Some(br#"{"description":"changed"}"#.to_vec());
        host.refuse_operation = refusal;
        if !binding {
            host.native = None;
        }
        let fixture = Fixture::new(discovery(generation), host.events.clone());
        let directory = TempRoot::new();
        let output = directory.output();
        let mut args = vec!["update", path.as_str(), "--mode", "patch", "--input", "-"];
        if collision {
            fs::write(&output, b"preserve").unwrap();
            args.extend(["--output", output.to_str().unwrap()]);
        }
        let result = v2_preview::dispatch_with_native(
            &parsed(&args),
            &fixture.reads(),
            fixture.executor.clone(),
            &native_registry(),
            &mut host,
            limits(),
            pending(),
        )
        .await;
        assert!(result.is_err());
        assert!(native.requests.lock().unwrap().is_empty());
        assert!(host.approved.is_empty());
        assert!(host.stdout.lock().unwrap().is_empty());
        if refusal {
            assert!(host.events.lock().unwrap().contains(&"authorize"));
        }
        if collision {
            assert_eq!(fs::read(output).unwrap(), b"preserve");
        }
    }
}

#[tokio::test]
async fn native_root_partial_accepted_and_writer_failure_keep_original_evidence_without_replay() {
    let collection = format!("{OWNER}/src/schema/entities");
    let entity = format!("{collection}/stable-lineage");
    for (partial, accepted, writer_failure) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let (mut host, native) = native_host(
            if partial {
                "ontology.entity.list"
            } else {
                "ontology.entity.patch"
            },
            native_response(
                if accepted { 202 } else { 200 },
                if partial {
                    json!({"value":[{"name":"Machine","lineageTag":"stable-lineage"}],"continuationToken":"next"})
                } else {
                    json!({"name":"Machine","lineageTag":"stable-lineage"})
                },
            ),
        );
        host.fail_writer = writer_failure;
        host.stdin = Some(br#"{"description":"changed"}"#.to_vec());
        let fixture = Fixture::new(discovery(json!(2)), host.events.clone());
        let args = if partial {
            vec!["list", collection.as_str()]
        } else {
            vec!["update", entity.as_str(), "--input", "-", "--mode", "patch"]
        };
        let result = v2_preview::dispatch_with_native(
            &parsed(&args),
            &fixture.reads(),
            fixture.executor.clone(),
            &native_registry(),
            &mut host,
            limits(),
            pending(),
        )
        .await;
        if writer_failure {
            let Err(PreviewError::Native(CapabilityError::Output { report, .. })) = result else {
                panic!("Original native output evidence required");
            };
            assert!(matches!(
                report.receipt.outcome(),
                Outcome::Succeeded { .. }
            ));
        } else {
            let report = result.unwrap();
            if partial {
                assert!(matches!(
                    report.receipt().outcome(),
                    Outcome::Partial { .. }
                ));
            } else {
                assert!(matches!(
                    report.receipt().outcome(),
                    Outcome::Unknown { .. }
                ));
                assert!(report.receipt().evidence().handles.is_empty());
                assert!(report.receipt().evidence().mutation_may_have_occurred);
            }
            assert_eq!(report.exit_code(), 1);
            assert!(!host.stderr.is_empty());
        }
        assert_eq!(native.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.requests().len(), 3);
    }
}
