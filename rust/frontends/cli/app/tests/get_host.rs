// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Full GET through public hosts, real SDK/decoder/query and private in-memory I/O.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod support;

#[cfg(feature = "native-host")]
#[path = "get_host_cases/resources.rs"]
mod resource_cases;

use fabric_cli::{
    PythonQuery, ReadConfig, ReadHost,
    parser::{Invocation, ReadCommand, parse_args},
    session::{
        ReadSession,
        get_host::{DEFINITION_CONFIRMATION, GetErrorKind, GetHost, output::LocalTimestamp},
    },
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_transfer::{TransferEvent, TransferFiles, TransferFuture, TransferUi};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    DefinitionLimits, ExportStorage, GetOutputNewline,
};
use fabric_sdk::{
    Client,
    capacities::{CapacityExecutionError, CapacityExecutor, CapacityRequest, CapacityResponse},
    folders::{FolderExecutionError, FolderExecutor, FolderRequest, FolderResponse},
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    io::{self, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use support::Credential;

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    for _ in 0..128 {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
    panic!("scripted GET exceeded its finite offline poll budget")
}

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";

type Events = Arc<Mutex<Vec<String>>>;

struct Reply {
    method: ResourceMethod,
    suffix: String,
    status: u16,
    body: Value,
    headers: BTreeMap<String, String>,
}

fn reply(method: ResourceMethod, suffix: &str, body: Value) -> Reply {
    Reply {
        method,
        suffix: suffix.into(),
        status: 200,
        body,
        headers: BTreeMap::new(),
    }
}

fn get(suffix: &str, body: Value) -> Reply {
    reply(ResourceMethod::Get, suffix, body)
}

fn post(suffix: &str, body: Value) -> Reply {
    reply(ResourceMethod::Post, suffix, body)
}

struct Backend {
    replies: Mutex<VecDeque<Reply>>,
    events: Events,
}

impl Backend {
    fn new(replies: Vec<Reply>, events: Events) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            events,
        }
    }

    fn take(&self, method: ResourceMethod, url: &str) -> OperationResponse {
        self.events
            .lock()
            .unwrap()
            .push(format!("{method:?} {url}"));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        assert_eq!(method, reply.method);
        assert!(
            url.ends_with(&reply.suffix),
            "{url} expected {}",
            reply.suffix
        );
        OperationResponse {
            status: reply.status,
            body: match reply.body {
                Value::String(raw) => raw.into_bytes(),
                value => serde_json::to_vec(&value).unwrap(),
            },
            headers: reply.headers,
        }
    }

    fn assert_consumed(&self) {
        assert!(self.replies.lock().unwrap().is_empty(), "unused requests");
    }
}

impl OperationExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(self.take(request.method(), request.url().as_str())) })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.events.lock().unwrap().push(format!("wait {delay:?}"));
        Box::pin(ready(Ok(())))
    }
}

impl ItemExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.take(ResourceMethod::Get, request.url().as_str());
            Ok(ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

impl WorkspaceExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let response = self.take(ResourceMethod::Get, request.url().as_str());
            Ok(WorkspaceResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

impl FolderExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: FolderRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FolderResponse, FolderExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let response = self.take(ResourceMethod::Get, request.url().as_str());
            Ok(FolderResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

impl CapacityExecutor for Backend {
    fn execute<'a>(
        &'a self,
        _: CapacityRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CapacityResponse, CapacityExecutionError>> + Send + 'a>>
    {
        panic!("GET must not discover capacities in this fixture")
    }
}

struct Ui {
    answer: Result<bool, CommandError>,
    events: Events,
    fail_emit: bool,
}

impl Ui {
    fn new(answer: bool, events: Events) -> Self {
        Self {
            answer: Ok(answer),
            events,
            fail_emit: false,
        }
    }
}

impl TransferUi for Ui {
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError> {
        self.events.lock().unwrap().push(format!("{event:?}"));
        if self.fail_emit {
            Err(CommandError::new(
                Some("synthetic UI writer failure".into()),
                Some("OutputError".into()),
            ))
        } else {
            Ok(())
        }
    }
    fn confirm<'a>(&'a self, message: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        assert_eq!(message, DEFINITION_CONFIRMATION);
        self.events.lock().unwrap().push("confirm".into());
        Box::pin(ready(self.answer.clone()))
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        panic!("GET must never select export items")
    }
}

struct Files {
    events: Events,
    expected_name: &'static str,
    storage: ExportStorage,
    writes: Vec<(String, Vec<u8>)>,
    fail: Option<&'static str>,
    cancel_on_write: Option<(Arc<AtomicBool>, bool)>,
}

impl Files {
    fn new(events: Events) -> Self {
        Self {
            events,
            expected_name: "Book.Notebook",
            storage: ExportStorage::Local,
            writes: Vec::new(),
            fail: None,
            cancel_on_write: None,
        }
    }
    fn phase(&self, phase: &'static str) -> io::Result<()> {
        self.events.lock().unwrap().push(phase.into());
        if self.fail == Some(phase) {
            Err(io::Error::other(format!("synthetic {phase} failure")))
        } else {
            Ok(())
        }
    }
}

impl DefinitionFileSystem for Files {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        self.phase("preflight")
    }
    fn create_directory(&mut self, _: &Path, name: &str) -> io::Result<()> {
        assert_eq!(name, self.expected_name);
        self.phase("mkdir")
    }
    fn write(&mut self, _: &Path, file: &DefinitionFile) -> io::Result<()> {
        let result = self.phase("write");
        self.writes.push((
            file.path.clone(),
            if result.is_err() {
                file.bytes[..1.min(file.bytes.len())].to_vec()
            } else {
                file.bytes.clone()
            },
        ));
        if let Some((flag, _)) = &self.cancel_on_write {
            flag.store(true, Ordering::SeqCst);
        }
        result
    }
}

impl DefinitionImportFileSystem for Files {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        panic!("no import")
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        panic!("no import")
    }
}

