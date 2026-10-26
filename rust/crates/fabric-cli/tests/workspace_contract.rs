// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Architecture checks for the additive workspace, using only offline Cargo metadata.

use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layer {
    Core,
    Identity,
    Sdk,
    Deploy,
    Cli,
}

impl Layer {
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "core" => Some(Self::Core),
            "identity" => Some(Self::Identity),
            "sdk" => Some(Self::Sdk),
            "deploy" => Some(Self::Deploy),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }

    fn allows(self, dependency: Self) -> bool {
        match self {
            Self::Core => dependency == Self::Core,
            Self::Identity => matches!(dependency, Self::Identity | Self::Core),
            Self::Sdk => matches!(dependency, Self::Sdk | Self::Core),
            Self::Deploy => matches!(dependency, Self::Deploy | Self::Sdk | Self::Core),
            Self::Cli => matches!(
                dependency,
                Self::Cli | Self::Identity | Self::Sdk | Self::Deploy
            ),
        }
    }
}

fn validate_packages(packages: &[Value]) -> Result<(), String> {
    if packages.is_empty() {
        return Err("the workspace must contain at least one crate".to_owned());
    }
    let mut layers = BTreeMap::new();
    for package in packages {
        let name = package["name"]
            .as_str()
            .ok_or("workspace package is missing its name")?;
        let layer = Layer::parse(&package["metadata"]["fabric"]["layer"])
            .ok_or_else(|| format!("{name}: missing or unknown package.metadata.fabric.layer"))?;
        if name == "fabric-core-json" && layer != Layer::Core {
            return Err("fabric-core-json: reviewed JSON encoder must remain core".to_owned());
        }
        if layers.insert(name, layer).is_some() {
            return Err(format!("duplicate workspace crate name: {name}"));
        }
        if package["edition"] != "2024"
            || package["publish"] != json!([])
            || package["rust_version"] != env!("CARGO_PKG_RUST_VERSION")
        {
            return Err(format!("{name}: inconsistent workspace package settings"));
        }
    }

    for package in packages {
        let name = package["name"].as_str().unwrap();
        let source_layer = layers[name];
        let dependencies = package["dependencies"]
            .as_array()
            .ok_or_else(|| format!("{name}: missing dependency metadata"))?;
        for dependency in dependencies {
            let target = dependency["name"]
                .as_str()
                .ok_or_else(|| format!("{name}: dependency is missing its name"))?;
            if let Some(&target_layer) = layers.get(target)
                && !source_layer.allows(target_layer)
                && !(source_layer == Layer::Cli
                    && target_layer == Layer::Core
                    && name == "fabric-cli-output"
                    && target == "fabric-core-json")
            {
                return Err(format!("forbidden internal edge: {name} -> {target}"));
            }
            if target == "azure_core"
                && (dependency["uses_default_features"] != false
                    || dependency["features"] != json!([]))
            {
                return Err(format!("{name}: azure_core features must remain disabled"));
            }
        }
    }
    Ok(())
}

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
    let members = metadata["workspace_members"].as_array().unwrap();
    assert_eq!(packages.len(), members.len());
    for package in packages {
        assert!(members.contains(&package["id"]));
    }
    validate_packages(packages).unwrap();

    let workspace_manifest = std::fs::read_to_string(
        Path::new(metadata["workspace_root"].as_str().unwrap()).join("Cargo.toml"),
    )
    .unwrap();
    assert!(workspace_manifest.contains("unsafe_code = \"forbid\""));
    let defaults: BTreeSet<_> = metadata["workspace_default_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(defaults.len(), packages.len() - 1);
    assert!(
        packages
            .iter()
            .any(|package| package["name"] == "fabric-identity-windows")
    );
    for package in packages {
        let name = package["name"].as_str().unwrap();
        let manifest = std::fs::read_to_string(package["manifest_path"].as_str().unwrap()).unwrap();
        if matches!(
            name,
            "fabric-cli" | "fabric-identity" | "fabric-sdk-transport"
        ) {
            assert_eq!(package["features"]["default"], json!([]));
        }
        if name == "fabric-identity-windows" {
            assert!(!defaults.contains(package["id"].as_str().unwrap()));
            assert_eq!(package["metadata"]["fabric"]["layer"], "identity");
            assert!(manifest.contains("unsafe_code = \"deny\""));
            assert!(manifest.contains("unsafe_op_in_unsafe_fn = \"deny\""));
            assert!(manifest.contains("undocumented_unsafe_blocks = \"deny\""));
            for dependency in package["dependencies"].as_array().unwrap() {
                assert!(
                    !packages
                        .iter()
                        .any(|candidate| candidate["name"] == dependency["name"]),
                    "the isolated native boundary must not depend on another workspace crate"
                );
            }
        } else {
            assert!(defaults.contains(package["id"].as_str().unwrap()));
            assert!(
                manifest.contains("[lints]\nworkspace = true")
                    || manifest.contains("[lints]\r\nworkspace = true"),
                "{name} must retain workspace unsafe_code=forbid"
            );
        }
        for dependency in package["dependencies"].as_array().unwrap() {
            let target = dependency["name"].as_str().unwrap();
            if target == "fabric-identity-windows" {
                assert_eq!(name, "fabric-identity");
                assert_eq!(dependency["optional"], true);
                assert_eq!(dependency["target"], "cfg(windows)");
            }
            if name == "fabric-cli" && target == "fabric-identity" {
                assert_eq!(dependency["optional"], true);
            }
            if target == "windows" || target == "windows-future" {
                assert_eq!(name, "fabric-identity-windows");
                assert_eq!(dependency["target"], "cfg(windows)");
            }
        }
    }
}

