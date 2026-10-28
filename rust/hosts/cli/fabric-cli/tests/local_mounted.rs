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
            "fabric-mounted-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
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
    invoke_in(root, root, args)
}

fn invoke_in(root: &Path, directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("FAB_RUST_CONFIG_ROOT", root)
        .env("FAB_RUST_SESSION_ID", "37")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap()
}

fn expected_cwd(directory: &Path) -> String {
    // POSIX getcwd resolves symlinks; Windows getcwd does not add canonicalize's verbatim prefix.
    let path = if cfg!(windows) {
        directory.to_path_buf()
    } else {
        fs::canonicalize(directory).unwrap()
    };
    path.to_string_lossy().into_owned()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
fn config_success_json_preserves_the_frozen_source_group_name() {
    let root = Root::new();
    let output = invoke(
        &root.0,
        &["config", "get", "output_format", "--output_format", "json"],
    );
    success(&output);
    assert!(output.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let captures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../reviews/20261027/python-output-captures.json"
    ))
    .unwrap();
    let capture = captures["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["name"] == "config-value")
        .unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(capture["stdout"].as_str().unwrap()).unwrap();
    for key in ["command", "status", "result"] {
        assert_eq!(actual[key], expected[key], "{key}");
    }
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(root.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(settings["output_format"], "text");
}

#[test]
fn all_config_success_leaves_keep_the_group_and_real_file_effects() {
    for format in ["text", "json"] {
        let root = Root::new();
        let set = invoke(
            &root.0,
            &[
                "config",
                "set",
                "show_hidden",
                "true",
                "--output_format",
                format,
            ],
        );
        success(&set);
        assert_eq!(set.stderr, b"Updating 'show_hidden' value...\n");
        let message = "Configuration 'show_hidden' set to 'true'";
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&set.stdout).unwrap();
            assert_eq!(value["command"], "config");
            assert_eq!(value["status"], "Success");
            assert_eq!(value["result"], serde_json::json!({"message":message}));
        } else {
            assert_eq!(set.stdout, format!("\n* {message}\n\n").as_bytes());
        }
        let settings = fs::read(root.0.join("config.json")).unwrap();
        let values: serde_json::Value = serde_json::from_slice(&settings).unwrap();
        assert_eq!(values["show_hidden"], "true");
        assert_eq!(values["output_format"], "text");
        let get = invoke(
            &root.0,
            &["config", "get", "show_hidden", "--output_format", format],
        );
        success(&get);
        assert!(get.stderr.is_empty());
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
            assert_eq!(value["command"], "config");
            assert_eq!(value["status"], "Success");
            assert_eq!(value["result"], serde_json::json!({"data":["true"]}));
        } else {
            assert_eq!(get.stdout, b"true\n");
        }
        let list = invoke(&root.0, &["config", "ls", "--output_format", format]);
        success(&list);
        assert!(list.stderr.is_empty());
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
            assert_eq!(value["command"], "config");
            assert_eq!(value["status"], "Success");
            let rows = value["result"]["data"].as_array().unwrap();
            assert_eq!(rows.len(), 18);
            assert!(rows.contains(&serde_json::json!({"setting":"show_hidden","value":"true"})));
        } else {
            let text = std::str::from_utf8(&list.stdout).unwrap();
            assert!(text.starts_with("setting"));
            assert!(text.contains("show_hidden"));
            assert!(text.contains("true"));
        }
        let context = root.0.join("context-37.json");
        fs::write(&context, br#"{"path":"/"}"#).unwrap();
        let cache = invoke(
            &root.0,
            &["config", "clear-cache", "--output_format", format],
        );
        success(&cache);
        assert!(cache.stderr.is_empty());
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&cache.stdout).unwrap();
            assert_eq!(value["command"], "config");
            assert_eq!(value["status"], "Success");
            assert_eq!(
                value["result"],
                serde_json::json!({"message":"Cleared the cache"})
            );
        } else {
            assert_eq!(cache.stdout, b"\n* Cleared the cache\n\n");
        }
        let absent = invoke(
            &root.0,
            &[
                "config",
                "get",
                "default_capacity",
                "--output_format",
                format,
            ],
        );
        success(&absent);
        assert!(absent.stdout.is_empty());
        assert!(absent.stderr.is_empty());
        assert_eq!(fs::read(root.0.join("config.json")).unwrap(), settings);
        assert_eq!(fs::read(context).unwrap(), br#"{"path":"/"}"#);
    }
}

#[test]
fn config_set_failure_preserves_leaf_name_and_does_not_change_the_setting() {
    for format in ["text", "json"] {
        let root = Root::new();
        success(&invoke(&root.0, &["config", "get", "show_hidden"]));
        let before = fs::read(root.0.join("config.json")).unwrap();
        let output = invoke(
            &root.0,
            &[
                "config",
                "set",
                "show_hidden",
                "invalid",
                "--output_format",
                format,
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["command"], "config set");
            assert_eq!(value["status"], "Failure");
            assert_eq!(value["result"]["error_code"], "InvalidInput");
            assert!(value["result"].get("data").is_none());
        } else {
            assert!(output.stdout.starts_with(b"x config set: [InvalidInput] "));
        }
        assert_eq!(fs::read(root.0.join("config.json")).unwrap(), before);
    }
}

