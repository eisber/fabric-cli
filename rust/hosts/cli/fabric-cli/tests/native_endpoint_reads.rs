// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Selected-host contracts through the production read session and shared HTTP.

#[allow(dead_code)]
mod support;

use async_trait::async_trait;
use azure_core::http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers};
use fabric_cli::{
    ReadConfig,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    capacities::{CapacityListErrorKind, CapacityListLimits},
    folders::{FolderErrorKind, FolderListOptions},
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fmt,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{Credential, Reply, reply};

const HOSTS: [&str; 3] = [
    "api.fabric.microsoft.com",
    "dailyapi.fabric.microsoft.com",
    "msitapi.fabric.microsoft.com",
];

struct Wire {
    host: String,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<String>>,
}

impl fmt::Debug for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfflineEndpointHttp")
            .finish_non_exhaustive()
    }
}

fn has_injected_authorization(headers: &Headers) -> bool {
    headers
        .get_str(&"authorization".into())
        .is_ok_and(|value| value == concat!("Bearer ", "offline-command-test"))
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.method(), Method::Get);
        assert_eq!(request.url().host_str(), Some(self.host.as_str()));
        assert!(
            has_injected_authorization(request.headers()),
            "request authorization must match the injected offline credential"
        );
        self.requests
            .lock()
            .unwrap()
            .push(request.url().to_string());
        let response = self.replies.lock().unwrap().pop_front().unwrap();
        let mut headers = Headers::new();
        for (name, value) in response.headers {
            headers.insert(name, value);
        }
        Ok(AsyncRawResponse::from_bytes(
            response.status.into(),
            headers,
            serde_json::to_vec(&response.body).unwrap(),
        ))
    }
}

#[tokio::test]
async fn authorization_guard_uses_raw_accessor_and_rejects_missing_masked_or_foreign_values() {
    use fabric_sdk::TokenCredential;

    let credential = Credential::default();
    let token = credential
        .get_token(&[Audience::Fabric.scope()], None)
        .await
        .unwrap();
    let mut headers = Headers::new();
    assert!(!has_injected_authorization(&headers));
    headers.insert("authorization", format!("Bearer {}", token.token.secret()));
    assert!(has_injected_authorization(&headers));
    assert!(!format!("{headers:?}").contains(token.token.secret()));
    for rejected in [
        "******",
        "offline-command-test",
        "Basic offline-command-test",
        "Bearer different-offline-credential",
        "Bearer offline-command-test ",
    ] {
        headers.insert("authorization", rejected);
        assert!(
            !has_injected_authorization(&headers),
            "an altered synthetic authorization value must be rejected"
        );
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == Duration::from_secs(240) {
                pending::<()>().await;
            }
        })
    }
}

fn configured(
    host: &str,
    replies: impl IntoIterator<Item = Reply>,
) -> (Client, HttpExecutor, Arc<Wire>, Arc<Credential>) {
    let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
    let credential = Arc::new(Credential::default());
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
        endpoint.clone(),
    ));
    let wire = Arc::new(Wire {
        host: host.into(),
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
    });
    let http = HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    (sdk, http, wire, credential)
}

async fn run(sdk: &Client, http: &HttpExecutor, args: &[&str]) -> (u8, Value) {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read required");
    };
    let session = ReadSession {
        sdk,
        executor: http,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = session
        .with_source_operations(http)
        .dispatch(&command, pending())
        .await
        .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    assert!(err.is_empty());
    (plan.exit_code, serde_json::from_slice(&out).unwrap())
}

fn discovery() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace","capacityId":"c"}]}))
}

