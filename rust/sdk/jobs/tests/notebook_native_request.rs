// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

//! Offline native Notebook schema contracts, not CLI/MCP or live service proof.

use fabric_sdk_jobs::notebook::{NotebookCompute, NotebookInputError, NotebookRunRequest};
use serde_json::{Value, json};

fn request(value: Value) -> Result<NotebookRunRequest, NotebookInputError> {
    NotebookRunRequest::from_input(Some(value))
}

fn parameter(parameter_type: &str, value: Value) -> Value {
    json!({"parameters":[{"name":"Input","type":parameter_type,"value":value}]})
}

#[test]
fn missing_input_is_distinct_from_explicit_empty_object() {
    let absent = NotebookRunRequest::from_input(None).unwrap();
    let explicit = request(json!({})).unwrap();
    assert_eq!(absent.body(), None);
    assert_eq!(explicit.body(), Some(&json!({})));
    assert_ne!(absent, explicit);
    for value in [Value::Null, json!([]), json!(""), json!(true)] {
        assert_eq!(request(value), Err(NotebookInputError::InvalidShape));
    }
}

#[test]
fn native_parameters_preserve_top_level_order_casing_and_json_types() {
    let original = json!({"parameters":[
        {"name":"day","type":"Text","value":"2026-11-05"},
        {"name":"batch","type":"Integer","value":500},
        {"name":"enabled","type":"Boolean","value":false},
        {"name":"fraction","type":"Number","value":1.5},
        {"name":"large","type":"Integer","value":9007199254740993_u64},
        {"name":"when","type":"DateTime","value":"2024-02-29T23:59:59Z"},
        {"name":"id","type":"Guid","value":"5171B288-8487-4D1E-82B3-693EDFA14AEE"}
    ],"executionData":{"compute":"Spark","computeConfiguration":{
        "highConcurrencyModeOptions":{"enabled":true,"sessionTag":"fixture-tag"}
    }}});
    let validated = request(original.clone()).unwrap();
    assert_eq!(validated.body(), Some(&original));
    assert_eq!(validated.compute(), Some(NotebookCompute::Spark));
    assert!(!format!("{validated:?}").contains("fixture-tag"));
    assert!(!format!("{validated:?}").contains("2026-11-05"));
}

