// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_ontology::{
    AdapterHost, AdapterLimits, HostError, InputSource, OntologyArguments, OntologyVerb,
    OutputDestination, bind_metadata_operations, parse_common_args, parse_facade_args,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk_items::ItemClient;
use fabric_sdk_ontology::{
    DefinitionLimits,
    v2::{
        ApprovalPolicy, Availability, Effect, ItemIdentity, OperationDescriptor, PreparedOperation,
        Registry, ResolvedTarget, Target, TargetKind, Verb, WorkspaceIdentity,
    },
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    future::{Future, ready},
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
pub(crate) const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
pub(crate) const OPERATION: &str = "99999999-8888-7777-6666-555555555555";
pub(crate) const PATH: &str = "/Demo.Workspace/Knowledge.Ontology";
pub(crate) type Events = Arc<Mutex<Vec<&'static str>>>;

pub(crate) fn arguments(verb: OntologyVerb, options: &[&str], facade: bool) -> OntologyArguments {
    let mut args = vec![verb.common_name(), PATH];
    args.extend(options);
    if facade {
        parse_facade_args(std::iter::once("ontology").chain(args)).unwrap()
    } else {
        parse_common_args(verb, args).unwrap()
    }
}

pub(crate) fn item_target(collection: bool) -> ResolvedTarget {
    let selected = Target {
        kind: if collection {
            TargetKind::WorkspaceCollection
        } else {
            TargetKind::Item
        },
        owner_type: "Ontology".into(),
        facet: "metadata".into(),
        representation: "json".into(),
        generation: if collection { None } else { Some(2) },
    };
    if collection {
        ResolvedTarget::workspace_collection(
            PATH,
            WorkspaceIdentity::new(WORKSPACE).unwrap(),
            selected,
        )
        .unwrap()
    } else {
        ResolvedTarget::new(
            PATH,
            ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
            selected,
            None,
        )
        .unwrap()
    }
}

pub(crate) fn registry() -> Registry {
    let mut registry = Registry::default();
    registry.alias("ontology", "Ontology").unwrap();
    for (id, verb, effects) in [
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
        registry.register(OperationDescriptor::new(
            id, verb,
            json!({"type":"object","description":"Offline fixture; actual metadata input is validated by the adapter."}),
            json!({"type":"object"}), effects,
        ).unwrap().with_optional_effects(vec![Effect::LocalRead, Effect::LocalWrite]).unwrap()).unwrap();
    }
    bind_metadata_operations(&mut registry).unwrap();
    registry
}

pub(crate) fn public_limits() -> AdapterLimits {
    AdapterLimits {
        input_bytes: NonZeroUsize::new(4096).unwrap(),
        data_bytes: NonZeroUsize::new(4096).unwrap(),
        definition: DefinitionLimits {
            max_parts: NonZeroUsize::new(8).unwrap(),
            max_decoded_bytes: NonZeroUsize::new(4096).unwrap(),
        },
        operation: OperationLimits {
            max_requests: NonZeroUsize::new(8).unwrap(),
            max_response_bytes: NonZeroUsize::new(8192).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        },
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
            return Err(io::Error::other("synthetic writer failure"));
        }
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct Host {
    pub(crate) target: ResolvedTarget,
    pub(crate) expected_id: &'static str,
    pub(crate) input: Option<Vec<u8>>,
    pub(crate) stdout: Arc<Mutex<Vec<u8>>>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) approved: Vec<PreparedOperation>,
    pub(crate) events: Events,
    pub(crate) refuse: bool,
    pub(crate) fail_writer: bool,
    pub(crate) fail_reservation: bool,
}
impl Host {
    pub(crate) fn new(target: ResolvedTarget, expected_id: &'static str) -> Self {
        Self {
            target,
            expected_id,
            input: None,
            stdout: Arc::default(),
            stderr: vec![],
            approved: vec![],
            events: Arc::default(),
            refuse: false,
            fail_writer: false,
            fail_reservation: false,
        }
    }
    pub(crate) fn body(&mut self, body: Value) {
        self.input = Some(serde_json::to_vec(&body).unwrap());
    }
}
impl ApprovalPolicy for Host {
    type Error = HostError;
    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), HostError> {
        assert!(self.events.lock().unwrap().contains(&"reserve"));
        if self.refuse {
            return Err(HostError("Synthetic policy refusal.".into()));
        }
        assert_eq!(prepared.operation_id(), self.expected_id);
        assert_eq!(prepared.target(), &self.target);
        assert!(
            matches!(prepared.availability(), Availability::Conditional(gates) if !gates.is_empty())
        );
        if self.events.lock().unwrap().contains(&"input") {
            assert!(prepared.effects().contains(&Effect::LocalRead));
        }
        assert_eq!(
            prepared.input()["request"]["context"]["account"],
            "offline-account"
        );
        assert_eq!(
            prepared.input()["request"]["context"]["tenant"],
            "offline-tenant"
        );
        self.events.lock().unwrap().push("approve");
        self.approved.push(prepared.clone());
        Ok(())
    }
}
impl AdapterHost for Host {
    fn resolve(&mut self, path: &str) -> Result<ResolvedTarget, HostError> {
        assert_eq!(path, PATH);
        Ok(self.target.clone())
    }
    fn read_input(
        &mut self,
        source: &InputSource,
        maximum: NonZeroUsize,
    ) -> Result<Vec<u8>, HostError> {
        self.events.lock().unwrap().push("input");
        let bytes = match source {
            InputSource::Stdin => self
                .input
                .take()
                .ok_or_else(|| HostError("Input already consumed.".into()))?,
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
        if self.fail_reservation {
            return Err(HostError("Output reservation failed.".into()));
        }
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
                .map_err(|_| HostError("Output exists or is unavailable.".into())),
        }
    }
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn deadline(&mut self, _: Option<Duration>) -> Result<(), HostError> {
        self.events.lock().unwrap().push("deadline");
        Ok(())
    }
}

