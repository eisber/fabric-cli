// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Architecture checks for the additive workspace, using only offline Cargo metadata.

use serde_json::{Value, json};
use std::{collections::BTreeSet, process::Command};

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
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Cargo must be available to validate workspace architecture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let names: BTreeSet<_> = packages
        .iter()
        .map(|package| package["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "fabric-core",
            "fabric-identity",
            "fabric-sdk",
            "fabric-deploy",
            "fabric-cli",
        ])
    );
    assert_eq!(metadata["workspace_members"].as_array().unwrap().len(), 5);

    for package in packages {
        let name = package["name"].as_str().unwrap();
        assert_eq!(package["edition"], "2024");
        assert_eq!(package["publish"], json!([]));
        assert_eq!(package["rust_version"], env!("CARGO_PKG_RUST_VERSION"));
        let allowed: &[&str] = match name {
            "fabric-core" => &[],
            "fabric-identity" | "fabric-sdk" => &["fabric-core"],
            "fabric-deploy" => &["fabric-core", "fabric-sdk"],
            "fabric-cli" => &["fabric-identity", "fabric-sdk", "fabric-deploy"],
            _ => unreachable!("the exact workspace members were checked above"),
        };
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
        }
    }
}
