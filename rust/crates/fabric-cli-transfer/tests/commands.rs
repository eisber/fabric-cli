// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual family handler -> released definitions -> shared policy/HTTP -> owned files.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::Command;
use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::{
    Indent, Timestamp,
    text::{OutputFormat, Presentation, ascii_width},
};
use fabric_cli_transfer::{
    LocalTransferFiles, TransferCommand, TransferContext, TransferEvent, TransferFailure,
    TransferFiles, TransferFuture, TransferHost, TransferItem, TransferKind, TransferMetadata,
    TransferUi, command_from_matches, render_error, render_result, transfer_subcommands,
    write_result,
};
use fabric_deploy::definitions::{CommandSupport, DefinitionErrorKind, DefinitionLimits};
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{
    ExecutionError, ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    future::pending,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn parse(args: &[&str]) -> TransferCommand {
    let matches = Command::new("test")
        .subcommands(transfer_subcommands())
        .try_get_matches_from(std::iter::once("test").chain(args.iter().copied()))
        .unwrap();
    command_from_matches(&matches).unwrap()
}

fn timestamp() -> Timestamp {
    Timestamp::parse("2026-10-26T12:00:00Z").unwrap()
}

fn source(case: &str) -> Value {
    let receipt: Value = serde_json::from_str(include_str!("source_receipt.json")).unwrap();
    assert_eq!(
        receipt["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    receipt["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["case"] == case)
        .unwrap()
        .clone()
}

fn request_body(request: &Request) -> &[u8] {
    match request.body() {
        Body::Bytes(bytes) => bytes.as_ref(),
        _ => panic!("definition requests must use an in-memory byte body"),
    }
}

fn definition() -> Value {
    json!({"definition":{"parts":[{
        "path":"pipeline-content.json", "payload":"eyJuYW1lIjoic3ludGhldGljIn0=",
        "payloadType":"InlineBase64",
    }]}})
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fabric-transfer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        fs::create_dir(&path).unwrap();
        let root = fs::canonicalize(path).unwrap();
        fs::create_dir(root.join("output")).unwrap();
        fs::create_dir(root.join("input")).unwrap();
        fs::write(
            root.join("input").join("pipeline-content.json"),
            br#"{"name":"synthetic"}"#,
        )
        .unwrap();
        fs::write(
            root.join("input").join("cache.abf"),
            b"synthetic cache exclusion",
        )
        .unwrap();
        Self(root)
    }

    fn files(&self) -> LocalTransferFiles {
        LocalTransferFiles::new(
            self.0.clone(),
            Some(self.0.clone()),
            DefinitionLimits::default(),
        )
        .unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("cleanup only this test's exact created directory");
    }
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-offline-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Http {
    responses: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or mutation replay"))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay >= Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(std::future::ready(()))
        }
    }
}

struct SharedHttp(HttpExecutor);

fn execution_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}

impl OperationExecutor for SharedHttp {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> TransferFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            self.0
                .execute_fabric(
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(execution_error)
        })
    }

    fn wait(&self, delay: Duration) -> TransferFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(execution_error) })
    }
}

fn reply(status: u16, body: Value) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        Headers::new(),
        serde_json::to_vec(&body).unwrap(),
    )
}

#[derive(Default)]
struct Ui {
    events: Mutex<Vec<TransferEvent>>,
    prompts: Mutex<Vec<String>>,
    answers: Mutex<VecDeque<bool>>,
    selection: Mutex<Option<Vec<String>>>,
}

impl TransferUi for Ui {
    fn emit(&self, event: TransferEvent) -> Result<(), fabric_cli_output::CommandError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    fn confirm<'a>(
        &'a self,
        message: &'a str,
    ) -> TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>> {
        self.prompts.lock().unwrap().push(message.into());
        Box::pin(std::future::ready(Ok(self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected prompt"))))
    }

    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>> {
        Box::pin(std::future::ready(Ok(self
            .selection
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| names.to_vec()))))
    }
}

struct Metadata {
    context: TransferContext,
    calls: AtomicUsize,
}

impl TransferMetadata for Metadata {
    fn resolve<'a>(
        &'a self,
        _: TransferKind,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<TransferContext, fabric_cli_output::CommandError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(self.context.clone())))
    }
}

