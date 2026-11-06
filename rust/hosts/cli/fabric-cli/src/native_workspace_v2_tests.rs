// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests of the opt-in workspace host, without ambient credential discovery.
#![deny(unnameable_test_items)]

use super::workspace_v2_native;
use fabric_cli::workspace_v2::{
    ParsedWorkspaceInvocation, WorkspaceCommand, failure_envelope, parse_args, write_result,
};
use fabric_sdk_api::workspace_reads::WorkspaceReadError;
use std::ffi::OsString;

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
#[cfg(any(windows, feature = "headless"))]
const OBSERVED_AT: &str = "2026-11-05T20:30:55.000000Z";

fn command(verb: &str, timeout: &str, extra: &[&str]) -> WorkspaceCommand {
    let path = format!("/{WORKSPACE}.Workspace");
    let mut args = vec![
        "fab-cli",
        verb,
        &path,
        "--max-bytes",
        "4096",
        "--timeout",
        timeout,
    ];
    if verb == "list" {
        args.extend(["--limit", "3", "--recursive", "false"]);
    }
    args.extend_from_slice(extra);
    let ParsedWorkspaceInvocation::Read(command) =
        parse_args(args.into_iter().map(OsString::from)).unwrap()
    else {
        panic!("expected workspace read")
    };
    command
}

#[test]
fn display_only_invocations_never_enter_native_composition() {
    for args in [
        vec!["fab-cli", "--help"],
        vec!["fab-cli", "--version"],
        vec!["fab-cli", "artifact", "workspace", "--help"],
    ] {
        assert!(matches!(
            parse_args(args.into_iter().map(OsString::from)).unwrap(),
            ParsedWorkspaceInvocation::Display(_)
        ));
    }
}

