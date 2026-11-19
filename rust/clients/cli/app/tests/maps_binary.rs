// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual native executable flows with synthetic local JSON, never live services.
#![cfg(feature = "maps")]
#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let config = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "invalid-offline-test-mode")
        .env("FAB_RUST_CONFIG_ROOT", config.path())
        .env_remove("FAB_RUST_CONTEXT")
        .output()
        .unwrap()
}

#[test]
fn native_help_version_invalidinput_and_defaultoff_do_not_require_identity() {
    assert!(run(&["--version"]).status.success());
    let help = run(&["item", "maps", "--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("dailyhistoricalweather")
    );
    let invalid = run(&[
        "item",
        "maps",
        "current",
        "--coordinates",
        "NaN,40",
        "--experimental-private-contract",
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    let blocked = run(&["item", "maps", "current", "--coordinates", "0,0"]);
    assert_eq!(blocked.status.code(), Some(2));
    assert!(
        String::from_utf8(blocked.stderr)
            .unwrap()
            .contains("PrivateContractOptInRequired")
    );
}

#[test]
fn actual_native_geocode_and_weather_fixture_flows_preserve_data_and_export() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("synthetic-weather.json");
    let payload = json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","temperature":{"value":8.3,"unit":"C"},"null":null}]});
    std::fs::write(&fixture, serde_json::to_vec(&payload).unwrap()).unwrap();
    let path = fixture.to_str().unwrap();
    let output = run(&[
        "item",
        "maps",
        "current",
        "--coordinates",
        "-73.9632,40.7794",
        "--offline-fixture",
        path,
    ]);
    assert!(output.status.success(), "{:?}", output);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["data"], payload);
    assert_eq!(document["validation"], "offline-synthetic-fixture");
    let export = temp.path().join("weather.parquet");
    let output = run(&[
        "item",
        "maps",
        "current",
        "--coordinates",
        "0,0",
        "--offline-fixture",
        path,
        "--format",
        "parquet",
        "--output",
        export.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{:?}", output);
    let data = std::fs::read(export).unwrap();
    assert_eq!(&data[0..4], b"PAR1");
    let geocode = temp.path().join("synthetic-geocode.json");
    let payload = json!({"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[-73.96016896337983,40.781764687428804]},"properties":{"confidence":"High","within250m":false,"distanceFromReferenceMeters":366}}]});
    std::fs::write(&geocode, serde_json::to_vec(&payload).unwrap()).unwrap();
    let output = run(&[
        "item",
        "maps",
        "geocode",
        "--address",
        "synthetic address",
        "--offline-fixture",
        geocode.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["data"], payload);
}

#[test]
fn unsupported_headless_mode_is_rejected_before_secret_configuration_capture() {
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args([
            "item",
            "maps",
            "current",
            "--coordinates",
            "0,0",
            "--experimental-private-contract",
        ])
        .env("FAB_RUST_AUTH_MODE", "client-secret")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("AZURE_TENANT_ID")
        .env_remove("FAB_RUST_TENANT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("UnsupportedIdentity")
    );
}

#[test]
fn csv_expansion_budget_fails_without_publishing_a_native_data_file() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("synthetic-large-geocode.json");
    let output_path = temp.path().join("must-not-exist.csv");
    let candidate = json!({"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"confidence":"High"}});
    let payload = json!({"type":"FeatureCollection","metadata":"\"".repeat(300_000),"features":vec![candidate;12]});
    std::fs::write(&fixture, serde_json::to_vec(&payload).unwrap()).unwrap();
    let output = run(&[
        "item",
        "maps",
        "geocode",
        "--address",
        "synthetic",
        "--offline-fixture",
        fixture.to_str().unwrap(),
        "--format",
        "csv",
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output_path.exists());
    assert!(String::from_utf8(output.stderr).unwrap().contains("8 MiB"));
}

#[test]
fn expanding_query_fails_before_offline_file_or_identity_access() {
    let output = run(&[
        "item",
        "maps",
        "current",
        "--coordinates",
        "0,0",
        "--query",
        "[@,@] | [@,@]",
        "--offline-fixture",
        "this-fixture-must-not-be-opened.json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid_arguments")
    );
}

#[test]
fn outer200_fixture_matrix_missing_and_failed_pairs_produce_nonzero_native_status() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("synthetic-partial-matrix.json");
    std::fs::write(&path,serde_json::to_vec(&json!({"type":"Feature","geometry":null,"properties":{"matrix":[{"originIndex":0,"destinationIndex":0,"statusCode":403}]}})).unwrap()).unwrap();
    let output = run(&[
        "item",
        "maps",
        "matrix",
        "--origin",
        "0,0",
        "--destination",
        "1,1",
        "--destination",
        "2,2",
        "--offline-fixture",
        path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["complete"], false);
    assert_eq!(document["matrixCells"].as_array().unwrap().len(), 2);
    assert!(document["matrixCells"][1]["statusCode"].is_null());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("PartialResult")
    );
}

#[test]
fn nested_maps_is_discoverable_and_top_level_maps_is_rejected() {
    let root = run(&["--help"]);
    assert!(root.status.success());
    let root_help = String::from_utf8(root.stdout).unwrap();
    assert!(
        root_help
            .lines()
            .any(|line| line.trim_start().starts_with("item "))
    );
    assert!(
        !root_help
            .lines()
            .any(|line| line.trim_start().starts_with("maps "))
    );
    for args in [&["item"][..], &["item", "--help"]] {
        let output = run(args);
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("maps"));
        assert!(help.contains("fab-rs item"));
    }
    let leaf = run(&["item", "maps", "current", "--help"]);
    assert!(leaf.status.success());
    assert!(
        String::from_utf8(leaf.stdout)
            .unwrap()
            .contains("fab-rs item maps current")
    );
    let old = run(&["maps", "current", "--coordinates", "0,0"]);
    assert_eq!(old.status.code(), Some(2));
    assert!(old.stdout.is_empty());
}

#[test]
fn nested_maps_fixture_uses_the_same_dispatch_in_batch_mode() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("weather.json");
    let payload = json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","temperature":{"value":8.3,"unit":"C"}}]});
    std::fs::write(&fixture, serde_json::to_vec(&payload).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .current_dir(temp.path())
        .args([
            "-c",
            "item maps current --coordinates 0,0 --offline-fixture weather.json",
        ])
        .env("FAB_RUST_AUTH_MODE", "invalid-offline-test-mode")
        .env("FAB_RUST_CONFIG_ROOT", temp.path().join("config"))
        .env_remove("FAB_RUST_CONTEXT")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["data"], payload);
}

#[test]
fn nested_global_options_reject_oversized_input_before_opening_a_fixture() {
    let output = run(&[
        "item",
        "--output_format",
        "json",
        "maps",
        "current",
        "--coordinates",
        "0,0",
        "--offline-fixture",
        &"x".repeat(4097),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid_arguments")
    );
}