#[test]
fn duplicate_names_are_rejected_case_insensitively_at_exact_length_boundary() {
    assert_eq!(
        request(json!({"parameters":[
            {"name":"Input","type":"Text","value":"one"},
            {"name":"INPUT","type":"Text","value":"two"}
        ]})),
        Err(NotebookInputError::ParameterName)
    );
    for (length, valid) in [(255, true), (256, true), (257, false)] {
        let result = request(json!({"parameters":[{
            "name":"x".repeat(length),"type":"Text","value":"secret"
        }]}));
        assert_eq!(result.is_ok(), valid);
        if let Err(error) = result {
            assert_eq!(error, NotebookInputError::ParameterName);
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
    }
}

#[test]
fn character_limit_is_not_a_utf8_byte_limit_and_unicode_case_is_checked() {
    let name = "\u{e9}".repeat(256);
    let body = json!({"parameters":[{"name":name,"type":"Text","value":""}]});
    assert_eq!(request(body.clone()).unwrap().body(), Some(&body));
    assert_eq!(
        request(json!({"parameters":[
            {"name":"\u{c9}","type":"Text","value":"one"},
            {"name":"\u{e9}","type":"Text","value":"two"}
        ]})),
        Err(NotebookInputError::ParameterName)
    );
}

#[test]
fn native_body_serialization_preserves_order_numbers_and_explicit_empty_arrays() {
    let original = r#"{"parameters":[{"name":"n","type":"Number","value":1.0},{"name":"large","type":"Integer","value":9007199254740993}],"executionData":{"compute":"Spark","computeConfiguration":{"files":[],"sparkProperties":[]}}}"#;
    let validated = request(serde_json::from_str(original).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_string(validated.body().unwrap()).unwrap(),
        original
    );
    let empty = request(json!({"parameters":[]})).unwrap();
    assert_eq!(empty.body(), Some(&json!({"parameters":[]})));
}

#[test]
fn parameters_never_coerce_guess_or_round_and_unavailable_types_are_explicit() {
    for (parameter_type, value) in [
        ("Integer", json!(1.0)),
        ("Integer", json!("1")),
        ("Integer", json!(false)),
        ("Number", json!("1.5")),
        ("Number", Value::Null),
        ("Boolean", json!(0)),
        ("Boolean", json!("false")),
        ("Text", json!(0)),
        ("Text", Value::Null),
        ("Guid", json!("{5171b288-8487-4d1e-82b3-693edfa14aee}")),
        ("Guid", json!("5171b28884874d1e82b3693edfa14aee")),
        ("Guid", json!("z171b288-8487-4d1e-82b3-693edfa14aee")),
    ] {
        assert_eq!(
            request(parameter(parameter_type, value)),
            Err(NotebookInputError::ParameterValue)
        );
    }
    for parameter_type in ["Automatic", "VariableReference", "Future", "int", "text"] {
        assert_eq!(
            request(parameter(parameter_type, json!("value"))),
            Err(NotebookInputError::UnsupportedParameterType)
        );
    }
    for value in [json!(0), json!(-1), json!(1.5), json!(u64::MAX)] {
        let validated = request(parameter("Number", value.clone())).unwrap();
        assert_eq!(validated.body(), Some(&parameter("Number", value)));
    }
}

#[test]
fn datetime_requires_documented_utc_format_and_real_calendar_date() {
    for value in [
        "2026-11-05T12:34:56Z",
        "2024-02-29T00:00:00Z",
        "2000-02-29T00:00:00Z",
        "0001-01-01T00:00:00Z",
    ] {
        request(parameter("DateTime", json!(value))).unwrap();
    }
    for value in [
        "1900-02-29T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "0000-01-01T00:00:00Z",
        "2026-00-01T00:00:00Z",
        "2026-01-00T00:00:00Z",
        "2026-11-05T24:00:00Z",
        "2026-11-05T00:60:00Z",
        "2026-11-05T00:00:60Z",
        "2026-11-05T00:00:00+00:00",
        "2026-11-05T00:00:00.000Z",
        "2026-11-05t00:00:00z",
        "2026-11-05",
        "2026-11-05T00:00:00Z/secret",
    ] {
        assert_eq!(
            request(parameter("DateTime", json!(value))),
            Err(NotebookInputError::ParameterValue)
        );
    }
}

#[test]
fn legacy_maps_null_and_unknown_fields_do_not_enter_native_body() {
    for value in [
        json!({"parameters":{"batch":{"type":"int","value":500}}}),
        json!({"parameters":null}),
        json!({"parameters":[{"name":"x","type":"Text"}]}),
        json!({"parameters":[{"type":"Text","value":"x"}]}),
        json!({"parameters":[{"name":"x","type":null,"value":"x"}]}),
        json!({"executionData":null}),
        json!({"executionData":{}}),
    ] {
        assert!(request(value).is_err());
    }
    for value in [
        json!({"configuration":{}}),
        json!({"cellId":"cell"}),
        json!({"executionData":{"compute":"Spark","parameters":{}}}),
        json!({"parameters":[{"name":"x","type":"Text","value":"x","future":true}]}),
    ] {
        assert_eq!(request(value), Err(NotebookInputError::UnsupportedField));
    }
}

#[test]
fn compute_configurations_are_native_and_never_conflated() {
    for (compute, expected) in [
        ("Spark", NotebookCompute::Spark),
        ("Jupyter", NotebookCompute::Jupyter),
        ("DataWarehouse", NotebookCompute::DataWarehouse),
    ] {
        let body = json!({"executionData":{"compute":compute}});
        let validated = request(body.clone()).unwrap();
        assert_eq!(validated.compute(), Some(expected));
        assert_eq!(validated.body(), Some(&body));
    }
    for body in [
        json!({"executionData":{"compute":"DataWarehouse","computeConfiguration":{}}}),
        json!({"executionData":{"compute":"DataWarehouse","computeConfiguration":null}}),
        json!({"executionData":{"compute":"Jupyter","computeConfiguration":{"driverCores":4}}}),
        json!({"executionData":{"compute":"Jupyter","computeConfiguration":{"highConcurrencyModeOptions":{"enabled":true}}}}),
        json!({"executionData":{"compute":"Spark","computeConfiguration":{"numCores":4}}}),
    ] {
        assert_eq!(request(body), Err(NotebookInputError::UnsupportedField));
    }
    for compute in ["spark", "Future", "Livy"] {
        assert_eq!(
            request(json!({"executionData":{"compute":compute}})),
            Err(NotebookInputError::UnsupportedCompute)
        );
    }
}

#[test]
fn resource_enums_and_int32_executor_bounds_are_exact() {
    for (compute, key, valid) in [
        ("Spark", "driverCores", &[4, 8, 16, 32, 64][..]),
        ("Spark", "executorCores", &[4, 8, 16, 32, 64][..]),
        ("Jupyter", "numCores", &[2, 4, 8, 16, 32, 64][..]),
    ] {
        for cores in valid {
            request(
                json!({"executionData":{"compute":compute,"computeConfiguration":{key:cores}}}),
            )
            .unwrap();
        }
        for cores in [
            json!(0),
            json!(3),
            json!(128),
            json!(4.0),
            json!("4"),
            json!(null),
        ] {
            assert_eq!(
                request(
                    json!({"executionData":{"compute":compute,"computeConfiguration":{key:cores}}})
                ),
                Err(NotebookInputError::ComputeResource)
            );
        }
    }
    for key in ["driverMemory", "executorMemory"] {
        for memory in ["28g", "56g", "112g", "224g", "400g"] {
            request(
                json!({"executionData":{"compute":"Spark","computeConfiguration":{key:memory}}}),
            )
            .unwrap();
        }
        for memory in [json!("28G"), json!(28), json!("1g"), Value::Null] {
            assert_eq!(
                request(
                    json!({"executionData":{"compute":"Spark","computeConfiguration":{key:memory}}})
                ),
                Err(NotebookInputError::ComputeResource)
            );
        }
    }
    for count in [
        json!(0),
        json!(-1),
        json!(1.0),
        json!(2147483648_u64),
        Value::Null,
    ] {
        assert_eq!(
            request(
                json!({"executionData":{"compute":"Spark","computeConfiguration":{"numExecutors":count}}})
            ),
            Err(NotebookInputError::ComputeResource)
        );
    }
    for count in [1, i32::MAX] {
        request(json!({"executionData":{"compute":"Spark","computeConfiguration":{"numExecutors":count}}})).unwrap();
    }
}

#[test]
fn documented_spark_and_jupyter_nested_configurations_are_retained() {
    let reference = json!({
        "referenceType":"ById",
        "itemId":"2434b3e1-d753-4438-8e72-00cb6703e83a",
        "workspaceId":"d9438604-fdf3-472d-93d8-fcb832a1d2b6"
    });
    let common = json!({
        "name":"mySessionName",
        "mountPoints":[{"source":"abfss://filesystem@account.dfs.core.windows.net/path","mountPointPath":"/myMountPoint"}],
        "defaultLakehouse":reference,
        "attachedEnvironment":reference
    });
    for compute in ["Spark", "Jupyter"] {
        let mut configuration = common.clone();
        if compute == "Spark" {
            configuration["instancePool"] = json!({"name":"poolName","type":"Workspace"});
            configuration["sparkProperties"] = json!([{"key":"spark.key","value":"value"}]);
            configuration["pyFiles"] =
                json!(["abfss://filesystem@account.dfs.core.windows.net/my.py"]);
            configuration["archives"] =
                json!(["abfss://filesystem@account.dfs.core.windows.net/my.tar.gz#directory"]);
            configuration["jars"] =
                json!(["abfss://filesystem@account.dfs.core.windows.net/my.jar"]);
            configuration["files"] =
                json!(["abfs://filesystem@account.dfs.core.windows.net/my.txt"]);
        } else {
            configuration["numCores"] = json!(4);
        }
        let body =
            json!({"executionData":{"compute":compute,"computeConfiguration":configuration}});
        assert_eq!(request(body.clone()).unwrap().body(), Some(&body));
    }
}

#[test]
fn nested_configuration_failures_do_not_silently_drop_fields() {
    for configuration in [
        json!({"defaultLakehouse":{"referenceType":"ByVariable","itemId":"name"}}),
        json!({"defaultLakehouse":{"referenceType":"ById","itemId":"../other","workspaceId":"other"}}),
        json!({"attachedEnvironment":null}),
        json!({"instancePool":{"maxClustersToHydrateLimit":10}}),
        json!({"instancePool":{"type":"Future"}}),
        json!({"instancePool":{"id":"not-guid"}}),
        json!({"highConcurrencyModeOptions":{"enabled":"true"}}),
        json!({"highConcurrencyModeOptions":{"sessionTag":1}}),
        json!({"sparkProperties":[{"key":"spark.key","value":1}]}),
        json!({"mountPoints":[{"source":"https://foreign.invalid/file","mountPointPath":"/mount"}]}),
        json!({"pyFiles":["abfss://filesystem@account.dfs.core.windows.net/file.txt"]}),
        json!({"archives":["abfss://filesystem@account.dfs.core.windows.net/file.exe"]}),
        json!({"files":["https://foreign.invalid/file"]}),
        json!({"files":["abfss://filesystem@account.dfs.core.windows.net/file?token=secret"]}),
    ] {
        let result = request(
            json!({"executionData":{"compute":"Spark","computeConfiguration":configuration}}),
        );
        assert!(result.is_err());
    }
}
