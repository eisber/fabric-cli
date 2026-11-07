// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic HTTP contracts derived from the frozen Python GET handlers.

use super::*;
use async_trait::async_trait;
use azure_core::http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers};
use fabric_cli::session::get_host::{
    ClockedGetHost, GetDispatch, GetEvidence, output::GetOutputClock, resources::GetResourceConfig,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use std::fmt;

struct Step {
    method: Method,
    path: String,
    body: Value,
    headers: Vec<(&'static str, &'static str)>,
    status: u16,
}

fn step(path: impl Into<String>, body: Value) -> Step {
    Step {
        method: Method::Get,
        path: path.into(),
        body,
        headers: vec![],
        status: 200,
    }
}

struct Wire {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<(Method, String)>>,
    host: String,
}

impl fmt::Debug for Wire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OfflineGetResources")
    }
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let expected = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected GET request");
        assert_eq!(request.method(), expected.method);
        assert_eq!(request.url().path(), expected.path);
        let host = request.url().host_str().unwrap();
        assert!(
            host == self.host
                || host == "management.azure.com"
                || host == "onelake.dfs.fabric.microsoft.com"
        );
        self.requests
            .lock()
            .unwrap()
            .push((request.method(), request.url().to_string()));
        let mut headers = Headers::new();
        for (name, value) in expected.headers {
            headers.insert(name, value);
        }
        Ok(AsyncRawResponse::from_bytes(
            expected.status.into(),
            headers,
            serde_json::to_vec(&expected.body).unwrap(),
        ))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

struct Clock;

impl GetOutputClock for Clock {
    fn local_timestamp(&self) -> TransferFuture<'_, Result<LocalTimestamp, CommandError>> {
        Box::pin(ready(Ok(LocalTimestamp::parse("20000102_030405").unwrap())))
    }
}

struct ResourceFixture {
    sdk: Client,
    http: HttpExecutor,
    wire: Arc<Wire>,
    credential: Arc<Credential>,
}

impl ResourceFixture {
    fn new(steps: Vec<Step>) -> Self {
        Self::with_host(steps, "api.fabric.microsoft.com")
    }

    fn with_host(steps: Vec<Step>, host: &str) -> Self {
        let endpoint =
            fabric_sdk::EndpointBinding::with_host(fabric_sdk::ServiceOrigin::Fabric, host)
                .unwrap();
        let credential = Arc::new(Credential::default());
        let sdk = Client::with_context(fabric_sdk::RequestContext::with_endpoint(
            credential.clone(),
            fabric_sdk::CredentialSelection::new(
                "offline",
                "offline-tenant",
                fabric_sdk::Audience::Fabric,
            )
            .unwrap(),
            endpoint.clone(),
        ));
        let wire = Arc::new(Wire {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(vec![]),
            host: host.into(),
        });
        let http = HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            sdk,
            http,
            wire,
            credential,
        }
    }

    async fn run(
        &self,
        path: &str,
        query: Option<&str>,
        output: Option<&str>,
        files: Option<&mut Files>,
        config: GetResourceConfig,
    ) -> GetDispatch {
        let mut args = vec!["fab-rs", "get", path, "--force", "--verbose"];
        if let Some(query) = query {
            args.extend(["-q", query]);
        }
        if let Some(output) = output {
            args.extend(["-o", output]);
        }
        let Invocation::Read(command) = parse_args(args).unwrap() else {
            panic!("GET")
        };
        let session = ReadSession {
            sdk: &self.sdk,
            executor: &self.http,
            tenant: Element::tenant("Tenant", "offline-tenant"),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
        };
        let ui = Ui::new(false, Arc::default());
        let mut host = ClockedGetHost {
            operations: &self.http,
            ui: &ui,
            files: files.map(|files| files as &mut dyn TransferFiles),
            clock: &Clock,
            newline: GetOutputNewline::Lf,
            definition_limits: DefinitionLimits::default(),
            operation_limits: OperationLimits::default(),
        };
        session
            .dispatch_get_with_resources(
                &command,
                &self.http,
                &mut host,
                &mut GetEvidence::default(),
                &config,
                pending(),
            )
            .await
    }

    fn consumed(&self) {
        assert!(
            self.wire.steps.lock().unwrap().is_empty(),
            "unused GET replies"
        );
    }
}

