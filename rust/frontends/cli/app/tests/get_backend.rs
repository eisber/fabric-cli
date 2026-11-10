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
    operation_requests: Mutex<Vec<(String, Vec<u8>)>>,
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
            self.operation_requests
                .lock()
                .unwrap()
                .push((request.url().as_str().into(), request.body().to_vec()));
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

fn report_metadata(fixture: &Fixture, owner: &str) {
    fixture.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/reports/{ITEM}"),
        200,
        &serde_json::to_vec(&serde_json::json!({
            "id":ITEM,"type":owner,"workspaceId":WS,"displayName":"Report","unknown":{"keep":false}
        }))
        .unwrap(),
        &[],
    );
}

fn dataagent_path(suffix: &str) -> String {
    format!("/{WS}.Workspace/{ITEM}.DataAgent{suffix}")
}

fn dataagent_metadata(f: &Fixture) {
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/dataAgents/{ITEM}"),
        200,
        &serde_json::to_vec(&serde_json::json!({
            "id":ITEM,"workspaceId":WS,"type":"DataAgent",
            "displayName":"Inert agent","future":{"keep":true}
        }))
        .unwrap(),
        &[],
    );
}

fn dataagent_definition() -> Value {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let part = |path: &str, bytes: &[u8]| {
        serde_json::json!({
            "path":path,"payloadType":"InlineBase64","payload":STANDARD.encode(bytes),
            "unknownPartField":{"keep":true}
        })
    };
    serde_json::json!({
        "definition":{"format":"JSON","future":42,"parts":[
            part("Files/Config/data_agent.json",b"shared"),
            part("Files/Config/draft/stage_config.json",b"draft instructions"),
            part("Files/Config/published/stage_config.json",b"published instructions"),
            part("Files/Config/publish_info.json",b"publish metadata"),
            {"path":"future/opaque","payloadType":"FuturePayload","payload":"opaque"},
            part("Files/Config/draft/lakehouse-example/fewshots.json",&[0,255,128,10])
        ]},
        "futureEnvelope":{"preserve":true}
    })
}

fn dataagent_source(f: &Fixture, value: &Value) {
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/getDefinition"),
        200,
        &serde_json::to_vec(value).unwrap(),
        &[],
    );
}

#[tokio::test]
async fn dataagent_metadata_and_facade_share_exact_typed_read_without_invented_stage() {
    for facade in [false, true] {
        let f = Fixture::new();
        dataagent_metadata(&f);
        let mut args = vec!["get".into(), dataagent_path("")];
        if facade {
            args.splice(0..0, ["artifact".into(), "data-agent".into()]);
        }
        let value: Value = serde_json::from_slice(&stdout(&f, &args).await.unwrap()).unwrap();
        assert_eq!(value["data"]["future"]["keep"], true);
        assert!(value["meta"].get("stage").is_none());
        assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
        assert_eq!(
            f.credential.0.lock().unwrap().as_slice(),
            ["https://api.fabric.microsoft.com/.default"]
        );
    }
}

#[tokio::test]
async fn dataagent_stage_roots_preserve_full_definition_and_account_for_every_part() {
    for (stage, indices) in [
        ("draft", serde_json::json!([1, 5])),
        ("published", serde_json::json!([2])),
    ] {
        let f = Fixture::new();
        let definition = dataagent_definition();
        dataagent_metadata(&f);
        dataagent_source(&f, &definition);
        let value: Value = serde_json::from_slice(
            &stdout(
                &f,
                &["get".into(), dataagent_path(&format!("/src/{stage}"))],
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["data"]["nativeDefinition"], definition);
        assert_eq!(value["data"]["selectedPartIndices"], indices);
        let evidence = &value["meta"]["dataAgentSource"];
        assert_eq!(evidence["stage"], stage);
        assert_eq!(evidence["scope"], "stage-projection");
        assert_eq!(evidence["fullSourceRoundtrip"], false);
        assert_eq!(evidence["sharedPartIndices"], serde_json::json!([0, 3]));
        assert_eq!(evidence["unclassifiedPartIndices"], serde_json::json!([4]));
        assert_eq!(evidence["parts"].as_array().unwrap().len(), 6);
        assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
        let requests = f.executor.operation_requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|(url, body)| !url.contains('?') && body.is_empty())
        );
        assert!(requests[1].0.ends_with("/getDefinition"));
    }
}

