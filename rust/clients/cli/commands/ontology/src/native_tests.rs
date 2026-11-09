// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Injected native entity execution contracts; no HTTP/auth/runtime implementation.

#![deny(unnameable_test_items)]

use super::*;
use crate::{HostError, InputSource, OutputDestination, parse_common_args, parse_facade_args};
use fabric_sdk_ontology::v2::{
    ApprovalPolicy, ChildIdentity, ItemIdentity, OperationDescriptor, PreparedOperation, Target,
};
use fabric_sdk_ontology_workload::workload::{WorkloadFuture, WorkloadRequest};
use std::{
    collections::{BTreeMap, VecDeque},
    future::pending,
    io::{self, Write},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[path = "native_boundary_tests.rs"]
mod boundaries;

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const PATH: &str = "/w.Workspace/o.Ontology/schema";
const LINEAGE: &str = "server-derived:entity/1";

type Observed = (String, String, Option<String>, Vec<u8>);
#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<Result<WorkloadResponse, WorkloadError>>>,
    seen: Mutex<Vec<Observed>>,
    approved: AtomicBool,
    pending: bool,
}
impl Executor {
    fn new(responses: Vec<WorkloadResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            seen: Mutex::new(vec![]),
            approved: AtomicBool::new(false),
            pending: false,
        }
    }
    fn failing(error: WorkloadError) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([Err(error)])),
            ..Self::default()
        }
    }
}
impl WorkloadExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            assert!(
                self.approved.load(Ordering::SeqCst),
                "native dispatch must follow exact host approval"
            );
            self.seen.lock().unwrap().push((
                request.method().as_str().into(),
                request.route().as_str().into(),
                request.content_type().map(str::to_owned),
                request.body().to_vec(),
            ));
            if self.pending {
                return pending().await;
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected one SDK request")
        })
    }
}
struct Writer(Arc<Mutex<Vec<u8>>>, bool);
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.1 {
            return Err(io::Error::other("fixture output failed"));
        }
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Host {
    context: NativeContext,
    input: Vec<u8>,
    output: Arc<Mutex<Vec<u8>>>,
    stderr: Vec<u8>,
    approved: Vec<PreparedOperation>,
    reserved: bool,
    deny: bool,
    exists: bool,
    fail_writer: bool,
    target: Option<ResolvedTarget>,
    observed_executor: Arc<Executor>,
}
impl ApprovalPolicy for Host {
    type Error = HostError;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), HostError> {
        assert!(self.reserved);
        if self.deny {
            return Err(HostError("approval denied".into()));
        }
        self.observed_executor
            .approved
            .store(true, Ordering::SeqCst);
        self.approved.push(operation.clone());
        Ok(())
    }
}
impl AdapterHost for Host {
    fn resolve(&mut self, _: &str) -> Result<ResolvedTarget, HostError> {
        self.target
            .clone()
            .ok_or(HostError("native target unavailable".into()))
    }
    fn read_input(&mut self, _: &InputSource, maximum: NonZeroUsize) -> Result<Vec<u8>, HostError> {
        if self.input.len() > maximum.get() {
            return Err(HostError("input too large".into()));
        }
        Ok(self.input.clone())
    }
    fn reserve_output(
        &mut self,
        destination: &OutputDestination,
    ) -> Result<Box<dyn Write>, HostError> {
        if self.exists {
            return Err(HostError("OutputExists".into()));
        }
        self.reserved = true;
        match destination {
            OutputDestination::Stdout => {
                Ok(Box::new(Writer(self.output.clone(), self.fail_writer)))
            }
            OutputDestination::File(path) => std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(|writer| Box::new(writer) as Box<dyn Write>)
                .map_err(|_| HostError("OutputExists or unavailable".into())),
        }
    }
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn deadline(&mut self, _: Option<Duration>) -> Result<(), HostError> {
        Ok(())
    }
}
impl NativeHost for Host {
    fn native_context(&mut self) -> Result<NativeContext, HostError> {
        Ok(self.context.clone())
    }
}
fn host(executor: Arc<Executor>, body: Option<Value>) -> Host {
    Host {
        context: NativeContext {
            executor: executor.clone(),
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
        },
        input: body.map_or_else(Vec::new, |body| serde_json::to_vec(&body).unwrap()),
        output: Arc::new(Mutex::new(vec![])),
        stderr: vec![],
        approved: vec![],
        reserved: false,
        deny: false,
        exists: false,
        fail_writer: false,
        target: None,
        observed_executor: executor,
    }
}
fn args(verb: OntologyVerb) -> OntologyArguments {
    arguments(verb, true)
}
fn arguments(verb: OntologyVerb, facade: bool) -> OntologyArguments {
    let mut tokens = vec![verb.common_name(), PATH];
    match verb {
        OntologyVerb::Create | OntologyVerb::Link | OntologyVerb::Move => {
            tokens.extend(["--input", "-"])
        }
        OntologyVerb::Update => tokens.extend(["--input", "-", "--mode", "patch"]),
        OntologyVerb::Delete => tokens.push("--permanent"),
        _ => {}
    }
    if facade {
        parse_facade_args(std::iter::once("ontology").chain(tokens)).unwrap()
    } else {
        parse_common_args(verb, tokens).unwrap()
    }
}
fn target(verb: OntologyVerb) -> ResolvedTarget {
    let collection = matches!(verb, OntologyVerb::Create | OntologyVerb::List);
    ResolvedTarget::new(
        PATH,
        ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
        Target {
            kind: TargetKind::Item,
            owner_type: "Ontology".into(),
            facet: if collection {
                "schema.entities"
            } else if verb == OntologyVerb::Link {
                "schema.binding"
            } else {
                "schema.entity"
            }
            .into(),
            representation: "native".into(),
            generation: Some(2),
        },
        if collection {
            None
        } else {
            Some(ChildIdentity {
                kind: "entity.lineage".into(),
                id: LINEAGE.into(),
            })
        },
    )
    .unwrap()
}
fn registry() -> Registry {
    let mut registry = Registry::default();
    registry.alias("ontology", "Ontology").unwrap();
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
                    json!({"type":"object"}),
                    json!({"type":"object"}),
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
fn limits() -> AdapterLimits {
    AdapterLimits {
        input_bytes: NonZeroUsize::new(4096).unwrap(),
        data_bytes: NonZeroUsize::new(4096).unwrap(),
        definition: fabric_sdk_ontology::DefinitionLimits {
            max_parts: NonZeroUsize::new(4).unwrap(),
            max_decoded_bytes: NonZeroUsize::new(4096).unwrap(),
        },
        operation: fabric_sdk_operations::OperationLimits {
            max_requests: NonZeroUsize::new(4).unwrap(),
            max_response_bytes: NonZeroUsize::new(8192).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        },
    }
}
fn response(status: u16, body: Value) -> WorkloadResponse {
    WorkloadResponse {
        status,
        headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
        body: serde_json::to_vec(&body).unwrap(),
    }
}

async fn run(
    args: &OntologyArguments,
    facade: bool,
    registry: &Registry,
    host: &mut Host,
    target: ResolvedTarget,
    limits: AdapterLimits,
) -> Result<CapabilityReport, CapabilityError> {
    host.target = Some(target);
    execute_native(args, facade, registry, host, limits, pending()).await
}

#[tokio::test]
async fn every_native_alias_executes_the_typed_sdk_once_with_same_generic_facade_identity() {
    for (verb, body, method) in [
        (OntologyVerb::Get, None, "GET"),
        (OntologyVerb::List, None, "GET"),
        (
            OntologyVerb::Create,
            Some(json!({"name":"Machine"})),
            "POST",
        ),
        (
            OntologyVerb::Update,
            Some(
                json!({"description":null,"properties":[{"name":"Id","dataType":"int64"}],"keyProperties":[]}),
            ),
            "PATCH",
        ),
        (OntologyVerb::Delete, None, "DELETE"),
        (
            OntologyVerb::Link,
            Some(
                json!({"backingTable":"Machines","additionalBackingTables":[{"table":"Details","relationship":"MachineDetails"}]}),
            ),
            "PATCH",
        ),
        (OntologyVerb::Move, Some(json!({"name":"Renamed"})), "PATCH"),
    ] {
        let mut results = vec![];
        for facade in [false, true] {
            let response = if verb == OntologyVerb::Delete {
                WorkloadResponse {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                }
            } else if verb == OntologyVerb::List {
                response(
                    200,
                    json!({"value":[{"name":"Machine","lineageTag":LINEAGE}]}),
                )
            } else {
                response(
                    if verb == OntologyVerb::Create {
                        201
                    } else {
                        200
                    },
                    json!({"name":"Machine","lineageTag":LINEAGE}),
                )
            };
            let executor = Arc::new(Executor::new(vec![response]));
            let mut host = host(executor.clone(), body.clone());
            let report = run(
                &arguments(verb, facade),
                facade,
                &registry(),
                &mut host,
                target(verb),
                limits(),
            )
            .await
            .unwrap();
            assert!(report.command_succeeded());
            assert_eq!(host.approved.len(), 1);
            assert_eq!(
                host.approved[0].input()["request"]["sourceClass"],
                "privateWorkload"
            );
            assert_eq!(
                host.approved[0].input()["request"]["features"]["tmsl"],
                true
            );
            assert_eq!(
                host.approved[0].input()["request"]["body"],
                body.clone().unwrap_or(Value::Null)
            );
            if body.is_some() {
                assert!(host.approved[0].effects().contains(&Effect::LocalRead));
            }
            assert!(host.stderr.is_empty());
            results.push(serde_json::from_slice::<Value>(&host.output.lock().unwrap()).unwrap());
            let seen = executor.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].0, method);
            assert!(seen[0].1.starts_with(&format!(
                "/v4/workspaces/{WORKSPACE}/ontologies/{ITEM}/entityTypes"
            )));
            if !matches!(verb, OntologyVerb::Create | OntologyVerb::List) {
                assert!(seen[0].1.ends_with("server-derived%3Aentity%2F1"));
            }
            if method == "PATCH" {
                assert_eq!(seen[0].2.as_deref(), Some("application/merge-patch+json"));
            }
            if let Some(body) = &body {
                assert_eq!(&serde_json::from_slice::<Value>(&seen[0].3).unwrap(), body);
            }
            if !matches!(verb, OntologyVerb::Get | OntologyVerb::List) {
                assert!(host.approved[0].effects().contains(&Effect::RemoteWrite));
                assert!(!host.approved[0].effects().contains(&Effect::Publication));
            }
            assert_eq!(
                host.approved[0].input()["request"]["context"],
                json!({
                    "account":"offline-account","tenant":"offline-tenant","audience":"offline-native-audience","nativeBinding":"offline-capacity-routing"
                })
            );
        }
        assert_eq!(results[0], results[1]);
    }
}

