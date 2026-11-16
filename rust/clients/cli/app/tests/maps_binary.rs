// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual native executable flows with synthetic local JSON, never live services.
#![cfg(feature = "maps")]
#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "invalid-offline-test-mode")
        .output()
        .unwrap()
}

#[test]
fn native_help_version_invalidinput_and_defaultoff_do_not_require_identity() {
    assert!(run(&["--version"]).status.success());
    let help = run(&["maps", "--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("dailyhistoricalweather")
    );
    let invalid = run(&[
        "maps",
        "current",
        "--coordinates",
        "NaN,40",
        "--experimental-private-contract",
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    let blocked = run(&["maps", "current", "--coordinates", "0,0"]);
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
fn outer200_fixture_matrix_missing_and_failed_pairs_produce_nonzero_native_status() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("synthetic-partial-matrix.json");
    std::fs::write(&path,serde_json::to_vec(&json!({"type":"Feature","geometry":null,"properties":{"matrix":[{"originIndex":0,"destinationIndex":0,"statusCode":403}]}})).unwrap()).unwrap();
    let output = run(&[
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
