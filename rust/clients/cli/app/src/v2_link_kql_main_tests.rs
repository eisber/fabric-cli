// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native file/resolver/approval adapters, without ambient credentials or effects.
#![deny(unnameable_test_items)]

use super::super::v2_test_support as support;
use super::*;
use fabric_sdk::{Audience, CredentialSelection, RequestContext};
use serde_json::{Value, json};
use std::sync::Arc;
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const DB: &str = "22222222-2222-4222-8222-222222222222";
const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.KQLDatabase/Tables/sales_ref.Shortcut";

fn input() -> Value {
    json!({"name":"sales_ref","enableQueryAcceleration":false,"target":{"oneLake":{"workspaceId":WS,"itemId":DB,"path":"Tables/sales"}}})
}

fn client() -> Client {
    Client::with_context(RequestContext::new(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    ))
}

fn executor(http: Arc<ScriptedHttp>) -> HttpExecutor {
    HttpExecutor::new(
        http,
        Arc::new(TestTimer::default()),
        fabric_sdk_http::ExecutionOptions::default(),
    )
    .unwrap()
}

#[test]
fn target_specific_dispatch_precedes_update_without_annexing_other_facades() {
    for args in [
        vec!["link", TARGET, "--input", "-"],
        vec!["artifact", "kql-database", "link", TARGET, "--input", "-"],
        vec!["artifact", "kql-database", "--help"],
    ] {
        assert!(handles(
            &args.into_iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
    for args in [
        vec!["artifact", "notebook", "update"],
        vec!["artifact", "lakehouse", "link"],
        vec!["artifact", "data-agent", "link"],
        vec!["update"],
    ] {
        assert!(!handles(
            &args.into_iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
}

#[tokio::test]
async fn actual_native_file_and_resolver_adapters_prepare_the_captured_native_operation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    serde_json::to_writer(&file, &input()).unwrap();
    let args = [
        "artifact",
        "kql-database",
        "link",
        TARGET,
        "--input",
        file.path().to_str().unwrap(),
    ];
    let command = host::parse(args.into_iter().map(OsString::from)).unwrap();
    let http = ScriptedHttp::new([]);
    let client = client();
    let executor = executor(http.clone());
    let mut resolver = Resolver {
        client: &client,
        executor: &executor,
    };
    let prepared = host::prepare(&command, &mut resolver, &mut Files)
        .await
        .unwrap();
    assert_eq!(prepared.operation().body(), Some(&input()));
    assert_eq!(
        prepared.operation().path(),
        format!("/v1/workspaces/{WS}/kqlDatabases/{DB}/shortcuts")
    );
    assert_eq!(prepared.target().workspace(), WS);
    assert_eq!(prepared.target().item(), DB);
    assert!(!prepared.effects().query_acceleration);
    assert!(
        http.requests.lock().unwrap().is_empty(),
        "GUID owners need no discovery"
    );
    let mut approval = CapturedApproval(prepared.target().clone(), prepared.effects());
    let mut operations = OperationSession::with_context(
        client.context(),
        executor.clone(),
        OperationLimits::default(),
        std::future::pending(),
    );
    // Missing approval sends no request even with fully configured native adapters.
    let delivery = host::Delivery::execute(prepared.clone(), &mut operations, None).await;
    assert!(delivery.result().is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    let different = KqlDestination::parse(&TARGET.replace("sales_ref", "other"))
        .unwrap()
        .resolved(WS, DB)
        .unwrap();
    assert!(
        approval
            .approve(&different, prepared.effects())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_resolver_names_are_bounded_typed_unique_and_never_permission_fallback() {
    for (status, items, success) in [
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"KQLDatabase"}]),
            true,
        ),
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"Lakehouse"}]),
            false,
        ),
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"KQLDatabase"},{"id":WS,"displayName":"db","type":"KQLDatabase"}]),
            false,
        ),
        (403, json!([]), false),
    ] {
        let http = ScriptedHttp::new([response(status, &json!({"value":items}).to_string(), &[])]);
        let client = client();
        let executor = executor(http.clone());
        let mut resolver = Resolver {
            client: &client,
            executor: &executor,
        };
        let path = format!("/{WS}.Workspace/db.KQLDatabase/Tables/sales_ref.Shortcut");
        assert_eq!(resolver.resolve(&path).await.is_ok(), success);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(
            http.requests.lock().unwrap()[0].method(),
            azure_core::http::Method::Get
        );
    }
}