impl TransferFiles for Files {
    fn resolve_directory(&mut self, _: &str) -> io::Result<PathBuf> {
        self.phase("resolve output")?;
        Ok(PathBuf::from("private-fixture"))
    }
    fn is_empty(&mut self, _: &Path) -> io::Result<bool> {
        panic!("GET does not require an empty output directory")
    }
    fn export_storage(&self, _: &Path) -> ExportStorage {
        self.storage
    }
    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if let Some((flag, true)) = &self.cancel_on_write {
                flag.store(true, Ordering::SeqCst);
                self.events.lock().unwrap().push("write pending".into());
                return poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::<io::Result<()>>::Pending
                })
                .await;
            }
            self.write(root, &file)
        })
    }
}

struct Fixture {
    sdk: Client,
    backend: Backend,
    catalog: Catalog,
    root: Arc<Element>,
}

impl Fixture {
    fn new(kind: &str, replies: Vec<Reply>, events: Events) -> Self {
        let root = Element::tenant("Tenant", "offline-tenant");
        let workspace = Element::child(
            "Demo",
            Some(WS.into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            root.clone(),
        )
        .unwrap();
        let item = Element::child(
            "Book",
            Some(ITEM.into()),
            NodeKind::Item(ItemType::parse(kind).unwrap()),
            workspace.clone(),
        )
        .unwrap();
        let folder = Element::child(
            "Folder",
            Some(OP.into()),
            NodeKind::Folder,
            workspace.clone(),
        )
        .unwrap();
        Self {
            sdk: Client::new(
                Arc::new(Credential::default()),
                fabric_sdk::CredentialSelection::new(
                    "offline",
                    "offline-tenant",
                    fabric_sdk::Audience::Fabric,
                )
                .unwrap(),
            ),
            backend: Backend::new(replies, events),
            catalog: Catalog::new(vec![workspace, item, folder]),
            root,
        }
    }

    fn reads(&self) -> ReadHost<'_> {
        ReadHost {
            sdk: &self.sdk,
            workspaces: &self.backend,
            items: &self.backend,
            capacities: &self.backend,
            lookup: &self.catalog,
            current: self.root.clone(),
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
            projection: &PythonQuery,
        }
    }

    fn host<'a>(&'a self, ui: &'a Ui, files: Option<&'a mut Files>) -> GetHost<'a> {
        GetHost {
            operations: &self.backend,
            ui,
            files: files.map(|files| files as &mut dyn TransferFiles),
            local_timestamp: LocalTimestamp::parse("20000102_030405").unwrap(),
            newline: GetOutputNewline::Lf,
            definition_limits: DefinitionLimits::default(),
            operation_limits: OperationLimits::default(),
        }
    }
}

fn command(kind: &str, query: Option<&str>, output: Option<&str>, force: bool) -> ReadCommand {
    let path = format!("Demo.Workspace/Book.{kind}");
    let mut args = vec!["fab-rs", "get", &path];
    if let Some(query) = query {
        args.extend(["--query", query]);
    }
    if let Some(output) = output {
        args.extend(["--output", output]);
    }
    if force {
        args.push("--force");
    }
    let Invocation::Read(command) = parse_args(args).unwrap() else {
        panic!("GET")
    };
    command
}