fn workspace() -> Step {
    step(
        "/v1/workspaces",
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    )
}

fn lakehouse() -> Step {
    step(
        format!("/v1/workspaces/{WS}/items"),
        json!({"value":[{"id":ITEM,"displayName":"Lake_name","type":"Lakehouse"}]}),
    )
}

#[tokio::test]
async fn hidden_tenant_singletons_use_typed_routes_and_the_existing_query_output_boundary() {
    for (collection, suffix) in [("connections", "Connection"), ("gateways", "Gateway")] {
        let fixture = ResourceFixture::new(vec![
            step(
                format!("/v1/{collection}"),
                json!({"value":[{"id":"resource","displayName":"Main"}]}),
            ),
            step(
                format!("/v1/{collection}/resource"),
                json!({"id":"resource","description":"value","properties":{"enabled":false}}),
            ),
        ]);
        let dispatch = fixture
            .run(
                &format!("/.{collection}/Main.{suffix}"),
                Some("."),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await;
        let outcome = dispatch.result.unwrap();
        assert_eq!(outcome.projected.unwrap()["properties"]["enabled"], false);
        assert_eq!(outcome.evidence.operations.len(), 1);
        assert!(!outcome.evidence.declined);
        fixture.consumed();
        assert!(
            fixture
                .credential
                .scopes
                .lock()
                .unwrap()
                .iter()
                .all(|scope| scope == &["https://api.fabric.microsoft.com/.default"])
        );
    }
}

#[tokio::test]
async fn hidden_connection_falsey_names_and_absent_gateway_names_use_source_specific_id_fallbacks()
{
    for display in [
        Value::Null,
        json!(""),
        json!(false),
        json!(0),
        json!([]),
        json!({}),
    ] {
        let fixture = ResourceFixture::new(vec![
            step(
                "/v1/connections",
                json!({"value":[{"id":"c","displayName":display}]}),
            ),
            step(
                "/v1/connections/c",
                json!({"id":"c","properties":{"source":"connection"}}),
            ),
        ]);
        let outcome = fixture
            .run(
                "/.connections/c.Connection",
                Some("."),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await
            .result
            .unwrap();
        assert_eq!(
            outcome.projected.unwrap()["properties"]["source"],
            "connection"
        );
        fixture.consumed();
    }
    let fixture = ResourceFixture::new(vec![
        step("/v1/gateways", json!({"value":[{"id":"g"}]})),
        step(
            "/v1/gateways/g",
            json!({"id":"g","properties":{"source":"gateway"}}),
        ),
    ]);
    let outcome = fixture
        .run(
            "/.gateways/g.Gateway",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(
        outcome.projected.unwrap()["properties"]["source"],
        "gateway"
    );
    fixture.consumed();
}

#[tokio::test]
async fn hidden_name_fallbacks_do_not_hide_malformed_metadata_or_guess_present_gateway_names() {
    for (collection, suffix, row) in [
        ("connections", "Connection", json!({"id":"c"})),
        (
            "connections",
            "Connection",
            json!({"id":"c","displayName":42}),
        ),
        (
            "connections",
            "Connection",
            json!({"id":null,"displayName":null}),
        ),
        ("gateways", "Gateway", json!({"id":"g","displayName":null})),
        ("gateways", "Gateway", json!({"id":"g","displayName":false})),
        ("gateways", "Gateway", json!({"displayName":"g"})),
    ] {
        let fixture = ResourceFixture::new(vec![step(
            format!("/v1/{collection}"),
            json!({"value":[row]}),
        )]);
        let failure = fixture
            .run(
                &format!("/.{collection}/g.{suffix}"),
                Some("."),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await
            .result
            .unwrap_err();
        assert!(matches!(failure.kind, GetErrorKind::Api(ref error)
            if **error == fabric_sdk_api::ApiError::InvalidResponse));
        fixture.consumed();
    }
    let fixture = ResourceFixture::new(vec![step(
        "/v1/gateways",
        json!({"value":[{"id":"g","displayName":""}]}),
    )]);
    let failure = fixture
        .run(
            "/.gateways/g.Gateway",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap_err();
    let GetErrorKind::Command(error) = failure.kind else {
        panic!("source name is not the ID")
    };
    assert_eq!(
        error
            .envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None)
            .result()
            .error_code(),
        Some("NotFound")
    );
    fixture.consumed();
}

#[tokio::test]
async fn hidden_domain_enrichment_pages_and_omissions_are_retained_without_partial_success() {
    for fail in [false, true] {
        let mut membership = step(
            "/v1/admin/domains/domain/workspaces",
            json!({"value":[{"id":"first"}],"continuationToken":"next+/="}),
        );
        if fail {
            membership.status = 403;
            membership.body = json!({"errorCode":"Forbidden","message":"denied"});
        }
        let mut steps = vec![
            step(
                "/v1/admin/domains",
                json!({"domains":[{"id":"domain","displayName":"Main"}]}),
            ),
            step("/v1/admin/domains/domain", json!({"id":"domain"})),
            membership,
        ];
        if !fail {
            steps.push(step(
                "/v1/admin/domains/domain/workspaces",
                json!({"value":[{"id":"second"}]}),
            ));
        }
        let fixture = ResourceFixture::new(steps);
        let outcome = fixture
            .run(
                "/.domains/Main.Domain",
                Some("."),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await
            .result
            .unwrap();
        if fail {
            assert!(outcome.projected.unwrap().get("domainWorkspaces").is_none());
            assert!(matches!(
                outcome.evidence.enrichment_failures[0].kind,
                GetErrorKind::Collection(_)
            ));
        } else {
            assert_eq!(
                outcome.projected.unwrap()["domainWorkspaces"],
                json!([{"id":"first"},{"id":"second"}])
            );
        }
        fixture.consumed();
    }
}

#[tokio::test]
async fn workspace_hidden_singletons_preserve_name_not_display_name_discovery() {
    for (collection, suffix) in [
        ("spark/pools", "SparkPool"),
        ("managedPrivateEndpoints", "ManagedPrivateEndpoint"),
    ] {
        let virtual_collection = if suffix == "SparkPool" {
            ".sparkpools"
        } else {
            ".managedprivateendpoints"
        };
        let fixture = ResourceFixture::new(vec![
            workspace(),
            step(
                format!("/v1/workspaces/{WS}/{collection}"),
                json!({"value":[{"id":"resource","name":"Main"}]}),
            ),
            step(
                format!("/v1/workspaces/{WS}/{collection}/resource"),
                json!({"id":"resource","settings":{"count":3}}),
            ),
        ]);
        let outcome = fixture
            .run(
                &format!("/Demo.Workspace/{virtual_collection}/Main.{suffix}"),
                Some("settings"),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await
            .result
            .unwrap();
        assert_eq!(outcome.projected, Some(json!({"count":3})));
        fixture.consumed();
    }
}

#[tokio::test]
async fn external_share_uses_real_parent_item_and_source_short_id_name() {
    let fixture = ResourceFixture::new(vec![
        workspace(),
        lakehouse(),
        step(
            format!("/v1/workspaces/{WS}/items/{ITEM}/externalDataShares"),
            json!({"value":[{"id":"abcd-remaining","itemId":ITEM,"status":"Active"}]}),
        ),
        step(
            format!("/v1/workspaces/{WS}/items/{ITEM}/externalDataShares/abcd-remaining"),
            json!({"id":"abcd-remaining","itemId":ITEM,"status":"Active"}),
        ),
    ]);
    let outcome = fixture
        .run(
            "/Demo.Workspace/.externaldatashares/Lake_name.Lakehouse_abcd.ExternalDataShare",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(outcome.projected.unwrap()["itemId"], ITEM);
    fixture.consumed();
}

#[tokio::test]
async fn onelake_root_metadata_reuses_directory_paging_and_preserves_source_fields() {
    let mut first = step(
        format!("/{WS}/"),
        json!({"paths":[{"name":"first"}],"extra":[1],"ContinuationToken":"remove"}),
    );
    first.headers.push(("x-ms-continuation", "next+/="));
    let fixture = ResourceFixture::new(vec![
        workspace(),
        lakehouse(),
        first,
        step(
            format!("/{WS}/"),
            json!({"paths":[{"name":"second"}],"extra":[2]}),
        ),
    ]);
    let outcome = fixture
        .run(
            "/Demo.Workspace/Lake_name.Lakehouse/Files",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    let data = outcome.projected.unwrap();
    assert_eq!(data["paths"], json!([{"name":"first"},{"name":"second"}]));
    assert_eq!(data["extra"], json!([1, 2]));
    assert!(data.get("ContinuationToken").is_none());
    let requests = fixture.wire.requests.lock().unwrap();
    assert!(requests[2].1.contains("directory="));
    assert!(requests[2].1.contains("getShortcutMetadata=true"));
    assert!(requests[3].1.contains("continuationToken=next%2B%2F%3D"));
    fixture.consumed();
}

#[tokio::test]
async fn onelake_folder_file_and_table_get_metadata_never_downloads_contents() {
    for (root, name, resource_type) in [
        ("Files", "folder", "directory"),
        ("Files", "file.csv", "file"),
        ("Tables", "table", "directory"),
        ("Files", "undefined", "future-type"),
    ] {
        let mut properties = step(format!("/{WS}/{ITEM}/{root}/{name}"), Value::Null);
        properties.method = Method::Head;
        properties
            .headers
            .push(("x-ms-resource-type", resource_type));
        let fixture = ResourceFixture::new(vec![
            workspace(),
            lakehouse(),
            properties,
            step(format!("/{WS}/"), json!({"paths":[],"source":"metadata"})),
        ]);
        let outcome = fixture
            .run(
                &format!("/Demo.Workspace/Lake_name.Lakehouse/{root}/{name}"),
                Some("."),
                None,
                None,
                GetResourceConfig::default(),
            )
            .await
            .result
            .unwrap();
        assert_eq!(outcome.projected.unwrap()["source"], "metadata");
        fixture.consumed();
    }
}

#[tokio::test]
async fn shortcut_get_uses_fabric_metadata_not_external_target_or_storage_download() {
    let mut properties = step(format!("/{WS}/{ITEM}/Files/link"), Value::Null);
    properties.method = Method::Head;
    properties.headers = vec![
        ("x-ms-resource-type", "directory"),
        ("x-ms-onelake-shortcut-path", "true"),
    ];
    let fixture = ResourceFixture::new(vec![
        workspace(),
        lakehouse(),
        properties,
        step(
            format!("/v1/workspaces/{WS}/items/{ITEM}/shortcuts/Files/link"),
            json!({"name":"link","path":"Files","target":{"adlsGen2":{"location":"https://untrusted.invalid"}}}),
        ),
    ]);
    let outcome = fixture
        .run(
            "/Demo.Workspace/Lake_name.Lakehouse/Files/link.Shortcut",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(outcome.projected.unwrap()["name"], "link");
    assert_eq!(outcome.evidence.operations[0].phase, "shortcut");
    let scopes = fixture.credential.scopes.lock().unwrap();
    assert_eq!(scopes[2], ["https://storage.azure.com/.default"]);
    assert_eq!(scopes[3], ["https://api.fabric.microsoft.com/.default"]);
    fixture.consumed();
}

#[tokio::test]
async fn hidden_get_output_writes_the_projected_value_and_keeps_filename_contract() {
    let fixture = ResourceFixture::new(vec![
        step(
            "/v1/connections",
            json!({"value":[{"id":"c","displayName":"Main"}]}),
        ),
        step(
            "/v1/connections/c",
            json!({"id":"c","properties":{"count":0}}),
        ),
    ]);
    let mut files = Files::new(Arc::default());
    files.expected_name = "Main.Connection";
    let outcome = fixture
        .run(
            "/.connections/Main.Connection",
            Some("properties"),
            Some("out"),
            Some(&mut files),
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(outcome.projected, Some(json!({"count":0})));
    assert_eq!(files.writes.len(), 1);
    assert_eq!(
        files.writes[0].0,
        "Main.Connection/get_20000102_030405.json"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&files.writes[0].1).unwrap(),
        json!({"count":0})
    );
    fixture.consumed();
}

#[tokio::test]
async fn source_unsupported_virtual_targets_fail_explicitly_without_fake_reads() {
    for path in [
        "/.connections",
        "/Demo.Workspace/.managedidentities/Demo.ManagedIdentity",
    ] {
        let steps = if path.starts_with("/Demo") {
            vec![workspace()]
        } else {
            vec![]
        };
        let fixture = ResourceFixture::new(steps);
        let failure = fixture
            .run(path, Some("."), None, None, GetResourceConfig::default())
            .await
            .result
            .unwrap_err();
        assert!(matches!(failure.kind, GetErrorKind::Command(_)));
        assert!(failure.evidence.operations.is_empty());
        fixture.consumed();
    }
}

#[tokio::test]
async fn hidden_get_honors_the_selected_fabric_host_without_changing_token_audience() {
    let fixture = ResourceFixture::with_host(
        vec![
            step(
                "/v1/connections",
                json!({"value":[{"id":"c","displayName":"Main"}]}),
            ),
            step("/v1/connections/c", json!({"id":"c"})),
        ],
        "dailyapi.fabric.microsoft.com",
    );
    assert!(
        fixture
            .run(
                "/.connections/Main.Connection",
                Some("."),
                None,
                None,
                GetResourceConfig::default()
            )
            .await
            .result
            .is_ok()
    );
    assert!(
        fixture
            .wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, url)| url.starts_with("https://dailyapi.fabric.microsoft.com/"))
    );
    fixture.consumed();
}

#[tokio::test]
async fn capacity_get_checks_f_sku_searches_arm_and_enriches_with_fabric_id() {
    for configured in [false, true] {
        let capacities = || {
            step(
                "/v1/capacities",
                json!({"value":[{"id":"fabric-capacity","displayName":"Main","sku":"F64"}]}),
            )
        };
        let arm =
            "/subscriptions/sub/resourceGroups/group/providers/Microsoft.Fabric/capacities/Main";
        let mut steps = vec![capacities(), capacities()];
        if configured {
            steps.push(step(arm, json!({"id":arm,"name":"Main"})));
        } else {
            steps.extend([
                step(
                    "/subscriptions",
                    json!({"value":[{"id":"/subscriptions/sub"}]}),
                ),
                step(
                    "/subscriptions/sub/providers/Microsoft.Fabric/capacities",
                    json!({"value":[{"id":arm,"name":"main"}]}),
                ),
            ]);
        }
        steps.push(step(
            arm,
            json!({"id":arm,"name":"Main","sku":{"name":"F64"}}),
        ));
        let fixture = ResourceFixture::new(steps);
        let config = if configured {
            GetResourceConfig {
                subscription_id: Some("sub".into()),
                resource_group: Some("group".into()),
            }
        } else {
            GetResourceConfig::default()
        };
        let outcome = fixture
            .run("/.capacities/Main.Capacity", Some("."), None, None, config)
            .await
            .result
            .unwrap();
        assert_eq!(outcome.projected.unwrap()["fabricId"], "fabric-capacity");
        let scopes = fixture.credential.scopes.lock().unwrap();
        assert!(
            scopes[2..]
                .iter()
                .all(|scope| scope == &["https://management.azure.com/.default"])
        );
        fixture.consumed();
    }
}

#[tokio::test]
async fn capacity_get_rejects_non_fabric_sku_before_any_arm_request() {
    let capacities = || {
        step(
            "/v1/capacities",
            json!({"value":[{"id":"c","displayName":"Main","sku":"P1"}]}),
        )
    };
    let fixture = ResourceFixture::new(vec![capacities(), capacities()]);
    assert!(
        fixture
            .run(
                "/.capacities/Main.Capacity",
                Some("."),
                None,
                None,
                GetResourceConfig::default()
            )
            .await
            .result
            .is_err()
    );
    fixture.consumed();
}

#[tokio::test]
async fn hidden_primary_failures_and_quiet_status_never_become_export_success() {
    for (status, body) in [
        (401, json!({"errorCode":"Unauthorized","message":"denied"})),
        (200, json!([])),
        (201, json!({"id":"c"})),
    ] {
        let mut primary = step("/v1/connections/c", body);
        primary.status = status;
        let fixture = ResourceFixture::new(vec![
            step(
                "/v1/connections",
                json!({"value":[{"id":"c","displayName":"Main"}]}),
            ),
            primary,
        ]);
        let mut files = Files::new(Arc::default());
        files.expected_name = "Main.Connection";
        let dispatch = fixture
            .run(
                "/.connections/Main.Connection",
                Some("."),
                Some("out"),
                Some(&mut files),
                GetResourceConfig::default(),
            )
            .await;
        match status {
            201 => {
                let outcome = dispatch.result.unwrap();
                assert!(outcome.projected.is_none());
                assert!(outcome.output.is_none());
            }
            401 => {
                let failure = dispatch.result.unwrap_err();
                let GetErrorKind::Operation(error) = failure.kind else {
                    panic!("original operation")
                };
                assert!(matches!(
                    error.kind,
                    fabric_sdk_operations::OperationErrorKind::Http { status: 401, .. }
                ));
                assert_eq!(
                    error.raw_service_error.as_ref().unwrap()["message"],
                    "denied"
                );
                assert_eq!(failure.evidence.operations.len(), 1);
            }
            _ => assert!(dispatch.result.is_err()),
        }
        assert!(files.writes.is_empty());
        fixture.consumed();
    }
}

#[tokio::test]
async fn domain_unsafe_continuation_is_a_typed_partial_failure_not_optional_metadata() {
    let rows = || {
        step(
            "/v1/admin/domains/domain/workspaces",
            json!({"value":[{"id":"row"}],"continuationToken":"same"}),
        )
    };
    let fixture = ResourceFixture::new(vec![
        step(
            "/v1/admin/domains",
            json!({"domains":[{"id":"domain","displayName":"Main"}]}),
        ),
        step("/v1/admin/domains/domain", json!({"id":"domain"})),
        rows(),
        rows(),
    ]);
    let failure = fixture
        .run(
            "/.domains/Main.Domain",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap_err();
    let GetErrorKind::Collection(error) = failure.kind else {
        panic!("original collection failure")
    };
    assert_eq!(error.error, fabric_sdk_api::ApiError::Continuation);
    assert!(!error.partial.rows.is_empty());
    fixture.consumed();
}

#[tokio::test]
async fn hidden_output_partial_write_keeps_existing_file_effects_and_is_not_replayed() {
    let fixture = ResourceFixture::new(vec![
        step(
            "/v1/connections",
            json!({"value":[{"id":"c","displayName":"Main"}]}),
        ),
        step("/v1/connections/c", json!({"id":"c"})),
    ]);
    let mut files = Files::new(Arc::default());
    files.expected_name = "Main.Connection";
    files.fail = Some("write");
    let failure = fixture
        .run(
            "/.connections/Main.Connection",
            Some("."),
            Some("out"),
            Some(&mut files),
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap_err();
    assert!(matches!(failure.kind, GetErrorKind::Definition(_)));
    assert_eq!(files.writes.len(), 1);
    assert!(failure.evidence.files.incomplete_path.is_some());
    fixture.consumed();
}

#[tokio::test]
async fn capacity_initial_201_202_reconcile_both_arm_headers_and_preserve_original_metadata() {
    const ARM: &str =
        "/subscriptions/sub/resourceGroups/group/providers/Microsoft.Fabric/capacities/Main";
    const POLL: &str = "https://management.azure.com/providers/Microsoft.Fabric/operations/op?api-version=2023-11-01";
    for status in [201, 202] {
        for header in ["azure-asyncoperation", "location"] {
            let capacities = || {
                step(
                    "/v1/capacities",
                    json!({"value":[{"id":"fabric-capacity","displayName":"Main","sku":"F64"}]}),
                )
            };
            let mut initial = step(
                ARM,
                json!({"id":ARM,"name":"original","properties":{"source":true}}),
            );
            initial.status = status;
            initial.headers = vec![(header, POLL), ("retry-after", "0")];
            let mut terminal = step(
                "/providers/Microsoft.Fabric/operations/op",
                if header == "azure-asyncoperation" {
                    json!({"status":"Succeeded"})
                } else {
                    json!({"id":"not-the-metadata-result"})
                },
            );
            terminal
                .headers
                .push(("location", "https://untrusted.invalid/never-fetch"));
            let fixture = ResourceFixture::new(vec![
                capacities(),
                capacities(),
                step(
                    "/subscriptions",
                    json!({"value":[{"id":"/subscriptions/sub"}]}),
                ),
                step(
                    "/subscriptions/sub/providers/Microsoft.Fabric/capacities",
                    json!({"value":[{"id":ARM,"name":"Main"}]}),
                ),
                initial,
                terminal,
            ]);
            let outcome = fixture
                .run(
                    "/.capacities/Main.Capacity",
                    Some("."),
                    None,
                    None,
                    GetResourceConfig::default(),
                )
                .await
                .result
                .unwrap();
            let data = outcome.projected.unwrap();
            assert_eq!(data["name"], "original");
            assert_eq!(data["fabricId"], "fabric-capacity");
            assert_eq!(
                outcome
                    .evidence
                    .operations
                    .last()
                    .unwrap()
                    .operation_id
                    .as_deref(),
                Some("op")
            );
            assert!(
                !outcome
                    .evidence
                    .operations
                    .last()
                    .unwrap()
                    .mutation_may_have_occurred
            );
            fixture.consumed();
        }
    }
}

#[tokio::test]
async fn capacity_arm_collection_reads_reconcile_through_existing_api_pager_and_shared_operation_loop()
 {
    const ARM: &str =
        "/subscriptions/sub/resourceGroups/group/providers/Microsoft.Fabric/capacities/Main";
    let capacities = || {
        step(
            "/v1/capacities",
            json!({"value":[{"id":"fabric-capacity","displayName":"Main","sku":"F64"}]}),
        )
    };
    let mut subscriptions = step(
        "/subscriptions",
        json!({"value":[{"id":"/subscriptions/sub"}]}),
    );
    subscriptions.status = 201;
    subscriptions.headers = vec![
        (
            "azure-asyncoperation",
            "https://management.azure.com/providers/Microsoft.Fabric/operations/subscriptions?api-version=2023-11-01",
        ),
        ("retry-after", "0"),
    ];
    let mut arm_list = step(
        "/subscriptions/sub/providers/Microsoft.Fabric/capacities",
        json!({"value":[{"id":ARM,"name":"Main"}]}),
    );
    arm_list.status = 202;
    arm_list.headers = vec![
        (
            "location",
            "https://management.azure.com/providers/Microsoft.Fabric/operations/list?api-version=2023-11-01",
        ),
        ("retry-after", "0"),
    ];
    let fixture = ResourceFixture::new(vec![
        capacities(),
        capacities(),
        subscriptions,
        step(
            "/providers/Microsoft.Fabric/operations/subscriptions",
            json!({"status":"Completed"}),
        ),
        arm_list,
        step("/providers/Microsoft.Fabric/operations/list", Value::Null),
        step(ARM, json!({"id":ARM,"name":"Main"})),
    ]);
    let outcome = fixture
        .run(
            "/.capacities/Main.Capacity",
            Some("."),
            None,
            None,
            GetResourceConfig::default(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(outcome.projected.unwrap()["fabricId"], "fabric-capacity");
    fixture.consumed();
}

#[tokio::test]
async fn configured_capacity_lookup_failure_falls_back_in_source_order_and_keeps_original_error() {
    const ARM: &str =
        "/subscriptions/sub/resourceGroups/group/providers/Microsoft.Fabric/capacities/Main";
    let capacities = || {
        step(
            "/v1/capacities",
            json!({"value":[{"id":"fabric-capacity","displayName":"Main","sku":"F64"}]}),
        )
    };
    let mut missing = step(
        "/subscriptions/configured/resourceGroups/missing/providers/Microsoft.Fabric/capacities/Main",
        json!({"errorCode":"NotFound","message":"configured lookup failed"}),
    );
    missing.status = 404;
    let fixture = ResourceFixture::new(vec![
        capacities(),
        capacities(),
        missing,
        step(
            "/subscriptions",
            json!({"value":[{"id":"/subscriptions/sub"}]}),
        ),
        step(
            "/subscriptions/sub/providers/Microsoft.Fabric/capacities",
            json!({"value":[{"id":ARM,"name":"Main"}]}),
        ),
        step(ARM, json!({"id":ARM,"name":"Main"})),
    ]);
    let outcome = fixture
        .run(
            "/.capacities/Main.Capacity",
            Some("."),
            None,
            None,
            GetResourceConfig {
                subscription_id: Some("configured".into()),
                resource_group: Some("missing".into()),
            },
        )
        .await
        .result
        .unwrap();
    assert_eq!(outcome.projected.unwrap()["fabricId"], "fabric-capacity");
    assert_eq!(
        outcome.evidence.enrichment_failures[0].phase,
        "configured capacity lookup"
    );
    assert!(matches!(
        outcome.evidence.enrichment_failures[0].kind,
        GetErrorKind::Operation(_)
    ));
    assert_eq!(outcome.evidence.operations.len(), 2);
    fixture.consumed();
}
