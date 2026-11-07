// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Isolated executable evidence for the source process's one-time context load.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = PathBuf::from("target").join(format!(
            "batch-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn invoke(&self, arguments: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
        command.env_clear();
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("FAB_RUST_CONFIG_ROOT", &self.0)
            .env("FAB_RUST_SESSION_ID", "37")
            .current_dir(&self.0)
            .args(arguments)
            .output()
            .unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn nonpersistent_root_cd_never_creates_a_context_file() {
    let root = Root::new();
    let output = root.invoke(&["-c", "cd /", "-c", "pwd"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!root.0.join("context-37.json").exists());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    assert!(String::from_utf8_lossy(&output.stdout).contains("2 commands executed."));
}

#[test]
fn explicit_command_line_persistence_enabled_mid_batch_applies_to_the_next_cd() {
    let root = Root::new();
    let output = root.invoke(&[
        "-c",
        "config set context_persistence_enabled true",
        "-c",
        "cd /",
        "-c",
        "pwd",
    ]);
    assert!(output.status.success(), "{output:?}");
    let context: Value =
        serde_json::from_slice(&fs::read(root.0.join("context-37.json")).unwrap()).unwrap();
    assert_eq!(context, serde_json::json!({"path": "/"}));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Command context path"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("3 commands executed."));
}

#[test]
fn enabling_persistence_mid_batch_never_reloads_or_overwrites_the_captured_context() {
    let root = Root::new();
    assert!(
        root.invoke(&["config", "set", "context_persistence_enabled", "false"])
            .status
            .success()
    );
    let evidence = br#"{"path":"/Unselected.Workspace"}"#;
    fs::write(root.0.join("context-37.json"), evidence).unwrap();
    let output = root.invoke(&[
        "-c",
        "config set context_persistence_enabled true",
        "-c",
        "pwd",
        "-c",
        "config set show_hidden true",
        "-c",
        "pwd",
    ]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Unselected"));
    assert!(!stderr.contains("Command context path"));
    assert_eq!(stderr.matches("\"local_path\"").count(), 2);
    assert_eq!(fs::read(root.0.join("context-37.json")).unwrap(), evidence);
    assert!(String::from_utf8_lossy(&output.stdout).contains("4 commands executed."));
    let settings: Value =
        serde_json::from_slice(&fs::read(root.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(settings["show_hidden"], "true");
}