fn context(workspace: bool, existing: bool) -> TransferContext {
    let tenant = Element::tenant("offline", "tenant");
    let ws = Element::child(
        "Demo",
        Some(WORKSPACE.into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant,
    )
    .unwrap();
    let item = Element::child(
        "Pipe",
        existing.then(|| ITEM.into()),
        NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
        ws.clone(),
    )
    .unwrap();
    TransferContext {
        target: if workspace { ws } else { item.clone() },
        items: vec![TransferItem {
            element: item.clone(),
            support: CommandSupport::Supported,
        }],
        selection_order: vec![item.name()],
    }
}

struct Harness {
    client: Client,
    credential: Arc<Credential>,
    http: Arc<Http>,
    executor: SharedHttp,
    metadata: Metadata,
    ui: Ui,
}

impl Harness {
    fn new(context: TransferContext, replies: Vec<AsyncRawResponse>) -> Self {
        let credential = Arc::new(Credential::default());
        let http = Arc::new(Http {
            responses: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        });
        Self {
            client: Client::new(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            ),
            credential,
            executor: SharedHttp(
                HttpExecutor::new(http.clone(), Arc::new(Timer), ExecutionOptions::default())
                    .unwrap(),
            ),
            http,
            metadata: Metadata {
                context,
                calls: AtomicUsize::new(0),
            },
            ui: Ui::default(),
        }
    }

    fn host<'a>(&'a self, files: &'a mut dyn TransferFiles) -> TransferHost<'a, SharedHttp> {
        TransferHost {
            client: &self.client,
            executor: &self.executor,
            metadata: &self.metadata,
            files,
            ui: &self.ui,
            definition_limits: DefinitionLimits::default(),
            operation_limits: OperationLimits::default(),
            command_line: true,
        }
    }

    fn no_requests(&self) {
        assert!(self.http.requests.lock().unwrap().is_empty());
        assert_eq!(self.credential.0.load(Ordering::SeqCst), 0);
    }
}

fn assert_file(root: &Path, case: &str) {
    let bytes = fs::read(
        root.join("output")
            .join("Pipe.DataPipeline")
            .join("pipeline-content.json"),
    )
    .unwrap();
    let source = source(case);
    let expected = STANDARD
        .decode(
            source["files"]["Pipe.DataPipeline/pipeline-content.json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let expected = if cfg!(windows) {
        expected
    } else {
        String::from_utf8(expected)
            .unwrap()
            .replace("\r\n", "\n")
            .into_bytes()
    };
    assert_eq!(bytes, expected);
}

#[tokio::test]
async fn export_real_handler_matches_source_output_requests_and_local_file() {
    let directory = Directory::new();
    let harness = Harness::new(
        context(false, true),
        vec![reply(200, json!({"id":ITEM})), reply(200, definition())],
    );
    let command = parse(&[
        "export",
        "Demo.Workspace/Pipe.DataPipeline",
        "-o",
        "output",
        "-f",
        "--output_format",
        "json",
    ]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    assert_eq!(
        result.outputs[0].message,
        source("export")["outputs"][0]["message"]
            .as_str()
            .map(str::to_owned)
    );
    assert_eq!(
        result.completed_paths,
        ["Pipe.DataPipeline/pipeline-content.json"]
    );
    assert!(result.effects.is_empty());
    assert_file(&directory.0, "export");
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(requests[1].method(), Method::Post);
    assert_eq!(
        requests[1].url().path(),
        format!("/v1/workspaces/{WORKSPACE}/items/{ITEM}/getDefinition")
    );
    assert_eq!(requests[1].url().query(), None);
    let plan = render_result(
        &result,
        &command,
        timestamp(),
        OutputFormat::Json,
        Presentation::default(),
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(write_result(&plan, &mut stdout, &mut stderr).unwrap(), 0);
    assert!(stderr.is_empty());
    let output: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["status"], "Success");
    assert_eq!(
        output["result"]["message"],
        source("export")["outputs"][0]["message"]
    );
}

#[tokio::test]
async fn bulk_real_handler_matches_source_all_request_summary_and_local_file() {
    let directory = Directory::new();
    let mut definition = definition()["definition"]["parts"].clone();
    definition[0]["path"] = json!("/Pipe.DataPipeline/pipeline-content.json");
    let harness = Harness::new(
        context(true, true),
        vec![reply(
            200,
            json!({
                "definitionParts":definition, "itemDefinitionsIndex":[{"id":ITEM,"rootPath":"/Pipe.DataPipeline"}],
            }),
        )],
    );
    let command = parse(&[
        "bulk-export",
        "Demo.Workspace",
        "-o",
        "output",
        "--recursive",
        "-f",
    ]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    assert_eq!(result.outputs[0].data.as_ref().unwrap()[0]["exported"], 1);
    assert_eq!(
        result.outputs[0].data.as_ref().unwrap()[0]["exported_types"],
        source("bulk-export")["outputs"][0]["data"][0]["exported_types"]
    );
    assert!(result.key_value);
    let mut expected = source("bulk-export")["outputs"][0]["data"].clone();
    expected[0]["output"] = json!("output");
    assert_eq!(result.outputs[0].data.as_ref().unwrap(), &expected);
    assert_eq!(
        result.outputs[0].message.as_deref(),
        Some("Exported 1 items to 'output'")
    );
    assert_file(&directory.0, "bulk-export");
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_body(&requests[0]),
        source("bulk-export")["calls"][0]["body"]
            .as_str()
            .unwrap()
            .as_bytes()
    );
    assert_eq!(requests[0].url().query(), Some("beta=true"));
    let plan = render_result(
        &result,
        &command,
        timestamp(),
        OutputFormat::Text,
        Presentation::default(),
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    write_result(&plan, &mut stdout, &mut stderr).unwrap();
    assert!(!stdout.is_empty());
}

#[tokio::test]
async fn import_create_and_update_match_source_exact_bodies_messages_and_cache() {
    for existing in [false, true] {
        let directory = Directory::new();
        let harness = Harness::new(
            context(false, existing),
            vec![reply(if existing { 200 } else { 201 }, json!({"id":ITEM}))],
        );
        let command = parse(&[
            "import",
            "Demo.Workspace/Pipe.DataPipeline",
            "-i",
            "input",
            "-f",
        ]);
        let result = harness
            .host(&mut directory.files())
            .execute(&command, pending())
            .await
            .unwrap();
        let case = source(if existing {
            "import-update"
        } else {
            "import-create"
        });
        assert_eq!(
            result.outputs[0].message.as_deref(),
            case["outputs"][0]["message"].as_str()
        );
        assert_eq!(result.effects.len(), 1);
        assert_eq!(result.effects[0].item_id, case["effects"][0]);
        assert!(
            result
                .excluded_paths
                .iter()
                .any(|path| path.ends_with("cache.abf"))
        );
        let requests = harness.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Post);
        assert_eq!(
            request_body(&requests[0]),
            case["calls"][0]["body"].as_str().unwrap().as_bytes()
        );
        assert_eq!(
            fs::read(directory.0.join("input").join("pipeline-content.json")).unwrap(),
            br#"{"name":"synthetic"}"#
        );
        assert!(result.completed_paths.is_empty());
    }
}

#[tokio::test]
async fn workspace_export_all_reuses_fresh_shared_sessions_and_source_selection_order() {
    let directory = Directory::new();
    let mut ctx = context(true, true);
    let ws = ctx.target.clone();
    let second = Element::child(
        "Other",
        Some(OPERATION.into()),
        NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
        ws,
    )
    .unwrap();
    ctx.items.push(TransferItem {
        element: second.clone(),
        support: CommandSupport::Supported,
    });
    ctx.selection_order = vec![second.name(), "Pipe.DataPipeline".into()];
    let harness = Harness::new(
        ctx,
        vec![
            reply(200, json!({"id":ITEM})),
            reply(200, definition()),
            reply(200, json!({"id":OPERATION})),
            reply(200, definition()),
        ],
    );
    let command = parse(&["export", "Demo.Workspace", "-o", "output", "-a", "-f"]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    assert_eq!(
        result.outputs[0].message.as_deref(),
        Some("2 items exported successfully")
    );
    assert_eq!(
        result.completed_paths,
        [
            "Pipe.DataPipeline/pipeline-content.json",
            "Other.DataPipeline/pipeline-content.json"
        ]
    );
    assert!(
        harness
            .ui
            .events
            .lock()
            .unwrap()
            .contains(&TransferEvent::Grey(
                "Other.DataPipeline\nPipe.DataPipeline".into()
            ))
    );
}

#[tokio::test]
async fn declined_import_initial_and_overwrite_preserve_distinct_source_cache_effects() {
    for answers in [vec![false], vec![true, false]] {
        let directory = Directory::new();
        let harness = Harness::new(context(false, true), Vec::new());
        harness.ui.answers.lock().unwrap().extend(answers.clone());
        let command = parse(&["import", "Demo.Workspace/Pipe.DataPipeline", "-i", "input"]);
        let result = harness
            .host(&mut directory.files())
            .execute(&command, pending())
            .await
            .unwrap();
        assert!(result.declined);
        assert!(result.outputs.is_empty());
        assert_eq!(result.effects.len(), usize::from(answers.len() == 2));
        harness.no_requests();
    }
}

#[tokio::test]
async fn declined_single_export_retains_source_message_without_claiming_file_effects() {
    let directory = Directory::new();
    let harness = Harness::new(context(false, true), Vec::new());
    harness.ui.answers.lock().unwrap().push_back(false);
    let command = parse(&["export", "Demo.Workspace/Pipe.DataPipeline", "-o", "output"]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    assert!(result.declined);
    assert_eq!(
        result.outputs[0].message.as_deref(),
        Some("'Pipe.DataPipeline' exported")
    );
    assert!(result.completed_paths.is_empty());
    harness.no_requests();
}

#[tokio::test]
async fn bulk_requires_recursive_and_prompts_nonempty_before_sensitivity() {
    let directory = Directory::new();
    fs::write(directory.0.join("output").join("existing.txt"), b"keep").unwrap();
    let harness = Harness::new(context(true, true), Vec::new());
    let command = parse(&["bulk-export", "Demo.Workspace", "-o", "output"]);
    assert!(
        harness
            .host(&mut directory.files())
            .execute(&command, pending())
            .await
            .is_err()
    );
    assert_eq!(harness.metadata.calls.load(Ordering::SeqCst), 0);
    harness.ui.answers.lock().unwrap().push_back(false);
    let command = parse(&[
        "bulk-export",
        "Demo.Workspace",
        "-o",
        "output",
        "--recursive",
    ]);
    assert!(
        harness
            .host(&mut directory.files())
            .execute(&command, pending())
            .await
            .unwrap()
            .declined
    );
    assert_eq!(harness.ui.prompts.lock().unwrap().len(), 1);
    assert!(harness.ui.prompts.lock().unwrap()[0].contains("not empty"));
    assert_eq!(
        fs::read(directory.0.join("output").join("existing.txt")).unwrap(),
        b"keep"
    );
    harness.no_requests();
}

#[tokio::test]
async fn bare_accepted_import_retains_uncertainty_and_never_upserts_or_replays() {
    let directory = Directory::new();
    let harness = Harness::new(context(false, false), vec![reply(202, json!({}))]);
    let command = parse(&[
        "import",
        "Demo.Workspace/Pipe.DataPipeline",
        "-i",
        "input",
        "-f",
    ]);
    let error = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap_err();
    assert!(error.effects.is_empty());
    let TransferFailure::Definition(definition) = &error.failure else {
        panic!("original definition error")
    };
    let DefinitionErrorKind::Operation(operation) = &definition.kind else {
        panic!("original operation evidence")
    };
    assert!(operation.mutation_may_have_occurred);
    assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    assert_eq!(
        error
            .envelope(timestamp(), Some("import".into()))
            .render(Indent::Four)
            .unwrap()
            .exit_code,
        1
    );
}

#[tokio::test]
async fn ready_cancellation_precedes_metadata_and_does_not_read_or_mutate() {
    let directory = Directory::new();
    let harness = Harness::new(context(false, false), Vec::new());
    let command = parse(&[
        "import",
        "Demo.Workspace/Pipe.DataPipeline",
        "-i",
        "input",
        "-f",
    ]);
    assert!(
        harness
            .host(&mut directory.files())
            .execute(&command, std::future::ready(()))
            .await
            .is_err()
    );
    assert_eq!(harness.metadata.calls.load(Ordering::SeqCst), 0);
    harness.no_requests();
}

#[tokio::test]
async fn deploy_is_an_explicit_source_gap_not_notebook_success() {
    let directory = Directory::new();
    let harness = Harness::new(context(false, false), Vec::new());
    let command = parse(&[
        "deploy",
        "--config",
        "config.yml",
        "--target_env",
        "dev",
        "-P",
        "x=1",
        "--bulk_publish",
        "-f",
    ]);
    let error = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap_err();
    let envelope = error
        .envelope(timestamp(), Some("deploy".into()))
        .to_value()
        .unwrap();
    assert_eq!(envelope["result"]["error_code"], "NotSupported");
    assert_eq!(harness.metadata.calls.load(Ordering::SeqCst), 0);
    harness.no_requests();
}

#[test]
fn parser_preserves_cardinality_options_and_rejects_non_source_flags() {
    let command = parse(&[
        "export",
        "Demo",
        "Workspace.Workspace/Pipe.DataPipeline",
        "-o",
        "out",
        "dir",
        "-a",
        "--format",
        "TMDL",
    ]);
    assert_eq!(command.path.len(), 2);
    assert_eq!(command.output.unwrap(), ["out", "dir"]);
    assert!(command.all);
    assert_eq!(command.format.as_deref(), Some("TMDL"));
    for args in [
        vec!["bulk-export", "Demo.Workspace", "-o"],
        vec!["import", "Pipe.DataPipeline", "-i"],
        vec!["export", "Pipe.DataPipeline", "-o", "out", "--query", "."],
        vec!["deploy", "--config", "config.yml", "--format", "ipynb"],
    ] {
        assert!(
            Command::new("test")
                .subcommands(transfer_subcommands())
                .try_get_matches_from(std::iter::once("test").chain(args.into_iter()))
                .is_err()
        );
    }
}

#[test]
fn absolute_missing_empty_and_escaping_roots_are_checked_without_creating_data() {
    let directory = Directory::new();
    let mut files = directory.files();
    assert_eq!(
        files
            .resolve_directory(directory.0.join("output").to_str().unwrap())
            .unwrap(),
        directory.0.join("output")
    );
    for input in [
        "",
        "missing",
        "../outside",
        "output/../../outside",
        "abfss://synthetic",
        "~someone",
    ] {
        assert!(files.resolve_directory(input).is_err(), "{input}");
    }
    assert_eq!(
        files.resolve_directory("~/output").unwrap(),
        directory.0.join("output")
    );
}

struct FailedWriter;

impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic stream failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_writer_failure_is_not_success() {
    let result = fabric_cli_transfer::TransferResult {
        outputs: vec![fabric_cli_output::OutputInput {
            message: Some("done".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let command = parse(&["export", "Pipe.DataPipeline", "-o", "output", "-f"]);
    let plan = render_result(
        &result,
        &command,
        timestamp(),
        OutputFormat::Json,
        Presentation::default(),
        &ascii_width,
    )
    .unwrap();
    assert!(write_result(&plan, &mut FailedWriter, &mut Vec::new()).is_err());
}

#[tokio::test]
async fn folder_bulk_uses_selective_request_and_strips_only_ancestors() {
    let directory = Directory::new();
    let original = context(true, true);
    let parent = Element::child(
        "Parent",
        Some(OPERATION.into()),
        NodeKind::Folder,
        original.target.clone(),
    )
    .unwrap();
    let folder =
        Element::child("Selected", Some(OPERATION.into()), NodeKind::Folder, parent).unwrap();
    let item = Element::child(
        "Pipe",
        Some(ITEM.into()),
        NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
        folder.clone(),
    )
    .unwrap();
    let context = TransferContext {
        target: folder,
        items: vec![TransferItem {
            element: item,
            support: CommandSupport::Supported,
        }],
        selection_order: Vec::new(),
    };
    let mut parts = definition()["definition"]["parts"].clone();
    parts[0]["path"] = json!("/Parent/Selected/Pipe.DataPipeline/pipeline-content.json");
    let harness = Harness::new(context, vec![reply(200, json!({"definitionParts": parts}))]);
    let command = parse(&[
        "bulk-export",
        "Demo.Workspace/Parent.Folder/Selected.Folder",
        "-o",
        "output",
        "--recursive",
        "-f",
    ]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    assert_eq!(
        result.completed_paths,
        ["Selected/Pipe.DataPipeline/pipeline-content.json"]
    );
    assert!(
        directory
            .0
            .join("output")
            .join("Selected")
            .join("Pipe.DataPipeline")
            .join("pipeline-content.json")
            .is_file()
    );
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_body(&requests[0]),
        format!("{{\"items\": [{{\"id\": \"{ITEM}\"}}], \"mode\": \"Selective\"}}").as_bytes()
    );
}

#[tokio::test]
async fn workspace_partial_failure_retains_first_item_files_without_batch_success() {
    let directory = Directory::new();
    let mut ctx = context(true, true);
    let second = Element::child(
        "Other",
        Some(OPERATION.into()),
        NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
        ctx.target.clone(),
    )
    .unwrap();
    ctx.selection_order.push(second.name());
    ctx.items.push(TransferItem {
        element: second,
        support: CommandSupport::Supported,
    });
    let harness = Harness::new(
        ctx,
        vec![
            reply(200, json!({"id":ITEM})),
            reply(200, definition()),
            reply(200, json!({"id":OPERATION})),
            reply(
                200,
                json!({"definition": {"parts": [{"path":"../escape.json","payload":"e30=","payloadType":"InlineBase64"}]}}),
            ),
        ],
    );
    let command = parse(&["export", "Demo.Workspace", "-o", "output", "-a", "-f"]);
    let error = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.completed_paths,
        ["Pipe.DataPipeline/pipeline-content.json"]
    );
    assert_file(&directory.0, "export");
    assert!(
        !directory
            .0
            .join("output")
            .join("Other.DataPipeline")
            .exists()
    );
    assert_eq!(harness.http.requests.lock().unwrap().len(), 4);
    let plan = render_error(
        &error,
        &command,
        timestamp(),
        OutputFormat::Json,
        Presentation::default(),
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(write_result(&plan, &mut stdout, &mut stderr).unwrap(), 1);
    assert!(stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["status"],
        "Failure"
    );
}

#[tokio::test]
async fn invalid_format_and_missing_output_fail_without_credentials_or_writes() {
    for (selector, output) in [
        ("not-a-format", "output"),
        ("default", "missing"),
        ("default", "../escape"),
    ] {
        let directory = Directory::new();
        let harness = Harness::new(context(false, true), Vec::new());
        let command = parse(&[
            "export",
            "Demo.Workspace/Pipe.DataPipeline",
            "-o",
            output,
            "--format",
            selector,
            "-f",
        ]);
        assert!(
            harness
                .host(&mut directory.files())
                .execute(&command, pending())
                .await
                .is_err()
        );
        harness.no_requests();
        assert!(
            fs::read_dir(directory.0.join("output"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[tokio::test]
async fn explicit_json_override_controls_stream_even_with_text_configuration() {
    let directory = Directory::new();
    let harness = Harness::new(
        context(false, true),
        vec![reply(200, json!({"id":ITEM})), reply(200, definition())],
    );
    let command = parse(&[
        "export",
        "Demo.Workspace/Pipe.DataPipeline",
        "-o",
        "output",
        "-f",
        "--output_format",
        "json",
    ]);
    let result = harness
        .host(&mut directory.files())
        .execute(&command, pending())
        .await
        .unwrap();
    let plan = render_result(
        &result,
        &command,
        timestamp(),
        OutputFormat::Text,
        Presentation::default(),
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    write_result(&plan, &mut stdout, &mut stderr).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["command"],
        "export"
    );
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn invalid_public_output_selection_and_metadata_bounds_fail_before_mutation() {
    let directory = Directory::new();
    let harness = Harness::new(context(false, false), Vec::new());
    let mut command = parse(&[
        "import",
        "Demo.Workspace/Pipe.DataPipeline",
        "-i",
        "input",
        "-f",
    ]);
    command.output_format = Some("yaml".into());
    assert!(
        harness
            .host(&mut directory.files())
            .execute(&command, pending())
            .await
            .is_err()
    );
    assert_eq!(harness.metadata.calls.load(Ordering::SeqCst), 0);
    harness.no_requests();

    let mut ctx = context(true, true);
    ctx.items.push(ctx.items[0].clone());
    let harness = Harness::new(ctx, Vec::new());
    let command = parse(&["export", "Demo.Workspace", "-o", "output", "-a", "-f"]);
    let mut files = directory.files();
    let mut host = harness.host(&mut files);
    host.definition_limits.max_files = std::num::NonZeroUsize::new(1).unwrap();
    assert!(host.execute(&command, pending()).await.is_err());
    harness.no_requests();
}

#[cfg(unix)]
#[test]
fn linked_directory_and_file_are_rejected_without_following_them() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let mut files = directory.files();
    let linked = directory.0.join("linked");
    symlink(directory.0.join("input"), &linked).unwrap();
    assert!(files.resolve_directory("linked").is_err());
    let linked_file = directory.0.join("output").join("linked.json");
    symlink(
        directory.0.join("input").join("pipeline-content.json"),
        &linked_file,
    )
    .unwrap();
    assert!(files.is_empty(&directory.0.join("output")).is_err());
}