fn assert_completed(wire: &Wire, credential: &Credential, root: &str, paths: &[&str]) {
    assert!(wire.replies.lock().unwrap().is_empty());
    assert_eq!(
        *wire.requests.lock().unwrap(),
        paths
            .iter()
            .map(|path| format!("{root}{path}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec!["https://api.fabric.microsoft.com/.default".to_owned()]; paths.len()]
    );
}

#[tokio::test]
async fn selected_host_covers_workspace_item_discovery_pages_and_specialized_get() {
    for host in HOSTS {
        let root = format!("https://{host}");
        let mut first = discovery();
        first.body["continuationToken"] = json!("next+/=");
        first.body["continuationUri"] = json!(format!("{root}/v1/workspaces"));
        let (sdk, http, wire, credential) = configured(
            host,
            [
                first,
                reply(json!({"value":[]})),
                reply(
                    json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}],
                    "continuationToken":"next+/=",
                    "continuationUri":format!("{root}/v1/workspaces/w/items")}),
                ),
                reply(json!({"value":[]})),
                reply(json!({"id":"n","displayName":"Book","description":"selected"})),
            ],
        );
        let (exit, output) = run(
            &sdk,
            &http,
            &["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
        )
        .await;
        assert_eq!(exit, 0);
        assert_eq!(output["result"]["data"], json!(["selected"]));
        assert_completed(
            &wire,
            &credential,
            &root,
            &[
                "/v1/workspaces",
                "/v1/workspaces?continuationToken=next%2B%2F%3D",
                "/v1/workspaces/w/items",
                "/v1/workspaces/w/items?continuationToken=next%2B%2F%3D",
                "/v1/workspaces/w/notebooks/n",
            ],
        );
    }
}

#[tokio::test]
async fn selected_host_covers_long_ls_capacity_pages() {
    for host in HOSTS {
        let root = format!("https://{host}");
        let (sdk, http, wire, credential) = configured(
            host,
            [
                discovery(),
                reply(json!({"value":[], "continuationToken":"next+/=",
                    "continuationUri":format!("{root}/v1/capacities")})),
                reply(json!({"value":[{"id":"c","displayName":"Capacity","region":"West"}]})),
            ],
        );
        let (exit, output) = run(&sdk, &http, &["ls", "/", "-l"]).await;
        assert_eq!(exit, 0);
        assert_eq!(output["result"]["data"][0]["capacityName"], "Capacity");
        assert_completed(
            &wire,
            &credential,
            &root,
            &[
                "/v1/workspaces",
                "/v1/capacities",
                "/v1/capacities?continuationToken=next%2B%2F%3D",
            ],
        );
    }
}

#[tokio::test]
async fn selected_host_covers_folder_discovery_pages_and_rendering() {
    for host in HOSTS {
        let root = format!("https://{host}");
        let graph = reply(json!({"value":[{"id":"f","displayName":"Folder"}]}));
        let (sdk, http, wire, credential) = configured(
            host,
            [
                discovery(),
                reply(json!({"value":[], "continuationToken":"next+/=",
                    "continuationUri":format!("{root}/v1/workspaces/w/folders")})),
                graph.clone(),
                reply(
                    json!({"value":[{"id":"n","displayName":"Book","type":"Notebook","folderId":"f"}]}),
                ),
                graph,
            ],
        );
        let (exit, output) = run(&sdk, &http, &["ls", "Demo.Workspace/Folder.Folder", "-l"]).await;
        assert_eq!(exit, 0);
        assert_eq!(
            output["result"]["data"],
            json!([{"name":"Book.Notebook","id":"n"}])
        );
        assert_completed(
            &wire,
            &credential,
            &root,
            &[
                "/v1/workspaces",
                "/v1/workspaces/w/folders?recursive=True",
                "/v1/workspaces/w/folders?recursive=True&continuationToken=next%2B%2F%3D",
                "/v1/workspaces/w/items",
                "/v1/workspaces/w/folders?recursive=True",
            ],
        );
    }
}

fn accepted(root: &str) -> Reply {
    Reply {
        status: 202,
        body: json!({}),
        headers: [
            ("x-ms-operation-id".into(), "op".into()),
            ("Retry-After".into(), "0".into()),
            ("Location".into(), format!("{root}/v1/operations/op")),
        ]
        .into(),
    }
}