#[tokio::test]
async fn native_gates_identity_unknown_fields_and_rename_binding_scope_fail_before_dispatch() {
    for (verb, body) in [
        (
            OntologyVerb::Create,
            json!({"name":"Machine","lineageTag":"caller-invented"}),
        ),
        (OntologyVerb::Update, json!({"readOnlyProjection":"bad"})),
        (
            OntologyVerb::Move,
            json!({"name":"Renamed","description":"out-of-scope"}),
        ),
        (OntologyVerb::Move, json!({"name":null})),
        (
            OntologyVerb::Link,
            json!({"backingTable":"Machines","properties":[]}),
        ),
        (OntologyVerb::Link, json!({"backingTable":null})),
    ] {
        let executor = Arc::new(Executor::default());
        let mut host = host(executor.clone(), Some(body));
        assert!(
            run(
                &args(verb),
                true,
                &registry(),
                &mut host,
                target(verb),
                limits()
            )
            .await
            .is_err()
        );
        assert!(executor.seen.lock().unwrap().is_empty());
        assert!(host.approved.is_empty());
    }
    let executor = Arc::new(Executor::default());
    let mut host = host(executor.clone(), None);
    host.context.features.tmsl = false;
    assert!(
        run(
            &args(OntologyVerb::Get),
            true,
            &registry(),
            &mut host,
            target(OntologyVerb::Get),
            limits()
        )
        .await
        .is_err()
    );
    assert!(executor.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_list_partial_records_have_truthful_continuation_and_never_skip_cut_rows() {
    for cut in [false, true] {
        let executor = Arc::new(Executor::new(vec![response(
            200,
            json!({"value":[{"name":"A"},{"name":"B"}],"continuationToken":"next+/=&"}),
        )]));
        let mut host = host(executor.clone(), None);
        let mut args = args(OntologyVerb::List);
        if cut {
            args.limit = NonZeroUsize::new(1);
        }
        let report = run(
            &args,
            true,
            &registry(),
            &mut host,
            target(OntologyVerb::List),
            limits(),
        )
        .await
        .unwrap();
        assert!(matches!(report.receipt.outcome(), Outcome::Partial { .. }));
        assert!(!report.command_succeeded());
        assert!(host.output.lock().unwrap().is_empty());
        let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
        assert_eq!(
            receipt["data"].as_array().unwrap().len(),
            if cut { 1 } else { 2 }
        );
        assert_eq!(
            receipt["meta"]["evidence"]["coverage"]["continuation"],
            if cut { Value::Null } else { json!("next+/=&") }
        );
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_acceptance_retains_private_response_without_inventing_public_handle_or_replay() {
    let mut accepted = response(202, Value::Null);
    accepted
        .headers
        .insert("Location".into(), "/private/operation/original".into());
    let executor = Arc::new(Executor::new(vec![accepted.clone()]));
    let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
    let report = run(
        &args(OntologyVerb::Move),
        true,
        &registry(),
        &mut host,
        target(OntologyVerb::Move),
        limits(),
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(report.receipt.evidence().handles.is_empty());
    let Some(CapabilityCause::Native(error)) = report.cause else {
        panic!("native response evidence required");
    };
    assert_eq!(error.response, Some(accepted));
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_output_collision_and_approval_refusal_prevent_executor_calls() {
    for collision in [false, true] {
        let executor = Arc::new(Executor::default());
        let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
        host.exists = collision;
        host.deny = !collision;
        assert!(
            run(
                &args(OntologyVerb::Move),
                true,
                &registry(),
                &mut host,
                target(OntologyVerb::Move),
                limits()
            )
            .await
            .is_err()
        );
        assert!(executor.seen.lock().unwrap().is_empty());
    }
}
