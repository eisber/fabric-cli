// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The production native host uses the selected official credential and executor.
#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    Bytes,
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
};
use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const CLIENT: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<&'static str>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let body = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP");
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::Ok,
            Headers::new(),
            body.as_bytes().to_vec(),
        ))
    }
}

fn selection(mode: &str, path: Option<&str>) -> crate::headless::Selection {
    crate::headless::Selection::from_inputs(mode, |name| -> Option<OsString> {
        match name {
            "FAB_RUST_TENANT" => Some(TENANT.into()),
            "FAB_RUST_CLIENT_ID" => Some(CLIENT.into()),
            "FAB_RUST_CLIENT_SECRET" => Some("synthetic-client-secret".into()),
            "FAB_RUST_ASSERTION_FILE" => path.map(Into::into),
            _ => None,
        }
    })
    .unwrap()
}

#[tokio::test]
async fn actual_native_host_lists_workspaces_with_official_lazy_headless_credential() {
    let http = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            r#"{"access_token":"synthetic-fabric-token","token_type":"Bearer","expires_in":3600}"#,
            r#"{"value":[{"id":"33333333-3333-3333-3333-333333333333","displayName":"Offline workspace","type":"Workspace","capacityId":null}]}"#,
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let Invocation::Read(command) =
        fabric_cli::parser::parse_args(["fab-rs", "ls", "/", "--output_format", "json"]).unwrap()
    else {
        panic!("read invocation")
    };
    let credential = NativeCredential::Headless(selection("client-secret", None));
    let mut state = BatchState::capture();
    credential
        .bind(&mut state, fabric_cli_context::HomeContext::Unspecified)
        .unwrap();
    let (plan, outcome) = execute_injected(
        NativeCommand::Read(&command),
        (credential, http.clone(), http.clone()),
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        EndpointBinding::public(ServiceOrigin::Fabric),
        settings::NativeSettings::select(None).unwrap(),
        &mut state,
    )
    .await
    .unwrap();
    assert_eq!(outcome, CommandOutcome::Returned(0));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("Offline workspace"));
    assert!(
        !output.contains("synthetic-fabric-token") && !output.contains("synthetic-client-secret")
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(
        requests[0].url().host_str(),
        Some("login.microsoftonline.com")
    );
    assert!(
        String::from_utf8(Bytes::from(requests[0].body()).to_vec())
            .unwrap()
            .contains("client_secret=synthetic-client-secret")
    );
    assert_eq!(
        requests[1].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces"
    );
    assert_eq!(
        requests[1]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        "Bearer synthetic-fabric-token"
    );
    assert!(http.replies.lock().unwrap().is_empty());
}

#[test]
fn shared_batch_and_repl_state_reject_provider_and_assertion_binding_switches() {
    let mut state = BatchState::capture();
    let home = fabric_cli_context::HomeContext::Unspecified;
    let credential = NativeCredential::Headless(selection("client-secret", None));
    credential.bind(&mut state, home.clone()).unwrap();
    credential.bind(&mut state, home.clone()).unwrap();
    assert!(
        state
            .bind_provider(
                TENANT,
                CLIENT,
                CLIENT,
                "workload-identity:changed-file",
                home.clone()
            )
            .is_err()
    );
    assert!(state.bind(TENANT, CLIENT, CLIENT, home).is_err());
}

#[tokio::test]
async fn official_app_service_credential_reaches_native_wire_and_real_command_executor() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/msi/token", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let length = socket.read(&mut chunk).await.unwrap();
            assert_ne!(length, 0);
            bytes.extend_from_slice(&chunk[..length]);
            assert!(bytes.len() <= 16 * 1024);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(bytes).unwrap();
        assert!(request.starts_with("GET /msi/token?"));
        assert!(request.contains("resource=https%3A%2F%2Fapi.fabric.microsoft.com"));
        assert!(request.contains("x-identity-header: synthetic-rotated-header"));
        assert!(request.contains(&format!("client_id={CLIENT}")));
        let expiry = azure_core::time::OffsetDateTime::now_utc().unix_timestamp() + 3600;
        let body = format!(
            r#"{{"access_token":"synthetic-managed-wire-token","expires_on":"{expiry}","token_type":"Bearer","resource":"https://api.fabric.microsoft.com"}}"#
        );
        socket.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
        ).as_bytes()).await.unwrap();
    });
    let child = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "native_read::headless_host_tests::native_managed_wire_process_child",
                "--nocapture",
            ])
            .env_clear()
            .env("FAB_HEADLESS_NATIVE_WIRE_TEST", "1")
            .env("FAB_RUST_TENANT", TENANT)
            .env("FAB_RUST_CLIENT_ID", CLIENT)
            .env("FAB_RUST_MANAGED_IDENTITY", "client")
            .env("IDENTITY_ENDPOINT", endpoint)
            .env("IDENTITY_HEADER", "synthetic-rotated-header");
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.output().unwrap()
    })
    .await
    .unwrap();
    let serving_result = tokio::time::timeout(std::time::Duration::from_secs(3), serving).await;
    assert!(
        child.status.success(),
        "server={serving_result:?}; {}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("native-managed-wire-exercised"));
    serving_result.unwrap().unwrap();
}

#[tokio::test]
async fn native_managed_wire_process_child() {
    if std::env::var("FAB_HEADLESS_NATIVE_WIRE_TEST")
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    let credential = NativeCredential::Headless(
        crate::headless::Selection::capture("managed-identity-app-service").unwrap(),
    );
    let metadata = credential.metadata_endpoint().unwrap();
    let identity_http = fabric_sdk_transport::NativeHttpClient::for_managed_identity(
        fabric_sdk_transport::TransportOptions {
            connect_timeout: std::time::Duration::from_secs(2),
            read_timeout: std::time::Duration::from_secs(2),
            write_timeout: std::time::Duration::from_secs(2),
            pool_idle_timeout: std::time::Duration::from_secs(2),
            max_idle_per_host: std::num::NonZeroUsize::new(1).unwrap(),
            trust: fabric_sdk_transport::TrustPolicy::Platform {
                additional_roots: Vec::new(),
            },
            proxy: fabric_sdk_transport::ProxyPolicy::None,
        },
        metadata,
    )
    .unwrap();
    let http = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([r#"{"value":[]}"#])),
        requests: Mutex::new(Vec::new()),
    });
    let Invocation::Read(command) = fabric_cli::parser::parse_args(["fab-rs", "ls", "/"]).unwrap()
    else {
        panic!("read invocation")
    };
    let mut state = BatchState::capture();
    credential
        .bind(&mut state, fabric_cli_context::HomeContext::Unspecified)
        .unwrap();
    let (_, outcome) = execute_injected(
        NativeCommand::Read(&command),
        (credential, http.clone(), Arc::new(identity_http)),
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        EndpointBinding::public(ServiceOrigin::Fabric),
        settings::NativeSettings::select(None).unwrap(),
        &mut state,
    )
    .await
    .unwrap();
    assert_eq!(outcome, CommandOutcome::Returned(0));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        "Bearer synthetic-managed-wire-token"
    );
    println!("native-managed-wire-exercised");
}
