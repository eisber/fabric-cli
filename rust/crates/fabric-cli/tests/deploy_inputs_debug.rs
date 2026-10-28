// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deployment parameter input Debug output must not disclose names or values.

#![deny(unnameable_test_items)]

use fabric_cli::deploy::DeployInputs;
use std::collections::BTreeMap;

#[test]
fn normal_and_pretty_debug_redact_all_environment_names_and_values() {
    let environment = BTreeMap::from([
        (
            "SYNTHETIC_PARAMETER_ONE".into(),
            "synthetic-private-value-1".into(),
        ),
        (
            "SYNTHETIC_PARAMETER_TWO".into(),
            "second-private-value".into(),
        ),
        ("café_name".into(), "Δ-sensitive-content🌲".into()),
    ]);
    let inputs = DeployInputs { environment };
    for output in [format!("{inputs:?}"), format!("{inputs:#?}")] {
        assert!(output.contains("DeployInputs"));
        assert!(output.contains("<redacted>"));
        assert!(output.contains("environment_entries: 3"));
        for (name, value) in &inputs.environment {
            assert!(!output.contains(name.as_str()));
            assert!(!output.contains(value.as_str()));
        }
    }
}

#[test]
fn redaction_does_not_change_default_clone_or_the_existing_explicit_map_api() {
    let empty = DeployInputs::default();
    assert!(empty.environment.is_empty());
    assert!(format!("{empty:?}").contains("<redacted>"));
    assert!(format!("{empty:?}").contains("environment_entries: 0"));
    let expected = BTreeMap::from([("PARAMETER_NAME".into(), "synthetic-value".into())]);
    let inputs = DeployInputs {
        environment: expected.clone(),
    };
    let cloned = inputs.clone();
    assert!(format!("{inputs:?} {cloned:#?}").contains("<redacted>"));
    assert_eq!(inputs.environment, expected);
    assert_eq!(cloned.environment, expected);
}
