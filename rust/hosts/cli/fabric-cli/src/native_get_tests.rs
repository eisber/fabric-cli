// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests of the mounted native GET, its control signal and final writer.

#![cfg(any(windows, feature = "headless"))]
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    parser::{Invocation, parse_args},
    session::get_host::GetOutcome,
};
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const WORKSPACE: &str = "22222222-2222-2222-2222-222222222222";
const ITEM: &str = "33333333-3333-3333-3333-333333333333";
const LAKE: &str = "44444444-4444-4444-4444-444444444444";
const OPERATION: &str = "55555555-5555-5555-5555-555555555555";
type Events = Arc<Mutex<Vec<String>>>;

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).into()).collect());
        Ok(AccessToken::new(
            "offline-native-get",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Reply {
    method: Method,
    suffix: String,
    status: StatusCode,
    body: Value,
    headers: Headers,
    hold: bool,
}

fn reply(method: Method, suffix: impl Into<String>, body: Value) -> Reply {
    Reply {
        method,
        suffix: suffix.into(),
        status: StatusCode::Ok,
        body,
        headers: Headers::new(),
        hold: false,
    }
}

fn get(suffix: impl Into<String>, body: Value) -> Reply {
    reply(Method::Get, suffix, body)
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    events: Events,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected GET HTTP");
        assert_eq!(request.method(), reply.method);
        assert!(
            request.url().as_str().ends_with(&reply.suffix),
            "{} expected {}",
            request.url(),
            reply.suffix,
        );
        self.events
            .lock()
            .unwrap()
            .push(format!("http {}", reply.suffix));
        if reply.hold {
            pending::<()>().await;
        }
        Ok(AsyncRawResponse::from_bytes(
            reply.status,
            reply.headers,
            serde_json::to_vec(&reply.body).unwrap(),
        ))
    }
}

fn discovery() -> Vec<Reply> {
    vec![
        get(
            "/v1/workspaces",
            json!({"value":[{"id":WORKSPACE,"displayName":"Demo","type":"Workspace"}]}),
        ),
        get(
            format!("/workspaces/{WORKSPACE}/items"),
            json!({"value":[
                {"id":ITEM,"displayName":"Book","type":"Notebook"},
                {"id":LAKE,"displayName":"Lake","type":"Lakehouse"},
            ]}),
        ),
    ]
}

fn acquisition(definition: bool) -> Vec<Reply> {
    let mut replies = discovery();
    replies.push(get(
        format!("/notebooks/{ITEM}"),
        json!({
            "id":ITEM,"displayName":"Book","properties":{"zero":0,"nested":false},
        }),
    ));
    if definition {
        replies.push(reply(
            Method::Post,
            format!("/items/{ITEM}/getDefinition?format=ipynb"),
            json!({"definition":{"parts":[{
                "path":"notebook-content.ipynb", "payloadType":"InlineBase64",
                "payload":"eyJjZWxscyI6W3sib3V0cHV0cyI6WzFdLCJzb3VyY2UiOlsiY2Fmw6kiXX1dfQ==",
            }]}}),
        ));
    }
    replies.extend([
        get(format!("/items/{ITEM}/connections"), json!({"value":[]})),
        get(
            format!("/items/{ITEM}/jobs/RunNotebook/schedules"),
            json!({"value":[]}),
        ),
    ]);
    replies
}

fn lakehouse_replies() -> Vec<Reply> {
    let mut replies = acquisition(false);
    replies.extend(discovery());
    let mut create = reply(Method::Put, "?resource=file", json!(null));
    create.status = StatusCode::Created;
    let mut append = reply(Method::Patch, "?action=append&position=0", json!(null));
    append.status = StatusCode::Accepted;
    replies.extend([
        create,
        append,
        reply(Method::Patch, "?action=flush&position=1", json!(null)),
    ]);
    replies
}

fn command(query: Option<&str>, output: Option<&str>, force: bool) -> ReadCommand {
    let mut args = vec!["fab-rs", "get", "Demo.Workspace/Book.Notebook"];
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
        panic!("GET command")
    };
    command
}

struct Ui {
    events: Events,
    answer: Result<bool, CommandError>,
    fail_emit: bool,
}

impl Ui {
    fn new(events: Events) -> Self {
        Self {
            events,
            answer: Ok(true),
            fail_emit: false,
        }
    }
}

impl TransferUi for Ui {
    fn emit(&self, event: fabric_cli_transfer::TransferEvent) -> Result<(), CommandError> {
        self.events.lock().unwrap().push(format!("ui {event:?}"));
        if self.fail_emit {
            return Err(CommandError::new(
                Some("offline progress failure".into()),
                Some("LocalIOError".into()),
            ));
        }
        Ok(())
    }
    fn confirm<'a>(&'a self, question: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        assert_eq!(
            question,
            fabric_cli::session::get_host::DEFINITION_CONFIRMATION
        );
        self.events.lock().unwrap().push("confirm".into());
        Box::pin(async { self.answer.clone() })
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        panic!("GET never selects items")
    }
}

