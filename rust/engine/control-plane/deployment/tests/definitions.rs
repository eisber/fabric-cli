// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public library -> shared SDK policy -> actual HTTP executor -> file I/O.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core::{Audience, CredentialSelection};
use fabric_deploy::definitions::{
    BulkExportSnapshot, BulkItem, BulkScope, CommandSupport, bulk_export, bulk_export_payload,
    filter_bulk_definition, strip_bulk_parent_prefix,
};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionErrorKind as Kind, DefinitionFile, DefinitionFileSystem,
    DefinitionImportFileSystem, DefinitionLimits, DefinitionRead, DefinitionTarget, ImportAction,
    ImportTarget, LocalDefinitionFileSystem, copy_payload, decode_export_files,
    decode_set_definition, encode_set_definition, ensure_notebook_dependency, export_item,
    load_import_definition, prepare_import, read_copy_definition, read_definition,
    read_item_definition, resolve_format,
};
use fabric_sdk_http::{
    ExecutionError, ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    future::{Future, pending, poll_fn},
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn target(item_type: &str) -> DefinitionTarget<'_> {
    DefinitionTarget {
        workspace_id: WORKSPACE,
        item_id: ITEM,
        item_type,
    }
}

fn definition(parts: &[(&str, &[u8])]) -> Value {
    json!({"definition":{"parts":parts.iter().map(|(path, bytes)|json!({
        "path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64"
    })).collect::<Vec<_>>()}})
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
            "synthetic-only-not-a-real-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct ScriptedHttp {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for ScriptedHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or replay")
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

// A caller composition adapter, not a transport/retry/polling implementation.
struct Http<'a>(&'a HttpExecutor);

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

impl OperationExecutor for Http<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
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

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(execution_error) })
    }
}

fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> azure_core::Result<AsyncRawResponse> {
    let mut result = Headers::new();
    for &(name, value) in headers {
        result.insert(name.to_owned(), value.to_owned());
    }
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        result,
        body.as_bytes().to_vec(),
    ))
}

struct Harness {
    credential: Credential,
    selection: CredentialSelection,
    http: Arc<ScriptedHttp>,
    executor: HttpExecutor,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = azure_core::Result<AsyncRawResponse>>) -> Self {
        let http = Arc::new(ScriptedHttp {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        });
        Self {
            credential: Credential::default(),
            selection: CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            executor: HttpExecutor::new(http.clone(), Arc::new(Timer), ExecutionOptions::default())
                .unwrap(),
            http,
        }
    }

    fn session<'a>(
        &'a self,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> OperationSession<'a, Http<'a>> {
        OperationSession::new(
            &self.credential,
            &self.selection,
            Http(&self.executor),
            OperationLimits::default(),
            cancelled,
        )
    }
}

#[derive(Default)]
struct Files {
    written: Vec<String>,
    calls: Vec<&'static str>,
    fail_at: Option<usize>,
    fail_preflight: bool,
    cancel_after: Option<Arc<AtomicBool>>,
}

impl DefinitionFileSystem for Files {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        self.calls.push("preflight");
        if self.fail_preflight {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic preflight failure",
            ));
        }
        Ok(())
    }

    fn create_directory(&mut self, _: &Path, _: &str) -> io::Result<()> {
        self.calls.push("directory");
        Ok(())
    }

    fn write(&mut self, _: &Path, file: &DefinitionFile) -> io::Result<()> {
        self.calls.push("write");
        if self.fail_at == Some(self.written.len()) {
            return Err(io::Error::other("synthetic partial write"));
        }
        self.written.push(file.path.clone());
        if let Some(cancel) = &self.cancel_after {
            cancel.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fabric-definition-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove only this exact synthetic test directory");
    }
}

