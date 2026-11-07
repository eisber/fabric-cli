// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit feature edges keep credentials out of default CLI and SDK graphs.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use std::{path::Path, process::Command};

fn graph(package: &str, feature: Option<&str>) -> String {
    let mut command = Command::new(env!("CARGO"));
    command.args([
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
    ]);
    if let Some(feature) = feature {
        command.args(["--features", feature]);
    }
    let output = command
        .current_dir(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join(".."),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn identity_is_explicit_and_never_introduces_sdk_to_identity_or_default_http_edges() {
    for package in ["fabric-sdk", "fabric-cli", "fabric-identity"] {
        let graph = graph(package, None);
        assert!(!graph.contains("azure_identity v"), "{package}: {graph}");
        assert!(
            !graph.contains("fabric-identity-windows v"),
            "{package}: {graph}"
        );
    }
    for package in ["fabric-cli", "fabric-identity"] {
        let graph = graph(package, Some("headless"));
        assert!(
            graph.contains("azure_identity v1.0.0"),
            "{package}: {graph}"
        );
        for excluded in ["reqwest v", "openssl v", "fabric-identity-windows v"] {
            assert!(!graph.contains(excluded), "{package}: {graph}");
        }
    }
}
