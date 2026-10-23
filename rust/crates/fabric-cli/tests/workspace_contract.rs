// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Architecture checks for the additive workspace, using only offline Cargo metadata.

use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, process::Command};

#[test]
fn workspace_members_and_internal_edges_respect_library_boundaries() {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
        .output()
        .expect("Cargo must be available to validate workspace architecture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let workspace_manifest = std::fs::read_to_string(
        std::path::Path::new(metadata["workspace_root"].as_str().unwrap()).join("Cargo.toml"),
    )
    .unwrap();
    assert!(workspace_manifest.contains("unsafe_code = \"forbid\""));
    let names: BTreeSet<_> = packages
        .iter()
        .map(|package| package["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "fabric-core",
            "fabric-identity",
            "fabric-identity-windows",
            "fabric-sdk",
            "fabric-deploy",
            "fabric-cli",
        ])
    );
    assert_eq!(metadata["workspace_members"].as_array().unwrap().len(), 6);
    let defaults: BTreeSet<_> = metadata["workspace_default_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(defaults.len(), 5);

    for package in packages {
        let name = package["name"].as_str().unwrap();
        assert_eq!(package["edition"], "2024");
        assert_eq!(package["publish"], json!([]));
        assert_eq!(package["rust_version"], env!("CARGO_PKG_RUST_VERSION"));
        let allowed: &[&str] = match name {
            "fabric-core" => &[],
            "fabric-identity" => &["fabric-core", "fabric-identity-windows"],
            "fabric-identity-windows" => &[],
            "fabric-sdk" => &["fabric-core"],
            "fabric-deploy" => &["fabric-core", "fabric-sdk"],
            "fabric-cli" => &["fabric-identity", "fabric-sdk", "fabric-deploy"],
            _ => unreachable!("the exact workspace members were checked above"),
        };
        if name == "fabric-identity-windows" {
            assert!(!defaults.contains(package["id"].as_str().unwrap()));
            let manifest =
                std::fs::read_to_string(package["manifest_path"].as_str().unwrap()).unwrap();
            assert!(manifest.contains("unsafe_code = \"deny\""));
            assert!(manifest.contains("unsafe_op_in_unsafe_fn = \"deny\""));
            assert!(manifest.contains("undocumented_unsafe_blocks = \"deny\""));
        } else {
            assert!(defaults.contains(package["id"].as_str().unwrap()));
            let manifest =
                std::fs::read_to_string(package["manifest_path"].as_str().unwrap()).unwrap();
            assert!(
                manifest.contains("[lints]\nworkspace = true")
                    || manifest.contains("[lints]\r\nworkspace = true"),
                "{name} must retain workspace unsafe_code=forbid"
            );
        }

        for dependency in package["dependencies"].as_array().unwrap() {
            let target = dependency["name"].as_str().unwrap();
            if names.contains(target) {
                assert!(
                    allowed.contains(&target),
                    "forbidden internal edge: {name} -> {target}"
                );
            }
            if target == "azure_core" {
                assert_eq!(dependency["uses_default_features"], false);
                assert_eq!(dependency["features"], json!([]));
            }
            if target == "fabric-identity-windows" {
                assert_eq!(name, "fabric-identity");
                assert_eq!(dependency["optional"], true);
                assert_eq!(dependency["target"], "cfg(windows)");
            }
            if target == "windows" || target == "windows-future" {
                assert_eq!(name, "fabric-identity-windows");
                assert_eq!(dependency["target"], "cfg(windows)");
            }
        }
    }
}

#[test]
fn windows_feature_selects_only_the_optional_windows_native_edge() {
    for (target, has_native) in [
        ("x86_64-pc-windows-msvc", true),
        ("x86_64-unknown-linux-gnu", false),
        ("aarch64-apple-darwin", false),
    ] {
        let output = Command::new(env!("CARGO"))
            .args([
                "tree",
                "--offline",
                "--locked",
                "-p",
                "fabric-identity",
                "--no-default-features",
                "--features",
                "windows-broker",
                "--target",
                target,
                "--edges",
                "normal",
                "--prefix",
                "none",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let graph = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            graph.contains("fabric-identity-windows v"),
            has_native,
            "{target}: {graph}"
        );
    }
}

#[test]
fn default_sdk_cli_and_identity_graphs_exclude_native_broker() {
    for package in ["fabric-sdk", "fabric-cli", "fabric-identity"] {
        let output = Command::new(env!("CARGO"))
            .args([
                "tree",
                "--offline",
                "--locked",
                "-p",
                package,
                "--no-default-features",
                "--edges",
                "normal",
                "--prefix",
                "none",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let graph = String::from_utf8(output.stdout).unwrap();
        for excluded in ["fabric-identity-windows v", "windows v", "windows-future v"] {
            assert!(!graph.contains(excluded), "{package}: {graph}");
        }
    }
}
