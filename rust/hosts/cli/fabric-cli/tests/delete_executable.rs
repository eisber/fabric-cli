// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable boundaries: no inherited providers, credentials, or service.

#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";
const PRIVATE: &str = "private-delete-acceptance-auth-input";
const ORIGINAL: &[u8] = b"original receipt must never be truncated\r\n";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory {
    root: PathBuf,
    receipt_created: bool,
}

impl Directory {
    fn new() -> Self {
        let root = std::env::current_dir().unwrap().join(format!(
            ".delete-executable-acceptance-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            receipt_created: false,
        }
    }

    fn invoke(&self, args: &[&str], poison_configuration: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
        command
            .env_clear()
            .current_dir(&self.root)
            .env("FAB_RUST_CONFIG_ROOT", self.root.join("uncreated-config"))
            .stdin(Stdio::null())
            .args(args);
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        if poison_configuration {
            command
                .env("FAB_RUST_AUTH_MODE", PRIVATE)
                .env("FAB_API_ENDPOINT_FABRIC", PRIVATE);
        }
        let output = command.output().unwrap();
        assert!(
            !self.root.join("uncreated-config").exists(),
            "help/capability/output/approval gates must precede configuration access"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(PRIVATE));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(PRIVATE));
        output
    }

    fn create_receipt(&mut self) {
        let path = self.root.join("receipt.json");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        self.receipt_created = true;
        std::io::Write::write_all(&mut file, ORIGINAL).unwrap();
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let cleanup = if self.receipt_created {
            fs::remove_file(self.root.join("receipt.json"))
                .and_then(|()| fs::remove_dir(&self.root))
        } else {
            fs::remove_dir(&self.root)
        };
        if !std::thread::panicking() {
            cleanup.unwrap();
        }
    }
}

fn failure(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.ends_with(b"\n"));
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["schemaVersion"], "0.1");
    assert_eq!(envelope["status"], "failed");
    assert!(envelope["data"].is_null());
    assert!(envelope["error"]["code"].is_string());
    envelope
}

fn assert_assessment(envelope: &Value) {
    assert_eq!(
        envelope["meta"]["target"],
        json!({
            "path": TARGET,
            "id": "22222222-2222-2222-2222-222222222222",
            "type": "Notebook",
            "scope": {"workspaceId": "11111111-1111-1111-1111-111111111111"}
        })
    );
    let metadata = &envelope["meta"]["operation"];
    assert_eq!(envelope["meta"]["assessment"]["metadata"], *metadata);
    assert_eq!(
        metadata["target"],
        json!({
            "workspace_id": "11111111-1111-1111-1111-111111111111",
            "item_id": "22222222-2222-2222-2222-222222222222",
            "resource_type": "notebook"
        })
    );
    assert_eq!(
        metadata["descriptor"]["id"],
        "fabric.items.notebook.delete.v2"
    );
    assert_eq!(metadata["deletion_mode"], "permanent");
    assert_eq!(metadata["traversal"], "exact_target");
    assert_eq!(metadata["precondition"], "unconditional");
    assert!(metadata["invocation_id"].as_u64().unwrap() > 0);
    assert_eq!(envelope["meta"]["context"], metadata["context"]);
    assert_eq!(
        envelope["meta"]["effects"]["declared"],
        json!([
            {"id": "notebook-lifecycle", "kind": "notebook_lifecycle"},
            {"id": "notebook-content", "kind": "notebook_content"}
        ])
    );
    assert!(envelope["meta"]["effects"]["observed"].is_null());
    assert!(envelope["meta"]["coverage"]["complete"].is_null());
    assert!(envelope["meta"]["receipt"].is_null());
    assert_eq!(
        envelope["meta"]["hostFailure"],
        json!({"dispatch": "not_dispatched", "mutationMayHaveOccurred": false})
    );
}

#[test]
fn help_and_unsupported_modes_exit_before_any_authentication_configuration() {
    let directory = Directory::new();
    let output = directory.invoke(&["delete", "--help"], true);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "--permanent",
        "--recursive",
        "--if-match",
        "--wait",
        "--timeout",
        "--format",
        "--project",
        "--output",
        "--profile",
    ] {
        assert!(help.contains(flag), "missing registered delete flag {flag}");
    }
    assert!(help.contains("json"));
    assert!(help.contains("Workspace Admin"));
    assert!(help.contains("Default recoverable deletion is unsupported"));
    assert!(!help.contains("--approved"));
    assert!(!help.contains("--force"));
    for options in [
        vec![],
        vec!["--permanent", "--recursive"],
        vec!["--permanent", "--if-match", "private-etag"],
    ] {
        let args = ["delete", TARGET]
            .into_iter()
            .chain(options)
            .collect::<Vec<_>>();
        let output = directory.invoke(&args, true);
        let envelope = failure(&output);
        assert_eq!(envelope["error"]["code"], "UnsupportedCapability");
        assert_eq!(envelope["meta"]["evidence"]["dispatch"], "not_dispatched");
        assert_eq!(
            envelope["meta"]["evidence"]["mutation_may_have_occurred"],
            false
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-etag"));
    }
}

#[test]
fn noninteractive_permanent_delete_keeps_metadata_and_never_selects_a_provider() {
    let directory = Directory::new();
    for poison_configuration in [false, true] {
        let output = directory.invoke(&["delete", TARGET, "--permanent"], poison_configuration);
        let envelope = failure(&output);
        assert_eq!(
            envelope["error"]["code"],
            if cfg!(all(
                feature = "native-host",
                any(windows, feature = "headless")
            )) {
                "ApprovalRequired"
            } else if cfg!(feature = "native-host") {
                "UnsupportedCapability"
            } else {
                "NotConfigured"
            }
        );
        assert_assessment(&envelope);
    }
}

#[test]
fn existing_output_fails_before_authentication_and_preserves_exact_original_bytes() {
    let mut directory = Directory::new();
    directory.create_receipt();
    let output = directory.invoke(
        &["delete", TARGET, "--permanent", "-o", "receipt.json"],
        true,
    );
    let envelope = failure(&output);
    assert_assessment(&envelope);
    assert_eq!(
        fs::read(directory.root.join("receipt.json")).unwrap(),
        ORIGINAL
    );
    assert_ne!(envelope["error"]["code"], "ApprovalRequired");
    assert_ne!(envelope["error"]["code"], "NotConfigured");
    assert_ne!(envelope["error"]["code"], "invalid_configuration");
}