fn succeeded(root: &str) -> Reply {
    Reply {
        headers: [("Location".into(), format!("{root}/v1/operations/op/result"))].into(),
        ..reply(json!({"status":"Succeeded"}))
    }
}

#[tokio::test]
async fn selected_host_covers_workspace_get_poll_result_and_enrichment() {
    for host in HOSTS {
        let root = format!("https://{host}");
        let (sdk, http, wire, credential) = configured(
            host,
            [
                discovery(),
                accepted(&root),
                succeeded(&root),
                reply(json!({"id":"w","displayName":"Demo"})),
                reply(json!({"value":[]})),
                reply(json!({"enabled":true})),
                reply(json!({"value":[]})),
            ],
        );
        let (exit, output) = run(&sdk, &http, &["get", "Demo.Workspace", "-q", "."]).await;
        assert_eq!(exit, 0);
        assert_eq!(
            output["result"]["data"][0]["sparkSettings"],
            json!({"enabled":true})
        );
        assert_completed(
            &wire,
            &credential,
            &root,
            &[
                "/v1/workspaces",
                "/v1/workspaces/w",
                "/v1/operations/op",
                "/v1/operations/op/result",
                "/v1/workspaces/w/managedPrivateEndpoints",
                "/v1/workspaces/w/spark/settings",
                "/v1/workspaces/w/roleAssignments",
            ],
        );
    }
}

#[tokio::test]
async fn folder_and_capacity_foreign_continuations_stop_before_another_token_or_request() {
    for folder in [true, false] {
        let path = if folder {
            "/v1/workspaces/w/folders"
        } else {
            "/v1/capacities"
        };
        let (sdk, http, wire, credential) = configured(
            "msitapi.fabric.microsoft.com",
            [reply(json!({"value":[], "continuationToken":"next",
                "continuationUri":format!("https://api.fabric.microsoft.com{path}")}))],
        );
        if folder {
            let error = sdk
                .list_folders(&http, "w", FolderListOptions::default(), pending())
                .await
                .unwrap_err();
            assert_eq!(error.kind, FolderErrorKind::InvalidContinuation);
        } else {
            let error = sdk
                .list_capacities(&http, CapacityListLimits::default(), pending())
                .await
                .unwrap_err();
            assert_eq!(error.kind, CapacityListErrorKind::InvalidContinuation);
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn read_discovery_and_operation_result_foreign_hosts_never_receive_another_request() {
    for result_link in [false, true] {
        let replies = if result_link {
            vec![
                discovery(),
                accepted("https://msitapi.fabric.microsoft.com"),
                succeeded("https://api.fabric.microsoft.com"),
            ]
        } else {
            vec![reply(json!({"value":[], "continuationToken":"next",
                "continuationUri":"https://api.fabric.microsoft.com/v1/workspaces"}))]
        };
        let (sdk, http, wire, credential) = configured("msitapi.fabric.microsoft.com", replies);
        let (exit, output) = run(&sdk, &http, &["get", "Demo.Workspace", "-q", "."]).await;
        assert_eq!(exit, 1);
        assert_eq!(output["status"], "Failure");
        let expected = if result_link { 3 } else { 1 };
        assert_eq!(wire.requests.lock().unwrap().len(), expected);
        assert_eq!(credential.scopes.lock().unwrap().len(), expected);
        assert!(wire.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn wrong_service_binding_is_rejected_before_folder_capacity_credentials_or_http() {
    let (sdk, http, wire, credential) = configured("msitapi.fabric.microsoft.com", []);
    let sdk = Client::with_context(RequestContext::with_endpoint(
        sdk.credential().clone(),
        sdk.selection().clone(),
        EndpointBinding::public(ServiceOrigin::Arm),
    ));
    assert_eq!(
        sdk.list_folders(&http, "w", FolderListOptions::default(), pending())
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::InvalidContinuation
    );
    assert_eq!(
        sdk.list_capacities(&http, CapacityListLimits::default(), pending())
            .await
            .unwrap_err()
            .kind,
        CapacityListErrorKind::InvalidContinuation
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}
