// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit projection preserves JSON scalar types without changing v1 search.

#![deny(unnameable_test_items)]

use fabric_cli_query::{project_value, search};
use serde_json::{Value, json};

#[test]
fn typed_projection_keeps_null_bool_number_and_empty_results() {
    let data = json!({"yes":true,"no":false,"n":0,"none":null,"empty":[],"object":{}});
    for (expression, expected) in [
        ("yes", json!(true)),
        ("no", json!(false)),
        ("n", json!(0)),
        ("none", Value::Null),
        ("missing", Value::Null),
        ("empty", json!([])),
        ("object", json!({})),
    ] {
        assert_eq!(project_value(&data, expression).unwrap(), expected);
    }
    assert_eq!(search(&data, "no").unwrap(), json!("False"));
    assert_eq!(search(&data, "missing").unwrap(), json!("None"));
    assert_eq!(project_value(&data, ".").unwrap(), data);
    assert!(project_value(&data, "").is_err());
    assert!(project_value(&data, "[invalid").is_err());
}