fn metadata(collection: &str) -> Reply {
    get(
        &format!("/{collection}/{ITEM}"),
        json!({"id":ITEM,"displayName":"Book","description":"","properties":{"nested":false}}),
    )
}

fn definition() -> Reply {
    // {"cells":[{"outputs":[1],"source":["café"]}]} with a retained ordinary field.
    post(
        &format!("/items/{ITEM}/getDefinition?format=ipynb"),
        json!({
            "definition": {"parts":[{"path":"notebook-content.ipynb","payloadType":"InlineBase64",
                "payload":"eyJjZWxscyI6W3sib3V0cHV0cyI6WzFdLCJzb3VyY2UiOlsiY2Fmw6kiXX1dfQ=="}]}
        }),
    )
}

fn connections() -> Reply {
    get(
        &format!("/items/{ITEM}/connections"),
        json!({"value":[{"id":"connection"}]}),
    )
}

fn schedules() -> Reply {
    get(
        &format!("/items/{ITEM}/jobs/RunNotebook/schedules"),
        json!({"value":[{"id":"schedule"}]}),
    )
}

#[test]
fn source_definition_prompt_precedes_specialized_metadata_and_real_decode_enrichment_projection() {
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events.clone());
    let outcome = complete(fixture.reads().execute_get(
        &command("Notebook", Some("."), None, false),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    fixture.backend.assert_consumed();
    let data = outcome.projected.unwrap();
    assert_eq!(
        data["definition"]["parts"][0]["payload"]["cells"][0]["outputs"],
        json!([])
    );
    assert_eq!(
        data["definition"]["parts"][0]["payload"]["cells"][0]["source"],
        json!(["café"])
    );
    assert_eq!(
        data["definition"]["parts"][0]["payloadType"],
        "DecodeBase64"
    );
    assert_eq!(data["connections"], json!([{"id":"connection"}]));
    assert_eq!(data["schedules"], json!([{"id":"schedule"}]));
    assert_eq!(events.lock().unwrap()[0], "confirm");
    assert!(events.lock().unwrap()[1].contains("/notebooks/"));
    assert_eq!(outcome.evidence.operations.len(), 3);
}

#[test]
fn no_query_discovers_paths_in_the_decoded_value_not_just_metadata() {
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events);
    let outcome = complete(fixture.reads().execute_get(
        &command("Notebook", None, None, false),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    let paths = outcome.projected.unwrap();
    assert!(
        paths
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().unwrap().starts_with("definition.parts"))
    );
    assert!(
        paths
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().unwrap().starts_with("connections"))
    );
    fixture.backend.assert_consumed();
}

#[test]
fn metadata_queries_bypass_definition_consent_but_keep_source_enrichment_and_verbose_is_inert() {
    for (force, verbose) in [(false, false), (true, false), (false, true), (true, true)] {
        let events = Events::default();
        let fixture = Fixture::new(
            "Notebook",
            vec![metadata("notebooks"), connections(), schedules()],
            events.clone(),
        );
        let ui = Ui::new(false, events.clone());
        let mut selected = command("Notebook", Some("id"), None, force);
        selected.verbose = verbose;
        let outcome = complete(fixture.reads().execute_get(
            &selected,
            &mut fixture.host(&ui, None),
            pending(),
        ))
        .unwrap();
        assert_eq!(outcome.projected, Some(json!(ITEM)));
        assert_eq!(outcome.evidence.operations.len(), 2);
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "confirm" || event.starts_with("Warning"))
        );
        fixture.backend.assert_consumed();
    }
}

