// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded numeric source behavior at the actual root argv/shared HTTP boundary.

#[allow(dead_code)]
mod job_execution_support;

use job_execution_support::{metadata, observe};
use serde_json::{Value, json};

#[test]
fn finite_numeric_spellings_match_actual_python_without_float_integer_coercion() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/numeric_source.json"
    ))
    .unwrap();
    assert_eq!(corpus["rows"].as_array().unwrap().len(), 11);
    for row in corpus["rows"].as_array().unwrap() {
        let case = &row["input"];
        let resource = case["resource"].as_str().unwrap_or("Notebook");
        let mut command = vec![
            "job".into(),
            case["leaf"].as_str().unwrap().into(),
            format!("Demo.Workspace/Book.{resource}"),
        ];
        command.extend(
            case["options"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned()),
        );
        command.push("--output_format=json".into());
        let mut responses = metadata(resource);
        responses.extend(case["responses"].as_array().unwrap().clone());
        let input = json!({
            "command": command, "responses": responses,
            "observe_confirmation": row["observed"].get("confirmations").is_some(),
        });
        assert_eq!(observe(&input), row["observed"], "{}", case["name"]);
    }
}

#[test]
fn numeric_lexical_compatibility_does_not_remove_json_value_domain_guards() {
    for parameter in [
        "n:int=18446744073709551616",
        "n:int=-9223372036854775809",
        "n:float=NaN",
        "n:float=Infinity",
        "n:float=-Infinity",
        "n:int=１８４４６７４４０７３７０９５５１６１６",
    ] {
        let row = observe(&json!({
            "command":["job","start","Demo.Workspace/Book.Notebook","--params",parameter],
            "responses":metadata("Notebook"),
        }));
        assert_eq!(row["exit"], 1, "{parameter}");
        assert_eq!(row["requests"].as_array().unwrap().len(), 2, "{parameter}");
        assert!(!row["stdout"].as_str().unwrap().contains("completed"));
    }
}