struct Clock {
    events: Events,
    fail: bool,
}

impl GetOutputClock for Clock {
    fn local_timestamp(&self) -> TransferFuture<'_, Result<LocalTimestamp, CommandError>> {
        self.events.lock().unwrap().push("clock".into());
        Box::pin(async move {
            if self.fail {
                Err(CommandError::new(
                    Some("offline local clock failure".into()),
                    Some("ClockFailed".into()),
                ))
            } else {
                LocalTimestamp::parse("20261030_211129")
            }
        })
    }
}

struct MemoryFiles {
    events: Events,
    writes: Vec<(String, Vec<u8>)>,
    fail_resolve: bool,
    cancel_on_write: Option<(tokio::sync::watch::Sender<bool>, bool)>,
}

impl MemoryFiles {
    fn new(events: Events) -> Self {
        Self {
            events,
            writes: Vec::new(),
            fail_resolve: false,
            cancel_on_write: None,
        }
    }
}

impl DefinitionFileSystem for MemoryFiles {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        self.events.lock().unwrap().push("preflight".into());
        Ok(())
    }
    fn create_directory(&mut self, _: &Path, name: &str) -> io::Result<()> {
        self.events.lock().unwrap().push(format!("mkdir {name}"));
        Ok(())
    }
    fn write(&mut self, _: &Path, file: &DefinitionFile) -> io::Result<()> {
        self.events.lock().unwrap().push("write".into());
        self.writes.push((file.path.clone(), file.bytes.clone()));
        Ok(())
    }
}

impl DefinitionImportFileSystem for MemoryFiles {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        panic!("not import")
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        panic!("not import")
    }
}

impl TransferFiles for MemoryFiles {
    fn resolve_directory(&mut self, _: &str) -> io::Result<PathBuf> {
        self.events.lock().unwrap().push("resolve local".into());
        if self.fail_resolve {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(PathBuf::from("offline-output"))
    }
    fn is_empty(&mut self, _: &Path) -> io::Result<bool> {
        panic!("GET does not test emptiness")
    }
    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if let Some((signal, complete)) = &self.cancel_on_write {
                let complete = *complete;
                signal.send(true).unwrap();
                if !complete {
                    pending::<()>().await;
                }
            }
            self.write(root, &file)
        })
    }
}

struct Fixture {
    wire: Arc<Wire>,
    credential: Arc<Credential>,
    events: Events,
}

impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        let events = Events::default();
        Self {
            wire: Arc::new(Wire {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
                events: events.clone(),
            }),
            credential: Arc::new(Credential::default()),
            events,
        }
    }

    async fn execute(
        &self,
        command: &ReadCommand,
        ui: &Ui,
        files: Option<&mut dyn TransferFiles>,
        clock: &Clock,
        cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<Delivery, Failure> {
        self.execute_with_files(command, ui, Files::injected(files), clock, cancelled)
            .await
    }

    async fn execute_with_files(
        &self,
        command: &ReadCommand,
        ui: &Ui,
        mut files: Files<'_>,
        clock: &Clock,
        cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<Delivery, Failure> {
        let sdk = Client::new(
            self.credential.clone(),
            CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
        );
        let http =
            HttpExecutor::new(self.wire.clone(), Arc::new(TokioTimer), Default::default()).unwrap();
        let mut state = super::super::BatchState::capture();
        let mut evidence = GetEvidence::default();
        super::super::execute_authenticated_injected(
            super::super::NativeCommand::Read(command),
            sdk,
            http,
            "/",
            fabric_cli_context::HomeContext::Unspecified,
            super::super::settings::NativeSettings::select(None).unwrap(),
            &mut state,
            false,
            Some(Inputs {
                evidence: &mut evidence,
                cancelled,
                clock,
                ui,
                files: files.reborrow(),
            }),
        )
        .await
    }

    fn consumed(&self) {
        assert!(
            self.wire.replies.lock().unwrap().is_empty(),
            "unconsumed native GET replies"
        );
    }
}

fn dispatch(delivery: &Delivery) -> &GetDispatch {
    match delivery {
        Delivery::Get { dispatch, .. } => dispatch,
        Delivery::Plan { .. } => panic!("native GET must retain its typed dispatch"),
        #[cfg(any(windows, feature = "headless"))]
        Delivery::Execution(_) => panic!("native GET tests cannot consume execution delivery"),
    }
}

fn outcome(delivery: &Delivery) -> CommandOutcome {
    match delivery {
        Delivery::Get { outcome, .. } | Delivery::Plan { outcome, .. } => *outcome,
        #[cfg(any(windows, feature = "headless"))]
        Delivery::Execution(_) => panic!("native GET tests cannot consume execution delivery"),
    }
}

#[test]
fn get_credential_binding_is_directory_independent_and_keeps_selector_checks() {
    use super::super::{BatchState, NativeCredential};
    let mut state = BatchState::capture();
    let credential = NativeCredential::Broker {
        tenant: TENANT.into(),
        account: "offline@example.test".into(),
        client: LAKE.into(),
    };
    let path = state.path().unwrap();
    credential.bind_get(&mut state).unwrap();
    credential.bind_get(&mut state).unwrap();
    assert!(state.navigation.is_none());
    assert_eq!(state.path().unwrap(), path);
    for (tenant, account, client) in [
        (WORKSPACE, "offline@example.test", LAKE),
        (TENANT, "different@example.test", LAKE),
        (TENANT, "offline@example.test", ITEM),
    ] {
        assert!(
            NativeCredential::Broker {
                tenant: tenant.into(),
                account: account.into(),
                client: client.into(),
            }
            .bind_get(&mut state)
            .is_err()
        );
        assert!(state.navigation.is_none());
    }
    #[cfg(feature = "headless")]
    {
        let selection =
            crate::headless::Selection::from_inputs("client-secret", |name| match name {
                "FAB_RUST_TENANT" => Some(TENANT.into()),
                "FAB_RUST_CLIENT_ID" => Some(LAKE.into()),
                "FAB_RUST_CLIENT_SECRET" => Some("offline-native-get-secret".into()),
                _ => None,
            })
            .unwrap();
        assert!(
            NativeCredential::Headless(selection)
                .bind_get(&mut state)
                .is_err()
        );
        assert!(state.navigation.is_none());
    }
    credential
        .bind(&mut state, fabric_cli_context::HomeContext::Unspecified)
        .unwrap();
    let before = Arc::clone(state.navigation.as_ref().unwrap().current());
    credential.bind_get(&mut state).unwrap();
    assert!(Arc::ptr_eq(
        &before,
        state.navigation.as_ref().unwrap().current()
    ));
}

#[tokio::test]
async fn mounted_definition_get_uses_shared_session_executor_decoder_and_projection() {
    let fixture = Fixture::new(acquisition(true));
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let delivery = fixture
        .execute(
            &command(Some("."), None, false),
            &ui,
            None,
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    let result = dispatch(&delivery).result.as_ref().unwrap();
    let projected = result.projected.as_ref().unwrap();
    assert_eq!(
        projected["definition"]["parts"][0]["payloadType"],
        "DecodeBase64"
    );
    assert_eq!(
        projected["definition"]["parts"][0]["payload"]["cells"][0]["source"],
        json!(["café"])
    );
    assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
    let events = fixture.events.lock().unwrap();
    assert!(
        events.iter().position(|event| event == "confirm").unwrap()
            < events
                .iter()
                .position(|event| event.contains("/notebooks/"))
                .unwrap()
    );
    assert!(!events.iter().any(|event| event == "clock"));
    drop(events);
    assert_eq!(
        fixture
            .wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
    assert!(
        fixture
            .credential
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|scopes| scopes == &["https://api.fabric.microsoft.com/.default"])
    );
    fixture.consumed();
}

#[cfg(feature = "headless")]
#[tokio::test]
async fn mounted_headless_definition_get_uses_the_official_lazy_credential_path() {
    let mut replies = vec![reply(
        Method::Post,
        format!("/{TENANT}/oauth2/v2.0/token"),
        json!({"access_token":"offline-headless-get","token_type":"Bearer","expires_in":3600}),
    )];
    replies.extend(acquisition(true));
    let fixture = Fixture::new(replies);
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let selection = crate::headless::Selection::from_inputs("client-secret", |name| match name {
        "FAB_RUST_TENANT" => Some(TENANT.into()),
        "FAB_RUST_CLIENT_ID" => Some(LAKE.into()),
        "FAB_RUST_CLIENT_SECRET" => Some("offline-native-get-secret".into()),
        _ => None,
    })
    .unwrap();
    let mut state = super::super::BatchState::capture();
    let mut evidence = GetEvidence::default();
    let command = command(Some("."), None, true);
    let delivery = super::super::execute_injected_delivery(
        super::super::NativeCommand::Read(&command),
        (
            super::super::NativeCredential::Headless(selection),
            fixture.wire.clone(),
            fixture.wire.clone(),
        ),
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric),
        super::super::settings::NativeSettings::select(None).unwrap(),
        &mut state,
        Some(Inputs {
            evidence: &mut evidence,
            cancelled: Box::pin(pending()),
            clock: &clock,
            ui: &ui,
            files: Files::injected(None),
        }),
    )
    .await
    .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
    assert!(dispatch(&delivery).result.is_ok());
    let requests = fixture.wire.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url().host_str() == Some("login.microsoftonline.com"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url().path().ends_with("/getDefinition"))
            .count(),
        1
    );
    fixture.consumed();
}

#[tokio::test]
async fn mounted_metadata_query_bypasses_consent_and_definition_with_force_and_verbose() {
    let fixture = Fixture::new(acquisition(false));
    let mut command = command(Some("properties.zero"), None, true);
    command.verbose = true;
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let delivery = fixture
        .execute(&command, &ui, None, &clock, Box::pin(pending()))
        .await
        .unwrap();
    assert_eq!(
        dispatch(&delivery).result.as_ref().unwrap().projected,
        Some(json!("0"))
    );
    assert!(
        !fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "confirm" || event.starts_with("ui "))
    );
    assert!(
        fixture
            .wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method() == Method::Get)
    );
    fixture.consumed();
}

