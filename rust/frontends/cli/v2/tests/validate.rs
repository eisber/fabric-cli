// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual offline executable and caller-controlled file boundaries.
#![deny(unnameable_test_items)]

use fabric_cli_v2::{
    Control,
    validate::{self, ValidateRequest},
};
use fabric_sdk_sql_definition::DefinitionLimits;
use serde_json::Value;
use std::{
    fs,
    num::NonZeroUsize,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const BODY: &[u8] = br#"{"format":"dacpac","parts":[{"path":"a.dacpac","payload":"eA==","payloadType":"InlineBase64"}]}"#;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let root = std::env::var_os("FAB_CLI_V2_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = root.join(format!(
            "validate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn input(&self, bytes: &[u8]) -> PathBuf {
        let path = self.0.join("input.json");
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn args(path: &str) -> Vec<&str> {
    vec![
        "validate",
        "--type",
        "SQLDatabase",
        "--native-format",
        "dacpac",
        "--rule-version",
        "1.0",
        "--input",
        path,
        "--max-bytes",
        "4096",
        "--max-decoded-bytes",
        "4096",
        "--max-part-bytes",
        "4096",
        "--max-parts",
        "16",
        "--max-path-bytes",
        "1024",
        "--max-depth",
        "32",
    ]
}

fn binary(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli"))
        .env_clear()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn payload(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn schema_is_neutral_local_and_bypasses_invalid_configuration_without_input() {
    let output = binary(&[
        "--config-root",
        "not-an-absolute-root",
        "validate",
        "--type",
        "SQLDatabase",
        "--native-format",
        "dacpac",
        "--rule-version",
        "1.0",
        "--schema",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value = payload(&output.stdout);
    assert_eq!(value["data"]["operation"]["verb"], "validate");
    assert_eq!(
        value["data"]["operation"]["effects"],
        serde_json::json!(["localRead"])
    );
    assert_eq!(
        value["data"]["operation"]["inputSchema"]["properties"]["format"]["const"],
        "dacpac"
    );
    assert_eq!(value["data"]["authorization"], "not_evaluated");
    assert_eq!(value["effects"], serde_json::json!([]));
}

#[test]
fn valid_and_invalid_documents_keep_structural_reports_with_distinct_exits() {
    let files = Files::new();
    let path = files.input(BODY);
    let output = binary(&args(path.to_str().unwrap()));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(payload(&output.stdout)["data"]["valid"], true);
    assert_eq!(fs::read(&path).unwrap(), BODY);
    fs::write(&path, b"{\"parts\":[]}").unwrap();
    let output = binary(&args(path.to_str().unwrap()));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let report = payload(&output.stdout);
    assert_eq!(report["data"]["valid"], false);
    assert_eq!(
        report["data"]["findings"][0]["ruleId"],
        "missing_primary_part"
    );
    assert_eq!(report["data"]["coverage"]["complete"], false);
}

#[test]
fn unreadable_unsafe_and_over_budget_inputs_are_errors_without_verdicts() {
    let files = Files::new();
    let path = files.input(&vec![b' '; 4097]);
    for (input, expected) in [
        (path.to_str().unwrap(), "BudgetExceeded"),
        (files.0.to_str().unwrap(), "UnsafePath"),
        ("relative.json", "UnsafePath"),
    ] {
        let output = binary(&args(input));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let value = payload(&output.stderr);
        assert_eq!(value["error"]["code"], expected);
        assert!(value["data"].is_null());
    }
    let output = binary(&args(files.0.join("missing.json").to_str().unwrap()));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(payload(&output.stderr)["error"]["code"], "InputReadFailed");
}

#[test]
fn malformed_duplicate_json_unknown_format_and_traversal_never_succeed() {
    let files = Files::new();
    for bytes in [
        &b"not-json"[..],
        b"{\"parts\":[],\"p\\u0061rts\":[]}",
        b"{\"format\":\"future\",\"parts\":[]}",
        b"{\"parts\":[{\"path\":\"../secret.dacpac\",\"payload\":\"eA==\",\"payloadType\":\"InlineBase64\"}]}",
    ] {
        let path = files.input(bytes);
        let output = binary(&args(path.to_str().unwrap()));
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret.dacpac"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret.dacpac"));
    }
}

#[test]
fn unsupported_options_and_missing_or_zero_budgets_fail_before_io() {
    let mut zero = args("missing.json");
    let index = zero
        .iter()
        .position(|value| *value == "--max-depth")
        .unwrap()
        + 1;
    zero[index] = "0";
    for arguments in [
        zero,
        vec![
            "validate",
            "--type",
            "Notebook",
            "--native-format",
            "dacpac",
            "--rule-version",
            "1.0",
            "--schema",
        ],
        vec![
            "validate",
            "--type",
            "SQLDatabase",
            "--native-format",
            "dacpac",
            "--rule-version",
            "future",
            "--schema",
        ],
        vec![
            "validate",
            "--type",
            "SQLDatabase",
            "--native-format",
            "dacpac",
            "--rule-version",
            "1.0",
            "--input",
            "missing.json",
        ],
        vec![
            "validate",
            "--type",
            "SQLDatabase",
            "--native-format",
            "dacpac",
            "--rule-version",
            "1.0",
            "--schema",
            "--input",
            "missing.json",
        ],
        vec!["validate", "--password", "do-not-echo"],
    ] {
        let output = binary(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("do-not-echo"));
    }
}

struct Cancelled;
impl Control for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn deadline_elapsed(&self) -> bool {
        false
    }
}
struct TimedOut;
impl Control for TimedOut {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn deadline_elapsed(&self) -> bool {
        true
    }
}
struct Proceed;
impl Control for Proceed {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn deadline_elapsed(&self) -> bool {
        false
    }
}

fn request(path: PathBuf) -> ValidateRequest {
    let nz = |value| NonZeroUsize::new(value).unwrap();
    ValidateRequest {
        item_type: "SQLDatabase".into(),
        native_format: "dacpac".into(),
        rule_version: "1.0".into(),
        media_type: "application/json".into(),
        input: Some(path),
        schema_only: false,
        limits: Some(DefinitionLimits {
            max_wire_bytes: nz(4096),
            max_decoded_bytes: nz(4096),
            max_part_bytes: nz(4096),
            max_parts: nz(16),
            max_path_bytes: nz(1024),
            max_depth: nz(32),
        }),
    }
}

#[test]
fn cancellation_deadline_and_unsupported_schema_produce_no_document_verdict() {
    let request = request(PathBuf::from("unread-input.json"));
    assert_eq!(
        validate::run(&request, &Cancelled).unwrap_err().code,
        "Cancelled"
    );
    assert_eq!(
        validate::run(&request, &TimedOut).unwrap_err().code,
        "Timeout"
    );
    let mut unknown = request;
    unknown.rule_version = "future".into();
    assert_eq!(
        validate::run(&unknown, &Proceed).unwrap_err().code,
        "Unsupported"
    );
}

#[cfg(unix)]
#[test]
fn symlink_input_and_symlink_parent_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let files = Files::new();
    let path = files.input(BODY);
    let linked = files.0.join("link.json");
    symlink(&path, &linked).unwrap();
    assert_eq!(
        validate::run(&request(linked), &Proceed).unwrap_err().code,
        "UnsafePath"
    );
    let directory = files.0.join("linked-parent");
    symlink(&files.0, &directory).unwrap();
    assert_eq!(
        validate::run(&request(directory.join("input.json")), &Proceed)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
}

#[cfg(windows)]
#[test]
fn network_and_device_paths_are_rejected_before_acquisition() {
    for path in [
        r"\\host\share\input.json",
        r"\\?\C:\input.json",
        r"\\.\pipe\input.json",
    ] {
        assert_eq!(
            validate::run(&request(PathBuf::from(path)), &Proceed)
                .unwrap_err()
                .code,
            "UnsafePath"
        );
    }
}

#[cfg(windows)]
#[test]
fn junction_parent_is_rejected_before_reading_target_bytes() {
    let files = Files::new();
    let original = files.input(BODY);
    let link = files.0.join("junction");
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&files.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated junction fixture must be created"
    );
    let error = validate::run(&request(link.join("input.json")), &Proceed).unwrap_err();
    fs::remove_dir(&link).unwrap();
    assert_eq!(error.code, "UnsafePath");
    assert_eq!(fs::read(original).unwrap(), BODY);
}
