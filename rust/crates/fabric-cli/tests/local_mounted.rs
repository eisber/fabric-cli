// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable local handlers, with isolated non-secret configuration.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-mounted-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("FAB_RUST_CONFIG_ROOT", root)
        .env("FAB_RUST_SESSION_ID", "37")
        .current_dir(root).args(args).output().unwrap()
}

fn success(output: &Output) {
    assert!(output.status.success(), "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn config_set_get_list_and_cache_have_real_file_effects_without_identity() {
    let root = Root::new();
    let set = invoke(&root.0, &["config", "set", "output_format", "json"]);
    success(&set);
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(root.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(settings["output_format"], "json");
    assert_eq!(settings["context_persistence_enabled"], "false");
    let get = invoke(&root.0, &["config", "get", "output_format"]);
    success(&get);
    assert!(get.stderr.is_empty());
    assert!(String::from_utf8_lossy(&get.stdout).contains("json"));
    let list = invoke(&root.0, &["config", "ls"]);
    success(&list);
    assert!(String::from_utf8_lossy(&list.stdout).contains("check_cli_version_updates"));
    let before = fs::read(root.0.join("config.json")).unwrap();
    let cache = invoke(&root.0, &["config", "clear-cache"]);
    success(&cache);
    assert_eq!(fs::read(root.0.join("config.json")).unwrap(), before);
    let absent = invoke(&root.0, &["config", "get", "default_capacity"]);
    success(&absent);
    assert!(absent.stdout.is_empty());
}

#[test]
fn navigation_persists_only_its_explicit_session_and_pwd_uses_stderr() {
    let root = Root::new();
    success(&invoke(&root.0, &["config", "set", "context_persistence_enabled", "true"]));
    let cd = invoke(&root.0, &["cd", "/"]);
    success(&cd);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(root.0.join("context-37.json")).unwrap()).unwrap(),
        serde_json::json!({"path": "/"}),
    );
    let pwd = invoke(&root.0, &["pwd"]);
    success(&pwd);
    assert!(pwd.stdout.is_empty());
    assert!(String::from_utf8_lossy(&pwd.stderr).contains("local_path"));
    assert!(String::from_utf8_lossy(&pwd.stderr).contains(root.0.to_string_lossy().as_ref()));
}

#[test]
fn named_version_never_creates_configuration_and_desc_is_a_handler() {
    let root = Root::new();
    let version = invoke(&root.0, &["version"]);
    success(&version);
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    assert!(!root.0.join("config.json").exists());
    let desc = invoke(&root.0, &["desc", ".Notebook", "--output_format", "json"]);
    success(&desc);
    assert!(String::from_utf8_lossy(&desc.stdout).contains("Notebook"));
    let extension = invoke(&root.0, &["extension"]);
    success(&extension);
    assert!(!extension.stderr.is_empty());
    let clear = invoke(&root.0, &["clear"]);
    success(&clear);
    assert_eq!(clear.stdout, b"\x1b[H\x1b[2J\x1b[3J");
}

#[test]
fn invalid_configuration_is_not_overwritten_or_hidden() {
    let root = Root::new();
    fs::write(root.0.join("config.json"), b"{broken").unwrap();
    let result = invoke(&root.0, &["config", "ls"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("InvalidJson"));
    assert_eq!(fs::read(root.0.join("config.json")).unwrap(), b"{broken");
}