#[test]
fn new_pinned_source_export_cases_preserve_bytes_and_scalar_distinctions() {
    let fixture: Value =
        serde_json::from_str(include_str!("definition_export_windows.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    assert_eq!(fixture["platform"], "win32");
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 12);
    for case in fixture["cases"].as_array().unwrap() {
        let files = decode_export_files(
            &case["input"],
            case["notebook"].as_bool().unwrap(),
            DefinitionLimits::default(),
        )
        .unwrap();
        let mut expected = STANDARD
            .decode(case["expected_base64"].as_str().unwrap())
            .unwrap();
        if !cfg!(windows) && case["name"] != "binary" {
            let mut native = Vec::new();
            for byte in expected {
                if byte == b'\n' && native.last() == Some(&b'\r') {
                    native.pop();
                }
                native.push(byte);
            }
            expected = native;
        }
        assert_eq!(files.len(), 1, "{}", case["name"]);
        assert_eq!(files[0].path, case["path"].as_str().unwrap());
        assert_eq!(files[0].bytes, expected, "{}", case["name"]);
    }
}

#[test]
fn new_pinned_set_codec_cases_preserve_base64_and_exact_default_json_body() {
    let fixture: Value = serde_json::from_str(include_str!("definition_set_source.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 6);
    for case in fixture["cases"].as_array().unwrap() {
        let decoded =
            decode_set_definition(case["input"].clone(), DefinitionLimits::default()).unwrap();
        assert_eq!(decoded, case["decoded"], "{}", case["name"]);
        let bytes =
            encode_set_definition(case["edited"].clone(), DefinitionLimits::default()).unwrap();
        assert_eq!(
            bytes,
            case["body"].as_str().unwrap().as_bytes(),
            "{}",
            case["name"]
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            case["encoded"]
        );
    }
}

#[test]
fn copy_payload_keeps_raw_parts_null_folder_and_truthy_description_in_source_order() {
    let raw = definition(&[("notebook.ipynb", br#"{"cells":[{"outputs":[1]}]}"#)]);
    let source = DefinitionRead {
        item: json!({"definition":raw["definition"],"description":"source"}),
        definition_access_denied: None,
    };
    let payload = copy_payload(&source, "Notebook", "Dest", None).unwrap();
    assert_eq!(payload["definition"], raw["definition"]);
    assert_eq!(payload["folderId"], Value::Null);
    assert_eq!(payload["description"], "source");
    assert_eq!(
        payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "type",
            "displayName",
            "definition",
            "folderId",
            "description"
        ]
    );
    let mut source = source;
    source.item["description"] = json!("");
    assert!(
        copy_payload(&source, "Notebook", "Dest", None)
            .unwrap()
            .get("description")
            .is_none()
    );
}

#[test]
fn notebook_dependency_initialization_retains_metadata_without_clearing_outputs() {
    let mut item =
        json!({"definition":{"parts":[{"payload":{"metadata":{"existing":null},"outputs":[1]}}]}});
    ensure_notebook_dependency(
        &mut item,
        "definition.parts[0].payload.metadata.dependencies.lakehouse.id",
    )
    .unwrap();
    assert_eq!(
        item.pointer("/definition/parts/0/payload/metadata"),
        Some(&json!({
            "existing":null,"dependencies":{"lakehouse":{}}
        }))
    );
    assert_eq!(
        item.pointer("/definition/parts/0/payload/outputs"),
        Some(&json!([1]))
    );
}

#[tokio::test]
async fn source_get_then_post_exports_real_local_files_and_keeps_raw_copy_definition() {
    let raw = definition(&[
        ("notebook.ipynb", br#"{"cells":[{"outputs":[1]}],"n":1e-5}"#),
        ("assets/image.bin", b"\xff\x00\x80"),
        ("empty.py", b""),
    ]);
    let harness = Harness::new([
        reply(
            200,
            r#"{"id":"original","displayName":"from metadata","description":null}"#,
            &[],
        ),
        reply(200, &raw.to_string(), &[]),
    ]);
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let mut session = harness.session(pending());
    let result = export_item(
        &mut session,
        &mut filesystem,
        target("Notebook"),
        "Context.Name.Notebook",
        &directory.0,
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        result.completed_paths,
        [
            "Context.Name.Notebook/notebook.ipynb",
            "Context.Name.Notebook/assets/image.bin",
            "Context.Name.Notebook/empty.py",
        ]
    );
    assert_eq!(result.read.item["definition"], raw["definition"]);
    let notebook = fs::read_to_string(
        directory
            .0
            .join("Context.Name.Notebook")
            .join("notebook.ipynb"),
    )
    .unwrap();
    let notebook: Value = serde_json::from_str(&notebook).unwrap();
    assert_eq!(notebook["cells"][0]["outputs"], json!([]));
    assert_eq!(
        fs::read(
            directory
                .0
                .join("Context.Name.Notebook")
                .join("assets")
                .join("image.bin")
        )
        .unwrap(),
        b"\xff\x00\x80"
    );
    assert_eq!(
        fs::read(directory.0.join("Context.Name.Notebook").join("empty.py")).unwrap(),
        b""
    );
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WORKSPACE}/items/{ITEM}")
    );
    assert_eq!(requests[1].method(), Method::Post);
    assert_eq!(requests[1].url().query(), Some("format=ipynb"));
    assert!(Bytes::from(requests[1].body()).is_empty());
    assert_eq!(harness.credential.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn copy_reads_omit_format_and_preserve_notebook_outputs_and_metadata_order() {
    let raw = definition(&[("notebook.ipynb", br#"{"cells":[{"outputs":[1]}]}"#)]);
    let harness = Harness::new([
        reply(
            200,
            r#"{"description":"first","displayName":"old","id":"source"}"#,
            &[],
        ),
        reply(
            200,
            &format!(
                r#"{{"displayName":"overridden","definition":{}}}"#,
                raw["definition"]
            ),
            &[],
        ),
    ]);
    let read = read_copy_definition(&mut harness.session(pending()), target("Notebook"))
        .await
        .unwrap();
    assert_eq!(read.item["definition"], raw["definition"]);
    assert_eq!(read.item["displayName"], "overridden");
    assert_eq!(
        read.item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["description", "displayName", "id", "definition"]
    );
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(requests[1].method(), Method::Post);
    assert_eq!(requests[1].url().query(), None);
    assert!(Bytes::from(requests[1].body()).is_empty());
}

#[tokio::test]
async fn get_definition_acceptance_uses_shared_poll_and_actual_result_once() {
    let raw = definition(&[("definition.json", b"{}")]);
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result_location = format!("{location}/result");
    let harness = Harness::new([
        reply(
            202,
            "",
            &[
                ("Location", &location),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "0"),
            ],
        ),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_location)],
        ),
        reply(200, &raw.to_string(), &[]),
    ]);
    let mut session = harness.session(pending());
    let result = read_definition(&mut session, target("Report"), None)
        .await
        .unwrap();
    assert_eq!(result, raw);
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(requests[1].method(), Method::Get);
    assert_eq!(
        requests[2].url().path(),
        format!("/v1/operations/{OPERATION}/result")
    );
}

#[tokio::test]
async fn failed_definition_operation_retains_handle_and_dispatch_evidence() {
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let harness = Harness::new([
        reply(
            202,
            "",
            &[
                ("Location", &location),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "0"),
            ],
        ),
        reply(
            200,
            r#"{"status":"Failed","error":{"errorCode":"Synthetic","message":"private diagnostic"}}"#,
            &[],
        ),
    ]);
    let error = read_definition(&mut harness.session(pending()), target("Report"), None)
        .await
        .unwrap_err();
    let Kind::Operation(operation) = error.kind else {
        panic!("expected shared operation failure");
    };
    assert_eq!(operation.operation_id.as_deref(), Some(OPERATION));
    assert!(operation.mutation_may_have_occurred);
    assert!(matches!(
        operation.kind,
        OperationErrorKind::OperationFailed { .. }
    ));
    assert!(!operation.to_string().contains("private diagnostic"));
    assert_eq!(harness.http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn denied_definition_is_explicit_metadata_fallback_not_export_success() {
    for status in [401, 403] {
        let harness = Harness::new([
            reply(200, r#"{"displayName":"visible"}"#, &[]),
            reply(status, r#"{"errorCode":"Denied"}"#, &[]),
        ]);
        let read = read_item_definition(&mut harness.session(pending()), target("Report"), None)
            .await
            .unwrap();
        assert_eq!(read.definition_access_denied, Some(status));
        assert_eq!(read.item, json!({"displayName":"visible"}));
        let harness = Harness::new([
            reply(200, r#"{"displayName":"visible"}"#, &[]),
            reply(status, r#"{"errorCode":"Denied"}"#, &[]),
        ]);
        let mut files = Files::default();
        let error = export_item(
            &mut harness.session(pending()),
            &mut files,
            target("Report"),
            "Report.Report",
            Path::new("unused"),
            None,
            true,
            DefinitionLimits::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error.kind, Kind::DefinitionAccessDenied(actual) if actual == status));
        assert!(files.calls.is_empty());
    }
}

#[tokio::test]
async fn confirmation_invalid_selector_and_early_cancel_prevent_credentials_and_writes() {
    for mode in ["declined", "invalid-format", "cancelled", "invalid-id"] {
        let harness = Harness::new([]);
        let mut files = Files::default();
        let mut target = target("Notebook");
        if mode == "invalid-id" {
            target.item_id = "../escape";
        }
        let cancelled = mode == "cancelled";
        let cancellation = poll_fn(move |_| {
            if cancelled {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let result = export_item(
            &mut harness.session(cancellation),
            &mut files,
            target,
            "Name.Notebook",
            Path::new("unused"),
            (mode == "invalid-format").then_some("bad"),
            mode != "declined",
            DefinitionLimits::default(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
        assert!(harness.http.requests.lock().unwrap().is_empty());
        assert!(files.calls.is_empty());
    }
}

#[tokio::test]
async fn write_failure_retains_ordered_completed_and_incomplete_paths_without_replay() {
    let raw = definition(&[("z.txt", b"one"), ("a.txt", b"two"), ("last.txt", b"three")]);
    let harness = Harness::new([reply(200, "{}", &[]), reply(200, &raw.to_string(), &[])]);
    let mut files = Files {
        fail_at: Some(1),
        ..Files::default()
    };
    let error = export_item(
        &mut harness.session(pending()),
        &mut files,
        target("Report"),
        "Name.Report",
        Path::new("unused"),
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.kind, Kind::FileSystem(_)));
    assert_eq!(error.completed_paths, ["Name.Report/z.txt"]);
    assert_eq!(error.incomplete_path.as_deref(), Some("Name.Report/a.txt"));
    assert_eq!(files.calls, ["preflight", "directory", "write", "write"]);
    assert_eq!(harness.http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn cancellation_between_local_writes_does_not_claim_rollback_or_completion() {
    let raw = definition(&[("first.txt", b"one"), ("last.txt", b"two")]);
    let harness = Harness::new([reply(200, "{}", &[]), reply(200, &raw.to_string(), &[])]);
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let cancellation = poll_fn(move |_| {
        if signal.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut files = Files {
        cancel_after: Some(cancel),
        ..Files::default()
    };
    let error = export_item(
        &mut harness.session(cancellation),
        &mut files,
        target("Report"),
        "Name.Report",
        Path::new("unused"),
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.kind, Kind::Operation(operation) if operation.kind == OperationErrorKind::Cancelled)
    );
    assert_eq!(error.completed_paths, ["Name.Report/first.txt"]);
    assert_eq!(error.incomplete_path, None);
    assert_eq!(files.written, ["Name.Report/first.txt"]);
}

#[tokio::test]
async fn bare_accepted_malformed_and_failed_operations_never_write_or_claim_success() {
    for replies in [
        vec![reply(200, "{}", &[]), reply(202, "", &[])],
        vec![reply(200, "{}", &[]), reply(200, "[]", &[])],
        vec![
            reply(200, "{}", &[]),
            reply(400, r#"{"errorCode":"BadRequest"}"#, &[]),
        ],
    ] {
        let harness = Harness::new(replies);
        let mut files = Files::default();
        let error = export_item(
            &mut harness.session(pending()),
            &mut files,
            target("Report"),
            "Name.Report",
            Path::new("unused"),
            None,
            true,
            DefinitionLimits::default(),
        )
        .await
        .unwrap_err();
        assert!(error.completed_paths.is_empty());
        assert!(files.calls.is_empty());
    }
}

#[test]
fn formats_match_source_mapping_not_an_unrestricted_query() {
    for (kind, selector, expected) in [
        ("Notebook", None, Some("ipynb")),
        ("Notebook", Some(""), Some("ipynb")),
        ("Notebook", Some(".py"), Some("fabricGitSource")),
        ("SemanticModel", None, None),
        ("SemanticModel", Some("TMDL"), Some("TMDL")),
        ("SparkJobDefinition", None, Some("SparkJobDefinitionV1")),
        ("Report", None, None),
        ("Environment", Some("default"), None),
    ] {
        assert_eq!(resolve_format(kind, selector).unwrap(), expected);
    }
    for (kind, selector) in [
        ("Report", "default"),
        ("Notebook", "IPYNB"),
        ("Report", "x&other=1"),
    ] {
        assert!(resolve_format(kind, Some(selector)).is_err());
    }
}

#[test]
fn unsafe_paths_and_decoded_formatted_file_bounds_fail_before_writes() {
    for path in [
        "../escape",
        "a/../b",
        "C:/escape",
        "a\\b",
        "NUL.txt",
        "file:stream",
        "a//b",
    ] {
        let raw = definition(&[(path, b"{}")]);
        assert!(
            decode_export_files(&raw, false, DefinitionLimits::default()).is_err(),
            "{path}"
        );
    }
    let raw = definition(&[("one.txt", b"123"), ("two.txt", b"45")]);
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(4).unwrap(),
        ..DefinitionLimits::default()
    };
    assert!(matches!(
        decode_export_files(&raw, false, limits).unwrap_err().kind,
        Kind::Limit
    ));
    let raw = definition(&[("one.txt", b"abc")]);
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(3).unwrap(),
        ..DefinitionLimits::default()
    };
    assert_eq!(
        decode_export_files(&raw, false, limits).unwrap()[0].bytes,
        b"abc"
    );
    let raw = definition(&[("one.json", b"{}"), ("ONE.json", b"{}")]);
    assert!(matches!(
        decode_export_files(&raw, false, DefinitionLimits::default())
            .unwrap_err()
            .kind,
        Kind::Collision
    ));
}

#[test]
fn leading_slashes_empty_parts_missing_payload_and_payload_type_follow_source() {
    let raw = json!({"definition":{"parts":[{"path":"///first.json"},{"path":"/second.txt","payload":"","payloadType":"Other"}]}});
    let files = decode_export_files(&raw, false, DefinitionLimits::default()).unwrap();
    assert_eq!(files[0].path, "first.json");
    assert_eq!(files[0].bytes, b"{}");
    assert_eq!(files[1].bytes, b"");
    assert!(
        decode_export_files(
            &json!({"definition":{}}),
            false,
            DefinitionLimits::default()
        )
        .unwrap()
        .is_empty()
    );
    assert!(decode_export_files(&json!({}), false, DefinitionLimits::default()).is_err());
}

#[test]
fn unimplemented_corrupt_payload_and_large_integer_branches_are_explicit() {
    for raw in [
        json!({"definition":{"parts":[{"path":"a.json","payload":"not-base64"}]}}),
        definition(&[("a.json", b"broken-json")]),
        definition(&[("a.json", b"18446744073709551616")]),
    ] {
        assert!(matches!(
            decode_export_files(&raw, false, DefinitionLimits::default())
                .unwrap_err()
                .kind,
            Kind::Unsupported(_)
        ));
    }
}

#[test]
fn ordinary_text_with_large_numeric_tokens_is_not_misclassified_as_lossy_json() {
    let raw = definition(&[("script.py", b"print(18446744073709551616)")]);
    let files = decode_export_files(&raw, false, DefinitionLimits::default()).unwrap();
    assert_eq!(files[0].bytes, b"print(18446744073709551616)");
}

#[test]
fn local_adapter_replaces_existing_regular_files_and_rejects_directory_collisions() {
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let file = DefinitionFile {
        path: "Name.Report/part.txt".into(),
        bytes: b"new".to_vec(),
    };
    filesystem
        .preflight(&directory.0, std::slice::from_ref(&file))
        .unwrap();
    filesystem
        .create_directory(&directory.0, "Name.Report")
        .unwrap();
    filesystem.write(&directory.0, &file).unwrap();
    filesystem.write(&directory.0, &file).unwrap();
    assert_eq!(
        fs::read(directory.0.join("Name.Report").join("part.txt")).unwrap(),
        b"new"
    );
    let collision = DefinitionFile {
        path: "Name.Report/part.txt/sub.txt".into(),
        bytes: vec![],
    };
    assert!(filesystem.preflight(&directory.0, &[collision]).is_err());
}

#[test]
fn local_adapter_round_trips_files_under_the_callers_resolved_root() {
    let directory = Directory::new();
    assert_eq!(fs::canonicalize(&directory.0).unwrap(), directory.0);
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let file = DefinitionFile {
        path: "Resolved.Report/part.txt".into(),
        bytes: b"exact\r\nbytes".to_vec(),
    };
    filesystem
        .preflight(&directory.0, std::slice::from_ref(&file))
        .unwrap();
    filesystem
        .create_directory(&directory.0, "Resolved.Report")
        .unwrap();
    filesystem.write(&directory.0, &file).unwrap();
    let loaded = load_import_definition(
        &mut filesystem,
        &directory.0.join("Resolved.Report"),
        None,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert_eq!(loaded.definition["parts"][0]["path"], "part.txt");
    assert_eq!(
        STANDARD
            .decode(loaded.definition["parts"][0]["payload"].as_str().unwrap())
            .unwrap(),
        file.bytes
    );
}

#[cfg(unix)]
#[test]
fn local_adapter_requires_root_resolution_without_following_links_inside_the_root() {
    use std::os::unix::fs::symlink;

    let directory = Directory::new();
    let outside = Directory::new();
    let root = directory.0.join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("source.txt"), b"source").unwrap();
    let alias = directory.0.join("alias");
    symlink(&root, &alias).unwrap();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let file = DefinitionFile {
        path: "Written.Report/part.txt".into(),
        bytes: b"written".to_vec(),
    };
    let parent_alias = alias.join("..").join("alias");
    for unresolved in [&alias, &parent_alias] {
        assert!(
            filesystem
                .preflight(unresolved, std::slice::from_ref(&file))
                .is_err()
        );
        assert!(
            filesystem
                .create_directory(unresolved, "Written.Report")
                .is_err()
        );
        assert!(filesystem.write(unresolved, &file).is_err());
        assert!(filesystem.entries(unresolved, "").is_err());
        assert!(filesystem.read(unresolved, "source.txt", 1024).is_err());
    }
    assert!(!root.join("Written.Report").exists());

    let resolved = fs::canonicalize(&alias).unwrap();
    assert_eq!(resolved, root);
    filesystem
        .preflight(&resolved, std::slice::from_ref(&file))
        .unwrap();
    filesystem
        .create_directory(&resolved, "Written.Report")
        .unwrap();
    filesystem.write(&resolved, &file).unwrap();
    assert_eq!(
        filesystem.read(&resolved, &file.path, 1024).unwrap(),
        file.bytes
    );

    fs::write(outside.0.join("source.txt"), b"outside").unwrap();
    symlink(&outside.0, resolved.join("linked-directory")).unwrap();
    symlink(
        outside.0.join("source.txt"),
        resolved.join("linked-file.txt"),
    )
    .unwrap();
    for relative in ["linked-directory/source.txt", "linked-file.txt"] {
        let linked = DefinitionFile {
            path: relative.into(),
            bytes: b"must not write".to_vec(),
        };
        assert!(
            filesystem
                .preflight(&resolved, std::slice::from_ref(&linked))
                .is_err()
        );
        assert!(filesystem.write(&resolved, &linked).is_err());
        assert!(filesystem.read(&resolved, relative, 1024).is_err());
    }
    assert!(
        filesystem
            .create_directory(&resolved, "linked-directory")
            .is_err()
    );
    assert!(filesystem.entries(&resolved, "").is_err());
    assert!(filesystem.entries(&resolved, "linked-directory").is_err());
    assert_eq!(fs::read(outside.0.join("source.txt")).unwrap(), b"outside");
}

#[tokio::test]
async fn empty_definition_still_creates_context_item_directory() {
    let harness = Harness::new([
        reply(200, "{}", &[]),
        reply(200, r#"{"definition":{"parts":[]}}"#, &[]),
    ]);
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let result = export_item(
        &mut harness.session(pending()),
        &mut filesystem,
        target("Report"),
        "Empty.Report",
        &directory.0,
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap();
    assert!(result.completed_paths.is_empty());
    assert!(directory.0.join("Empty.Report").is_dir());
}

#[tokio::test]
async fn preflight_failure_performs_no_directory_creation_or_file_write() {
    let raw = definition(&[("a.txt", b"source")]);
    let harness = Harness::new([reply(200, "{}", &[]), reply(200, &raw.to_string(), &[])]);
    let mut files = Files {
        fail_preflight: true,
        ..Files::default()
    };
    let error = export_item(
        &mut harness.session(pending()),
        &mut files,
        target("Report"),
        "Name.Report",
        Path::new("unused"),
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.kind, Kind::FileSystem(_)));
    assert!(error.completed_paths.is_empty());
    assert_eq!(error.incomplete_path, None);
    assert_eq!(files.calls, ["preflight"]);
}

#[tokio::test]
async fn cancellation_after_last_write_retains_written_file_without_reporting_completion() {
    let raw = definition(&[("last.txt", b"source")]);
    let harness = Harness::new([reply(200, "{}", &[]), reply(200, &raw.to_string(), &[])]);
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let cancellation = poll_fn(move |_| {
        if signal.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut files = Files {
        cancel_after: Some(cancel),
        ..Files::default()
    };
    let error = export_item(
        &mut harness.session(cancellation),
        &mut files,
        target("Report"),
        "Name.Report",
        Path::new("unused"),
        None,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.kind, Kind::Operation(operation) if operation.kind == OperationErrorKind::Cancelled)
    );
    assert_eq!(error.completed_paths, ["Name.Report/last.txt"]);
    assert_eq!(files.written, ["Name.Report/last.txt"]);
}

#[test]
fn local_adapter_checks_empty_directory_alias_and_parent_before_creation() {
    let directory = Directory::new();
    fs::create_dir(directory.0.join("existing.Report")).unwrap();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    assert!(
        filesystem
            .create_directory(&directory.0, "EXISTING.Report")
            .is_err()
    );
    fs::write(directory.0.join("blocked"), b"file").unwrap();
    let file = DefinitionFile {
        path: "blocked/child/part.txt".into(),
        bytes: b"source".to_vec(),
    };
    assert!(filesystem.write(&directory.0, &file).is_err());
    assert_eq!(fs::read(directory.0.join("blocked")).unwrap(), b"file");
}

#[test]
fn repeated_exact_part_path_preserves_source_overwrite_order_but_parent_case_aliases_fail() {
    let raw = definition(&[("same.txt", b"first"), ("same.txt", b"second")]);
    let files = decode_export_files(&raw, false, DefinitionLimits::default()).unwrap();
    assert_eq!(files[0].bytes, b"first");
    assert_eq!(files[1].bytes, b"second");
    let raw = definition(&[("folder/a.txt", b"a"), ("FOLDER/b.txt", b"b")]);
    assert!(matches!(
        decode_export_files(&raw, false, DefinitionLimits::default())
            .unwrap_err()
            .kind,
        Kind::Collision
    ));
}

fn import_target(item_id: Option<&str>) -> ImportTarget<'_> {
    ImportTarget {
        workspace_id: WORKSPACE,
        item_id,
        item_type: "Notebook",
        display_name: "Synthetic",
        folder_id: None,
    }
}

#[test]
fn actual_pinned_import_cases_preserve_file_order_binary_newlines_report_checks_and_cache_exclusion()
 {
    let fixture: Value =
        serde_json::from_str(include_str!("definition_import_source.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 7);
    for case in fixture["cases"].as_array().unwrap() {
        let directory = Directory::new();
        for file in case["files"].as_array().unwrap() {
            let path = directory.0.join(file["path"].as_str().unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                STANDARD.decode(file["bytes"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        }
        let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
        let result = load_import_definition(
            &mut filesystem,
            &directory.0,
            case["format"].as_str(),
            DefinitionLimits::default(),
        );
        if case.get("error").is_some() {
            assert!(
                matches!(result.unwrap_err().kind, Kind::Unsupported(_)),
                "{}",
                case["name"]
            );
        } else {
            let result = result.unwrap();
            assert_eq!(result.definition, case["definition"], "{}", case["name"]);
            assert_eq!(
                fabric_core_json::json_dumps(&json!({"definition":result.definition})),
                case["update_body"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            if case["name"] == "ordered" {
                assert_eq!(result.excluded_paths, ["cache.abf", "sub-a/cache.abf.copy"]);
            }
            if case["name"] == "notebook-format" {
                let prepared = prepare_import(
                    &mut filesystem,
                    &directory.0,
                    import_target(None),
                    Some(".ipynb"),
                    true,
                    DefinitionLimits::default(),
                )
                .unwrap();
                assert_eq!(
                    prepared.body(),
                    case["create_body"].as_str().unwrap().as_bytes()
                );
            }
        }
    }
}

#[tokio::test]
async fn local_import_create_and_definition_only_update_use_actual_sdk_http_and_exact_bodies() {
    let directory = Directory::new();
    fs::write(directory.0.join("notebook.ipynb"), b"{\"cells\":[]}\r\n").unwrap();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    for existing in [None, Some(ITEM)] {
        let prepared = prepare_import(
            &mut filesystem,
            &directory.0,
            import_target(existing),
            None,
            true,
            DefinitionLimits::default(),
        )
        .unwrap();
        let expected = prepared.body().to_vec();
        let harness = Harness::new([reply(
            if existing.is_some() { 204 } else { 201 },
            if existing.is_some() {
                ""
            } else {
                r#"{"id":"22222222-2222-2222-2222-222222222222"}"#
            },
            &[],
        )]);
        let result = prepared
            .execute(&mut harness.session(pending()))
            .await
            .unwrap();
        assert_eq!(result.item_id, ITEM);
        assert_eq!(
            result.action,
            if existing.is_some() {
                ImportAction::Update
            } else {
                ImportAction::Create
            }
        );
        let requests = harness.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Post);
        assert_eq!(Bytes::from(requests[0].body()).as_ref(), expected);
        let body: Value = serde_json::from_slice(&expected).unwrap();
        assert_eq!(body["definition"]["format"], "ipynb");
        let path = if existing.is_some() {
            assert_eq!(body.as_object().unwrap().len(), 1);
            format!("/v1/workspaces/{WORKSPACE}/items/{ITEM}/updateDefinition")
        } else {
            assert_eq!(
                body.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["type", "folderId", "displayName", "definition"]
            );
            assert_eq!(body["folderId"], Value::Null);
            format!("/v1/workspaces/{WORKSPACE}/items")
        };
        assert_eq!(requests[0].url().path(), path);
        assert_eq!(requests[0].url().query(), None);
    }
}

#[tokio::test]
async fn local_import_create_acceptance_retrieves_actual_result_and_never_replays_post() {
    let directory = Directory::new();
    fs::write(directory.0.join("part.txt"), b"source").unwrap();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let prepared = prepare_import(
        &mut filesystem,
        &directory.0,
        import_target(None),
        None,
        true,
        DefinitionLimits::default(),
    )
    .unwrap();
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result_location = format!("{location}/result");
    let harness = Harness::new([
        reply(
            202,
            "",
            &[
                ("Location", &location),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "0"),
            ],
        ),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_location)],
        ),
        reply(200, &json!({"id":ITEM}).to_string(), &[]),
    ]);
    let result = prepared
        .execute(&mut harness.session(pending()))
        .await
        .unwrap();
    assert_eq!(result.item_id, ITEM);
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
    assert_eq!(requests.len(), 3);
}

struct NoImportReads;

impl DefinitionImportFileSystem for NoImportReads {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        panic!("invalid/unsupported preparation must not touch filesystem")
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        panic!("invalid/unsupported preparation must not touch filesystem")
    }
}

#[test]
fn import_confirmation_invalid_target_and_environment_lifecycle_fail_before_file_or_service_work() {
    let mut filesystem = NoImportReads;
    assert!(matches!(
        prepare_import(
            &mut filesystem,
            Path::new("unused"),
            import_target(None),
            None,
            false,
            DefinitionLimits::default()
        )
        .unwrap_err()
        .kind,
        Kind::ConfirmationRequired
    ));
    let target = ImportTarget {
        item_type: "Environment",
        ..import_target(None)
    };
    assert!(matches!(
        prepare_import(
            &mut filesystem,
            Path::new("unused"),
            target,
            None,
            true,
            DefinitionLimits::default()
        )
        .unwrap_err()
        .kind,
        Kind::Unsupported(_)
    ));
    let target = ImportTarget {
        workspace_id: "../escape",
        ..import_target(None)
    };
    assert!(matches!(
        prepare_import(
            &mut filesystem,
            Path::new("unused"),
            target,
            None,
            true,
            DefinitionLimits::default()
        )
        .unwrap_err()
        .kind,
        Kind::InvalidInput
    ));
    assert!(
        prepare_import(
            &mut filesystem,
            Path::new("unused"),
            import_target(None),
            Some("invalid"),
            true,
            DefinitionLimits::default()
        )
        .is_err()
    );
}

#[tokio::test]
async fn import_cancel_accepted_malformed_and_lost_post_keep_uncertainty_without_retry() {
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    for response in [
        reply(202, "", &[]),
        reply(201, "{}", &[]),
        reply(204, "", &[]),
        Err(azure_core::Error::with_message(
            azure_core::error::ErrorKind::Other,
            "synthetic lost POST",
        )),
    ] {
        let prepared = prepare_import(
            &mut filesystem,
            &directory.0,
            import_target(None),
            None,
            true,
            DefinitionLimits::default(),
        )
        .unwrap();
        let harness = Harness::new([response]);
        let error = prepared
            .execute(&mut harness.session(pending()))
            .await
            .unwrap_err();
        let Kind::Operation(operation) = error.kind else {
            panic!("expected shared operation evidence");
        };
        assert!(operation.mutation_may_have_occurred);
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    let prepared = prepare_import(
        &mut filesystem,
        &directory.0,
        import_target(None),
        None,
        true,
        DefinitionLimits::default(),
    )
    .unwrap();
    let harness = Harness::new([]);
    let error = prepared
        .execute(&mut harness.session(std::future::ready(())))
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind, Kind::Operation(operation) if operation.kind == OperationErrorKind::Cancelled
        && !operation.mutation_may_have_occurred)
    );
    assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
}

#[test]
fn import_input_and_request_bounds_are_explicit_and_do_not_ignore_files() {
    let directory = Directory::new();
    fs::write(directory.0.join("part.txt"), b"abcd").unwrap();
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(3).unwrap(),
        ..DefinitionLimits::default()
    };
    let mut filesystem = LocalDefinitionFileSystem::new(limits);
    assert!(load_import_definition(&mut filesystem, &directory.0, None, limits).is_err());
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(100).unwrap(),
        ..DefinitionLimits::default()
    };
    let mut filesystem = LocalDefinitionFileSystem::new(limits);
    assert!(matches!(
        prepare_import(
            &mut filesystem,
            &directory.0,
            import_target(None),
            None,
            true,
            limits
        )
        .unwrap_err()
        .kind,
        Kind::Limit
    ));
}

#[tokio::test]
async fn export_and_prepared_import_futures_are_send_with_caller_owned_file_operations() {
    fn assert_send<T: Send>(value: T) -> T {
        value
    }
    let raw = definition(&[("a.txt", b"source")]);
    let harness = Harness::new([reply(200, "{}", &[]), reply(200, &raw.to_string(), &[])]);
    let mut files = Files::default();
    assert_send(export_item(
        &mut harness.session(pending()),
        &mut files,
        target("Report"),
        "Name.Report",
        Path::new("unused"),
        None,
        true,
        DefinitionLimits::default(),
    ))
    .await
    .unwrap();
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let prepared = prepare_import(
        &mut filesystem,
        &directory.0,
        import_target(Some(ITEM)),
        None,
        true,
        DefinitionLimits::default(),
    )
    .unwrap();
    let harness = Harness::new([reply(204, "", &[])]);
    assert_send(prepared.execute(&mut harness.session(pending())))
        .await
        .unwrap();
}

fn bulk_items() -> Vec<BulkItem> {
    vec![
        BulkItem {
            id: WORKSPACE.into(),
            item_type: "Notebook".into(),
            support: CommandSupport::Supported,
        },
        BulkItem {
            id: ITEM.into(),
            item_type: "Unknown".into(),
            support: CommandSupport::Unsupported,
        },
        BulkItem {
            id: OPERATION.into(),
            item_type: "Notebook".into(),
            support: CommandSupport::Supported,
        },
    ]
}

fn bulk_snapshot(items: &[BulkItem], scope: BulkScope) -> BulkExportSnapshot<'_> {
    BulkExportSnapshot {
        workspace_id: WORKSPACE,
        from_path: "WS.Workspace",
        scope,
        items,
    }
}

#[test]
fn pinned_bulk_source_requests_index_prefix_filter_and_parent_path_cases_match() {
    let fixture: Value = serde_json::from_str(include_str!("definition_bulk_source.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    assert_eq!(
        bulk_export_payload(&[], DefinitionLimits::default()).unwrap(),
        fixture["all_body"].as_str().unwrap().as_bytes()
    );
    let items = bulk_items();
    assert_eq!(
        bulk_export_payload(
            &[items[0].id.clone(), items[1].id.clone()],
            DefinitionLimits::default()
        )
        .unwrap(),
        fixture["selective_body"].as_str().unwrap().as_bytes()
    );
    let mut response = fixture["workspace_filter"]["input"].clone();
    let (supported, skipped) = filter_bulk_definition(
        bulk_snapshot(&items, BulkScope::Workspace),
        &mut response,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert_eq!(response, fixture["workspace_filter"]["output"]);
    assert_eq!(
        json!(supported),
        fixture["workspace_filter"]["supported_ids"]
    );
    assert_eq!(
        json!(skipped),
        fixture["workspace_filter"]["unsupported_ids"]
    );
    for case in fixture["paths"].as_array().unwrap() {
        let mut response = case["input"].clone();
        let result = strip_bulk_parent_prefix(&mut response, case["from_path"].as_str().unwrap());
        if case.get("error").is_some() {
            assert!(result.is_err());
        } else {
            result.unwrap();
            assert_eq!(response, case["output"]);
        }
    }
}

#[tokio::test]
async fn actual_workspace_bulk_all_http_filters_unknown_unsupported_and_missing_items_then_writes()
{
    let fixture: Value = serde_json::from_str(include_str!("definition_bulk_source.json")).unwrap();
    let response = fixture["workspace_filter"]["input"].clone();
    let harness = Harness::new([reply(200, &response.to_string(), &[])]);
    let items = bulk_items();
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let outcome = bulk_export(
        &mut harness.session(pending()),
        &mut filesystem,
        bulk_snapshot(&items, BulkScope::Workspace),
        &directory.0,
        true,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.supported_ids, [WORKSPACE]);
    assert_eq!(outcome.skipped_ids, [ITEM, OPERATION]);
    assert_eq!(outcome.completed_paths, ["Folder/A.Notebook/a.txt"]);
    assert_eq!(
        fs::read(directory.0.join("Folder").join("A.Notebook").join("a.txt")).unwrap(),
        b"a"
    );
    assert!(!directory.0.join("Unsupported.Item").exists());
    assert!(!directory.0.join("Unknown.Item").exists());
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WORKSPACE}/items/bulkExportDefinitions")
    );
    assert_eq!(requests[0].url().query(), Some("beta=true"));
    assert_eq!(
        Bytes::from(requests[0].body()).as_ref(),
        fixture["all_body"].as_str().unwrap().as_bytes()
    );
}

#[tokio::test]
async fn folder_bulk_selective_keeps_target_folder_and_notebook_outputs_without_single_export_cleanup()
 {
    let mut items = bulk_items();
    items[2].support = CommandSupport::Rejected;
    let snapshot = BulkExportSnapshot {
        from_path: "WS.Workspace/Parent.Folder/Child.Folder",
        ..bulk_snapshot(&items, BulkScope::Folder)
    };
    let raw = json!({"definitionParts":[{
        "path":"/Parent/Child/Report.Notebook/notebook.ipynb",
        "payload":STANDARD.encode(br#"{"cells":[{"outputs":[1]}]}"#)
    }]});
    let harness = Harness::new([reply(200, &raw.to_string(), &[])]);
    let directory = Directory::new();
    let mut filesystem = LocalDefinitionFileSystem::new(DefinitionLimits::default());
    let result = bulk_export(
        &mut harness.session(pending()),
        &mut filesystem,
        snapshot,
        &directory.0,
        true,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.supported_ids, [WORKSPACE]);
    assert_eq!(result.skipped_ids, [OPERATION]);
    assert_eq!(
        result.completed_paths,
        ["Child/Report.Notebook/notebook.ipynb"]
    );
    let bytes = fs::read(
        directory
            .0
            .join("Child")
            .join("Report.Notebook")
            .join("notebook.ipynb"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["cells"][0]["outputs"],
        json!([1])
    );
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(
        Bytes::from(requests[0].body()).as_ref(),
        bulk_export_payload(&[WORKSPACE.into()], DefinitionLimits::default()).unwrap()
    );
}

#[tokio::test]
async fn bulk_recursive_confirmation_empty_and_unsupported_folder_fail_before_credentials() {
    let items = bulk_items();
    for (recursive, confirmed) in [(false, true), (true, false)] {
        let harness = Harness::new([]);
        let mut files = Files::default();
        assert!(
            bulk_export(
                &mut harness.session(pending()),
                &mut files,
                bulk_snapshot(&items, BulkScope::Workspace),
                Path::new("unused"),
                recursive,
                confirmed,
                DefinitionLimits::default()
            )
            .await
            .is_err()
        );
        assert!(harness.http.requests.lock().unwrap().is_empty());
        assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
    }
    for items in [
        vec![],
        vec![BulkItem {
            id: WORKSPACE.into(),
            item_type: "Notebook".into(),
            support: CommandSupport::Rejected,
        }],
    ] {
        let harness = Harness::new([]);
        let mut files = Files::default();
        assert!(
            bulk_export(
                &mut harness.session(pending()),
                &mut files,
                bulk_snapshot(&items, BulkScope::Folder),
                Path::new("unused"),
                true,
                true,
                DefinitionLimits::default()
            )
            .await
            .is_err()
        );
        assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn bulk_parent_mismatch_and_bare_acceptance_never_write_and_partial_failure_keeps_progress() {
    let items = [BulkItem {
        id: WORKSPACE.into(),
        item_type: "Notebook".into(),
        support: CommandSupport::Supported,
    }];
    let snapshot = BulkExportSnapshot {
        from_path: "WS.Workspace/Parent.Folder/Child.Folder",
        ..bulk_snapshot(&items, BulkScope::Folder)
    };
    let mismatch = json!({"definitionParts":[{"path":"/Wrong/Child/a.txt","payload":"YQ=="}]});
    for response in [reply(200, &mismatch.to_string(), &[]), reply(202, "", &[])] {
        let harness = Harness::new([response]);
        let mut files = Files::default();
        assert!(
            bulk_export(
                &mut harness.session(pending()),
                &mut files,
                snapshot,
                Path::new("unused"),
                true,
                true,
                DefinitionLimits::default()
            )
            .await
            .is_err()
        );
        assert!(files.calls.is_empty());
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    let response = json!({"definitionParts":[
        {"path":"/Parent/Child/a.txt","payload":"YQ=="},
        {"path":"/Parent/Child/b.txt","payload":"Yg=="}
    ]});
    let harness = Harness::new([reply(200, &response.to_string(), &[])]);
    let mut files = Files {
        fail_at: Some(1),
        ..Files::default()
    };
    let error = bulk_export(
        &mut harness.session(pending()),
        &mut files,
        snapshot,
        Path::new("unused"),
        true,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.completed_paths, ["Child/a.txt"]);
    assert_eq!(error.incomplete_path.as_deref(), Some("Child/b.txt"));
}

#[tokio::test]
async fn bulk_lro_uses_actual_shared_result_without_replaying_bulk_post() {
    let items = [BulkItem {
        id: WORKSPACE.into(),
        item_type: "Notebook".into(),
        support: CommandSupport::Supported,
    }];
    let response = json!({"definitionParts":[{"path":"/N.Notebook/a.txt","payload":"YQ=="}]});
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result_location = format!("{location}/result");
    let harness = Harness::new([
        reply(
            202,
            "",
            &[
                ("Location", &location),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "0"),
            ],
        ),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_location)],
        ),
        reply(200, &response.to_string(), &[]),
    ]);
    let mut files = Files::default();
    let result = bulk_export(
        &mut harness.session(pending()),
        &mut files,
        bulk_snapshot(&items, BulkScope::Folder),
        Path::new("unused"),
        true,
        true,
        DefinitionLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.completed_paths, ["N.Notebook/a.txt"]);
    let requests = harness.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
}
