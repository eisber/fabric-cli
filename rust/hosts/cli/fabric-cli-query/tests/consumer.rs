// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public query/output contracts and unchanged workspace-query regression.

use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, OutputStatus, Timestamp};
use fabric_cli_query::{
    QueryErrorKind, SearchOptions, join_query_args, project, search, search_with_options,
};
use serde_json::{Value, json};

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/compatibility/query/cases.json"
    ))
    .unwrap()
}

#[test]
fn frozen_public_search_contract() {
    for case in fixtures()["cases"].as_array().unwrap() {
        let data = case["data"].clone();
        let before = data.to_string();
        let tokens = case["tokens"].as_array().map(|tokens| {
            tokens
                .iter()
                .map(|token| token.as_str().unwrap())
                .collect::<Vec<_>>()
        });
        let joined = tokens.as_ref().map(|tokens| join_query_args(tokens));
        let expression = joined.as_deref().or(case["expression"].as_str());
        let result = match expression {
            None => project(&data, None),
            Some(expression) => search_with_options(
                &data,
                expression,
                SearchOptions {
                    deep_traversal: case["deep"].as_bool().unwrap_or(false),
                },
            ),
        };
        if case["support"] == "unsupported" {
            assert_eq!(
                result.unwrap_err().kind(),
                QueryErrorKind::Unsupported,
                "{}",
                case["name"]
            );
        } else if case.get("error").is_some() {
            let error = result.expect_err(case["name"].as_str().unwrap());
            assert_eq!(error.code(), case["error"].as_str().unwrap());
        } else {
            assert_eq!(
                result.unwrap().to_string(),
                case["expected"].to_string(),
                "{}",
                case["name"]
            );
        }
        assert_eq!(data.to_string(), before, "input mutated: {}", case["name"]);
    }
}

#[test]
fn unchanged_workspace_query_projection_golden() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/workspaces/cases.json"
    ))
    .unwrap();
    let case = fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "query_projection")
        .unwrap();
    let rows = json!([
        {"name":"Alpha.Personal","id":"a","unknown":"retained before query"},
        {"name":"Beta.Workspace","id":"b"},
        {"name":"Ignored.Workspace","id":"skip"}
    ]);
    let projected = search(&rows, case["query"].as_str().unwrap()).unwrap();
    assert_eq!(
        projected.to_string(),
        case["expected"]["output"]["result"]["data"].to_string()
    );
}

#[test]
fn public_output_join_distinguishes_scalar_strings_from_dot_identity() {
    let timestamp = Timestamp::parse("2000-01-01T00:00:00Z").unwrap();
    let data = json!({"v": false});
    let scalar = OutputEnvelope::new(
        timestamp.clone(),
        OutputInput {
            data: Some(search(&data, "v").unwrap()),
            ..OutputInput::default()
        },
    );
    assert_eq!(scalar.result().data().unwrap(), &[json!("False")]);
    let identity = OutputEnvelope::new(
        timestamp.clone(),
        OutputInput {
            data: Some(search(&json!(false), ".").unwrap()),
            ..OutputInput::default()
        },
    );
    assert!(identity.result().data().is_none());
    let unmodified = OutputEnvelope::new(
        timestamp,
        OutputInput {
            data: Some(project(&data, None).unwrap()),
            ..OutputInput::default()
        },
    );
    assert_eq!(unmodified.result().data().unwrap(), &[data]);
}

#[test]
fn query_error_uses_existing_failure_envelope_and_never_empty_success() {
    let error = search(&json!([]), "length(`1`)").unwrap_err();
    assert_eq!(error.kind(), QueryErrorKind::InvalidType);
    let output = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        OutputInput {
            status: OutputStatus::Failure,
            error_code: Some(error.code().to_owned()),
            message: Some(error.message().to_owned()),
            ..OutputInput::default()
        },
    );
    assert_eq!(output.result().error_code(), Some("InvalidInput"));
    assert_eq!(output.render(Indent::Four).unwrap().exit_code, 1);
    assert!(output.result().data().is_none());
}

#[test]
fn error_kinds_are_typed_and_do_not_disclose_query_text() {
    for (expression, kind) in [
        ("[?", QueryErrorKind::Syntax),
        ("length()", QueryErrorKind::InvalidArity),
        ("not_a_function(@)", QueryErrorKind::UnknownFunction),
        ("__fab_eq(`1`, `1`)", QueryErrorKind::UnknownFunction),
        ("[::0]", QueryErrorKind::Evaluation),
    ] {
        let error = search(&json!([1]), expression).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(!format!("{error:?}").contains(expression));
        assert_eq!(error.message(), error.to_string());
    }
}

#[test]
fn known_numeric_and_index_limits_are_explicit_failures() {
    for (data, expression) in [
        (
            json!([9007199254740993_u64, 9007199254740992_u64]),
            "sort(@)",
        ),
        (json!([1e16, 1.0, -1e16]), "sum(@)"),
        (json!("NaN"), "to_number(@)"),
        (json!(1e30), "ceil(@)"),
        (json!([1, 2]), "[-2147483648]"),
        (json!([1, 2]), "[1::2147483647]"),
        (json!({}), "&id"),
    ] {
        assert_eq!(
            search(&data, expression).unwrap_err().kind(),
            QueryErrorKind::Unsupported
        );
    }
}

#[test]
fn expression_nesting_is_a_typed_error_not_a_panic() {
    let expression = std::iter::repeat_n("a", 150).collect::<Vec<_>>().join(".");
    assert_eq!(
        search(&json!({}), &expression).unwrap_err().kind(),
        QueryErrorKind::ResourceLimit
    );
}
