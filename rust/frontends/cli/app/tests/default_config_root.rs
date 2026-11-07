// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The executable shares its default non-secret profile across commands without explicit setup.

#![deny(unnameable_test_items)]

use serde_json::Value;
#[cfg(all(windows, feature = "native-read"))]
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = parent.join(format!(
            "fabric-default-profile-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn configuration(&self) -> PathBuf {
        #[cfg(windows)]
        return self.0.join("Microsoft").join("FabricCLI");
        #[cfg(not(windows))]
        return self.0.join("microsoft").join("fabric-cli");
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn process(profile: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear().env("CI", "1");
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    command.env("LOCALAPPDATA", profile);
    #[cfg(not(windows))]
    command.env("XDG_CONFIG_HOME", profile);
    command
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn setting_updated(output: Output) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Updating 'output_format' value...\n"
    );
}

#[test]
fn configuration_works_without_fab_rust_config_root_and_is_shared_with_batches() {
    let root = Root::new();
    setting_updated(
        process(&root.0)
            .args(["config", "set", "output_format", "json"])
            .output()
            .unwrap(),
    );
    let file = root.configuration().join("config.json");
    let settings: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(settings["output_format"], "json");
    assert_eq!(settings["context_persistence_enabled"], "false");
    let output = success(
        process(&root.0)
            .args(["config", "get", "output_format"])
            .output()
            .unwrap(),
    );
    assert!(output.contains("json"));
    let output = process(&root.0)
        .args(["-c", "version", "-c", "version"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"command\": ["), "{text}");
    assert!(text.contains("2 commands executed."), "{text}");
    assert!(!text.contains("\u{1b}["), "{text}");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap(),
        settings
    );
}

#[test]
fn explicit_override_wins_and_malformed_overrides_cannot_fall_back_to_the_default_profile() {
    let root = Root::new();
    let explicit = root.0.join("override");
    setting_updated(
        process(&root.0)
            .env("FAB_RUST_CONFIG_ROOT", &explicit)
            .args(["config", "set", "output_format", "json"])
            .output()
            .unwrap(),
    );
    assert!(explicit.join("config.json").is_file());
    assert!(!root.configuration().exists());
    for invalid in ["", "relative-private-marker"] {
        let output = process(&root.0)
            .env("FAB_RUST_CONFIG_ROOT", invalid)
            .args(["config", "get", "output_format"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("relative-private-marker"));
        assert!(!root.configuration().exists());
    }
}

#[test]
fn help_and_version_do_not_create_profile_state() {
    let root = Root::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["version"],
        vec!["find", "--help"],
    ] {
        success(process(&root.0).args(args).output().unwrap());
        assert!(!root.configuration().exists());
    }
}

#[cfg(all(windows, feature = "native-read"))]
#[test]
fn remote_find_and_reads_obey_the_same_default_profile_settings_before_credentials() {
    let root = Root::new();
    let store = root.configuration();
    fs::create_dir_all(&store).unwrap();
    fs::write(
        store.join("broker-session.json"),
        serde_json::to_vec(&json!({
            "schema": 1, "generation": 1,
            "tenant": "11111111-2222-3333-4444-555555555555",
            "account": "selected@example.test",
            "client": "5814bfb4-2705-4994-b8d6-39aabeb5eaeb"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        store.join("config.json"),
        br#"{"fab_output_format":"invalid-value"}"#,
    )
    .unwrap();
    for args in [vec!["find", "marcozo"], vec!["ls", "/"]] {
        let output = process(&root.0).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_configuration");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("output_format"),
            "{error}"
        );
        assert!(
            !error
                .to_string()
                .contains("Local commands require FAB_RUST_CONFIG_ROOT")
        );
        let migrated: Value =
            serde_json::from_slice(&fs::read(store.join("config.json")).unwrap()).unwrap();
        assert_eq!(migrated["output_format"], "invalid-value");
        assert!(migrated.get("fab_output_format").is_none());
    }
}
