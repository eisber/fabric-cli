// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![cfg(feature = "workspace-read-v2")]
#![deny(unnameable_test_items)]

mod folder_support;

use fabric_cli::workspace_v2::{
    OutputReservation, ParsedWorkspaceInvocation, WorkspaceCommand, execute, failure_envelope,
    parse_args, write_result,
};
use fabric_sdk::{Audience, CredentialSelection};
use fabric_sdk_api::{ApiClient, workspace_reads::WorkspaceReadError};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    future::{pending, poll_fn, ready},
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::Poll,
};

const W: &str = "11111111-2222-3333-4444-555555555555";
const PATH: &str = "/11111111-2222-3333-4444-555555555555.Workspace";
const NOW: &str = "2026-11-05T20:00:00Z";

fn parsed(args: &[&str]) -> WorkspaceCommand {
    let args = std::iter::once(OsString::from("fab-cli"))
        .chain(args.iter().map(|arg| OsString::from(*arg)));
    let ParsedWorkspaceInvocation::Read(command) = parse_args(args).unwrap() else {
        panic!("a read is required");
    };
    command
}

fn client(replies: impl IntoIterator<Item = Reply>) -> (ApiClient, Arc<Http>, Arc<Credential>) {
    let http = Arc::new(Http::new(replies));
    let credential = Arc::new(Credential::default());
    let executor = Arc::new(executor(http.clone(), Arc::new(Timer::default())));
    let client = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    );
    (client, http, credential)
}

fn item(id: &str) -> Value {
    json!({"id":id,"displayName":id,"type":"Notebook","workspaceId":W})
}

fn value(envelope: &impl serde::Serialize) -> Value {
    serde_json::to_value(envelope).unwrap()
}

#[test]
fn primary_common_and_facade_contracts_are_equal_and_never_enrich() {
    let metadata = json!({"id":W,"displayName":"Viewer workspace","unknown":{"null":null,"n":1.0}});
    let mut envelopes = Vec::new();
    for args in [
        vec!["get", PATH, "--max-bytes", "4096", "--timeout", "30s"],
        vec![
            "artifact",
            "workspace",
            "get",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
    ] {
        let command = parsed(&args);
        let (client, http, credential) = client([reply(metadata.clone())]);
        let result = complete(execute(&client, &command, NOW, pending(), pending()));
        assert_eq!(result.exit_code(), 0);
        let envelope = value(&result);
        assert_eq!(envelope["data"], metadata);
        assert_eq!(envelope["meta"]["coverage"]["complete"], true);
        assert_eq!(envelope["meta"]["context"]["tenant"], "selected-tenant");
        assert_eq!(
            *http.requests.lock().unwrap(),
            [format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{W}"
            )],
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        http.assert_consumed();
        envelopes.push(envelope);
    }
    assert_eq!(envelopes[0], envelopes[1]);
}

#[test]
fn list_aliases_preserve_empty_singleton_and_many_arrays() {
    for rows in [vec![], vec![item("a")], vec![item("a"), item("b")]] {
        let mut envelopes = Vec::new();
        for args in [
            vec![
                "list",
                PATH,
                "--max-bytes",
                "4096",
                "--limit",
                "3",
                "--recursive",
                "false",
                "--timeout",
                "1m",
            ],
            vec![
                "artifact",
                "workspace",
                "list",
                PATH,
                "--max-bytes",
                "4096",
                "--limit",
                "3",
                "--recursive",
                "false",
                "--timeout",
                "1m",
            ],
        ] {
            let command = parsed(&args);
            let (client, http, _) = client([reply(json!({"value": rows}))]);
            let result = complete(execute(&client, &command, NOW, pending(), pending()));
            let envelope = value(&result);
            assert_eq!(result.exit_code(), 0);
            assert_eq!(envelope["data"], json!(rows));
            assert_eq!(
                *http.requests.lock().unwrap(),
                [format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{W}/items?recursive=false",
                )],
            );
            envelopes.push(envelope);
        }
        assert_eq!(envelopes[0], envelopes[1]);
    }
}

