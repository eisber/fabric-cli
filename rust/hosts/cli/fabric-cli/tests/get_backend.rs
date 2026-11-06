// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public get-head composition through actual SDK operations and offline transport.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::{
    get_backend::SdkGetBackend,
    get_command::{GetCommandError, PreparedGet, parse_get},
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    documents::{
        Coverage, DocEdge, DocPage, DocRequest, DocsKey, DocumentsFuture, DocumentsProvider,
        EdgeDisposition, ProviderError, ProviderErrorKind,
    },
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeExecutionError, OneLakeExecutor, OneLakeRequest, OneLakeResponse,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    future::{Future, pending},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);

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
            .extend(scopes.iter().map(|value| (*value).to_owned()));
        Ok(AccessToken::new(
            "offline-get-composition",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Reply {
    path: String,
    method: &'static str,
    status: u16,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Reply>>,
    calls: Mutex<Vec<(String, String)>>,
    waits: Mutex<Vec<Duration>>,
    block_item: AtomicBool,
    item_started: tokio::sync::Notify,
}

impl Executor {
    fn response(
        &self,
        method: &'static str,
        path: impl Into<String>,
        status: u16,
        body: &[u8],
        headers: &[(&str, String)],
    ) {
        self.replies.lock().unwrap().push_back(Reply {
            method,
            path: path.into(),
            status,
            body: body.to_vec(),
            headers: headers
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone()))
                .collect(),
        });
    }

    fn take(&self, method: &str, path: &str) -> Reply {
        self.calls
            .lock()
            .unwrap()
            .push((method.into(), path.into()));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected remote operation");
        assert_eq!(method, reply.method);
        assert_eq!(path, reply.path);
        reply
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            if self.block_item.load(Ordering::SeqCst) {
                self.calls
                    .lock()
                    .unwrap()
                    .push((request.method().into(), request.url().path().into()));
                self.item_started.notify_one();
                return pending().await;
            }
            let reply = self.take(request.method(), request.url().path());
            Ok(ItemResponse {
                status: reply.status,
                body: reply.body,
                headers: reply.headers,
            })
        })
    }
}

impl WorkspaceExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let reply = self.take(request.method(), request.url().path());
            Ok(WorkspaceResponse {
                status: reply.status,
                body: reply.body,
                headers: reply.headers,
            })
        })
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let method = format!("{:?}", request.method()).to_ascii_uppercase();
            let reply = self.take(&method, request.url().path());
            Ok(OperationResponse {
                status: reply.status,
                body: reply.body,
                headers: reply.headers,
            })
        })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(async { Ok(()) })
    }
}

impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let method = format!("{:?}", request.method()).to_ascii_uppercase();
            let reply = self.take(&method, request.url().path());
            Ok(OneLakeResponse {
                status: reply.status,
                body: reply.body,
                headers: reply.headers,
            })
        })
    }
}

struct Fixture {
    root: PathBuf,
    credential: Arc<Credential>,
    client: Client,
    storage: OneLakeClient,
    executor: Executor,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fabric-get-backend-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let credential = Arc::new(Credential::default());
        Self {
            root,
            client: Client::new(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            ),
            storage: OneLakeClient::new(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
            ),
            credential,
            executor: Executor::default(),
        }
    }

    fn backend(&self) -> SdkGetBackend<'_> {
        SdkGetBackend {
            client: &self.client,
            items: &self.executor,
            workspaces: &self.executor,
            operations: &self.executor,
            storage: Some((&self.storage, &self.executor)),
            documents: None,
        }
    }

    fn prepared(&self, args: &[String]) -> PreparedGet {
        let command = parse_get(args.iter().map(OsString::from)).unwrap();
        PreparedGet::preflight(command, &self.root).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).expect("only this test-owned directory");
    }
}

fn path(suffix: &str) -> String {
    format!("/workspaces/{WS}/items/{ITEM}{suffix}")
}

async fn stdout(fixture: &Fixture, args: &[String]) -> Result<Vec<u8>, GetCommandError> {
    let delivery = fixture
        .prepared(args)
        .acquire(&fixture.backend(), pending())
        .await?;
    let mut bytes = Vec::new();
    delivery.write_to(&mut bytes)?;
    Ok(bytes)
}