#[test]
fn force_warns_before_retrieval_and_decline_eof_ui_failure_are_not_retrieval() {
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(false, events.clone());
    complete(fixture.reads().execute_get(
        &command("Notebook", Some("."), None, true),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    assert!(events.lock().unwrap()[0].starts_with("Warning"));
    fixture.backend.assert_consumed();
    for mode in ["decline", "eof", "writer"] {
        let events = Events::default();
        let fixture = Fixture::new("Notebook", vec![], events.clone());
        let mut ui = Ui::new(false, events.clone());
        if mode == "eof" {
            ui.answer = Err(CommandError::new(
                Some("EOF".into()),
                Some("InputError".into()),
            ));
        }
        ui.fail_emit = mode == "writer";
        let result = complete(fixture.reads().execute_get(
            &command("Notebook", Some("."), Some("output"), mode == "writer"),
            &mut fixture.host(&ui, None),
            pending(),
        ));
        if mode == "decline" {
            let result = result.unwrap();
            assert!(result.evidence.declined);
            assert!(result.output.is_none());
            assert!(result.projected.is_none());
        } else {
            assert!(matches!(result.unwrap_err().kind, GetErrorKind::Command(_)));
        }
        fixture.backend.assert_consumed();
        assert_eq!(events.lock().unwrap().len(), 1);
    }
}

#[test]
fn get_unsupported_types_fail_and_ml_types_use_specialized_metadata_without_definition() {
    for kind in ["Dashboard", "SQLEndpoint", "AISkill"] {
        let events = Events::default();
        let fixture = Fixture::new(kind, vec![], events.clone());
        let ui = Ui::new(true, events.clone());
        assert!(
            complete(fixture.reads().execute_get(
                &command(kind, Some("."), None, true),
                &mut fixture.host(&ui, None),
                pending()
            ))
            .is_err()
        );
        assert!(events.lock().unwrap().is_empty());
    }
    for (kind, collection) in [("MLModel", "mlModels"), ("MLExperiment", "mlExperiments")] {
        let events = Events::default();
        let fixture = Fixture::new(
            kind,
            vec![metadata(collection), connections()],
            events.clone(),
        );
        let ui = Ui::new(false, events.clone());
        let result = complete(fixture.reads().execute_get(
            &command(kind, Some("."), None, false),
            &mut fixture.host(&ui, None),
            pending(),
        ))
        .unwrap();
        assert!(result.projected.unwrap().get("definition").is_none());
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "confirm")
        );
        fixture.backend.assert_consumed();
    }
}

#[test]
fn projected_output_not_envelope_is_written_even_when_falsey_with_source_scalar_spelling() {
    for (query, expected, suffix) in [
        ("`[]`", b"[]".as_slice(), ".json"),
        ("`{}`", b"{}".as_slice(), ".json"),
        ("description", b"".as_slice(), ""),
        ("properties.nested", b"False".as_slice(), ""),
        ("`0`", b"0".as_slice(), ".json"),
        ("`null`", b"None".as_slice(), ""),
    ] {
        let events = Events::default();
        let metadata_query = query.starts_with("description") || query.starts_with("properties");
        let mut replies = vec![metadata("notebooks")];
        if !metadata_query {
            replies.push(definition());
        }
        replies.extend([connections(), schedules()]);
        let fixture = Fixture::new("Notebook", replies, events.clone());
        let ui = Ui::new(true, events.clone());
        let mut files = Files::new(events);
        let result = complete(fixture.reads().execute_get(
            &command("Notebook", Some(query), Some("output"), true),
            &mut fixture.host(&ui, Some(&mut files)),
            pending(),
        ))
        .unwrap();
        assert_eq!(
            files.writes,
            vec![(
                format!("Book.Notebook/get_20000102_030405{suffix}"),
                expected.to_vec()
            )]
        );
        assert_eq!(
            result.output.unwrap().message.as_deref(),
            Some("Export completed")
        );
        assert_eq!(result.evidence.files.completed_paths.len(), 1);
        fixture.backend.assert_consumed();
    }
}

#[test]
fn file_output_uses_decoded_projected_values_and_explicit_windows_newlines() {
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events.clone());
    let mut files = Files::new(events);
    let mut host = fixture.host(&ui, Some(&mut files));
    host.newline = GetOutputNewline::CrLf;
    let result = complete(fixture.reads().execute_get(
        &command(
            "Notebook",
            Some("definition.parts[0].payload.cells[0].source"),
            Some("output"),
            false,
        ),
        &mut host,
        pending(),
    ))
    .unwrap();
    drop(host);
    assert_eq!(files.writes[0].1, "[\r\n    \"café\"\r\n]".as_bytes());
    assert_eq!(result.projected, Some(json!(["café"])));
}

#[test]
fn lakehouse_policy_uses_ascii_single_suffix_and_never_uses_local_serialization() {
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events.clone());
    let mut files = Files::new(events);
    files.storage = ExportStorage::Lakehouse;
    let result = complete(fixture.reads().execute_get(
        &command(
            "Notebook",
            Some("definition.parts[0].payload.cells[0].source"),
            Some("lakehouse"),
            false,
        ),
        &mut fixture.host(&ui, Some(&mut files)),
        pending(),
    ))
    .unwrap();
    assert_eq!(
        files.writes,
        vec![(
            "Book.Notebook/get_20000102_030405.json".into(),
            br#"["caf\u00e9"]"#.to_vec()
        )]
    );
    assert_eq!(result.evidence.files.completed_paths.len(), 1);
}