#[test]
fn record_truncation_is_explicit_nonresumable_partial_not_a_skipping_cursor() {
    let command = parsed(&[
        "list",
        PATH,
        "--max-bytes",
        "4096",
        "--limit",
        "2",
        "--recursive",
        "true",
        "--timeout",
        "30s",
    ]);
    let (client, http, _) = client([reply(json!({
        "value":[item("a"),item("b"),item("c")],
        "continuationToken":"provider-token-never-emit"
    }))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let envelope = value(&result);
    assert_eq!(result.exit_code(), 1);
    assert_eq!(envelope["status"], "partial");
    assert_eq!(envelope["data"], json!([item("a"), item("b")]));
    assert_eq!(envelope["meta"]["coverage"]["complete"], false);
    assert_eq!(envelope["meta"]["coverage"]["truncated"], true);
    assert_eq!(envelope["meta"]["coverage"]["continuation"], Value::Null);
    assert_eq!(envelope["meta"]["coverage"]["resumable"], false);
    assert!(!envelope.to_string().contains("provider-token-never-emit"));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn projection_changes_only_data_and_preserves_json_scalar_types() {
    let command = parsed(&[
        "list",
        PATH,
        "--max-bytes",
        "4096",
        "--limit",
        "2",
        "--recursive",
        "false",
        "--timeout",
        "30s",
        "--project",
        "length(@)",
    ]);
    let (client, _, _) = client([reply(json!({"value":[item("a")]}))]);
    let envelope = value(&complete(execute(
        &client,
        &command,
        NOW,
        pending(),
        pending(),
    )));
    assert_eq!(envelope["data"], json!(1));
    assert_eq!(envelope["meta"]["target"]["id"], W);
    assert_eq!(envelope["meta"]["coverage"]["complete"], true);
}

#[test]
fn projection_cannot_rescue_over_budget_primary_metadata() {
    let command = parsed(&[
        "get",
        PATH,
        "--max-bytes",
        "2",
        "--timeout",
        "30s",
        "--project",
        "id",
    ]);
    let (client, _, _) = client([reply(json!({"id":W,"displayName":"large"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let envelope = value(&result);
    assert_ne!(result.exit_code(), 0);
    assert_eq!(envelope["data"], Value::Null);
    assert_eq!(envelope["error"]["code"], "BudgetExceeded");
}

#[test]
fn service_denials_are_not_retried_as_admin_or_another_identity() {
    for (status, code) in [
        (401, "Unauthenticated"),
        (403, "AccessDenied"),
        (404, "NotFound"),
    ] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let mut response = reply(json!({"errorCode":"Denied","message":"secret provider text"}));
        response.status = status;
        let (client, http, credential) = client([response]);
        let envelope = value(&complete(execute(
            &client,
            &command,
            NOW,
            pending(),
            pending(),
        )));
        assert_eq!(envelope["error"]["code"], code);
        assert!(!envelope.to_string().contains("secret provider text"));
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[test]
fn ready_cancellation_and_deadline_win_before_credentials() {
    for cancel in [true, false] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let (client, http, credential) = client([]);
        let result = if cancel {
            complete(execute(&client, &command, NOW, ready(()), pending()))
        } else {
            complete(execute(&client, &command, NOW, pending(), ready(())))
        };
        assert_eq!(
            value(&result)["error"]["code"],
            if cancel { "Cancelled" } else { "Timeout" },
        );
        assert!(http.requests.lock().unwrap().is_empty());
        assert!(credential.scopes.lock().unwrap().is_empty());
    }
}

#[test]
fn in_flight_cancellation_and_deadline_drop_read_without_reissue() {
    for cancel in [true, false] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let mut transport = Http::new([]);
        transport.hold_after = Some(1);
        let http = Arc::new(transport);
        let credential = Arc::new(Credential::default());
        let client = ApiClient::new(
            credential.clone(),
            CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric)
                .unwrap(),
            Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
                executor(http.clone(), Arc::new(Timer::default())),
            ))),
        );
        let interrupt = poll_fn(|cx| {
            if http.requests.lock().unwrap().is_empty() {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        let result = if cancel {
            complete(execute(&client, &command, NOW, interrupt, pending()))
        } else {
            complete(execute(&client, &command, NOW, pending(), interrupt))
        };
        assert_eq!(
            value(&result)["error"]["code"],
            if cancel { "Cancelled" } else { "Timeout" },
        );
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[test]
fn oversized_projection_is_an_input_error_before_remote_work() {
    let expression = "a".repeat(4097);
    let error = parse_args(
        [
            "fab-cli",
            "get",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
            "--project",
            &expression,
        ]
        .map(OsString::from),
    )
    .unwrap_err();
    assert_eq!(error, WorkspaceReadError::InvalidInput);
    assert_eq!(failure_envelope(None, error, None).exit_code(), 2);
}

#[test]
fn parser_requires_explicit_budgets_and_rejects_unadvertised_surfaces() {
    let invalid = [
        vec!["get", PATH],
        vec!["get", PATH, "--max-bytes", "0", "--timeout", "30s"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "0s"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "25h"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "1.5s"],
        vec![
            "get",
            PATH,
            "--max-bytes",
            "1",
            "--timeout",
            "30s",
            "--project",
            "[?",
        ],
        vec![
            "get",
            PATH,
            "--max-bytes",
            "1",
            "--timeout",
            "30s",
            "--profile",
            "other",
        ],
        vec![
            "list",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
            "--limit",
            "2",
        ],
        vec!["run", PATH, "--timeout", "30s"],
        vec![
            "get",
            "/name.Workspace",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
        vec![
            "get",
            "/../x.Workspace",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
        vec![
            "get",
            "/x.Lakehouse",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
    ];
    for args in invalid {
        assert!(
            parse_args(
                std::iter::once(OsString::from("fab-cli"))
                    .chain(args.iter().map(|arg| OsString::from(*arg))),
            )
            .is_err(),
            "{args:?}",
        );
    }
    let uppercase = format!("/{}.Workspace", W.to_uppercase());
    let command = parsed(&["get", &uppercase, "--max-bytes", "4096", "--timeout", "24h"]);
    assert_eq!(command.request().workspace_id(), W);
}

#[test]
fn explicit_items_collection_has_the_same_canonical_contract() {
    let path = format!("{PATH}/items");
    let command = parsed(&[
        "list",
        &path,
        "--max-bytes",
        "4096",
        "--limit",
        "3",
        "--recursive",
        "false",
        "--timeout",
        "1ms",
    ]);
    let envelope = failure_envelope(Some(&command), WorkspaceReadError::Cancelled, Some(NOW));
    assert_eq!(value(&envelope)["meta"]["target"]["path"], path);
    assert!(matches!(
        parse_args(["fab-cli", "--help"].map(OsString::from)).unwrap(),
        ParsedWorkspaceInvocation::Display(_),
    ));
    assert!(matches!(
        parse_args(["fab-cli", "--version"].map(OsString::from)).unwrap(),
        ParsedWorkspaceInvocation::Display(_),
    ));
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-workspace-v2-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        // Resolve system temp aliases once; product output rejects symlink parents.
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn no_clobber_preflight_and_atomic_publication_preserve_existing_output() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    fs::write(&path, "original").unwrap();
    assert!(matches!(
        OutputReservation::new(Some(&path)),
        Err(WorkspaceReadError::OutputExists),
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    fs::remove_file(&path).unwrap();
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    assert!(!path.exists());
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &result, &mut stdout, &mut stderr).unwrap(),
        0,
    );
    assert!(stdout.is_empty() && stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        value(&result),
    );
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn output_created_after_preflight_is_never_overwritten_or_retried() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    fs::write(&path, "racing writer").unwrap();
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &result, &mut stdout, &mut stderr).unwrap(),
        1,
    );
    assert!(stdout.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "racing writer");
    assert_eq!(
        serde_json::from_slice::<Value>(&stderr).unwrap()["error"]["code"],
        "OutputExists",
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn failed_read_cleans_reservation_without_publishing_a_success_file() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    let failed = failure_envelope(None, WorkspaceReadError::AccessDenied, None);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &failed, &mut stdout, &mut stderr).unwrap(),
        1,
    );
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected local stream failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_failure_retains_target_without_reissuing_read() {
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let mut stderr = Vec::new();
    assert_eq!(
        write_result(
            OutputReservation::new(None).unwrap(),
            &result,
            &mut BrokenWriter,
            &mut stderr,
        )
        .unwrap(),
        1,
    );
    let failed: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(failed["error"]["code"], "OutputFailed");
    assert_eq!(failed["meta"]["target"]["id"], W);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

fn process(args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.args(args);
    command
}

#[test]
fn executable_help_and_version_do_not_enter_native_or_filesystem_setup() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["artifact", "workspace", "--help"],
    ] {
        let output = process(&args)
            .env("FAB_RUST_CONFIG_ROOT", "must-not-read-this-relative-config")
            .env("FAB_RUST_AUTH_MODE", "must-not-select-this-mode")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("fab-cli"));
        assert!(!text.contains("must-not-"));
    }
}

#[test]
fn executable_invalid_input_is_a_sanitized_v2_stderr_envelope() {
    let output = process(&["get", PATH, "--unadvertised-secret", "must-not-echo"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-echo"));
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["schemaVersion"], "0.1");
    assert_eq!(envelope["status"], "failed");
    assert_eq!(envelope["error"]["code"], "InvalidInput");
}

#[cfg(not(feature = "native-host"))]
#[test]
fn executable_without_host_returns_not_configured_instead_of_success() {
    let output = process(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"]["code"], "NotConfigured");
    assert_eq!(envelope["meta"]["target"]["id"], W);
}

#[test]
fn executable_output_exists_precedes_invalid_native_identity_configuration() {
    let directory = TestDirectory::new();
    let path = directory.0.join("existing.json");
    fs::write(&path, "original").unwrap();
    let output = process(&[
        "get",
        PATH,
        "--max-bytes",
        "4096",
        "--timeout",
        "30s",
        "--output",
        path.to_str().unwrap(),
    ])
    .env("FAB_RUST_AUTH_MODE", "must-not-select-this-mode")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"]["code"], "OutputExists");
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}
