// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! External CLI-layer consumer of the existing pure SET helper, not validation.

use fabric_cli_mutations::update_fabric_element;
use serde_json::json;

#[test]
fn public_set_retains_only_changed_top_property_and_existing_siblings() {
    let result = update_fabric_element(
        json!({"name":"old","properties":{"values":[1,2],"keep":true}}),
        "properties.values[-1]",
        "null",
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"properties":{"values":[1,null],"keep":true}})
    );
    assert_eq!(
        update_fabric_element(json!({}), "name", "plain input").unwrap(),
        json!({"name":"plain input"})
    );
}

#[test]
fn public_set_preserves_source_raw_projection_oddity_and_explicit_failures() {
    assert_eq!(
        update_fabric_element(json!({"values":[1,2]}), "values[0]", "3").unwrap(),
        json!({})
    );
    assert_eq!(
        update_fabric_element(json!({}), "missing.child", "x")
            .unwrap_err()
            .code,
        "InvalidQuery"
    );
    assert_eq!(
        update_fabric_element(json!({"name":"old"}), "name", "18446744073709551616")
            .unwrap_err()
            .code,
        "InvalidJson"
    );
}