#[test]
fn file_errors_and_cancellation_retain_completed_directory_and_partial_write_without_replay() {
    for failing in ["resolve output", "preflight", "mkdir", "write"] {
        let events = Events::default();
        let fixture = Fixture::new(
            "Notebook",
            vec![metadata("notebooks"), connections(), schedules()],
            events.clone(),
        );
        let ui = Ui::new(true, events.clone());
        let mut files = Files::new(events);
        files.fail = Some(failing);
        let error = complete(fixture.reads().execute_get(
            &command("Notebook", Some("properties"), Some("output"), false),
            &mut fixture.host(&ui, Some(&mut files)),
            pending(),
        ))
        .unwrap_err();
        assert!(matches!(error.kind, GetErrorKind::Definition(_)));
        assert_eq!(
            error.evidence.files.incomplete_path.is_some(),
            failing == "write"
        );
        assert_eq!(
            error.evidence.files.completed_directory.is_some(),
            failing == "write"
        );
        assert!(error.evidence.files.completed_paths.is_empty());
        assert_eq!(files.writes.len(), usize::from(failing == "write"));
        fixture.backend.assert_consumed();
    }
    for pending_write in [false, true] {
        let events = Events::default();
        let fixture = Fixture::new(
            "Notebook",
            vec![metadata("notebooks"), connections(), schedules()],
            events.clone(),
        );
        let ui = Ui::new(true, events.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let mut files = Files::new(events);
        files.cancel_on_write = Some((flag.clone(), pending_write));
        let signal = poll_fn(move |_| {
            if flag.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let error = complete(fixture.reads().execute_get(
            &command("Notebook", Some("properties"), Some("output"), false),
            &mut fixture.host(&ui, Some(&mut files)),
            signal,
        ))
        .unwrap_err();
        assert_eq!(
            error.evidence.files.completed_paths.len(),
            usize::from(!pending_write)
        );
        assert_eq!(
            error.evidence.files.incomplete_path.is_some(),
            pending_write
        );
        fixture.backend.assert_consumed();
    }
}

#[test]
fn definition_denial_preserves_source_metadata_fallback_and_denial_receipt() {
    for status in [401, 403] {
        let events = Events::default();
        let mut denied = definition();
        denied.status = status;
        denied.body = json!({"error":{"code":"Denied","message":"offline denial"}});
        let fixture = Fixture::new(
            "Notebook",
            vec![metadata("notebooks"), denied, connections(), schedules()],
            events.clone(),
        );
        let ui = Ui::new(true, events);
        let outcome = complete(fixture.reads().execute_get(
            &command("Notebook", Some("."), None, false),
            &mut fixture.host(&ui, None),
            pending(),
        ))
        .unwrap();
        assert_eq!(outcome.evidence.definition_access_denied, Some(status));
        assert_eq!(outcome.evidence.enrichment_failures.len(), 1);
        assert!(outcome.projected.unwrap().get("definition").is_none());
        assert!(outcome.evidence.operations[0].mutation_may_have_occurred);
    }
}

#[test]
fn accepted_definition_reuses_shared_reconciliation_and_incomplete_acceptance_is_not_success() {
    for has_operation in [false, true] {
        let events = Events::default();
        let mut accepted = definition();
        accepted.status = 202;
        accepted.body = json!({});
        if has_operation {
            accepted
                .headers
                .insert("x-ms-operation-id".into(), OP.into());
            accepted.headers.insert("retry-after".into(), "0".into());
        }
        let mut replies = vec![metadata("notebooks"), accepted];
        if has_operation {
            let mut completed = get(&format!("/operations/{OP}"), json!({"status":"Succeeded"}));
            completed.headers.insert(
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
            );
            replies.extend([
                completed,
                get(&format!("/operations/{OP}/result"), definition().body),
                connections(),
                schedules(),
            ]);
        }
        let fixture = Fixture::new("Notebook", replies, events.clone());
        let ui = Ui::new(true, events.clone());
        let result = complete(fixture.reads().execute_get(
            &command("Notebook", Some("."), None, false),
            &mut fixture.host(&ui, None),
            pending(),
        ));
        if has_operation {
            assert_eq!(
                result.unwrap().evidence.operations[0]
                    .operation_id
                    .as_deref(),
                Some(OP)
            );
        } else {
            assert!(matches!(
                result.unwrap_err().kind,
                GetErrorKind::Definition(_)
            ));
        }
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.starts_with("Post "))
                .count(),
            1
        );
        fixture.backend.assert_consumed();
    }
}

#[test]
fn decoded_payload_type_is_preserved_through_identity_and_scalar_projection() {
    for (query, expected) in [
        (".", json!("DecodeBase64")),
        ("definition.parts[0].payloadType", json!("DecodeBase64")),
    ] {
        let events = Events::default();
        let fixture = Fixture::new(
            "Notebook",
            vec![
                metadata("notebooks"),
                definition(),
                connections(),
                schedules(),
            ],
            events.clone(),
        );
        let ui = Ui::new(true, events);
        let outcome = complete(fixture.reads().execute_get(
            &command("Notebook", Some(query), None, false),
            &mut fixture.host(&ui, None),
            pending(),
        ))
        .unwrap();
        let projected = outcome.projected.unwrap();
        let actual = if query == "." {
            &projected["definition"]["parts"][0]["payloadType"]
        } else {
            &projected
        };
        assert_eq!(actual, &expected);
        fixture.backend.assert_consumed();
    }
}

#[test]
fn metadata_property_queries_preserve_embedded_payloads_without_consent_or_decoding() {
    let events = Events::default();
    let mut metadata = metadata("notebooks");
    metadata.body["properties"]["definition"] = definition().body;
    let expected = metadata.body["properties"]["definition"].clone();
    let fixture = Fixture::new(
        "Notebook",
        vec![metadata, connections(), schedules()],
        events.clone(),
    );
    let ui = Ui::new(false, events.clone());
    let outcome = complete(fixture.reads().execute_get(
        &command("Notebook", Some("properties.definition"), None, false),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    assert_eq!(outcome.projected, Some(expected));
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|event| { event == "confirm" || event.starts_with("Post ") })
    );
    fixture.backend.assert_consumed();
}

#[test]
fn projected_scalar_types_control_source_filename_suffix_and_falsey_stdout() {
    for (query, expected, suffix, stdout) in [
        ("`[]`", b"[]".as_slice(), ".json", false),
        ("`{}`", b"{}".as_slice(), ".json", false),
        ("description", b"".as_slice(), "", false),
        ("properties.nested", b"False".as_slice(), "", true),
        ("`0`", b"0".as_slice(), ".json", true),
        ("`false`", b"False".as_slice(), "", true),
        ("`null`", b"None".as_slice(), "", true),
        ("`\"false\"`", b"false".as_slice(), ".json", true),
    ] {
        for destination in [None, Some("output")] {
            let events = Events::default();
            let mut replies = vec![metadata("notebooks")];
            if !fabric_cli::session::get_options::is_metadata_property_query(query) {
                replies.push(definition());
            }
            replies.extend([connections(), schedules()]);
            let fixture = Fixture::new("Notebook", replies, events.clone());
            let ui = Ui::new(true, events.clone());
            let mut files = Files::new(events.clone());
            let result = complete(fixture.reads().execute_get(
                &command("Notebook", Some(query), destination, true),
                &mut fixture.host(&ui, Some(&mut files)),
                pending(),
            ))
            .unwrap();
            assert_eq!(result.output.is_some(), destination.is_some() || stdout);
            if destination.is_some() {
                let path = format!("Book.Notebook/get_20000102_030405{suffix}");
                assert_eq!(files.writes, vec![(path.clone(), expected.to_vec())]);
                assert_eq!(result.evidence.files.completed_paths, vec![path.clone()]);
                assert!(events.lock().unwrap().iter().any(|event| {
                    event.contains("Exporting result to") && event.contains(&path)
                }));
            } else {
                assert!(files.writes.is_empty());
                assert!(result.evidence.files.completed_paths.is_empty());
            }
            fixture.backend.assert_consumed();
        }
    }
}

#[test]
fn completed_definition_without_result_location_keeps_initial_body_and_never_fetches_result() {
    let events = Events::default();
    let mut accepted = definition();
    accepted.status = 202;
    accepted
        .headers
        .insert("x-ms-operation-id".into(), OP.into());
    accepted.headers.insert("retry-after".into(), "0".into());
    let fixture = Fixture::new(
        "Notebook",
        vec![
            metadata("notebooks"),
            accepted,
            get(&format!("/operations/{OP}"), json!({"status":"Completed"})),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events.clone());
    let outcome = complete(fixture.reads().execute_get(
        &command(
            "Notebook",
            Some("definition.parts[0].payloadType"),
            None,
            false,
        ),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    assert_eq!(outcome.projected, Some(json!("DecodeBase64")));
    assert_eq!(
        outcome.evidence.operations[0].operation_id.as_deref(),
        Some(OP)
    );
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("Post "))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| event.contains("/result")));
    fixture.backend.assert_consumed();
}

#[test]
fn cancellation_after_definition_acceptance_retains_operation_and_never_replays_or_enriches() {
    let events = Events::default();
    let mut accepted = definition();
    accepted.status = 202;
    accepted.body = json!({});
    accepted
        .headers
        .insert("x-ms-operation-id".into(), OP.into());
    accepted.headers.insert("retry-after".into(), "0".into());
    let fixture = Fixture::new(
        "Notebook",
        vec![metadata("notebooks"), accepted],
        events.clone(),
    );
    let ui = Ui::new(true, events.clone());
    let signal_events = events.clone();
    let cancelled = poll_fn(move |_| {
        if signal_events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.starts_with("wait "))
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let failure = complete(fixture.reads().execute_get(
        &command("Notebook", Some("."), None, false),
        &mut fixture.host(&ui, None),
        cancelled,
    ))
    .unwrap_err();
    let GetErrorKind::Definition(error) = failure.kind else {
        panic!("cancelled definition")
    };
    assert!(matches!(
        error.kind,
        fabric_deploy::definitions::DefinitionErrorKind::Operation(operation)
            if matches!(operation.kind, fabric_sdk_operations::OperationErrorKind::Cancelled)
    ));
    assert_eq!(
        failure.evidence.operations[0].operation_id.as_deref(),
        Some(OP)
    );
    assert!(failure.evidence.operations[0].mutation_may_have_occurred);
    assert!(failure.evidence.files.completed_paths.is_empty());
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.starts_with("Post "))
            .count(),
        1
    );
    fixture.backend.assert_consumed();
}

