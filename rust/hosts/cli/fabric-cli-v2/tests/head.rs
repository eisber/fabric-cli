// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual additive-head contracts; no broker, browser, network or installer runs.
#![deny(unnameable_test_items)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use fabric_cli_v2::parser::{self, Operation, Parsed};
use serde_json::Value;

const WORKSPACE: &str = "/11111111-2222-4333-8444-555555555555.Workspace";
static NEXT: AtomicU64 = AtomicU64::new(0);

fn binary(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli"))
        .env_clear()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the built local head must execute")
}

fn fixture() -> PathBuf {
    let root = std::env::var_os("FAB_CLI_V2_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|path| path.join("rust-toolchain.toml").is_file())
                .expect("workspace root")
                .join("target")
                .join("v2-cli-test-files")
        });
    assert!(root.is_absolute());
    let path = root.join(format!(
        "head-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn json(output: &Output) -> Value {
    let stream = if output.status.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected one structured result: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn selected_group_has_exactly_six_leaves() {
    let command = parser::command();
    let cli = command
        .get_subcommands()
        .find(|command| command.get_name() == "cli")
        .unwrap();
    let names = cli
        .get_subcommands()
        .map(|command| command.get_name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["configure", "login", "logout", "report", "show", "upgrade"]
    );
}

#[test]
fn option_only_invocations_return_a_sanitized_error_instead_of_panicking() {
    for args in [
        vec!["--format", "text"],
        vec!["--config-root", "relative"],
        vec!["cli", "--format", "text"],
        vec!["--format", "json", "cli", "--config-root", "relative"],
    ] {
        let output = binary(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(json(&output)["error"]["code"], "InvalidInput");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}

#[test]
fn removed_leaves_group_aliases_and_v1_roots_are_not_executable() {
    for args in [
        vec!["cli", "rebind", "a-local-directory"],
        vec!["cli", "open", WORKSPACE],
        vec!["cli", "version"],
        vec!["cli", "completion"],
        vec!["cli", "serve"],
        vec!["client", "show", WORKSPACE],
        vec!["local", "show", WORKSPACE],
        vec!["tool", "show", WORKSPACE],
        vec!["auth", "status"],
        vec!["open", WORKSPACE],
        vec!["update", "--check"],
    ] {
        let output = binary(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(json(&output)["error"]["code"], "InvalidInput");
    }
}

#[test]
fn help_and_global_version_bypass_configuration_and_native_hosts() {
    for args in [
        vec!["--help"],
        vec!["cli", "--help"],
        vec!["cli", "login", "--help"],
        vec!["--config-root", "not-an-absolute-root", "--version"],
    ] {
        let output = binary(&args);
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }
    let output = binary(&["--version"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("fab-cli {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn parser_rejects_credential_flags_without_echoing_supplied_values() {
    let secret = "do-not-echo-this-value";
    let output = binary(&["cli", "login", "/.auth", "--password", secret]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
}

#[test]
fn typed_auth_and_report_targets_are_not_generic_artifact_paths() {
    for args in [
        vec![
            "cli",
            "login",
            WORKSPACE,
            "--tenant",
            "11111111-2222-4333-8444-555555555555",
            "--account",
            "sample@example.test",
        ],
        vec![
            "cli",
            "report",
            WORKSPACE,
            "--destination",
            "ideas",
            "--title",
            "title",
            "--message",
            "message",
        ],
    ] {
        let output = binary(&args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(json(&output)["effects"], serde_json::json!([]));
    }
}

#[test]
fn show_uri_handoff_needs_no_environment_config_identity_or_launcher() {
    let output = binary(&["cli", "show", WORKSPACE, "--uri-only"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result = json(&output);
    assert_eq!(result["schemaVersion"], "0.1");
    assert_eq!(result["command"], "cli show");
    assert_eq!(result["status"], "handoff");
    assert_eq!(result["effects"], serde_json::json!([]));
    assert_eq!(result["error"], Value::Null);
    assert!(
        output
            .stdout
            .windows(b"https://app.fabric.microsoft.com/groups/".len())
            .any(|window| window == b"https://app.fabric.microsoft.com/groups/")
    );
}

#[test]
fn report_preparation_and_manual_handoff_never_claim_delivery() {
    for (flag, expected) in [(None, "prepared"), (Some("--handoff"), "handoff")] {
        let mut args = vec![
            "cli",
            "report",
            "/.support/feedback",
            "--destination",
            "github:microsoft/fabric-cli",
            "--title",
            "An explicit draft",
            "--message",
            "Caller-supplied content only",
        ];
        if let Some(flag) = flag {
            args.push(flag);
        }
        let output = binary(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(json(&output)["status"], expected);
        assert_eq!(json(&output)["effects"], serde_json::json!([]));
    }
    let output = binary(&[
        "cli",
        "report",
        "/.support/feedback",
        "--destination",
        "ideas",
        "--title",
        "draft",
        "--message",
        "message",
        "--send",
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn explicit_preference_root_is_required_when_no_platform_root_exists() {
    let output = binary(&[
        "cli",
        "configure",
        "/.config/output_format",
        "--value",
        "json",
    ]);
    assert!(!output.status.success());
    assert_eq!(json(&output)["error"]["code"], "MissingConfiguration");
}

#[test]
fn configure_executes_persistence_through_the_actual_head() {
    let root = fixture();
    let root_text = root.to_str().unwrap();
    let output = binary(&[
        "--config-root",
        root_text,
        "cli",
        "configure",
        "/.config/output_format",
        "--value",
        "json",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(json(&output)["command"], "cli configure");
    assert_eq!(json(&output)["status"], "succeeded");
    assert!(
        fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .any(|path| path.extension().is_some_and(|ext| ext == "json"))
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn invalid_preferences_do_not_create_or_migrate_files() {
    let root = fixture();
    let output = binary(&[
        "--config-root",
        root.to_str().unwrap(),
        "cli",
        "configure",
        "/.config/encryption_fallback_enabled",
        "--value",
        "true",
    ]);
    assert!(!output.status.success());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn saved_presentation_preferences_are_used_and_explicit_options_win_without_writes() {
    let root = fixture();
    let root_text = root.to_str().unwrap();
    for (key, value) in [
        ("/.config/output_format", "text"),
        ("/.config/default_open_experience", "powerbi"),
    ] {
        let output = binary(&[
            "--config-root",
            root_text,
            "cli",
            "configure",
            key,
            "--value",
            value,
        ]);
        assert!(output.status.success());
    }
    let store = root.join("preferences-v2.json");
    let before = fs::read(&store).unwrap();
    let report = binary(&[
        "--config-root",
        root_text,
        "cli",
        "report",
        "/.support/feedback",
        "--destination",
        "ideas",
        "--title",
        "draft",
        "--message",
        "message",
    ]);
    assert!(report.status.success());
    assert!(String::from_utf8_lossy(&report.stdout).contains("prepared"));
    assert!(serde_json::from_slice::<Value>(&report.stdout).is_err());
    let saved = binary(&[
        "--config-root",
        root_text,
        "--format",
        "json",
        "cli",
        "show",
        WORKSPACE,
        "--uri-only",
    ]);
    assert!(saved.status.success());
    assert_eq!(json(&saved)["status"], "handoff");
    assert!(String::from_utf8_lossy(&saved.stdout).contains("experience=power-bi"));
    let explicit = binary(&[
        "--config-root",
        root_text,
        "--format",
        "json",
        "cli",
        "show",
        WORKSPACE,
        "--uri-only",
        "--experience",
        "fabric",
    ]);
    assert!(explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stdout).contains("experience=fabric-developer"));
    assert_eq!(fs::read(&store).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn optional_preference_reads_never_create_a_missing_root() {
    let root = fixture();
    let missing = root.join("missing");
    let output = binary(&[
        "--config-root",
        missing.to_str().unwrap(),
        "cli",
        "report",
        "/.support/feedback",
        "--destination",
        "ideas",
        "--title",
        "draft",
        "--message",
        "message",
    ]);
    assert!(output.status.success());
    assert!(!missing.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_preferences_fail_before_effects_but_help_and_explicit_overrides_bypass_reads() {
    let root = fixture();
    let root_text = root.to_str().unwrap();
    fs::write(root.join("preferences-v2.json"), b"invalid JSON").unwrap();
    let output = binary(&[
        "--config-root",
        root_text,
        "cli",
        "show",
        WORKSPACE,
        "--uri-only",
    ]);
    assert!(!output.status.success());
    assert_eq!(json(&output)["effects"], serde_json::json!([]));
    let help = binary(&["--config-root", root_text, "cli", "show", "--help"]);
    assert!(help.status.success());
    let explicit = binary(&[
        "--config-root",
        root_text,
        "--format",
        "json",
        "cli",
        "show",
        WORKSPACE,
        "--uri-only",
        "--experience",
        "fabric",
    ]);
    assert!(explicit.status.success());
    assert_eq!(json(&explicit)["status"], "handoff");
    assert_eq!(
        fs::read(root.join("preferences-v2.json")).unwrap(),
        b"invalid JSON"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn text_presentation_is_explicit_and_needs_no_saved_setting() {
    let output = binary(&[
        "--format",
        "text",
        "cli",
        "report",
        "/.support/feedback",
        "--destination",
        "ideas",
        "--title",
        "draft",
        "--message",
        "message",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("prepared"));
}

#[test]
fn source_login_flags_are_not_silently_admitted_or_fallback_selected() {
    let parsed = parser::parse_args([
        "fab-cli",
        "cli",
        "login",
        "/.auth",
        "--account",
        "sample@example.test",
        "--tenant",
        "11111111-2222-4333-8444-555555555555",
        "--no-interactive",
    ])
    .unwrap();
    let Parsed::Execute(invocation) = parsed else {
        panic!("expected execution");
    };
    let Operation::Auth { request, .. } = invocation.operation else {
        panic!("expected explicit auth");
    };
    assert!(!request.allow_interaction);
    assert_eq!(request.account, "sample@example.test");
}

#[cfg(not(feature = "windows-broker"))]
#[test]
fn missing_broker_feature_is_an_explicit_failure_without_discovery() {
    let output = binary(&[
        "cli",
        "login",
        "/.auth",
        "--tenant",
        "11111111-2222-4333-8444-555555555555",
        "--account",
        "sample@example.test",
        "--no-interactive",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(json(&output)["error"]["code"], "UnsupportedCapability");
}

#[cfg(not(feature = "self-update"))]
#[test]
fn missing_update_feature_is_not_a_successful_check() {
    let output = binary(&["cli", "upgrade"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(json(&output)["error"]["code"], "UnsupportedCapability");
}