#[tokio::test]
async fn metadata_projection_keeps_json_types_and_never_fetches_source() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/items/{ITEM}"),
        200,
        format!(r#"{{"id":"{ITEM}","displayName":"A","enabled":false}}"#).as_bytes(),
        &[],
    );
    let bytes = stdout(
        &f,
        &["get".into(), path(""), "--project".into(), "enabled".into()],
    )
    .await
    .unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["data"], false);
    assert_eq!(result["meta"]["outcome"], "completed");
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn guid_qualified_owner_type_is_checked_before_metadata_or_representation_delivery() {
    for suffix in ["", "/src/definition", "/Files/test.bin"] {
        for actual_type in [Some("SemanticModel"), None] {
            let f = Fixture::new();
            let mut data = serde_json::json!({"id": ITEM, "displayName": "Wrong owner"});
            if let Some(actual_type) = actual_type {
                data["type"] = actual_type.into();
            }
            f.executor.response(
                "GET",
                format!("/v1/workspaces/{WS}/items/{ITEM}"),
                200,
                &serde_json::to_vec(&data).unwrap(),
                &[],
            );
            let result = stdout(
                &f,
                &[
                    "get".into(),
                    format!("/{WS}.Workspace/{ITEM}.Notebook{suffix}"),
                ],
            )
            .await;
            assert!(matches!(
                result,
                Err(GetCommandError::Backend {
                    code: "TargetResolutionFailed",
                    ..
                })
            ));
            assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
            assert_eq!(
                f.credential.0.lock().unwrap().as_slice(),
                ["https://api.fabric.microsoft.com/.default"]
            );
        }
    }
}

#[tokio::test]
async fn guid_qualified_metadata_reuses_the_authoritative_type_check() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/items/{ITEM}"),
        200,
        format!(r#"{{"id":"{ITEM}","type":"Notebook","displayName":"Book"}}"#).as_bytes(),
        &[],
    );
    let bytes = stdout(
        &f,
        &["get".into(), format!("/{WS}.Workspace/{ITEM}.Notebook")],
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["data"]["type"],
        "Notebook"
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancelling_guid_owner_verification_never_fetches_its_representation() {
    let f = Fixture::new();
    f.executor.block_item.store(true, Ordering::SeqCst);
    let args = [
        "get".into(),
        format!("/{WS}.Workspace/{ITEM}.Notebook/src/definition"),
    ];
    let prepared = f.prepared(&args);
    let backend = f.backend();
    let result = prepared
        .acquire(&backend, async {
            f.executor.item_started.notified().await;
            fabric_cli::get_command::GetInterruption::Cancelled
        })
        .await;
    assert!(matches!(
        result,
        Err(GetCommandError::Interrupted(
            fabric_cli::get_command::GetInterruption::Cancelled
        ))
    ));
    let calls = f.executor.calls.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        &[("GET".into(), format!("/v1/workspaces/{WS}/items/{ITEM}"))]
    );
}

#[tokio::test]
async fn accepted_definition_is_observed_without_replaying_and_commits_exact_part() {
    let f = Fixture::new();
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/items/{ITEM}/getDefinition"),
        202,
        b"",
        &[
            ("x-ms-operation-id", OP.into()),
            (
                "Location",
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
        ],
    );
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}"),
        200,
        br#"{"status":"Succeeded"}"#,
        &[(
            "Location",
            format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
        )],
    );
    f.executor.response("GET",format!("/v1/operations/{OP}/result"),200,
        br#"{"definition":{"parts":[{"path":"notebook.ipynb","payload":"AAEC/w==","payloadType":"InlineBase64"}]}}"#,&[]);
    let out = f.root.join("exact.native");
    let args = [
        "get".into(),
        path("/src/definition/parts/notebook.ipynb"),
        "--wait".into(),
        "--format".into(),
        "raw".into(),
        "--output".into(),
        out.to_string_lossy().into_owned(),
    ];
    let delivery = f
        .prepared(&args)
        .acquire(&f.backend(), pending())
        .await
        .unwrap();
    assert!(!out.exists());
    delivery.commit_file().unwrap();
    assert_eq!(std::fs::read(out).unwrap(), [0, 1, 2, 255]);
    let calls = f.executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls.iter().filter(|(method, _)| method == "POST").count(),
        1
    );
}

