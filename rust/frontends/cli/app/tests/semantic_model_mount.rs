// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Optional semantic-model CLI registration and unavailable-host checks.

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

#[cfg(feature = "semantic-model-dotnet")]
fn analyze_fixture(evidence: bool, extra: &[&str]) -> std::process::Output {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("rust-toolchain.toml").is_file())
        .unwrap();
    let hostfxr = std::env::var_os("FABRIC_BPA_TEST_HOSTFXR")
        .expect("Hosted acceptance requires an explicitly configured .NET runtime");
    let mut command = binary();
    command
        .args(["artifact", "semantic-model", "analyze", "--input"])
        .arg(root.join("frontends/cli/commands/semantic-model/tests/analyze.bim"))
        .arg("--hostfxr")
        .arg(hostfxr);
    if evidence {
        command.arg("--evidence").arg(root.join(
            "frontends/cli/commands/semantic-model/tests/complete-evidence.json",
        ));
    }
    command.args(extra).output().unwrap()
}

#[cfg(feature = "semantic-model-dotnet")]
#[test]
fn actual_managed_cli_preserves_complete_inventory_and_strict_findings() {
    let output = analyze_fixture(true, &[]);
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "succeeded");
    assert_eq!(report["coverage"]["evaluated"], 64);
    assert_eq!(report["rules"].as_array().unwrap().len(), 64);
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["ruleName"] == "Do not use floating point data types"
                    && finding["objectName"] == "'Sales'[Amount]"
            })
    );
    assert!(output.stderr.is_empty());
    let strict = analyze_fixture(true, &["--strict", "--severity", "warning"]);
    assert_eq!(strict.status.code(), Some(3), "{:?}", strict);
}

#[cfg(feature = "semantic-model-dotnet")]
#[test]
fn actual_managed_cli_keeps_missing_evidence_partial_even_when_findings_filtered() {
    let output = analyze_fixture(false, &["--severity", "error"]);
    assert_eq!(output.status.code(), Some(1), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "partial");
    assert!(report["coverage"]["unavailable"].as_u64().unwrap() > 0);
    assert_eq!(report["rules"].as_array().unwrap().len(), 64);
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["severity"] == "error")
    );
}

#[cfg(feature = "semantic-model-dotnet")]
#[test]
fn malformed_local_model_reports_invalid_input_without_runtime_or_path_disclosure() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("rust-toolchain.toml").is_file())
        .unwrap();
    let hostfxr = std::env::var_os("FABRIC_BPA_TEST_HOSTFXR").unwrap();
    let output = binary()
        .args(["artifact", "semantic-model", "analyze", "--input"])
        .arg(root.join("tests/fixtures/semantic_model_bpa/models/malformed.bim"))
        .arg("--hostfxr")
        .arg(hostfxr)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "InvalidInput");
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("malformed.bim")
    );
}