#[tokio::test]
async fn mounted_decline_eof_and_ui_failure_remain_distinct() {
    for mode in ["decline", "eof", "writer"] {
        let fixture = Fixture::new(discovery());
        let mut ui = Ui::new(fixture.events.clone());
        match mode {
            "decline" => ui.answer = Ok(false),
            "eof" => {
                let error = fabric_cli::console::read_confirmation(
                    &mut io::Cursor::new(Vec::<u8>::new()),
                    &mut Vec::new(),
                    "Confirm",
                )
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
                ui.answer = Err(CommandError::new(
                    Some(error.to_string()),
                    Some("ConfirmationInputFailed".into()),
                ));
            }
            "writer" => ui.fail_emit = true,
            _ => unreachable!(),
        }
        let clock = Clock {
            events: fixture.events.clone(),
            fail: false,
        };
        let delivery = fixture
            .execute(
                &command(None, None, mode == "writer"),
                &ui,
                None,
                &clock,
                Box::pin(pending()),
            )
            .await
            .unwrap();
        if mode == "decline" {
            assert!(
                dispatch(&delivery)
                    .result
                    .as_ref()
                    .unwrap()
                    .evidence
                    .declined
            );
            assert!(dispatch(&delivery).plan.records.is_empty());
            assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
        } else {
            assert!(matches!(
                dispatch(&delivery).result.as_ref().unwrap_err().kind,
                GetErrorKind::Command(_)
            ));
            assert!(matches!(outcome(&delivery), CommandOutcome::Stop(_)));
        }
        fixture.consumed();
    }
}

