// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fab-rs"))
}

#[test]
fn default_binary_does_not_acquire_an_artifact_or_managed_runtime_surface() {
    let help = binary().arg("--help").output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    #[cfg(not(feature = "semantic-model"))]
    assert!(!text.contains("artifact"));
    #[cfg(feature = "semantic-model")]
    assert!(text.contains("artifact"));
}

#[cfg(all(feature = "semantic-model", not(feature = "semantic-model-dotnet")))]
#[test]
fn disabled_dotnet_fails_before_opening_a_missing_input() {
    let output = binary()
        .args([
            "artifact",
            "semantic-model",
            "analyze",
            "--input",
            "does-not-exist.bim",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "UnsupportedCapability");
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("does-not-exist")
    );
}

#[cfg(feature = "semantic-model")]
#[test]
fn remote_path_is_explicitly_unavailable_without_identity_or_transport_fallback() {
    let output = binary()
        .args([
            "artifact",
            "semantic-model",
            "analyze",
            "/demo.Workspace/model.SemanticModel",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "UnsupportedCapability");
}
