// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Saved broker identity selection is exercised without contacting WAM or services.

#![cfg(all(windows, feature = "native-read"))]
#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const CLIENT: &str = "5814bfb4-2705-4994-b8d6-39aabeb5eaeb";
const ACCOUNT: &str = "selected@example.test";

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let parent = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = parent.join(format!(
            "broker-selection-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn binding(&self) -> PathBuf {
        self.0
            .join("Microsoft")
            .join("FabricCLI")
            .join("broker-session.json")
    }

    fn save(&self) {
        let path = self.binding();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "schema": 1, "generation": 1, "tenant": TENANT,
                "account": ACCOUNT, "client": CLIENT
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn process(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear().env("LOCALAPPDATA", root);
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

fn error(output: &Output, code: &str) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code, "{value}");
    value
}

#[test]
fn saved_login_selects_the_remote_host_without_any_auth_environment_selectors() {
    let root = Root::new();
    root.save();
    let original = fs::read(root.binding()).unwrap();
    let result = process(&root.0)
        .env("FAB_RUST_CONTEXT", "relative-context-is-invalid")
        .args(["ls", "/"])
        .output()
        .unwrap();
    // Invalid context fails after credential selection but before WAM connection.
    let value = error(&result, "invalid_configuration");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("working context")
    );
    assert_eq!(fs::read(root.binding()).unwrap(), original);
}

#[test]
fn full_environment_selection_overrides_even_a_broken_saved_binding_without_reading_it() {
    let root = Root::new();
    root.save();
    fs::write(root.binding(), b"broken-state").unwrap();
    let result = process(&root.0)
        .env("FAB_RUST_TENANT", TENANT)
        .env("FAB_RUST_ACCOUNT", ACCOUNT)
        .env("FAB_RUST_CLIENT_ID", CLIENT)
        .env("FAB_RUST_CONTEXT", "relative-context-is-invalid")
        .args(["ls", "/"])
        .output()
        .unwrap();
    let value = error(&result, "invalid_configuration");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("working context")
    );
    assert!(!value.to_string().contains("saved Windows broker selection"));
    assert_eq!(fs::read(root.binding()).unwrap(), b"broken-state");
}

#[test]
fn conflicting_partial_override_cannot_mix_a_new_tenant_with_the_saved_account() {
    let root = Root::new();
    root.save();
    let result = process(&root.0)
        .env("FAB_RUST_TENANT", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        .env("FAB_RUST_CONTEXT", "relative-context-is-invalid")
        .args(["ls", "/"])
        .output()
        .unwrap();
    error(&result, "NotConfigured");
}

#[test]
fn bare_status_logout_and_silent_login_never_open_a_picker_when_no_binding_exists() {
    let root = Root::new();
    for args in [
        vec!["auth", "status"],
        vec!["auth", "logout"],
        vec!["auth", "login", "--no-interactive"],
    ] {
        let result = process(&root.0)
            .args(&args)
            .args(["--output_format", "json"])
            .output()
            .unwrap();
        error(
            &result,
            if args.contains(&"login") {
                "interaction_required"
            } else {
                "NotConfigured"
            },
        );
        assert!(!root.binding().exists());
    }
}

#[test]
fn auth_presentation_obeys_default_saved_and_explicit_formats_without_accessing_wam() {
    let root = Root::new();
    let configuration = root.binding().parent().unwrap().join("config.json");
    let invoke = |format: Option<&str>| {
        let mut command = process(&root.0);
        command.args(["auth", "status"]);
        if let Some(format) = format {
            command.args(["--output_format", format]);
        }
        command.output().unwrap()
    };
    let text = invoke(None);
    assert_eq!(text.status.code(), Some(1));
    assert!(text.stdout.is_empty());
    assert!(
        String::from_utf8(text.stderr)
            .unwrap()
            .contains("[NotConfigured]")
    );
    assert!(!configuration.exists());
    fs::create_dir_all(configuration.parent().unwrap()).unwrap();
    fs::write(&configuration, br#"{"output_format":"json"}"#).unwrap();
    error(&invoke(None), "NotConfigured");
    let text = invoke(Some("text"));
    assert!(
        String::from_utf8(text.stderr)
            .unwrap()
            .contains("[NotConfigured]")
    );
    fs::write(&configuration, br#"{"output_format":"text"}"#).unwrap();
    error(&invoke(Some("json")), "NotConfigured");
    fs::write(&configuration, br#"{"fab_output_format":"json"}"#).unwrap();
    error(&invoke(None), "NotConfigured");
    fs::write(&configuration, br#"{"output_format":false}"#).unwrap();
    let invalid = invoke(None);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("[invalid_configuration]")
    );
    error(&invoke(Some("json")), "NotConfigured");
    assert!(!root.binding().exists());
}

#[test]
fn help_is_credential_and_state_free_and_invalid_state_is_not_silently_discarded() {
    let root = Root::new();
    root.save();
    fs::write(root.binding(), b"corrupt-with-private-canary").unwrap();
    for args in [
        vec!["auth", "login", "--help"],
        vec!["--help"],
        vec!["--version"],
    ] {
        let result = process(&root.0).args(args).output().unwrap();
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
    }
    for args in [vec!["auth", "status"], vec!["ls", "/"]] {
        let result = process(&root.0)
            .args(args)
            .args(["--output_format", "json"])
            .output()
            .unwrap();
        let value = error(&result, "invalid_configuration");
        assert!(!value.to_string().contains("private-canary"));
    }
    assert_eq!(
        fs::read(root.binding()).unwrap(),
        b"corrupt-with-private-canary"
    );
}