#[tokio::test]
async fn acceptance_without_wait_retains_handle_and_never_creates_source_file() {
    let f = Fixture::new();
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/items/{ITEM}/getDefinition"),
        202,
        b"",
        &[
            ("x-ms-operation-id", OP.into()),
            (
                "Location",
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
        ],
    );
    let out = f.root.join("not-completed.json");
    let args = [
        "get".into(),
        path("/src/definition"),
        "--output".into(),
        out.to_string_lossy().into_owned(),
    ];
    let error = f
        .prepared(&args)
        .acquire(&f.backend(), pending())
        .await
        .unwrap_err();
    assert!(
        matches!(error,GetCommandError::Incomplete{data} if data["data"]["handle"]==format!("/operations/{OP}") && data["meta"]["outcome"]=="accepted")
    );
    assert!(!out.exists());
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn definition_and_observation_consume_one_response_budget() {
    let f = Fixture::new();
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/items/{ITEM}/getDefinition"),
        202,
        b"",
        &[
            ("x-ms-operation-id", OP.into()),
            (
                "Location",
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
        ],
    );
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}"),
        200,
        br#"{"status":"Running"}"#,
        &[("Retry-After", "1".into())],
    );
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}"),
        200,
        br#"{"status":"Succeeded"}"#,
        &[(
            "Location",
            format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
        )],
    );
    let error = stdout(
        &f,
        &[
            "get".into(),
            path("/src/definition"),
            "--wait".into(),
            "--max-bytes".into(),
            "30".into(),
        ],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, GetCommandError::Backend { .. }));
    assert_eq!(f.executor.calls.lock().unwrap().len(), 3);
    assert!(f.executor.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn exact_file_bytes_and_storage_audience_survive_without_json_overhead() {
    let f = Fixture::new();
    let bytes = [0, 255, 1, 2, 3, 4, 0, 9];
    f.executor.response(
        "GET",
        format!("/{WS}/{ITEM}/Files/test.bin"),
        200,
        &bytes,
        &[("Content-Type", "application/octet-stream".into())],
    );
    let result = stdout(
        &f,
        &[
            "get".into(),
            path("/Files/test.bin"),
            "--max-bytes".into(),
            "8".into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(result, bytes);
    assert_eq!(
        f.credential.0.lock().unwrap().as_slice(),
        ["https://storage.azure.com/.default"]
    );
}

#[tokio::test]
async fn file_read_requires_completed_http_200_not_compatibility_created_status() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/{WS}/{ITEM}/Files/test.bin"),
        201,
        b"not-a-read",
        &[],
    );
    let result = stdout(&f, &["get".into(), path("/Files/test.bin")]).await;
    assert!(matches!(result, Err(GetCommandError::Backend { .. })));
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn no_docs_provider_and_unadvertised_facets_fail_without_credentials() {
    let f = Fixture::new();
    for suffix in [
        "/docs/schema",
        "/src/context/topics/a",
        "/src/owl",
        "/settings/credentials",
    ] {
        assert!(stdout(&f, &["get".into(), path(suffix)]).await.is_err());
    }
    assert!(f.credential.0.lock().unwrap().is_empty());
    assert!(f.executor.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_metadata_format_and_wait_do_not_retrieve_content() {
    let f = Fixture::new();
    for flag in [vec!["--format".into(), "raw".into()], vec!["--wait".into()]] {
        let mut args = vec!["get".into(), path("")];
        args.extend(flag);
        assert!(stdout(&f, &args).await.is_err());
    }
    assert!(f.credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ambiguous_names_never_pick_a_first_candidate() {
    let f = Fixture::new();
    f.executor.response("GET","/v1/workspaces",200,
        br#"{"value":[{"id":"one","displayName":"Sales","type":"Workspace"},{"id":"two","displayName":"Sales","type":"Workspace"}]}"#,&[]);
    let error = stdout(&f, &["get".into(), "/Sales.Workspace/Report.Report".into()])
        .await
        .unwrap_err();
    assert!(
        matches!(error,GetCommandError::Backend{source,..} if matches!(
            source.downcast_ref::<fabric_cli::get_resolution::GetResolutionError>(),
            Some(fabric_cli::get_resolution::GetResolutionError::AmbiguousTarget)
        ))
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[test]
fn existing_output_fails_before_backend_even_for_a_source_target() {
    let f = Fixture::new();
    let out = f.root.join("existing");
    std::fs::write(&out, b"keep").unwrap();
    let command = parse_get(
        [
            "get",
            &path("/src/definition"),
            "--output",
            out.to_str().unwrap(),
        ]
        .map(OsString::from),
    )
    .unwrap();
    assert!(PreparedGet::preflight(command, Path::new(&f.root)).is_err());
    assert_eq!(std::fs::read(out).unwrap(), b"keep");
    assert!(f.credential.0.lock().unwrap().is_empty());
}

struct Docs {
    calls: Mutex<Vec<String>>,
    deny_child: bool,
    oversized: bool,
    deny_root: bool,
}

impl DocumentsProvider for Docs {
    fn check_scope(&self, _: &DocsKey) -> Result<(), ProviderError> {
        Ok(())
    }

    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request.key.view.clone());
            if self.deny_root || (self.deny_child && request.key.view != "docs") {
                return Err(ProviderError {
                    kind: ProviderErrorKind::Denied,
                    detail: None,
                });
            }
            let mut child = request.key.clone();
            child.view = "docs/schema".into();
            Ok(DocPage {
                content: if self.oversized {
                    "x".repeat(request.max_bytes.get() + 1)
                } else {
                    "served native Markdown".into()
                },
                media_type: "text/markdown".into(),
                revision: Some("r1".into()),
                snapshot: None,
                edges: if request.key.view == "docs" {
                    vec![DocEdge {
                        target: child,
                        kind: Some("schema".into()),
                        provenance: Some("advertised structural child".into()),
                        disposition: Some(EdgeDisposition::Child),
                    }]
                } else {
                    Vec::new()
                },
                continuation: None,
                coverage: Coverage::Complete,
            })
        })
    }
}

#[tokio::test]
async fn bounded_docs_depth_zero_delivers_and_projection_retains_coverage() {
    let f = Fixture::new();
    let docs = Docs {
        calls: Mutex::new(Vec::new()),
        deny_child: false,
        oversized: false,
        deny_root: false,
    };
    let mut backend = f.backend();
    backend.documents = Some(&docs);
    let args = [
        "get".into(),
        path("/docs"),
        "--recursive".into(),
        "--depth".into(),
        "0".into(),
        "--format".into(),
        "json".into(),
        "--project".into(),
        "nodes[0].pages[0].content".into(),
    ];
    let delivery = f
        .prepared(&args)
        .acquire(&backend, pending())
        .await
        .unwrap();
    let mut output = Vec::new();
    delivery.write_to(&mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["data"], "served native Markdown");
    assert_eq!(value["meta"]["coverage"]["status"], "incomplete");
    assert_eq!(
        value["meta"]["coverage"]["reasons"][0]["kind"],
        "depth_limit"
    );
    assert_eq!(docs.calls.lock().unwrap().as_slice(), ["docs"]);
}

struct DeniedEdgeDocs(Docs);

impl DocumentsProvider for DeniedEdgeDocs {
    fn check_scope(&self, key: &DocsKey) -> Result<(), ProviderError> {
        self.0.check_scope(key)
    }

    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a> {
        Box::pin(async move {
            let mut page = self.0.get(request).await?;
            for edge in &mut page.edges {
                edge.disposition = Some(EdgeDisposition::Denied);
            }
            Ok(page)
        })
    }
}

struct LargeMarkdown;

impl DocumentsProvider for LargeMarkdown {
    fn check_scope(&self, _: &DocsKey) -> Result<(), ProviderError> {
        Ok(())
    }

    fn get<'a>(&'a self, _: DocRequest<'a>) -> DocumentsFuture<'a> {
        Box::pin(async {
            Ok(DocPage {
                content: "#".repeat(600_000),
                media_type: "text/markdown".into(),
                revision: Some("r1".into()),
                snapshot: None,
                edges: Vec::new(),
                continuation: None,
                coverage: Coverage::Complete,
            })
        })
    }
}

#[tokio::test]
async fn json_docs_do_not_render_or_charge_unselected_markdown_escaping() {
    let f = Fixture::new();
    let provider = LargeMarkdown;
    let mut backend = f.backend();
    backend.documents = Some(&provider);
    let json_args = [
        "get".into(),
        path("/docs/schema"),
        "--format".into(),
        "json".into(),
    ];
    let delivery = f
        .prepared(&json_args)
        .acquire(&backend, pending())
        .await
        .unwrap();
    assert!(delivery.bytes().len() < 1_048_576);
    let markdown_args = [
        "get".into(),
        path("/docs/schema"),
        "--format".into(),
        "markdown".into(),
    ];
    assert!(
        f.prepared(&markdown_args)
            .acquire(&backend, pending())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_shaped_markdown_projection_cannot_override_acquisition_coverage() {
    let f = Fixture::new();
    let docs = Docs {
        calls: Mutex::new(Vec::new()),
        deny_child: false,
        oversized: false,
        deny_root: false,
    };
    let mut backend = f.backend();
    backend.documents = Some(&docs);
    let expression = r#"{root:root,coverage:`{"status":"complete"}`,nodes:nodes,edges:`[]`,references:`[]`,pages_read:pages_read}"#;
    let args = [
        "get".into(),
        path("/docs"),
        "--recursive".into(),
        "--depth".into(),
        "0".into(),
        "--format".into(),
        "markdown".into(),
        "--project".into(),
        expression.into(),
    ];
    let delivery = f
        .prepared(&args)
        .acquire(&backend, pending())
        .await
        .unwrap();
    let markdown = std::str::from_utf8(delivery.bytes()).unwrap();
    assert!(markdown.contains("depth_limit"));
    assert!(markdown.contains("projectionApplied"));
    assert!(markdown.contains("\"meta\""));
    assert_eq!(docs.calls.lock().unwrap().as_slice(), ["docs"]);
}

#[tokio::test]
async fn advertised_denied_descendant_is_partial_without_attempting_its_read() {
    let f = Fixture::new();
    let docs = DeniedEdgeDocs(Docs {
        calls: Mutex::new(Vec::new()),
        deny_child: false,
        oversized: false,
        deny_root: false,
    });
    let mut backend = f.backend();
    backend.documents = Some(&docs);
    let args = [
        "get".into(),
        path("/docs"),
        "--recursive".into(),
        "--depth".into(),
        "1".into(),
        "--format".into(),
        "json".into(),
    ];
    let delivery = f
        .prepared(&args)
        .acquire(&backend, pending())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    delivery.write_to(&mut bytes).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["meta"]["outcome"], "partial");
    assert_eq!(value["meta"]["coverage"]["status"], "incomplete");
    assert_eq!(docs.0.calls.lock().unwrap().as_slice(), ["docs"]);
}

#[tokio::test]
async fn descendant_denial_is_labelled_partial_but_root_denial_creates_no_file() {
    for deny_root in [false, true] {
        let f = Fixture::new();
        let docs = Docs {
            calls: Mutex::new(Vec::new()),
            deny_child: true,
            oversized: false,
            deny_root,
        };
        let mut backend = f.backend();
        backend.documents = Some(&docs);
        let output = f.root.join("docs.json");
        let args = [
            "get".into(),
            path("/docs"),
            "--recursive".into(),
            "--depth".into(),
            "1".into(),
            "--format".into(),
            "json".into(),
            "--output".into(),
            output.to_string_lossy().into_owned(),
        ];
        let result = f.prepared(&args).acquire(&backend, pending()).await;
        if deny_root {
            assert!(result.is_err());
            assert!(!output.exists());
        } else {
            let delivery = result.unwrap();
            delivery.commit_file().unwrap();
            let value: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
            assert_eq!(value["meta"]["outcome"], "partial");
            assert_eq!(value["meta"]["coverage"]["status"], "incomplete");
            assert!(
                value["data"]["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["status"]["status"] == "failed")
            );
        }
    }
}

#[tokio::test]
async fn docs_budget_exhaustion_cannot_be_hidden_by_a_small_projection() {
    let f = Fixture::new();
    let docs = Docs {
        calls: Mutex::new(Vec::new()),
        deny_child: false,
        oversized: true,
        deny_root: false,
    };
    let mut backend = f.backend();
    backend.documents = Some(&docs);
    let output = f.root.join("not-clipped.json");
    let args = [
        "get".into(),
        path("/docs"),
        "--format".into(),
        "json".into(),
        "--project".into(),
        "nodes[0].key".into(),
        "--output".into(),
        output.to_string_lossy().into_owned(),
    ];
    assert!(
        f.prepared(&args)
            .acquire(&backend, pending())
            .await
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(docs.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn status_only_get_reports_failed_work_without_reclassifying_the_read() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}"),
        200,
        br#"{"status":"Failed","error":{"code":"WorkFailed"}}"#,
        &[],
    );
    let bytes = stdout(&f, &["get".into(), format!("/operations/{OP}")])
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"]["status"]["status"], "Failed");
    assert_eq!(value["data"]["workSucceeded"], false);
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}