#[test]
fn local_failure_text_keeps_the_leaf_prefix_and_json_failure_fields() {
    let root = Root::new();
    for format in ["text", "json"] {
        let output = invoke(
            &root.0,
            &["config", "get", "bad", "--output_format", format],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let message = "'bad' is not a recognized configuration key. Please check the available configuration keys";
        if format == "text" {
            assert_eq!(
                std::str::from_utf8(&output.stdout).unwrap(),
                format!("x config get: [InvalidInput] {message}\n"),
            );
        } else {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["status"], "Failure");
            assert_eq!(value["command"], "config get");
            assert_eq!(value["result"]["error_code"], "InvalidInput");
            assert_eq!(value["result"]["message"], message);
        }
    }
}

#[test]
fn navigation_persists_only_its_explicit_session_and_pwd_uses_stderr() {
    let root = Root::new();
    success(&invoke(
        &root.0,
        &["config", "set", "context_persistence_enabled", "true"],
    ));
    let cd = invoke(&root.0, &["cd", "/"]);
    success(&cd);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(root.0.join("context-37.json")).unwrap()
        )
        .unwrap(),
        serde_json::json!({"path": "/"}),
    );
    let pwd = invoke(&root.0, &["pwd"]);
    success(&pwd);
    assert!(pwd.stdout.is_empty());
    let stderr = std::str::from_utf8(&pwd.stderr).unwrap();
    let data = stderr
        .strip_prefix("! Command context path: root\n")
        .expect("restored context warning must precede pwd data");
    let output: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(output["local_path"], expected_cwd(&root.0));
}

#[test]
fn pwd_reports_process_cwd_not_the_config_root_or_a_parent_component_alias() {
    let root = Root::new();
    let directory = root.0.join("working");
    let nested = directory.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let alias = nested.join("..").join(".");
    let pwd = invoke_in(&root.0, &alias, &["pwd"]);
    success(&pwd);
    assert!(pwd.stdout.is_empty());
    let output: serde_json::Value = serde_json::from_slice(&pwd.stderr).unwrap();
    assert_eq!(output["local_path"], expected_cwd(&directory));
    assert_ne!(output["local_path"], expected_cwd(&root.0));
    assert!(root.0.join("config.json").is_file());
    assert!(!directory.join("config.json").exists());
    assert!(!root.0.join("context-37.json").exists());
}

#[test]
fn named_version_never_creates_configuration_and_desc_is_a_handler() {
    let root = Root::new();
    let version = invoke(&root.0, &["version"]);
    success(&version);
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    assert!(!root.0.join("config.json").exists());
    let expected: String = serde_json::from_str(include_str!(
        "../../../../capabilities/local/fabric-cli-local/tests/fixtures/notebook-desc.json"
    ))
    .unwrap();
    for format in ["text", "json"] {
        let desc = invoke(&root.0, &["desc", ".Notebook", "--output_format", format]);
        assert_eq!(
            desc.status.code(),
            Some(0),
            "{format}: stdout={:?} stderr={:?}",
            desc.stdout,
            desc.stderr,
        );
        assert_eq!(
            desc.stderr, b"",
            "{format}: stdout={:?} stderr={:?}",
            desc.stdout, desc.stderr,
        );
        assert_eq!(
            desc.stdout,
            expected.as_bytes(),
            "{format}: stderr={:?}",
            desc.stderr,
        );
    }
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
    let result = invoke(&root.0, &["config", "ls", "--output_format", "json"]);
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(envelope["status"], "Failure");
    assert_eq!(envelope["command"], "config ls");
    assert_eq!(envelope["result"]["error_code"], "InvalidJson");
    assert_eq!(fs::read(root.0.join("config.json")).unwrap(), b"{broken");
}

#[test]
fn configured_label_list_local_runs_without_any_identity_selectors() {
    let root = Root::new();
    let definitions = root.0.join("labels.json");
    fs::write(
        &definitions,
        br#"{"labels":[{"name":"Public","id":"label"}]}"#,
    )
    .unwrap();
    success(&invoke(
        &root.0,
        &[
            "config",
            "set",
            "local_definition_labels",
            definitions.to_str().unwrap(),
        ],
    ));
    let output = invoke(&root.0, &["label", "list-local", "--output_format", "json"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Public"));
    assert!(output.stderr.is_empty());
}

#[test]
fn configuration_stays_credential_free_with_a_persisted_remote_context() {
    let root = Root::new();
    success(&invoke(
        &root.0,
        &["config", "set", "context_persistence_enabled", "true"],
    ));
    fs::write(
        root.0.join("context-37.json"),
        br#"{"path":"/Demo.Workspace"}"#,
    )
    .unwrap();
    success(&invoke(&root.0, &["config", "get", "output_format"]));
    let pwd = invoke(&root.0, &["pwd"]);
    assert!(!pwd.status.success());
    if cfg!(feature = "native-read") {
        assert!(pwd.stdout.is_empty());
        assert!(String::from_utf8_lossy(&pwd.stderr).contains("NotConfigured"));
    } else {
        assert!(pwd.stderr.is_empty());
        let envelope: serde_json::Value = serde_json::from_slice(&pwd.stdout).unwrap();
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["command"], "pwd");
        assert_eq!(envelope["result"]["error_code"], "NotConfigured");
    }
}

#[test]
fn remote_family_schema_is_not_a_successful_stub_or_implicit_identity() {
    let root = Root::new();
    for args in [
        vec!["mkdir", "Demo.Workspace"],
        vec!["acl", "ls", "Demo.Workspace"],
        vec!["api", "workspaces"],
        vec!["export", "Demo.Workspace/Book.Notebook", "-o", "output"],
    ] {
        let output = invoke(&root.0, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("NotConfigured"));
    }
}