fn package(name: &str, layer: &str, dependencies: &[&str]) -> Value {
    json!({
        "name": name,
        "edition": "2024",
        "publish": [],
        "rust_version": env!("CARGO_PKG_RUST_VERSION"),
        "metadata": {"fabric": {"layer": layer}},
        "dependencies": dependencies.iter().map(|name| json!({
            "name": name,
            "uses_default_features": false,
            "features": [],
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn focused_crates_are_not_limited_to_the_initial_five() {
    let packages = [
        package("shared-types", "core", &[]),
        package("request-policy", "core", &["shared-types"]),
        package("identity-adapter", "identity", &["shared-types"]),
        package("workspace-service", "sdk", &["request-policy"]),
        package("item-service", "sdk", &["request-policy"]),
        package("sdk-facade", "sdk", &["workspace-service", "item-service"]),
        package("deployment-plan", "deploy", &["sdk-facade"]),
        package("workspace-command", "cli", &["workspace-service"]),
        package(
            "application",
            "cli",
            &["workspace-command", "identity-adapter"],
        ),
    ];
    assert_eq!(validate_packages(&packages), Ok(()));
}

#[test]
fn missing_and_unknown_layers_fail_closed() {
    for layer in [Value::Null, json!("unclassified")] {
        let mut candidate = package("new-service", "sdk", &[]);
        candidate["metadata"]["fabric"]["layer"] = layer;
        assert_eq!(
            validate_packages(&[candidate]),
            Err("new-service: missing or unknown package.metadata.fabric.layer".to_owned())
        );
    }
}

#[test]
fn dependency_policy_covers_every_layer_direction() {
    let cases = [
        ("core", Layer::Core, ["core"].as_slice()),
        ("identity", Layer::Identity, ["identity", "core"].as_slice()),
        ("sdk", Layer::Sdk, ["sdk", "core"].as_slice()),
        (
            "deploy",
            Layer::Deploy,
            ["deploy", "sdk", "core"].as_slice(),
        ),
        (
            "cli",
            Layer::Cli,
            ["cli", "identity", "sdk", "deploy"].as_slice(),
        ),
    ];
    for (source_name, source_layer, allowed) in cases {
        for (target_name, target_layer, _) in cases {
            assert_eq!(
                source_layer.allows(target_layer),
                allowed.contains(&target_name),
                "{source_name} -> {target_name}"
            );
        }
    }
}

#[test]
fn reviewed_output_json_edge_is_not_a_general_cli_to_core_permission() {
    assert_eq!(
        validate_packages(&[
            package("fabric-cli-output", "cli", &["fabric-core-json"]),
            package("fabric-core-json", "core", &[]),
        ]),
        Ok(())
    );
    for (source, target) in [
        ("other-cli", "fabric-core-json"),
        ("fabric-cli-output", "other-core"),
    ] {
        assert_eq!(
            validate_packages(&[
                package(source, "cli", &[target]),
                package(target, "core", &[]),
            ]),
            Err(format!("forbidden internal edge: {source} -> {target}"))
        );
    }
    for target_layer in ["identity", "sdk", "deploy", "cli"] {
        assert_eq!(
            validate_packages(&[
                package("fabric-cli-output", "cli", &["fabric-core-json"]),
                package("fabric-core-json", target_layer, &[]),
            ]),
            Err("fabric-core-json: reviewed JSON encoder must remain core".to_owned())
        );
    }
}

#[test]
fn reviewed_json_core_cannot_hide_a_forbidden_transitive_edge() {
    for target_layer in ["identity", "deploy", "cli"] {
        assert_eq!(
            validate_packages(&[
                package("fabric-cli-output", "cli", &["fabric-core-json"]),
                package("fabric-core-json", "core", &["implementation"]),
                package("implementation", target_layer, &[]),
            ]),
            Err("forbidden internal edge: fabric-core-json -> implementation".to_owned())
        );
    }
}

#[test]
fn additional_crates_cannot_hide_a_forbidden_transitive_edge() {
    for target_layer in ["identity", "deploy", "cli"] {
        let packages = [
            package("workspace-service", "sdk", &["new-helper"]),
            package("new-helper", "core", &["implementation"]),
            package("implementation", target_layer, &[]),
        ];
        assert_eq!(
            validate_packages(&packages),
            Err("forbidden internal edge: new-helper -> implementation".to_owned())
        );
    }
}

#[test]
fn additional_crates_inherit_package_and_azure_feature_checks() {
    for (field, invalid) in [
        ("edition", json!("2021")),
        ("publish", Value::Null),
        ("rust_version", json!("1.0")),
    ] {
        let mut candidate = package("new-service", "sdk", &[]);
        candidate[field] = invalid;
        assert_eq!(
            validate_packages(&[candidate]),
            Err("new-service: inconsistent workspace package settings".to_owned())
        );
    }
    for (field, invalid) in [
        ("uses_default_features", json!(true)),
        ("features", json!(["reqwest"])),
    ] {
        let mut candidate = package("new-service", "sdk", &["azure_core"]);
        candidate["dependencies"][0][field] = invalid;
        assert_eq!(
            validate_packages(&[candidate]),
            Err("new-service: azure_core features must remain disabled".to_owned())
        );
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
    for package in [
        "fabric-sdk",
        "fabric-sdk-workspaces",
        "fabric-cli",
        "fabric-identity",
    ] {
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