#[test]
fn an_existing_output_prevents_any_native_setup() {
    use std::{
        cell::Cell,
        fs::{self, OpenOptions},
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::current_dir().unwrap().join(format!(
        ".workspace-v2-native-existing-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(b"unchanged").unwrap();
    drop(file);
    let command = command("get", "2s", &["--output", path.to_str().unwrap()]);
    let setup_calls = Cell::new(0);
    let result = workspace_v2_native::with_reservation(&command, |_| {
        setup_calls.set(setup_calls.get() + 1);
        failure_envelope(Some(&command), WorkspaceReadError::OperationFailed, None)
    });
    assert!(matches!(result, Err(WorkspaceReadError::OutputExists)));
    assert_eq!(setup_calls.get(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"unchanged");
    fs::remove_file(path).unwrap();
}

#[test]
fn failed_native_setup_consumes_the_reservation_without_publishing_output() {
    use std::{
        cell::Cell,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::current_dir().unwrap().join(format!(
        ".workspace-v2-native-failed-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let command = command("get", "2s", &["--output", path.to_str().unwrap()]);
    let setup_calls = Cell::new(0);
    let (reservation, envelope) = workspace_v2_native::with_reservation(&command, |_| {
        setup_calls.set(setup_calls.get() + 1);
        failure_envelope(Some(&command), WorkspaceReadError::NotConfigured, None)
    })
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = write_result(reservation, &envelope, &mut stdout, &mut stderr).unwrap();
    assert_eq!(setup_calls.get(), 1);
    assert_eq!(exit, 1);
    assert!(!path.exists());
    assert!(stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "NotConfigured");
}

#[cfg(not(any(windows, feature = "headless")))]
#[test]
fn unavailable_native_host_is_a_sanitized_failure_not_a_success_stub() {
    let command = command("get", "2s", &[]);
    let envelope =
        workspace_v2_native::run_reserved(&command, std::time::Instant::now()).unwrap_err();
    assert_eq!(envelope.exit_code(), 2);
    let value = serde_json::to_value(envelope).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(value["error"]["code"], "UnsupportedCapability");
    assert!(value["data"].is_null());
}

#[cfg(any(windows, feature = "headless"))]
mod native {
    use super::*;
    use async_trait::async_trait;
    use azure_core::{
        credentials::{AccessToken, TokenCredential, TokenRequestOptions},
        http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
        time::OffsetDateTime,
    };
    use fabric_cli::workspace_v2::{OutputReservation, PreparedWorkspaceResult};
    use fabric_sdk::{
        Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    };
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_transport::TokioTimer;
    use std::{
        future::{pending, ready},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    #[derive(Debug, Default)]
    struct Credential(AtomicUsize);

    #[async_trait]
    impl TokenCredential for Credential {
        async fn get_token(
            &self,
            scopes: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(scopes, [Audience::Fabric.scope()]);
            Ok(AccessToken {
                token: "synthetic-workspace-v2-token".into(),
                expires_on: OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
            })
        }
    }

    #[derive(Debug)]
    struct Wire {
        requests: Mutex<Vec<Request>>,
        body: &'static str,
    }

    #[async_trait]
    impl HttpClient for Wire {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::Ok,
                Headers::new(),
                self.body.as_bytes().to_vec(),
            ))
        }
    }

    fn setup(
        body: &'static str,
        endpoint: EndpointBinding,
        timeout: Duration,
    ) -> (Client, HttpExecutor, Arc<Credential>, Arc<Wire>) {
        let credential = Arc::new(Credential::default());
        let wire = Arc::new(Wire {
            requests: Mutex::new(Vec::new()),
            body,
        });
        let sdk = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            endpoint.clone(),
        ));
        let executor = HttpExecutor::new(
            wire.clone(),
            Arc::new(TokioTimer),
            ExecutionOptions {
                timeout,
                max_delay: timeout,
            },
        )
        .unwrap()
        .with_endpoint(endpoint);
        (sdk, executor, credential, wire)
    }

    #[tokio::test]
    async fn primary_get_uses_one_request_without_navigation_admin_or_enrichment() {
        let command = command("get", "2s", &[]);
        let (sdk, executor, credential, wire) = setup(
            r#"{"id":"11111111-1111-1111-1111-111111111111","displayName":"Offline workspace"}"#,
            EndpointBinding::public(ServiceOrigin::Fabric),
            command.timeout(),
        );
        let envelope =
            workspace_v2_native::execute_authenticated(&command, sdk, executor, OBSERVED_AT)
                .await
                .unwrap();
        assert_eq!(envelope.exit_code(), 0);
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["meta"]["observedAt"], OBSERVED_AT);
        assert_eq!(value["data"]["displayName"], "Offline workspace");
        assert!(!value.to_string().contains("synthetic-workspace-v2-token"));
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}")
        );
    }

    #[tokio::test]
    async fn list_uses_one_native_items_page_without_fetching_continuation() {
        let command = command("list", "2s", &[]);
        let (sdk, executor, credential, wire) = setup(
            r#"{"value":[{"id":"22222222-2222-2222-2222-222222222222","displayName":"Offline item","type":"Notebook"}],"continuationToken":"opaque-workspace-v2-token"}"#,
            EndpointBinding::public(ServiceOrigin::Fabric),
            command.timeout(),
        );
        let envelope =
            workspace_v2_native::execute_authenticated(&command, sdk, executor, OBSERVED_AT)
                .await
                .unwrap();
        assert_ne!(envelope.exit_code(), 0);
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["status"], "partial");
        assert_eq!(value["meta"]["coverage"]["complete"], false);
        assert!(!value.to_string().contains("opaque-workspace-v2-token"));
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().as_str(),
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?recursive=false"
            )
        );
    }

    #[tokio::test]
    async fn nonpublic_binding_is_rejected_before_token_or_http() {
        let command = command("get", "2s", &[]);
        let endpoint =
            EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com")
                .unwrap();
        let (sdk, executor, credential, wire) = setup("{}", endpoint, command.timeout());
        let result =
            workspace_v2_native::execute_authenticated(&command, sdk, executor, OBSERVED_AT).await;
        assert!(matches!(
            result,
            Err(WorkspaceReadError::UnsupportedCapability)
        ));
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(
            workspace_v2_native::workspace_endpoint(Some("dailyapi.fabric.microsoft.com".into()))
                .unwrap_err(),
            WorkspaceReadError::UnsupportedCapability
        );
    }

    #[tokio::test]
    async fn cancellation_wins_over_simultaneous_deadline_and_completion() {
        let result = workspace_v2_native::controlled(
            ready(Err::<(), _>(WorkspaceReadError::NotFound)),
            ready(Ok(())),
            ready(()),
        )
        .await;
        assert_eq!(result, Err(WorkspaceReadError::Cancelled));
    }

    #[tokio::test]
    async fn explicit_total_deadline_also_covers_pending_local_setup() {
        let command = command("get", "1ms", &[]);
        let completed = AtomicBool::new(false);
        let result = workspace_v2_native::controlled(
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                completed.store(true, Ordering::SeqCst);
                Err::<(), _>(WorkspaceReadError::NotFound)
            },
            pending(),
            tokio::time::sleep(command.timeout()),
        )
        .await;
        assert_eq!(command.timeout(), Duration::from_millis(1));
        assert!(!completed.load(Ordering::SeqCst));
        assert_eq!(result, Err(WorkspaceReadError::Timeout));
    }

    #[tokio::test]
    async fn completion_repolls_the_deadline_before_accepting_rendered_bytes() {
        let command = command("get", "2s", &[]);
        let prepared = PreparedWorkspaceResult::new(failure_envelope(
            Some(&command),
            WorkspaceReadError::NotFound,
            Some(OBSERVED_AT),
        ))
        .unwrap();
        let mut polls = 0;
        let deadline = std::future::poll_fn(|_| {
            polls += 1;
            if polls == 1 {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        });
        let result =
            workspace_v2_native::controlled(ready(Ok(prepared)), pending(), deadline).await;
        assert!(matches!(result, Err(WorkspaceReadError::Timeout)));
    }

    #[tokio::test]
    async fn prepared_delivery_checks_before_commit_and_rejects_late_io_success() {
        let command = command("get", "2s", &[]);
        let (sdk, executor, credential, wire) = setup(
            r#"{"id":"11111111-1111-1111-1111-111111111111","displayName":"Offline workspace"}"#,
            EndpointBinding::public(ServiceOrigin::Fabric),
            command.timeout(),
        );
        let envelope =
            workspace_v2_native::execute_authenticated(&command, sdk, executor, OBSERVED_AT)
                .await
                .unwrap();
        let mut expected = serde_json::to_vec(&envelope).unwrap();
        expected.push(b'\n');
        let before = Instant::now();
        let deadline = before + command.timeout();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let prepared = workspace_v2_native::controlled(
            ready(PreparedWorkspaceResult::new(envelope.clone())),
            pending(),
            pending(),
        )
        .await
        .unwrap();
        let exit = workspace_v2_native::deliver(
            &command,
            OutputReservation::new(None).unwrap(),
            Ok(prepared),
            Some(deadline),
            || before,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 0);
        assert_eq!(stdout, expected);
        assert!(stderr.is_empty());

        stdout.clear();
        let exit = workspace_v2_native::deliver(
            &command,
            OutputReservation::new(None).unwrap(),
            Ok(PreparedWorkspaceResult::new(envelope.clone()).unwrap()),
            Some(deadline),
            || deadline,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 1);
        assert!(stdout.is_empty());
        let failed: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(failed["error"]["code"], "Timeout");

        stderr.clear();
        let mut polls = 0;
        let exit = workspace_v2_native::deliver(
            &command,
            OutputReservation::new(None).unwrap(),
            Ok(PreparedWorkspaceResult::new(envelope).unwrap()),
            Some(deadline),
            || {
                polls += 1;
                if polls == 1 { before } else { deadline }
            },
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 1);
        assert_eq!(stdout, expected);
        let failed: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"]["code"], "Timeout");
        assert!(
            failed["error"]["hints"][0]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deadline_during_stderr_delivery_never_appends_a_second_envelope() {
        let command = command("get", "2s", &[]);
        let envelope = failure_envelope(
            Some(&command),
            WorkspaceReadError::NotFound,
            Some(OBSERVED_AT),
        );
        let prepared = PreparedWorkspaceResult::new(envelope).unwrap();
        let before = Instant::now();
        let deadline = before + command.timeout();
        let mut polls = 0;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let exit = workspace_v2_native::deliver(
            &command,
            OutputReservation::new(None).unwrap(),
            Ok(prepared),
            Some(deadline),
            || {
                polls += 1;
                if polls < 3 { before } else { deadline }
            },
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 1);
        assert_eq!(polls, 3);
        assert!(stdout.is_empty());
        assert_eq!(stderr.iter().filter(|byte| **byte == b'\n').count(), 1);
        let failed: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(failed["error"]["code"], "NotFound");
    }

    #[tokio::test]
    async fn late_output_failure_retains_the_original_delivery_evidence() {
        struct FailedOutput;
        impl std::io::Write for FailedOutput {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic-output-failure"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let command = command("get", "2s", &[]);
        let (sdk, executor, _, _) = setup(
            r#"{"id":"11111111-1111-1111-1111-111111111111","displayName":"Offline workspace"}"#,
            EndpointBinding::public(ServiceOrigin::Fabric),
            command.timeout(),
        );
        let envelope =
            workspace_v2_native::execute_authenticated(&command, sdk, executor, OBSERVED_AT)
                .await
                .unwrap();
        let before = Instant::now();
        let deadline = before + command.timeout();
        let mut polls = 0;
        let mut stderr = Vec::new();
        let exit = workspace_v2_native::deliver(
            &command,
            OutputReservation::new(None).unwrap(),
            Ok(PreparedWorkspaceResult::new(envelope).unwrap()),
            Some(deadline),
            || {
                polls += 1;
                if polls == 1 { before } else { deadline }
            },
            &mut FailedOutput,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 1);
        let failed: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(failed["error"]["code"], "OutputFailed");
        assert_eq!(failed["meta"]["delivery"]["published"], false);
        assert_eq!(failed["meta"]["delivery"]["cleanupComplete"], true);
        assert!(
            failed["error"]["hints"][0]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );
        assert!(
            !String::from_utf8(stderr)
                .unwrap()
                .contains("synthetic-output-failure")
        );
    }

    #[test]
    fn native_failure_mapping_never_exposes_provider_text() {
        for (code, expected) in [
            ("interaction_required", WorkspaceReadError::Unauthenticated),
            ("invalid_configuration", WorkspaceReadError::InvalidInput),
            ("NotConfigured", WorkspaceReadError::NotConfigured),
            ("unsupported", WorkspaceReadError::UnsupportedCapability),
            ("timed_out", WorkspaceReadError::Timeout),
            ("cancelled", WorkspaceReadError::Cancelled),
            ("transport_failed", WorkspaceReadError::OperationFailed),
        ] {
            let actual =
                workspace_v2_native::native_error((code, "private-provider-data".into(), 1));
            assert_eq!(actual, expected);
            assert!(!actual.to_string().contains("private-provider-data"));
        }
    }
}