#[tokio::test]
async fn dataagent_full_source_keeps_native_order_unknown_fields_and_duplicate_paths() {
    let f = Fixture::new();
    let mut definition = dataagent_definition();
    let duplicate = definition["definition"]["parts"][4].clone();
    definition["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    dataagent_metadata(&f);
    dataagent_source(&f, &definition);
    let value: Value = serde_json::from_slice(
        &stdout(
            &f,
            &["get".into(), dataagent_path("/src/definition/formats/JSON")],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(value["data"], definition);
    assert_eq!(
        value["meta"]["dataAgentSource"]["fullSourceRoundtrip"],
        true
    );
    let requests = f.executor.operation_requests.lock().unwrap();
    assert!(requests[1].0.ends_with("/getDefinition?format=JSON"));
}

#[tokio::test]
async fn dataagent_exact_stage_part_uses_strict_shared_bytes_and_no_stage_http_route() {
    for (stage, expected) in [
        ("draft", b"draft instructions".as_slice()),
        ("published", b"published instructions"),
    ] {
        let f = Fixture::new();
        dataagent_metadata(&f);
        dataagent_source(&f, &dataagent_definition());
        let bytes = stdout(
            &f,
            &[
                "get".into(),
                dataagent_path(&format!("/src/{stage}/stage_config.json")),
                "--format".into(),
                "raw".into(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(bytes, expected);
        assert!(
            f.executor
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, path)| !path.contains("/staging/"))
        );
    }
    let f = Fixture::new();
    dataagent_metadata(&f);
    dataagent_source(&f, &dataagent_definition());
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            dataagent_path("/src/draft/lakehouse-example/fewshots.json"),
            "--format".into(),
            "raw".into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(bytes, [0, 255, 128, 10]);
}

#[tokio::test]
async fn dataagent_settings_and_native_datasource_pages_require_the_selected_stage() {
    for (stage, native_prefix) in [("draft", "staging/"), ("published", "")] {
        for facet in ["settings", "datasources"] {
            let f = Fixture::new();
            dataagent_metadata(&f);
            let body = if facet == "settings" {
                serde_json::json!({"aiInstructions":format!("inert {stage}"),"unknown":true})
            } else {
                serde_json::json!({"value":[],"continuationToken":"opaque + next",
                    "continuationUri":"https://untrusted.invalid/inert"})
            };
            f.executor.response(
                "GET",
                format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/{native_prefix}{facet}"),
                200,
                &serde_json::to_vec(&body).unwrap(),
                &[],
            );
            let value: Value = serde_json::from_slice(
                &stdout(
                    &f,
                    &[
                        "get".into(),
                        dataagent_path(&format!("/{facet}")),
                        "--stage".into(),
                        stage.into(),
                    ],
                )
                .await
                .unwrap(),
            )
            .unwrap();
            assert_eq!(value["data"], body);
            assert_eq!(value["meta"]["stage"], stage);
            assert_eq!(value["meta"]["preview"], true);
            if facet == "datasources" {
                assert_eq!(value["meta"]["collectionComplete"], false);
                assert_eq!(value["meta"]["scope"], "one-native-page");
            }
            assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
            assert!(
                f.executor
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(method, _)| method == "GET")
            );
            assert!(f.executor.waits.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn dataagent_wrong_owner_or_response_identity_never_reaches_source_or_configuration() {
    for (field, wrong) in [("type", "Notebook"), ("id", OP), ("workspaceId", OP)] {
        let f = Fixture::new();
        let mut metadata = serde_json::json!({
            "id":ITEM,"workspaceId":WS,"type":"DataAgent","displayName":"Mismatch"
        });
        metadata[field] = wrong.into();
        f.executor.response(
            "GET",
            format!("/v1/workspaces/{WS}/dataAgents/{ITEM}"),
            200,
            &serde_json::to_vec(&metadata).unwrap(),
            &[],
        );
        assert!(
            stdout(&f, &["get".into(), dataagent_path("/src/draft")])
                .await
                .is_err()
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn dataagent_absent_stage_duplicate_or_unsupported_part_never_falls_back() {
    for case in [
        "missing-stage",
        "duplicate",
        "opaque",
        "missing-part",
        "invalid-base64",
    ] {
        let f = Fixture::new();
        let mut definition = dataagent_definition();
        let suffix = match case {
            "missing-stage" => {
                definition["definition"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .remove(2);
                "/src/published"
            }
            "duplicate" => {
                let part = definition["definition"]["parts"][1].clone();
                definition["definition"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .push(part);
                "/src/draft/stage_config.json"
            }
            "opaque" => "/src/definition/parts/future/opaque",
            "missing-part" => "/src/draft/shared.json",
            _ => {
                definition["definition"]["parts"][1]["payload"] = "not base64".into();
                "/src/draft/stage_config.json"
            }
        };
        dataagent_metadata(&f);
        dataagent_source(&f, &definition);
        assert!(
            stdout(&f, &["get".into(), dataagent_path(suffix)])
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn dataagent_configuration_denial_retains_original_error_without_source_fallback() {
    use fabric_sdk::dataagents::DataAgentError;
    use fabric_sdk_operations::OperationErrorKind;
    let f = Fixture::new();
    dataagent_metadata(&f);
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/staging/settings"),
        403,
        br#"{"error":{"code":"Forbidden","message":"inert"}} "#,
        &[],
    );
    let error = stdout(
        &f,
        &[
            "get".into(),
            dataagent_path("/settings"),
            "--stage".into(),
            "draft".into(),
        ],
    )
    .await
    .unwrap_err();
    let GetCommandError::Backend { source, .. } = error else {
        panic!("original SDK error")
    };
    let Some(DataAgentError::Operation(error)) = source.downcast_ref::<DataAgentError>() else {
        panic!("typed DataAgent error");
    };
    assert!(matches!(
        error.kind,
        OperationErrorKind::Http { status: 403, .. }
    ));
    assert!(error.raw_service_error.is_some());
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dataagent_unprojected_definition_budget_cannot_be_bypassed_by_stage_or_query() {
    let f = Fixture::new();
    let mut definition = dataagent_definition();
    definition["unknown"] = "x".repeat(4096).into();
    dataagent_metadata(&f);
    dataagent_source(&f, &definition);
    let result = stdout(
        &f,
        &[
            "get".into(),
            dataagent_path("/src/draft"),
            "--project".into(),
            "selectedPartIndices".into(),
            "--max-bytes".into(),
            "2048".into(),
        ],
    )
    .await;
    assert!(result.is_err());
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dataagent_accepted_definition_observes_exact_handle_once_before_stage_selection() {
    for wait in [false, true] {
        let f = Fixture::new();
        dataagent_metadata(&f);
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/getDefinition"),
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
        let mut args = vec!["get".into(), dataagent_path("/src/published")];
        if wait {
            args.push("--wait".into());
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
            f.executor.response(
                "GET",
                format!("/v1/operations/{OP}/result"),
                200,
                &serde_json::to_vec(&dataagent_definition()).unwrap(),
                &[],
            );
        }
        let delivery = f
            .prepared(&args)
            .acquire(&f.backend(), pending())
            .await
            .unwrap();
        assert_eq!(delivery.complete(), wait);
        let mut bytes = Vec::new();
        delivery.write_to(&mut bytes).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if wait {
            assert_eq!(value["data"]["selectedPartIndices"], serde_json::json!([2]));
        } else {
            assert_eq!(value["data"]["handle"], format!("/operations/{OP}"));
            assert_eq!(value["meta"]["stageVerified"], false);
        }
        let calls = f.executor.calls.lock().unwrap();
        assert_eq!(calls.len(), if wait { 4 } else { 2 });
        assert_eq!(
            calls.iter().filter(|(method, _)| method == "POST").count(),
            1
        );
    }
}

#[tokio::test]
async fn dataagent_projection_does_not_claim_a_full_source_roundtrip() {
    let f = Fixture::new();
    dataagent_metadata(&f);
    dataagent_source(&f, &dataagent_definition());
    let value: Value = serde_json::from_slice(
        &stdout(
            &f,
            &[
                "get".into(),
                dataagent_path("/src/definition"),
                "--project".into(),
                "definition.parts[1].path".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(value["data"], "Files/Config/draft/stage_config.json");
    assert_eq!(value["meta"]["projectionApplied"], true);
    assert_eq!(
        value["meta"]["dataAgentSource"]["fullSourceRoundtrip"],
        false
    );
    assert_eq!(
        value["meta"]["dataAgentSource"]["parts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
}

#[tokio::test]
async fn dataagent_resolved_stage_cannot_be_substituted_before_retrieval() {
    use fabric_cli::{get_command::GetBackend, get_target::DataAgentStage};
    let f = Fixture::new();
    let command = parse_get(
        ["get", &dataagent_path("/settings"), "--stage", "draft"]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let backend = f.backend();
    let resolved = backend
        .resolve_with_options(&command.path, command.options)
        .await
        .unwrap();
    let mut options = command.options;
    options.stage = Some(DataAgentStage::Published);
    assert!(backend.retrieve(&resolved, options).await.is_err());
    assert!(f.executor.calls.lock().unwrap().is_empty());
    assert!(f.credential.0.lock().unwrap().is_empty());
}

fn report_definition(version: &str) -> Value {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let part = |path: &str, bytes: &[u8]| serde_json::json!({"path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64"});
    let properties = serde_json::json!({
        "version":version,"datasetReference":{"byConnection":{"connectionString":"semanticmodelid=unresolved-model"}}
    });
    serde_json::json!({"definition":{"format":"PBIR","parts":[
        part("definition.pbir",&serde_json::to_vec(&properties).unwrap()),
        part("definition/version.json",br#"{"version":"2.0.0"}"#),
        part("definition/report.json",b"{}"),
        part("definition/pages/p/page.json",br#"{"name":"p"}"#),
        part("StaticResources/native.bin",&[0,255,128,0])
    ]},"unknown":"preserved"})
}

#[tokio::test]
async fn dataagent_explicit_msit_binding_is_preserved_through_facade_and_sdk() {
    use fabric_sdk::{EndpointBinding, RequestContext, ServiceOrigin};
    let mut f = Fixture::new();
    f.client = Client::with_context(RequestContext::with_endpoint(
        f.credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap(),
    ));
    dataagent_metadata(&f);
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/staging/settings"),
        200,
        br#"{"aiInstructions":"inert draft"}"#,
        &[],
    );
    stdout(
        &f,
        &[
            "artifact".into(),
            "data-agent".into(),
            "get".into(),
            dataagent_path("/settings"),
            "--stage".into(),
            "draft".into(),
        ],
    )
    .await
    .unwrap();
    let requests = f.executor.operation_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|(url, body)| {
        url.starts_with("https://msitapi.fabric.microsoft.com/v1/")
            && !url.contains('?')
            && body.is_empty()
    }));
    assert_eq!(
        f.credential.0.lock().unwrap().as_slice(),
        ["https://api.fabric.microsoft.com/.default"; 2]
    );
}

#[tokio::test]
async fn dataagent_raw_part_budget_does_not_charge_an_unselected_json_inventory() {
    for raw in [false, true] {
        let f = Fixture::new();
        let definition = serde_json::json!({
            "definition":{"parts":[dataagent_definition()["definition"]["parts"][1].clone()]}
        });
        let source_size = serde_json::to_vec(&definition).unwrap().len();
        dataagent_metadata(&f);
        let metadata_size = f.executor.replies.lock().unwrap()[0].body.len();
        dataagent_source(&f, &definition);
        let mut args = vec![
            "get".into(),
            dataagent_path("/src/draft/stage_config.json"),
            "--max-bytes".into(),
            (metadata_size + source_size).to_string(),
        ];
        if raw {
            args.extend(["--format".into(), "raw".into()]);
        }
        let result = stdout(&f, &args).await;
        if raw {
            assert_eq!(result.unwrap(), b"draft instructions");
        } else {
            assert!(
                matches!(result, Err(GetCommandError::ByteLimit { .. })),
                "{result:?}"
            );
        }
    }
}

#[tokio::test]
async fn dataagent_configuration_rejects_positional_pages_and_undocumented_acceptance() {
    for (status, body) in [(200, b"[]".as_slice()), (202, b"{}")] {
        let f = Fixture::new();
        dataagent_metadata(&f);
        f.executor.response(
            "GET",
            format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/datasources"),
            status,
            body,
            &[],
        );
        assert!(
            stdout(
                &f,
                &[
                    "get".into(),
                    dataagent_path("/datasources"),
                    "--stage".into(),
                    "published".into(),
                ],
            )
            .await
            .is_err()
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
        assert!(f.executor.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn report_metadata_uses_typed_endpoint_without_source_or_model_read() {
    let f = Fixture::new();
    report_metadata(&f, "Report");
    let bytes = stdout(
        &f,
        &["get".into(), format!("/{WS}.Workspace/{ITEM}.Report")],
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"]["unknown"]["keep"], false);
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn report_malformed_required_json_blocks_complete_and_raw_file_delivery() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for selected in ["definition/report.json", "definition/pages/p/page.json"] {
        for bytes in [b"null".as_slice(), b"{malformed"] {
            for raw in [false, true] {
                let f = Fixture::new();
                report_metadata(&f, "Report");
                let mut source = report_definition("4.0");
                let part = source["definition"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|part| part["path"] == selected)
                    .unwrap();
                part["payload"] = STANDARD.encode(bytes).into();
                f.executor.response(
                    "POST",
                    format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
                    200,
                    &serde_json::to_vec(&source).unwrap(),
                    &[],
                );
                let suffix = if raw {
                    "/src/definition/parts/StaticResources/native.bin"
                } else {
                    "/src/definition"
                };
                let mut args = vec![
                    "get".into(),
                    format!("/{WS}.Workspace/{ITEM}.Report{suffix}"),
                    "--output".into(),
                    "invalid-source.bin".into(),
                ];
                if raw {
                    args.extend(["--format".into(), "raw".into()]);
                }
                assert!(
                    matches!(
                        stdout(&f, &args).await,
                        Err(GetCommandError::Incomplete { .. })
                    ),
                    "malformed {selected} must block delivery, raw={raw}"
                );
                assert!(!f.root.join("invalid-source.bin").exists());
                assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
            }
        }
    }
}

#[tokio::test]
async fn report_invalid_required_json_retains_partial_receipt_and_blocks_raw_stdout() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for raw in [false, true] {
        let f = Fixture::new();
        report_metadata(&f, "Report");
        let mut source = report_definition("4.0");
        source["definition"]["parts"][2]["payload"] = STANDARD.encode(b"null").into();
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
            200,
            &serde_json::to_vec(&source).unwrap(),
            &[],
        );
        let suffix = if raw {
            "/src/definition/parts/StaticResources/native.bin"
        } else {
            "/src/definition"
        };
        let mut args = vec![
            "get".into(),
            format!("/{WS}.Workspace/{ITEM}.Report{suffix}"),
        ];
        if raw {
            args.extend(["--format".into(), "raw".into()]);
            assert!(matches!(
                stdout(&f, &args).await,
                Err(GetCommandError::Incomplete { .. })
            ));
        } else {
            let value: Value = serde_json::from_slice(&stdout(&f, &args).await.unwrap()).unwrap();
            assert_eq!(value["data"], source);
            assert_eq!(value["meta"]["outcome"], "partial");
            assert_eq!(
                value["meta"]["reportSource"]["incomplete"],
                serde_json::json!(["invalidReportStructure"])
            );
        }
    }
}

#[tokio::test]
async fn report_legacy_nullable_connection_keeps_complete_bytes_and_unverified_model() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = Fixture::new();
    report_metadata(&f, "Report");
    let properties = serde_json::json!({
        "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/1.0.0/schema.json",
        "version":"4.0","datasetReference":{"byConnection":{
            "connectionString":null,"pbiServiceModelId":null,
            "pbiModelVirtualServerName":"sobe_wowvirtualserver",
            "pbiModelDatabaseName":ITEM,"name":"EntityDataSource",
            "connectionType":"pbiServiceXmlaStyleLive"
        }}
    });
    let mut source = report_definition("4.0");
    source["definition"]["parts"][0]["payload"] = STANDARD
        .encode(serde_json::to_vec(&properties).unwrap())
        .into();
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
        200,
        &serde_json::to_vec(&source).unwrap(),
        &[],
    );
    let result = f
        .prepared(&[
            "get".into(),
            format!("/{WS}.Workspace/{ITEM}.Report/src/definition"),
        ])
        .acquire(&f.backend(), pending())
        .await
        .expect("official legacy nullable connection must not fail acquisition");
    assert!(result.complete());
    let mut bytes = Vec::new();
    result.write_to(&mut bytes).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"], source);
    assert_eq!(
        value["meta"]["reportSource"]["relatedModelAuthorized"],
        false
    );
    assert_eq!(
        value["meta"]["reportSource"]["modelReference"],
        properties["datasetReference"]
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn report_native_source_evidence_survives_projection_and_preserves_binary_parts() {
    let f = Fixture::new();
    report_metadata(&f, "Report");
    let source = report_definition("4.0");
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
        200,
        &serde_json::to_vec(&source).unwrap(),
        &[],
    );
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            format!("/{WS}.Workspace/{ITEM}.Report/src/definition"),
            "--project".into(),
            "unknown".into(),
        ],
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"], "preserved");
    assert_eq!(value["meta"]["reportSource"]["version"], "4.0");
    assert_eq!(value["meta"]["reportSource"]["nativeVersion"], "2.0.0");
    assert_eq!(
        value["meta"]["reportSource"]["relatedModelAuthorized"],
        false
    );
    assert_eq!(
        value["meta"]["reportSource"]["incomplete"],
        serde_json::json!([])
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn report_native_part_delivers_exact_bytes_not_pbix_or_text_conversion() {
    let f = Fixture::new();
    report_metadata(&f, "Report");
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
        200,
        &serde_json::to_vec(&report_definition("4.0")).unwrap(),
        &[],
    );
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            format!(
                "/{WS}.Workspace/{ITEM}.Report/src/definition/parts/StaticResources/native.bin"
            ),
            "--format".into(),
            "raw".into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(bytes, [0, 255, 128, 0]);
}

#[tokio::test]
async fn report_unknown_version_is_partial_and_cannot_be_projected_or_written_as_complete() {
    for file in [false, true] {
        let f = Fixture::new();
        report_metadata(&f, "Report");
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
            200,
            &serde_json::to_vec(&report_definition("99.0")).unwrap(),
            &[],
        );
        let mut args = vec![
            "get".into(),
            format!("/{WS}.Workspace/{ITEM}.Report/src/definition"),
        ];
        if file {
            args.extend(["--output".into(), "not-complete.json".into()]);
            assert!(matches!(
                stdout(&f, &args).await,
                Err(GetCommandError::Incomplete { .. })
            ));
            assert!(!f.root.join("not-complete.json").exists());
        } else {
            let value: Value = serde_json::from_slice(&stdout(&f, &args).await.unwrap()).unwrap();
            assert_eq!(value["meta"]["outcome"], "partial");
            assert_eq!(value["meta"]["reportSource"]["version"], "99.0");
        }
    }
}

#[tokio::test]
async fn report_wrong_owner_and_unadvertised_native_selector_have_no_definition_effect() {
    let f = Fixture::new();
    report_metadata(&f, "PaginatedReport");
    assert!(
        stdout(
            &f,
            &[
                "get".into(),
                format!("/{WS}.Workspace/{ITEM}.Report/src/definition")
            ]
        )
        .await
        .is_err()
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
    let f = Fixture::new();
    assert!(
        stdout(
            &f,
            &[
                "get".into(),
                format!("/{WS}.Workspace/{ITEM}.Report/src/definition/formats/PBIX")
            ]
        )
        .await
        .is_err()
    );
    assert!(f.executor.calls.lock().unwrap().is_empty());
    assert!(f.credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn report_accepted_source_uses_shared_observer_once_and_keeps_native_evidence() {
    let f = Fixture::new();
    report_metadata(&f, "Report");
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/reports/{ITEM}/getDefinition"),
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
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}/result"),
        200,
        &serde_json::to_vec(&report_definition("4.0")).unwrap(),
        &[],
    );
    let value: Value = serde_json::from_slice(
        &stdout(
            &f,
            &[
                "get".into(),
                format!("/{WS}.Workspace/{ITEM}.Report/src/definition"),
                "--wait".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(value["meta"]["reportSource"]["version"], "4.0");
    let calls = f.executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls.iter().filter(|(method, _)| method == "POST").count(),
        1
    );
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

fn copyjob_metadata() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id":ITEM,"workspaceId":WS,"type":"CopyJob","displayName":"Copy",
        "enabled":false,"future":null
    }))
    .unwrap()
}

fn copyjob_definition() -> Vec<u8> {
    use base64::Engine;
    serde_json::to_vec(&serde_json::json!({"definition":{"parts":[{
        "path":"copyjob-content.json","payloadType":"InlineBase64",
        "payload":base64::engine::general_purpose::STANDARD.encode(
            br#"{"properties":{"jobMode":"Batch"},"activities":[]}"#
        )
    }]}}))
    .unwrap()
}

fn copyjob_path(suffix: &str) -> String {
    format!("/{WS}.Workspace/{ITEM}.CopyJob{suffix}")
}

#[tokio::test]
async fn copyjob_root_get_uses_typed_native_metadata_and_shared_projection() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/copyJobs/{ITEM}"),
        200,
        &copyjob_metadata(),
        &[],
    );
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            copyjob_path(""),
            "--project".into(),
            "enabled".into(),
        ],
    )
    .await
    .unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["data"], false);
    assert_eq!(result["meta"]["outcome"], "completed");
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn copyjob_definition_uses_native_post_after_authoritative_owner_read() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/copyJobs/{ITEM}"),
        200,
        &copyjob_metadata(),
        &[],
    );
    let definition = copyjob_definition();
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/copyJobs/{ITEM}/getDefinition"),
        200,
        &definition,
        &[],
    );
    let bytes = stdout(&f, &["get".into(), copyjob_path("/src/definition")])
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["data"],
        serde_json::from_slice::<Value>(&definition).unwrap()
    );
    let calls = f.executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls.iter().filter(|(method, _)| method == "POST").count(),
        1
    );
}

#[tokio::test]
async fn copyjob_unadvertised_runtime_and_file_facets_fail_before_discovery() {
    for suffix in [
        "/Files/data.bin",
        "/docs/schema",
        &format!("/runs/{OP}"),
        "/src/definition/formats/unsafe:format",
    ] {
        let f = Fixture::new();
        let result = stdout(&f, &["get".into(), copyjob_path(suffix)]).await;
        assert!(result.is_err());
        assert!(f.executor.calls.lock().unwrap().is_empty());
        assert!(f.credential.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn copyjob_denied_or_wrong_owner_never_fetches_source_or_returns_metadata_fallback() {
    for status in [200, 403] {
        let f = Fixture::new();
        let mut metadata: Value = serde_json::from_slice(&copyjob_metadata()).unwrap();
        metadata["type"] = "Notebook".into();
        f.executor.response(
            "GET",
            format!("/v1/workspaces/{WS}/copyJobs/{ITEM}"),
            status,
            &serde_json::to_vec(&metadata).unwrap(),
            &[],
        );
        assert!(
            stdout(&f, &["get".into(), copyjob_path("/src/definition")])
                .await
                .is_err()
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
    }
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/copyJobs/{ITEM}"),
        200,
        &copyjob_metadata(),
        &[],
    );
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/copyJobs/{ITEM}/getDefinition"),
        403,
        br#"{"errorCode":"AccessDenied"}"#,
        &[],
    );
    assert!(
        stdout(&f, &["get".into(), copyjob_path("/src/definition")])
            .await
            .is_err()
    );
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn copyjob_accepted_definition_reuses_observer_and_validates_completed_source() {
    for valid in [true, false] {
        let f = Fixture::new();
        f.executor.response(
            "GET",
            format!("/v1/workspaces/{WS}/copyJobs/{ITEM}"),
            200,
            &copyjob_metadata(),
            &[],
        );
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/copyJobs/{ITEM}/getDefinition"),
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
        let body = if valid {
            copyjob_definition()
        } else {
            br#"{"definition":{"parts":[]}}"#.to_vec()
        };
        f.executor.response(
            "GET",
            format!("/v1/operations/{OP}/result"),
            200,
            &body,
            &[],
        );
        let result = stdout(
            &f,
            &[
                "get".into(),
                copyjob_path("/src/definition"),
                "--wait".into(),
            ],
        )
        .await;
        assert_eq!(result.is_ok(), valid);
        let calls = f.executor.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls.iter().filter(|(method, _)| method == "POST").count(),
            1
        );
    }
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
