// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The HTTP foundation must remain below every service and front end.

#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{collections::BTreeMap, process::Command};

fn validate(packages: &[Value]) -> Result<(), String> {
    let packages: BTreeMap<_, _> = packages
        .iter()
        .map(|package| (package["name"].as_str().unwrap(), package))
        .collect();
    for root in ["fabric-core", "fabric-http", "fabric-http-transport"] {
        let mut pending = vec![root];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !visited.insert(name) {
                continue;
            }
            if !matches!(
                name,
                "fabric-core" | "fabric-http" | "fabric-http-transport"
            ) {
                return Err(format!("{root}: forbidden foundation dependency {name}"));
            }
            let package = packages
                .get(name)
                .ok_or_else(|| format!("missing {name}"))?;
            for dependency in package["dependencies"].as_array().unwrap() {
                let target = dependency["name"].as_str().unwrap();
                if packages.contains_key(target) {
                    pending.push(target);
                }
                if dependency["kind"] != "dev"
                    && root != "fabric-http-transport"
                    && matches!(target, "tokio" | "async-std" | "fabric-http-transport")
                {
                    return Err(format!(
                        "{root}: runtime or transport creation dependency {target}"
                    ));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn all_http_foundation_features_exclude_services_facade_and_providers() {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .expect("Cargo is required to inspect all declared dependency edges");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    validate(metadata["packages"].as_array().unwrap()).unwrap();
}

#[test]
fn optional_transitive_service_edges_are_rejected_not_hidden_by_features() {
    use serde_json::json;
    let foundation = |name, dependencies| json!({"name": name, "dependencies": dependencies});
    for source in ["fabric-http", "fabric-http-transport", "fabric-core"] {
        let mut packages = vec![
            foundation("fabric-core", json!([])),
            foundation("fabric-http", json!([{"name":"fabric-core","kind":null}])),
            foundation(
                "fabric-http-transport",
                json!([{"name":"fabric-http","kind":null}]),
            ),
            foundation("fabric-sdk", json!([])),
        ];
        let package = packages
            .iter_mut()
            .find(|package| package["name"] == source)
            .unwrap();
        package["dependencies"].as_array_mut().unwrap().push(json!({
            "name": "fabric-sdk", "kind": null, "optional": true
        }));
        assert!(validate(&packages).unwrap_err().contains("fabric-sdk"));
    }
}