#[derive(Debug, Default)]
pub(crate) struct Credential(pub(crate) AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, &[Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-metadata-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
pub(crate) fn public_client() -> (ItemClient, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    (
        ItemClient::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric)
                .unwrap(),
            EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com")
                .unwrap(),
        )),
        credential,
    )
}

#[derive(Clone)]
pub(crate) struct PublicWire {
    pub(crate) requests: Arc<Mutex<Vec<Value>>>,
    pub(crate) replies: Arc<Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>>,
    events: Events,
}
impl PublicWire {
    pub(crate) fn new(replies: Vec<OperationResponse>, events: Events) -> Self {
        Self {
            requests: Arc::default(),
            replies: Arc::new(Mutex::new(replies.into_iter().map(Ok).collect())),
            events,
        }
    }
    pub(crate) fn calls(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl OperationExecutor for PublicWire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert!(self.events.lock().unwrap().contains(&"approve"));
            assert_eq!(request.token().token.secret(), "offline-metadata-only");
            let body = if request.body().is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(request.body()).unwrap()
            };
            self.requests.lock().unwrap().push(json!({
                "method":format!("{:?}",request.method()), "url":request.url().as_str(), "body":body,
            }));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected mutation replay")
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(ready(Ok(())))
    }
}
pub(crate) fn public_response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}
pub(crate) fn accepted() -> OperationResponse {
    let mut response = public_response(202, Value::Null);
    response.headers.insert(
        "Location".into(),
        format!("https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    response
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    response.headers.insert("Retry-After".into(), "0".into());
    response
}
pub(crate) fn metadata() -> Value {
    json!({"id":ITEM,"type":"Ontology","displayName":"Knowledge","workspaceId":WORKSPACE,"properties":{"generation":2}})
}

pub(crate) struct TempRoot(PathBuf);
impl TempRoot {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-ontology-capability-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub(crate) fn input(&self) -> PathBuf {
        self.0.join("input.json")
    }
    pub(crate) fn output(&self) -> PathBuf {
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