#[test]
fn binary_wide_and_nonfinite_decoded_values_fail_before_projection_or_files() {
    for (payload, path) in [
        ("/w==", "binary.bin"),
        ("MTg0NDY3NDQwNzM3MDk1NTE2MTY=", "wide.json"),
        ("TmFO", "nan.json"),
    ] {
        let events = Events::default();
        let mut value = definition();
        value.body = json!({"definition":{"parts":[{"path":path,"payloadType":"InlineBase64","payload":payload}]}});
        // Report avoids Notebook-specific cleaning so the domain check is isolated.
        value.suffix = format!("/items/{ITEM}/getDefinition");
        let fixture = Fixture::new("Report", vec![metadata("reports"), value], events.clone());
        let ui = Ui::new(true, events.clone());
        let mut files = Files::new(events);
        let error = complete(fixture.reads().execute_get(
            &command("Report", Some("."), Some("output"), true),
            &mut fixture.host(&ui, Some(&mut files)),
            pending(),
        ))
        .unwrap_err();
        assert!(matches!(error.kind, GetErrorKind::Codec(_)));
        assert!(files.writes.is_empty());
        assert_eq!(error.evidence.operations.len(), 1);
        fixture.backend.assert_consumed();
    }
}

#[test]
fn environment_and_mirroring_source_enrichment_sequences_have_distinct_failure_shapes() {
    let events = Events::default();
    let mut unavailable = get(
        "/staging/libraries",
        json!({"error":{"code":"Denied","message":"offline"}}),
    );
    unavailable.status = 403;
    let mut ignored = get("/sparkcompute", json!({}));
    ignored.status = 204;
    let fixture = Fixture::new(
        "Environment",
        vec![
            metadata("environments"),
            post(
                &format!("/items/{ITEM}/getDefinition"),
                json!({"definition":{"parts":[]}}),
            ),
            connections(),
            get("/libraries", json!({"published":true})),
            unavailable,
            ignored,
            get("/staging/sparkcompute", Value::String("malformed".into())),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events);
    let result = complete(fixture.reads().execute_get(
        &command("Environment", Some("."), None, true),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    let value = result.projected.unwrap();
    assert_eq!(value["published"], json!({"libraries":{"published":true}}));
    assert_eq!(
        value["staging"],
        json!({"libraries":[],"sparkComputeSettings":[]})
    );
    assert_eq!(result.evidence.enrichment_failures.len(), 2);
    fixture.backend.assert_consumed();
    let events = Events::default();
    let fixture = Fixture::new(
        "MirroredDatabase",
        vec![
            metadata("mirroredDatabases"),
            post(
                &format!("/items/{ITEM}/getDefinition"),
                json!({"definition":{"parts":[]}}),
            ),
            connections(),
            post("/getMirroringStatus", json!({"state":"Running"})),
            post("/getTablesMirroringStatus", json!({"value":[]})),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events);
    let result = complete(fixture.reads().execute_get(
        &command("MirroredDatabase", Some("."), None, true),
        &mut fixture.host(&ui, None),
        pending(),
    ))
    .unwrap();
    assert_eq!(
        result.projected.as_ref().unwrap()["status"],
        json!({"state":"Running"})
    );
    assert_eq!(result.projected.unwrap()["tablesStatus"], json!([]));
    assert_eq!(
        result
            .evidence
            .operations
            .iter()
            .filter(|receipt| receipt.mutation_may_have_occurred)
            .count(),
        3
    );
    fixture.backend.assert_consumed();
}

#[test]
fn invalid_query_missing_file_host_empty_output_invalid_format_and_precancel_are_truthful() {
    for query in ["[", "properties.["] {
        let events = Events::default();
        let metadata_only = query.starts_with("properties");
        let mut replies = vec![metadata("notebooks")];
        if !metadata_only {
            replies.push(definition());
        }
        replies.extend([connections(), schedules()]);
        let fixture = Fixture::new("Notebook", replies, events.clone());
        let ui = Ui::new(true, events);
        assert!(
            complete(fixture.reads().execute_get(
                &command("Notebook", Some(query), None, false),
                &mut fixture.host(&ui, None),
                pending()
            ))
            .is_err()
        );
        fixture.backend.assert_consumed();
    }
    for destination in [None, Some(""), Some("output")] {
        let events = Events::default();
        let fixture = Fixture::new(
            "Notebook",
            vec![metadata("notebooks"), connections(), schedules()],
            events.clone(),
        );
        let ui = Ui::new(true, events);
        let result = complete(fixture.reads().execute_get(
            &command("Notebook", Some("id"), destination, false),
            &mut fixture.host(&ui, None),
            pending(),
        ));
        assert_eq!(result.is_err(), destination == Some("output"));
        fixture.backend.assert_consumed();
    }
    let events = Events::default();
    let fixture = Fixture::new("Notebook", vec![], events.clone());
    let ui = Ui::new(true, events.clone());
    let mut invalid = command("Notebook", Some("."), None, false);
    invalid.output_format = Some("invalid".into());
    assert!(
        complete(
            fixture
                .reads()
                .execute_get(&invalid, &mut fixture.host(&ui, None), pending())
        )
        .is_err()
    );
    assert!(
        complete(fixture.reads().execute_get(
            &command("Notebook", Some("."), None, false),
            &mut fixture.host(&ui, None),
            ready(())
        ))
        .is_err()
    );
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn real_session_discovery_selects_new_definition_path_and_writer_failure_keeps_receipts() {
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("broken pipe"))
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("must not flush")
        }
    }
    let events = Events::default();
    let fixture = Fixture::new(
        "Notebook",
        vec![
            get(
                "/v1/workspaces",
                json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
            ),
            get(
                &format!("/workspaces/{WS}/items"),
                json!({"value":[{"id":ITEM,"displayName":"Book","type":"Notebook"}]}),
            ),
            metadata("notebooks"),
            definition(),
            connections(),
            schedules(),
        ],
        events.clone(),
    );
    let ui = Ui::new(true, events);
    let session = ReadSession {
        sdk: &fixture.sdk,
        executor: &fixture.backend,
        tenant: fixture.root.clone(),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let dispatch = complete(session.dispatch_get(
        &command("Notebook", Some("."), None, false),
        &mut fixture.host(&ui, None),
        pending(),
    ));
    assert!(dispatch.result.is_ok());
    assert_eq!(dispatch.plan.exit_code, 0);
    let error = dispatch
        .write_to(&mut BrokenWriter, &mut Vec::new())
        .unwrap_err();
    assert_eq!(error.dispatch.result.unwrap().evidence.operations.len(), 3);
    fixture.backend.assert_consumed();
}

#[test]
fn local_timestamp_requires_a_valid_explicit_wall_clock_name() {
    for timestamp in [
        "",
        "../20000102_030405",
        "20000102_03040",
        "20000102_030405x",
        "20000230_030405",
        "20000102_240000",
    ] {
        assert!(LocalTimestamp::parse(timestamp).is_err(), "{timestamp}");
    }
    for timestamp in ["20000102_030405", "20000229_235959", "20261029_230000"] {
        assert_eq!(
            LocalTimestamp::parse(timestamp).unwrap().as_str(),
            timestamp
        );
    }
}