#[tokio::test]
async fn mounted_invalid_format_fails_before_discovery_or_output_capabilities() {
    let fixture = Fixture::new(Vec::new());
    let mut command = command(Some("."), Some("output"), true);
    command.output_format = Some("invalid".into());
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut files = MemoryFiles::new(fixture.events.clone());
    let error = fixture
        .execute(&command, &ui, Some(&mut files), &clock, Box::pin(pending()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.0, "invalid_configuration");
    assert!(fixture.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn absent_or_empty_output_never_activates_files_or_local_clock() {
    for output in [None, Some("")] {
        let fixture = Fixture::new(acquisition(false));
        let ui = Ui::new(fixture.events.clone());
        let clock = Clock {
            events: fixture.events.clone(),
            fail: true,
        };
        let mut files = MemoryFiles::new(fixture.events.clone());
        files.fail_resolve = true;
        let delivery = fixture
            .execute(
                &command(Some("properties.zero"), output, true),
                &ui,
                Some(&mut files),
                &clock,
                Box::pin(pending()),
            )
            .await
            .unwrap();
        assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
        assert!(
            dispatch(&delivery)
                .result
                .as_ref()
                .unwrap()
                .evidence
                .files
                .root
                .is_none()
        );
        assert!(
            !fixture
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "clock" || event == "resolve local")
        );
        fixture.consumed();
    }
}

#[tokio::test]
async fn projection_failure_does_not_resolve_output_or_sample_clock() {
    let fixture = Fixture::new(acquisition(false));
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut files = MemoryFiles::new(fixture.events.clone());
    let delivery = fixture
        .execute(
            &command(Some("properties.["), Some("output"), true),
            &ui,
            Some(&mut files),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(dispatch(&delivery).result.is_err());
    assert!(
        !fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "clock" || event == "resolve local")
    );
    fixture.consumed();
}

#[tokio::test]
async fn projected_output_resolves_then_samples_clock_and_writes_one_source_suffix() {
    for (query, expected, suffix) in [
        ("properties.zero", json!(0), ".json"),
        ("properties.nested", json!("False"), ""),
        ("properties.absent", json!("None"), ""),
    ] {
        let fixture = Fixture::new(acquisition(false));
        let ui = Ui::new(fixture.events.clone());
        let clock = Clock {
            events: fixture.events.clone(),
            fail: false,
        };
        let mut files = MemoryFiles::new(fixture.events.clone());
        let delivery = fixture
            .execute(
                &command(Some(query), Some("output"), true),
                &ui,
                Some(&mut files),
                &clock,
                Box::pin(pending()),
            )
            .await
            .unwrap();
        assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
        assert_eq!(files.writes.len(), 1);
        assert_eq!(
            files.writes[0].0,
            format!("Book.Notebook/get_20261030_211129{suffix}")
        );
        if suffix == ".json" {
            assert_eq!(
                serde_json::from_slice::<Value>(&files.writes[0].1).unwrap(),
                expected
            );
        } else {
            assert_eq!(
                std::str::from_utf8(&files.writes[0].1).unwrap(),
                expected.as_str().unwrap()
            );
        }
        let events = fixture.events.lock().unwrap();
        let index = |name| events.iter().position(|event| event == name).unwrap();
        assert!(index("resolve local") < index("clock"));
        assert!(index("clock") < index("preflight"));
        assert!(index("preflight") < index("write"));
        fixture.consumed();
    }
}

#[tokio::test]
async fn clock_failure_never_uses_utc_or_epoch_or_starts_output_writes() {
    let fixture = Fixture::new(acquisition(false));
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: true,
    };
    let mut files = MemoryFiles::new(fixture.events.clone());
    let delivery = fixture
        .execute(
            &command(Some("id"), Some("output"), true),
            &ui,
            Some(&mut files),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(matches!(outcome(&delivery), CommandOutcome::Stop(_)));
    assert!(matches!(
        dispatch(&delivery).result.as_ref().unwrap_err().kind,
        GetErrorKind::Command(_)
    ));
    assert!(files.writes.is_empty());
    assert!(
        !fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "preflight" || event.starts_with("mkdir "))
    );
    fixture.consumed();
}

#[tokio::test]
async fn native_local_output_uses_existing_deferred_confined_file_worker() {
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let name = format!("native-get-output-{}", std::process::id());
    let root = std::env::current_dir().unwrap().join(&name);
    std::fs::create_dir(&root).unwrap();
    let directory = Directory(root.clone());
    let fixture = Fixture::new(acquisition(false));
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let delivery = fixture
        .execute_with_files(
            &command(Some("properties.zero"), Some(&name), true),
            &ui,
            Files::native(),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    let path = root.join("Book.Notebook").join("get_20261030_211129.json");
    let actual = std::fs::read(&path);
    let contents = std::fs::read_dir(root.join("Book.Notebook")).map(|entries| {
        entries
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    });
    drop(directory);
    assert_eq!(
        outcome(&delivery),
        CommandOutcome::Returned(0),
        "{:?}",
        dispatch(&delivery)
    );
    assert_eq!(actual.unwrap(), b"0");
    assert_eq!(
        contents.unwrap(),
        [std::ffi::OsString::from("get_20261030_211129.json")]
    );
    fixture.consumed();
}

#[tokio::test]
async fn destination_service_failure_cannot_activate_a_local_fallback() {
    for status in [StatusCode::Unauthorized, StatusCode::BadRequest] {
        let mut replies = acquisition(false);
        let mut denied = get(
            "/v1/workspaces",
            json!({"error":{"code":"Denied","message":"offline"}}),
        );
        denied.status = status;
        replies.push(denied);
        let fixture = Fixture::new(replies);
        let ui = Ui::new(fixture.events.clone());
        let clock = Clock {
            events: fixture.events.clone(),
            fail: false,
        };
        let mut files = MemoryFiles::new(fixture.events.clone());
        let delivery = fixture
            .execute(
                &command(
                    Some("id"),
                    Some("Demo.Workspace/Lake.Lakehouse/Files"),
                    true,
                ),
                &ui,
                Some(&mut files),
                &clock,
                Box::pin(pending()),
            )
            .await
            .unwrap();
        assert!(matches!(
            dispatch(&delivery).result.as_ref().unwrap_err().kind,
            GetErrorKind::Definition(_)
        ));
        assert!(
            !fixture
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "resolve local" || event == "clock")
        );
        fixture.consumed();
    }
}

#[tokio::test]
async fn unsupported_fabric_destination_is_not_local_authority() {
    let mut replies = acquisition(false);
    replies.extend(discovery().into_iter().take(1));
    let fixture = Fixture::new(replies);
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut files = MemoryFiles::new(fixture.events.clone());
    let delivery = fixture
        .execute(
            &command(Some("id"), Some("Demo.Workspace"), true),
            &ui,
            Some(&mut files),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(dispatch(&delivery).result.is_err());
    assert!(
        !fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "resolve local" || event == "clock")
    );
    fixture.consumed();
}

#[tokio::test]
async fn unmounted_virtual_destination_preserves_capability_failure_without_local_fallback() {
    let fixture = Fixture::new(acquisition(false));
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut files = MemoryFiles::new(fixture.events.clone());
    let delivery = fixture
        .execute(
            &command(Some("id"), Some(".domains/Test.Domain"), true),
            &ui,
            Some(&mut files),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(matches!(outcome(&delivery), CommandOutcome::Stop(_)));
    let failure = dispatch(&delivery).result.as_ref().unwrap_err();
    let GetErrorKind::Definition(error) = &failure.kind else {
        panic!("expected classified output failure: {:?}", failure.kind)
    };
    let fabric_deploy::definitions::DefinitionErrorKind::FileSystem(error) = &error.kind else {
        panic!("expected original filesystem-boundary error: {error:?}")
    };
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    let original = error
        .get_ref()
        .unwrap()
        .downcast_ref::<fabric_cli_context::ContextError>()
        .unwrap();
    assert_eq!(original.kind, fabric_cli_context::ErrorKind::Lookup);
    assert_eq!(original.code, "HostCapabilityUnavailable");
    assert!(
        !fixture.events.lock().unwrap().iter().any(|event| {
            matches!(
                event.as_str(),
                "resolve local" | "clock" | "preflight" | "write"
            ) || event.starts_with("mkdir ")
        }),
        "the injected local adapter would succeed, but unavailable metadata grants no local authority",
    );
    assert!(files.writes.is_empty());
    assert!(failure.evidence.files.root.is_none());
    fixture.consumed();
}

#[tokio::test]
async fn actual_lakehouse_output_uses_onelake_audience_and_never_activates_local_files() {
    let fixture = Fixture::new(lakehouse_replies());
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut files = LocalFiles::new("unrelated-local-selection".into(), Handle::current());
    let delivery = fixture
        .execute(
            &command(
                Some("properties.zero"),
                Some("Demo.Workspace/Lake.Lakehouse/Files"),
                true,
            ),
            &ui,
            Some(&mut files),
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
    assert_eq!(
        dispatch(&delivery)
            .result
            .as_ref()
            .unwrap()
            .evidence
            .files
            .completed_paths,
        ["Book.Notebook/get_20261030_211129.json"]
    );
    assert!(
        !fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "resolve local")
    );
    assert!(files.selected.is_none());
    assert_eq!(
        files
            .resolve_export_directory_async("Demo.Workspace/Lake.Lakehouse/Files")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput,
        "the injected local adapter must fail if incorrectly selected",
    );
    assert!(files.selected.is_none());
    let requests = fixture.wire.requests.lock().unwrap();
    let upload = requests
        .iter()
        .find(|request| request.method() == Method::Put)
        .unwrap();
    assert_eq!(
        upload.url().host_str(),
        Some("onelake.dfs.fabric.microsoft.com")
    );
    assert!(upload.url().path().contains(&format!(
        "/{WORKSPACE}/{LAKE}/Files/Book.Notebook/get_20261030_211129.json"
    )));
    assert!(
        fixture
            .credential
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|scopes| scopes == &["https://storage.azure.com/.default"])
    );
    fixture.consumed();
}

#[cfg(unix)]
#[test]
fn isolated_missing_cwd_process_keeps_remote_get_directory_independent() {
    use std::{
        process::{Command, Stdio},
        time::Instant,
    };
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let root = std::env::current_dir()
        .unwrap()
        .join(format!("native-get-missing-cwd-{}", std::process::id(),));
    std::fs::create_dir(&root).unwrap();
    let _directory = Directory(root.clone());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_read::get::tests::missing_cwd_process_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("FAB_NATIVE_GET_MISSING_CWD_TEST", "1")
        .current_dir(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(10);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("waiting for the isolated missing-cwd GET failed: {error}");
            }
        }
        if Instant::now() >= expires {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = child.wait_with_output().unwrap();
    assert!(
        !timed_out,
        "isolated missing-cwd GET exceeded its owned deadline"
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native-get-missing-cwd-exercised"));
}

#[cfg(unix)]
#[tokio::test]
async fn missing_cwd_process_child() {
    if std::env::var("FAB_NATIVE_GET_MISSING_CWD_TEST")
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    let root = std::env::current_dir().unwrap();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("native-get-missing-cwd-")
    );
    std::fs::remove_dir(root).unwrap();
    assert!(std::env::current_dir().is_err());
    let mut state = super::super::BatchState::capture();
    let credential = super::super::NativeCredential::Broker {
        tenant: TENANT.into(),
        account: "offline@example.test".into(),
        client: LAKE.into(),
    };
    let before = state.path().unwrap();
    credential.bind_get(&mut state).unwrap();
    assert!(state.navigation.is_none());
    let fixture = Fixture::new(lakehouse_replies());
    let sdk = Client::new(
        fixture.credential.clone(),
        CredentialSelection::new("offline@example.test", TENANT, Audience::Fabric).unwrap(),
    );
    let executor = HttpExecutor::new(
        fixture.wire.clone(),
        Arc::new(TokioTimer),
        Default::default(),
    )
    .unwrap();
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let mut evidence = GetEvidence::default();
    let command = command(
        Some("properties.zero"),
        Some("Demo.Workspace/Lake.Lakehouse/Files"),
        true,
    );
    let delivery = super::super::execute_authenticated_injected(
        super::super::NativeCommand::Read(&command),
        sdk,
        executor,
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        super::super::settings::NativeSettings::select(None).unwrap(),
        &mut state,
        false,
        Some(Inputs {
            evidence: &mut evidence,
            cancelled: Box::pin(pending()),
            clock: &clock,
            ui: &ui,
            files: Files::native(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Returned(0));
    assert_eq!(
        dispatch(&delivery)
            .result
            .as_ref()
            .unwrap()
            .evidence
            .files
            .completed_paths
            .len(),
        1
    );
    assert!(state.navigation.is_none());
    assert_eq!(state.path().unwrap(), before);
    assert!(std::env::current_dir().is_err());
    fixture.consumed();
    println!("native-get-missing-cwd-exercised");
}

#[tokio::test]
async fn cancelled_output_retains_incomplete_or_completed_file_effects() {
    for completed in [false, true] {
        let fixture = Fixture::new(acquisition(false));
        let ui = Ui::new(fixture.events.clone());
        let clock = Clock {
            events: fixture.events.clone(),
            fail: false,
        };
        let (signal, mut receiver) = tokio::sync::watch::channel(false);
        let mut files = MemoryFiles::new(fixture.events.clone());
        files.cancel_on_write = Some((signal, completed));
        let cancelled = Box::pin(async move {
            while !*receiver.borrow() {
                receiver.changed().await.unwrap();
            }
        });
        let delivery = fixture
            .execute(
                &command(Some("id"), Some("output"), true),
                &ui,
                Some(&mut files),
                &clock,
                cancelled,
            )
            .await
            .unwrap();
        assert!(matches!(outcome(&delivery), CommandOutcome::Cancelled(_)));
        let effects = &dispatch(&delivery)
            .result
            .as_ref()
            .unwrap_err()
            .evidence
            .files;
        assert_eq!(
            effects.completed_directory.as_deref(),
            Some("Book.Notebook")
        );
        assert_eq!(effects.completed_paths.len(), usize::from(completed));
        assert_eq!(effects.incomplete_path.is_some(), !completed);
        fixture.consumed();
    }
}

#[tokio::test]
async fn terminal_cancelled_definition_keeps_original_operation_and_raw_error() {
    let mut replies = acquisition(true);
    replies.truncate(3);
    let mut accepted = reply(
        Method::Post,
        format!("/items/{ITEM}/getDefinition?format=ipynb"),
        json!({}),
    );
    accepted.status = StatusCode::Accepted;
    accepted.headers.insert("x-ms-operation-id", OPERATION);
    accepted.headers.insert("retry-after", "0");
    replies.extend([
        accepted,
        get(format!("/operations/{OPERATION}"), json!({"status":"Cancelled","error":{"errorCode":"cancelled-by-service","message":"???..."}})),
    ]);
    let fixture = Fixture::new(replies);
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let delivery = fixture
        .execute(
            &command(Some("."), None, true),
            &ui,
            None,
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(matches!(outcome(&delivery), CommandOutcome::Cancelled(_)));
    let failure = dispatch(&delivery).result.as_ref().unwrap_err();
    let GetErrorKind::Definition(error) = &failure.kind else {
        panic!("{:?}", failure.kind)
    };
    let fabric_deploy::definitions::DefinitionErrorKind::Operation(error) = &error.kind else {
        panic!("original operation")
    };
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(
        matches!(&error.kind, OperationErrorKind::OperationFailed { status, .. } if status == "Cancelled")
    );
    let raw = error.raw_service_error.as_ref().unwrap();
    assert_eq!(raw["message"], "???...");
    assert_eq!(raw["errorCode"], "cancelled-by-service");
    assert!(
        failure
            .evidence
            .operations
            .iter()
            .any(|receipt| receipt.operation_id.as_deref() == Some(OPERATION))
    );
    fixture.consumed();
}

#[tokio::test]
async fn polled_enrichment_cancellation_is_not_swallowed_as_best_effort() {
    let mut replies = acquisition(false);
    replies.truncate(3);
    let mut accepted = get(format!("/items/{ITEM}/connections"), json!({}));
    accepted.status = StatusCode::Accepted;
    accepted.headers.insert("x-ms-operation-id", OPERATION);
    accepted.headers.insert("retry-after", "0");
    replies.extend([
        accepted,
        get(format!("/operations/{OPERATION}"), json!({"status":"Cancelled","error":{"errorCode":"cancelled-by-service","message":"raw cancellation"}})),
    ]);
    let fixture = Fixture::new(replies);
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let delivery = fixture
        .execute(
            &command(Some("id"), None, true),
            &ui,
            None,
            &clock,
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert!(matches!(outcome(&delivery), CommandOutcome::Cancelled(_)));
    let failure = dispatch(&delivery).result.as_ref().unwrap_err();
    let GetErrorKind::Operation(error) = &failure.kind else {
        panic!("{:?}", failure.kind)
    };
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(
        error.raw_service_error.as_ref().unwrap()["message"],
        "raw cancellation"
    );
    assert!(failure.evidence.enrichment_failures.is_empty());
    fixture.consumed();
}

#[tokio::test]
async fn cooperative_ctrl_c_drains_accepted_definition_without_replay() {
    let mut replies = acquisition(true);
    replies.truncate(3);
    let mut accepted = reply(
        Method::Post,
        format!("/items/{ITEM}/getDefinition?format=ipynb"),
        json!({}),
    );
    accepted.status = StatusCode::Accepted;
    accepted.headers.insert("x-ms-operation-id", OPERATION);
    accepted.headers.insert("retry-after", "0");
    let mut poll = get(format!("/operations/{OPERATION}"), json!({}));
    poll.hold = true;
    replies.extend([accepted, poll]);
    let fixture = Fixture::new(replies);
    let ui = Ui::new(fixture.events.clone());
    let clock = Clock {
        events: fixture.events.clone(),
        fail: false,
    };
    let command = command(Some("."), None, true);
    let sdk = Client::new(
        fixture.credential.clone(),
        CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
    );
    let executor = HttpExecutor::new(
        fixture.wire.clone(),
        Arc::new(TokioTimer),
        Default::default(),
    )
    .unwrap();
    let mut state = super::super::BatchState::capture();
    let delivery = controlled(
        async |evidence, cancelled| {
            super::super::execute_authenticated_injected(
                super::super::NativeCommand::Read(&command),
                sdk,
                executor,
                "/",
                fabric_cli_context::HomeContext::Unspecified,
                super::super::settings::NativeSettings::select(None).unwrap(),
                &mut state,
                false,
                Some(Inputs {
                    evidence,
                    cancelled,
                    clock: &clock,
                    ui: &ui,
                    files: Files::injected(None),
                }),
            )
            .await
        },
        async {
            loop {
                if fixture
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event.contains(&format!("/operations/{OPERATION}")))
                {
                    break super::super::interrupted_failure("cancelled", "Read cancelled.", 2);
                }
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Cancelled(2));
    let failure = dispatch(&delivery).result.as_ref().unwrap_err();
    assert!(
        failure
            .evidence
            .operations
            .iter()
            .any(|receipt| receipt.operation_id.as_deref() == Some(OPERATION)
                && receipt.mutation_may_have_occurred)
    );
    assert!(matches!(failure.kind, GetErrorKind::Definition(_)));
    assert_eq!(dispatch(&delivery).plan.records.len(), 1);
    fixture.consumed();
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("GET delivery must never flush")
    }
}

#[test]
fn final_writer_preserves_broken_pipe_and_owned_dispatch_effects() {
    let mut evidence = GetEvidence::default();
    evidence
        .files
        .completed_paths
        .push("Book.Notebook/get_20261030_211129.json".into());
    let dispatch = GetDispatch {
        plan: TextPlan {
            records: vec![RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: b"result\n".to_vec(),
            }],
            exit_code: 0,
        },
        result: Ok(GetOutcome {
            projected: Some(json!({"ok":true})),
            output: None,
            evidence,
        }),
    };
    let failure = Delivery::get(dispatch)
        .write_to(&mut BrokenWriter, &mut Vec::new())
        .unwrap_err();
    assert_eq!(failure.kind(), io::ErrorKind::BrokenPipe);
    let typed = failure
        .get_ref()
        .unwrap()
        .downcast_ref::<DeliveryWriteFailure>()
        .unwrap();
    assert_eq!(typed.0.error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(
        typed
            .0
            .dispatch
            .result
            .as_ref()
            .unwrap()
            .evidence
            .files
            .completed_paths
            .len(),
        1
    );
}

#[tokio::test]
async fn control_signal_is_latched_and_retains_completed_effects() {
    let delivery = controlled(
        async |evidence, cancelled| {
            evidence
                .files
                .completed_paths
                .push("already-completed.json".into());
            cancelled.await;
            Ok(Delivery::abandoned(
                std::mem::take(evidence),
                super::super::failure("cancelled", "cancelled", 1),
            ))
        },
        async { super::super::failure("cancelled", "Read cancelled.", 2) },
    )
    .await
    .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Cancelled(2));
    assert_eq!(
        dispatch(&delivery)
            .result
            .as_ref()
            .unwrap_err()
            .evidence
            .files
            .completed_paths,
        ["already-completed.json"]
    );
}

#[tokio::test]
async fn expired_bounded_drain_retains_started_effects_without_claiming_rollback() {
    let delivery = controlled(
        async |evidence, _cancelled| {
            evidence.files.incomplete_path = Some("possibly-partial.json".into());
            pending().await
        },
        async { super::super::interrupted_failure("timed_out", "Read deadline elapsed.", 1) },
    )
    .await
    .unwrap();
    assert_eq!(outcome(&delivery), CommandOutcome::Stop(1));
    let failure = dispatch(&delivery).result.as_ref().unwrap_err();
    assert_eq!(
        failure.evidence.files.incomplete_path.as_deref(),
        Some("possibly-partial.json")
    );
    let mut stderr = Vec::new();
    delivery.write_to(&mut Vec::new(), &mut stderr).unwrap();
    let text = String::from_utf8(stderr).unwrap();
    assert!(text.contains("bounded GET drain expired"));
    assert!(text.contains("No rollback is implied"));
}

#[cfg(not(windows))]
#[test]
fn unix_local_paths_do_not_apply_windows_named_user_expansion() {
    assert!(local_candidate("~other/output", Path::new("/cwd")).is_err());
    assert_eq!(
        local_candidate("output", Path::new("/cwd")).unwrap(),
        PathBuf::from("/cwd/output")
    );
    assert_eq!(
        local_candidate("/absolute", Path::new("/cwd")).unwrap(),
        PathBuf::from("/absolute")
    );
}