#[tokio::test]
async fn native_file_bounds_and_missing_inputs_fail_explicitly() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"12345").unwrap();
    assert!(
        Files
            .read(file.path().to_str().unwrap(), NonZeroUsize::new(4).unwrap())
            .await
            .is_err()
    );
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.json");
    assert!(
        Files
            .read(missing.to_str().unwrap(), NonZeroUsize::new(4).unwrap())
            .await
            .is_err()
    );
}

#[test]
fn typed_root_dispatch_is_disjoint_for_valid_malformed_and_help_permutations() {
    let dataagent = "/w.Workspace/agent.DataAgent/settings/staging/datasources";
    for (target, kql) in [
        (TARGET, true),
        ("/w.Workspace/db.KQLDatabase/Files/not-a-shortcut", true),
        ("/w.Workspace/db.KQLDatabase", true),
        (dataagent, false),
        (
            "/w.Workspace/agent.DataAgent/Tables/invalid.Shortcut",
            false,
        ),
    ] {
        for args in [
            vec!["link", target, "--input", "-"],
            vec!["link", "--input", "-", target],
            vec!["link", "--input=-", "--wait", target],
            vec!["link", "--help", target],
            vec!["link", "-h", "--input", "-", target],
            vec!["link", target, "--help"],
            vec!["link", target, "--input"],
            vec!["link", "--unknown", target],
            vec!["link", "--input", "-", "--", target],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert_eq!(handles(&args), kql, "{args:?}");
            assert_eq!(
                fabric_cli::v2_link_dataagent_host::handles(&args),
                !kql,
                "{args:?}"
            );
        }
    }
}

#[test]
fn input_values_and_generic_help_never_annex_another_link_owner() {
    let dataagent = "/w.Workspace/agent.DataAgent/settings/staging/datasources";
    for args in [
        vec!["link", "--input", TARGET, dataagent],
        vec!["link", "--input", TARGET, "--help"],
        vec!["link", "--input", TARGET],
        vec!["link", "--help"],
        vec!["link", "-h"],
        vec!["artifact", "data-agent", "link", "--help"],
    ] {
        let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
        assert!(!handles(&args), "{args:?}");
    }
    for flag in ["--help", "-h"] {
        let args = ["link", flag].map(OsString::from);
        assert!(fabric_cli::v2_link_dataagent_host::handles(&args));
    }
    let args = ["link", "--input", dataagent, TARGET].map(OsString::from);
    assert!(handles(&args));
    assert!(!fabric_cli::v2_link_dataagent_host::handles(&args));
}

