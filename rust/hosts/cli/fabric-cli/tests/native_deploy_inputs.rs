// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic checks for the binary's explicit parameter environment boundary.

#![deny(unnameable_test_items)]

#[path = "../src/native_deploy_inputs.rs"]
mod native_deploy_inputs;

use fabric_cli::deploy::DeployInputs;
use native_deploy_inputs::{EnvironmentInputError, select_environment, snapshot_environment};
use std::{collections::BTreeMap, ffi::OsString};

fn entry(name: &str, value: &str) -> (OsString, OsString) {
    (OsString::from(name), OsString::from(value))
}

#[test]
fn empty_iterator_retains_the_existing_empty_explicit_map_default() {
    let inputs = select_environment([]).unwrap();
    assert!(inputs.environment.is_empty());
    assert_eq!(inputs.environment, DeployInputs::default().environment);
}

#[test]
fn checked_selection_preserves_unicode_empty_values_and_literal_parameter_names() {
    let inputs = select_environment([
        entry("PARAMETER_NAME", "synthetic-value"),
        entry("EMPTY", ""),
        entry("UNICODE_Δ", "café 🌲"),
        entry("FAB_FAKE_AUTH_TOKEN", "synthetic-parameter-only"),
    ])
    .unwrap();
    assert_eq!(
        inputs.environment,
        BTreeMap::from([
            ("PARAMETER_NAME".into(), "synthetic-value".into()),
            ("EMPTY".into(), String::new()),
            ("UNICODE_Δ".into(), "café 🌲".into()),
            (
                "FAB_FAKE_AUTH_TOKEN".into(),
                "synthetic-parameter-only".into()
            ),
        ])
    );
}

#[test]
fn duplicates_use_last_value_without_case_folding_or_value_normalization() {
    let inputs = select_environment([
        entry("Name", "first"),
        entry("NAME", "distinct"),
        entry("Name", " last value "),
    ])
    .unwrap();
    assert_eq!(inputs.environment.len(), 2);
    assert_eq!(inputs.environment["Name"], " last value ");
    assert_eq!(inputs.environment["NAME"], "distinct");
}

#[cfg(windows)]
fn non_unicode() -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(&[0xd800])
}

#[cfg(unix)]
fn non_unicode() -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(vec![0xff])
}

#[cfg(any(windows, unix))]
#[test]
fn invalid_unicode_name_rejects_the_snapshot_instead_of_dropping_an_entry() {
    let result = select_environment([
        entry("VALID_PREFIX", "synthetic"),
        (non_unicode(), OsString::from("invalid-name-value")),
        entry("VALID_SUFFIX", "never-returned-as-partial-success"),
    ]);
    assert_eq!(result.unwrap_err(), EnvironmentInputError::NonUnicodeName);
}

#[cfg(any(windows, unix))]
#[test]
fn invalid_unicode_value_rejects_the_snapshot_without_including_input_in_diagnostics() {
    let result = select_environment([
        entry("VALID_PREFIX", "synthetic"),
        (OsString::from("SYNTHETIC_PRIVATE_NAME"), non_unicode()),
    ]);
    let error = result.unwrap_err();
    assert_eq!(error, EnvironmentInputError::NonUnicodeValue);
    assert!(!format!("{error} {error:?}").contains("SYNTHETIC_PRIVATE_NAME"));
    assert!(!format!("{error} {error:?}").contains("synthetic"));
}

#[test]
fn native_snapshot_api_is_explicit_and_never_invoked_by_the_offline_tests() {
    let _: fn() -> Result<DeployInputs, EnvironmentInputError> = snapshot_environment;
}