async fn prepared_for_presentation() -> PreparedLink {
    let file = tempfile::NamedTempFile::new().unwrap();
    serde_json::to_writer(&file, &input()).unwrap();
    let command = host::parse(
        ["link", TARGET, "--input", file.path().to_str().unwrap()]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let client = client();
    let executor = executor(ScriptedHttp::new([]));
    host::prepare(
        &command,
        &mut Resolver {
            client: &client,
            executor: &executor,
        },
        &mut Files,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn approval_discloses_exact_request_and_immutable_identity_context() {
    let prepared = prepared_for_presentation().await;
    let context = RequestContext::with_endpoint(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        fabric_sdk::EndpointBinding::with_host(
            fabric_sdk::ServiceOrigin::Fabric,
            "dailyapi.fabric.microsoft.com",
        )
        .unwrap(),
    );
    let question = approval_question(&prepared, &context);
    let actual_context: Value = serde_json::from_str(
        question
            .lines()
            .find_map(|line| line.strip_prefix("Context: "))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        actual_context,
        json!({
            "tenant":"tenant", "account":"offline",
            "audience":"https://api.fabric.microsoft.com/.default",
            "endpoint":"dailyapi.fabric.microsoft.com",
        })
    );
    for expected in [
        WS,
        DB,
        "sales_ref",
        "offline",
        "tenant",
        "dailyapi.fabric.microsoft.com",
        "https://api.fabric.microsoft.com/.default",
        prepared.operation().path(),
        &input().to_string(),
        "No query acceleration",
    ] {
        assert!(question.contains(expected), "{expected}");
    }
}

#[tokio::test]
async fn original_output_deadline_retains_accepted_handle_without_replay() {
    let http = ScriptedHttp::new([response(
        202,
        "",
        &[(
            "location",
            "https://api.fabric.microsoft.com/v1/operations/55555555-5555-4555-8555-555555555555",
        )],
    )]);
    let client = client();
    let mut operations = OperationSession::with_context(
        client.context(),
        executor(http.clone()),
        OperationLimits::default(),
        std::future::pending(),
    );
    let prepared = prepared_for_presentation().await;
    let mut approval = CapturedApproval(prepared.target().clone(), prepared.effects());
    let delivery = host::Delivery::execute(prepared, &mut operations, Some(&mut approval)).await;
    let expected = delivery.metadata();
    let error = deliver(
        delivery,
        Instant::now() - Duration::from_secs(1),
        io::sink(),
        io::sink(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "TimedOut");
    assert_eq!(error.2, expected);
    assert_eq!(
        error.2["operationId"],
        "55555555-5555-4555-8555-555555555555"
    );
    assert_eq!(error.2["remoteReplayed"], false);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn interruption_classification_does_not_hide_timeout_or_signal_setup_failure() {
    for (state, expected) in [(1, "TimedOut"), (2, "Cancelled"), (3, "SignalFailed")] {
        assert_eq!(interruption_code(&AtomicU8::new(state)), expected);
    }

    #[tokio::test]
    async fn stalled_stderr_delivery_retains_accepted_handle_with_bounded_return() {
        struct Stalled;
        impl Write for Stalled {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(200));
                Err(io::Error::other("synthetic blocked pipe"))
            }

            #[test]
            fn owned_stderr_worker_cannot_prevent_supervised_process_exit_after_deadline() {
                const CHILD: &str = "FAB_KQL_STALLED_STDERR_CHILD";
                if std::env::var_os(CHILD).is_some() {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .max_blocking_threads(1)
                        .build()
                        .unwrap();
                    runtime.block_on(async {
                        let writer =
                            owned_output::owned_stream(OutputStream::Stderr, true).unwrap();
                        let result = owned_output::write_owned(
                            writer,
                            Instant::now() + Duration::from_millis(100),
                            std::future::pending::<()>(),
                            |writer| -> io::Result<()> {
                                let block = [b'x'; 65_536];
                                loop {
                                    writer.write_all(&block)?;
                                }
                            },
                        )
                        .await;
                        assert!(matches!(result, owned_output::OwnedWrite::Deadline));
                    });
                    runtime.shutdown_timeout(Duration::from_millis(20));
                    return;
                }
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "v2_link_kql_main::tests::owned_stderr_worker_cannot_prevent_supervised_process_exit_after_deadline",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        assert!(status.success());
                        break;
                    }
                    if Instant::now() >= deadline {
                        child.kill().unwrap();
                        let _ = child.wait();
                        panic!("owned stalled stderr prevented bounded child exit");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("synthetic output failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let http = ScriptedHttp::new([response(
            202,
            "",
            &[(
                "location",
                "https://api.fabric.microsoft.com/v1/operations/55555555-5555-4555-8555-555555555555",
            )],
        )]);
        let client = client();
        let mut operations = OperationSession::with_context(
            client.context(),
            executor(http.clone()),
            OperationLimits::default(),
            std::future::pending(),
        );
        let prepared = prepared_for_presentation().await;
        let mut approval = CapturedApproval(prepared.target().clone(), prepared.effects());
        let delivery =
            host::Delivery::execute(prepared, &mut operations, Some(&mut approval)).await;
        let started = Instant::now();
        let error = deliver(
            delivery,
            started + Duration::from_millis(20),
            Broken,
            Stalled,
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, "TimedOut");
        assert_eq!(
            error.2["operationId"],
            "55555555-5555-4555-8555-555555555555"
        );
        assert_eq!(error.2["remoteReplayed"], false);
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
}
