// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public contracts, with expectations from the Python search wrapper.

#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_cli_query::{
    QueryErrorKind, SearchOptions, project, search, search_with_options, validate,
};
use serde_json::{Value, json};

#[test]
fn scalar_results_preserve_python_strings_including_falsey_values() {
    for (data, expression, expected) in [
        (json!({"v": false}), "v", json!("False")),
        (json!({"v": true}), "v", json!("True")),
        (json!({"v": 0}), "v", json!("0")),
        (json!({"v": 0.0}), "v", json!("0.0")),
        (json!({"v": -0.0}), "v", json!("-0.0")),
        (json!({"v": ""}), "v", json!("")),
        (json!({"v": null}), "v", json!("None")),
        (json!({}), "missing", json!("None")),
        (json!({"v": 1e-7}), "v", json!("1e-07")),
        (json!({"v": 1e16}), "v", json!("1e+16")),
        (json!({"v": 0.0001}), "v", json!("0.0001")),
        (
            json!({"v": 9007199254740993_u64}),
            "v",
            json!("9007199254740993"),
        ),
    ] {
        assert_eq!(search(&data, expression).unwrap(), expected);
    }
}

#[test]
fn absent_query_and_dot_identity_do_not_stringify_scalars() {
    for data in [
        json!(false),
        json!(0),
        json!(""),
        Value::Null,
        json!([]),
        json!({}),
    ] {
        assert_eq!(project(&data, None).unwrap(), data);
        assert_eq!(search(&data, ".").unwrap(), data);
        assert_eq!(
            search(&data, "@").unwrap(),
            match &data {
                Value::Null => json!("None"),
                Value::Bool(false) => json!("False"),
                Value::Number(_) => json!("0"),
                _ => data.clone(),
            }
        );
    }
    assert_eq!(search(&json!({"rows": []}), "rows").unwrap(), json!([]));
    assert_eq!(search(&json!({"row": {}}), "row").unwrap(), json!({}));
}

#[test]
fn filters_projections_pipes_and_expression_references_are_real_queries() {
    let data = json!({
        "rows": [
            {"id": "b", "n": 2},
            {"id": "a", "n": 1},
            {"id": "ignored", "n": 0}
        ]
    });
    let before = data.clone();
    assert_eq!(
        search(
            &data,
            "rows[?n > `0`] | sort_by(@, &n)[].{key: id, count: n}"
        )
        .unwrap(),
        json!([{"key": "a", "count": 1}, {"key": "b", "count": 2}])
    );
    assert_eq!(
        search(&data, "map(&id, rows)").unwrap(),
        json!(["b", "a", "ignored"])
    );
    assert_eq!(search(&data, "length(rows)").unwrap(), json!("3"));
    assert_eq!(data, before);
}

#[test]
fn python_function_adapters_preserve_numeric_unicode_and_container_equality() {
    for (data, expression, expected) in [
        (json!([1, 2, 3]), "sum(@)", json!("6")),
        (json!([]), "avg(@)", json!("None")),
        (json!([1, 2]), "avg(@)", json!("1.5")),
        (json!("  +1_000 "), "to_number(@)", json!("1000")),
        (json!("1__0"), "to_number(@)", json!("None")),
        (json!(false), "to_number(@)", json!("None")),
        (json!(true), "@ == `1`", json!("False")),
        (json!([true]), "@ == `[1]`", json!("True")),
        (json!([true]), "contains(@, `1`)", json!("True")),
        (
            json!({"a": 1.0, "b": "é😀"}),
            "to_string(@)",
            json!(r#"{"a":1.0,"b":"\u00e9\ud83d\ude00"}"#),
        ),
        (json!([1, 2, 3]), "[::-1]", json!([3, 2, 1])),
    ] {
        assert_eq!(search(&data, expression).unwrap(), expected, "{expression}");
    }
    let ordered: Value = serde_json::from_str(r#"{"z":1,"a":2}"#).unwrap();
    assert_eq!(search(&ordered, "keys(@)").unwrap(), json!(["z", "a"]));
}

#[test]
fn explicit_empty_query_preserves_python_path_inventory() {
    let data = json!({"a": {"b": {"c": {"d": {"e": {"f": 1}}}}}, "empty": []});
    assert_eq!(search(&data, "").unwrap(), json!(["a.b.c.d.e", "empty"]));
    assert_eq!(
        search_with_options(
            &data,
            "",
            SearchOptions {
                deep_traversal: true
            }
        )
        .unwrap(),
        json!(["a.b.c.d.e.f", "empty"])
    );
    assert_eq!(search(&json!({}), "").unwrap(), json!([]));
    assert_eq!(search(&json!([]), "").unwrap(), json!([""]));
    assert_eq!(search(&json!(false), "").unwrap(), json!([""]));
}

#[test]
fn error_kinds_are_typed_and_do_not_disclose_query_or_input() {
    let data = json!(["not for diagnostics"]);
    for (expression, kind) in [
        ("[?", QueryErrorKind::Syntax),
        ("[::0]", QueryErrorKind::Evaluation),
        ("map(&[::0], rows)", QueryErrorKind::Evaluation),
        ("`true` || rows[::0]", QueryErrorKind::Evaluation),
        ("length(`1`)", QueryErrorKind::InvalidType),
        ("length()", QueryErrorKind::InvalidArity),
        ("not_a_function(@)", QueryErrorKind::UnknownFunction),
        ("__fab_eq(`1`, `1`)", QueryErrorKind::UnknownFunction),
        ("[::0]", QueryErrorKind::Evaluation),
    ] {
        let error = search(&data, expression).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.code(), "InvalidInput");
        assert_eq!(
            error.message(),
            "Invalid jmespath query (https://jmespath.org)"
        );
        assert_eq!(error.message(), error.to_string());
        assert!(!format!("{error:?}").contains(expression));
        assert!(!format!("{error:?}").contains("not for diagnostics"));
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
            QueryErrorKind::Unsupported,
            "{expression}"
        );
    }
}

#[test]
fn syntax_preflight_is_offline_and_accepts_wrapper_operations() {
    for expression in ["", ".", "rows[?n > `0`].id", "sort_by(rows, &n)"] {
        validate(expression).unwrap();
    }
    for (expression, kind) in [
        ("[?", QueryErrorKind::Syntax),
        ("&id", QueryErrorKind::Unsupported),
        ("not_a_function(@)", QueryErrorKind::UnknownFunction),
        ("length()", QueryErrorKind::InvalidArity),
        ("length(@, @)", QueryErrorKind::InvalidArity),
        ("merge()", QueryErrorKind::InvalidArity),
        ("not_null()", QueryErrorKind::InvalidArity),
        (
            "map(&not_a_function(@), rows)",
            QueryErrorKind::UnknownFunction,
        ),
        ("sort_by(rows, &length())", QueryErrorKind::InvalidArity),
        ("`true` || length()", QueryErrorKind::InvalidArity),
        ("[?not_a_function(@)]", QueryErrorKind::UnknownFunction),
        ("`1e400`", QueryErrorKind::Unsupported),
        ("`[1e400]`", QueryErrorKind::Unsupported),
        ("`18446744073709551617`", QueryErrorKind::Unsupported),
        ("__fab_eq(`1`, `1`)", QueryErrorKind::UnknownFunction),
        ("`deprecated unquoted string`", QueryErrorKind::Unsupported),
    ] {
        assert_eq!(validate(expression).unwrap_err().kind(), kind);
    }
    // Preflight has no response data; type checking remains evaluation's job.
    validate("length(`1`)").unwrap();
    assert_eq!(
        search(&json!({}), "length(`1`)").unwrap_err().kind(),
        QueryErrorKind::InvalidType
    );
}

#[test]
fn preflight_accepts_runtime_signatures_including_variadic_functions() {
    for expression in [
        "abs(@)",
        "avg(@)",
        "ceil(@)",
        "contains(@, @)",
        "ends_with(@, @)",
        "floor(@)",
        "join(@, @)",
        "keys(@)",
        "length(@)",
        "map(&id, @)",
        "min(@)",
        "max(@)",
        "max_by(@, &id)",
        "min_by(@, &id)",
        "merge(@)",
        "merge(@, @, @)",
        "not_null(@)",
        "not_null(@, @, @)",
        "reverse(@)",
        "sort(@)",
        "sort_by(@, &id)",
        "starts_with(@, @)",
        "sum(@)",
        "to_array(@)",
        "to_number(@)",
        "to_string(@)",
        "type(@)",
        "values(@)",
    ] {
        validate(expression).unwrap();
    }
}

#[test]
fn arbitrary_width_integer_projections_never_coerce_to_float() {
    for number in [
        "18446744073709551617",
        "-9223372036854775809",
        "170141183460469231731687303715884105728",
    ] {
        let value: Value = serde_json::from_str(number).unwrap();
        let object = json!({"value": value.clone(), "rows": [value.clone()]});
        assert_eq!(search(&object, "value").unwrap(), json!(number));
        assert_eq!(search(&object, "rows[0]").unwrap(), json!(number));
        assert_eq!(
            search(&object, "[value, rows[0]]").unwrap(),
            json!([value.clone(), value.clone()])
        );
        assert_eq!(search(&object, "rows[*]").unwrap(), json!([value.clone()]));
        assert_eq!(
            search(&object, "{n: value}").unwrap(),
            json!({"n": value.clone()})
        );
        assert_eq!(search(&object, "to_string(value)").unwrap(), json!(number));
        assert_eq!(
            search(&object, "to_string(rows)").unwrap(),
            json!(format!("[{number}]"))
        );
        assert_eq!(search(&object, "to_number(value)").unwrap(), json!(number));
        for expression in [
            "abs(value)",
            "ceil(value)",
            "floor(value)",
            "sum(rows)",
            "avg(rows)",
            "sort(rows)",
            "value == value",
            "value > `0`",
        ] {
            assert_eq!(
                search(&object, expression).unwrap_err().kind(),
                QueryErrorKind::Unsupported
            );
        }
    }
}

#[test]
fn arbitrary_precision_float_lexemes_still_follow_python_binary64_strings() {
    let data: Value = serde_json::from_str(r#"{"n":1.00000000000000000001}"#).unwrap();
    assert_eq!(search(&data, "n").unwrap(), json!("1.0"));
    assert_eq!(search(&data, "to_string(n)").unwrap(), json!("1.0"));
}

#[test]
fn genuine_private_number_keys_in_json_literals_are_never_numeric_markers() {
    for raw in [
        r#"{"$serde_json::private::Number":"123"}"#,
        r#"{"$serde_json::private::Number":"not a number"}"#,
        r#"{"$serde_json::private::Number":"18446744073709551617"}"#,
        r#"{"$serde_json::private::Number":123}"#,
        r#"{"$serde_json::private::Number":null}"#,
        r#"{"\u0024serde_json::private::Number":"123"}"#,
        r#"{"before":1,"nested":{"$serde_json::private::Number":"123"},"after":2}"#,
        r#"[{"$serde_json::private::Number":"123"},{"$serde_json::private::Number":false}]"#,
    ] {
        let expected = fabric_core_json::decode(raw.as_bytes()).unwrap();
        let expression = format!("`{raw}`");
        validate(&expression).unwrap();
        assert_eq!(search(&json!({}), &expression).unwrap(), expected);
        assert_eq!(
            search(&json!({}), &format!("`{raw}` == `{raw}`")).unwrap(),
            json!("True")
        );
        assert_eq!(
            search(&json!({}), &format!("to_string(`{raw}`)")).unwrap(),
            json!(expected.to_string())
        );
    }
}

#[test]
fn literal_masking_preserves_utf8_escaped_backticks_and_expression_references() {
    let expected = json!({"$serde_json::private::Number": "123", "unicode": "é`"});
    let literal = r#"`{"$serde_json::private::Number":"123","unicode":"é\`"}`"#;
    let expression = format!("[{literal}, `1`, 'é', {literal}]");
    validate(&expression).unwrap();
    assert_eq!(
        search(&json!({}), &expression).unwrap(),
        json!([expected.clone(), 1, "é", expected.clone()])
    );
    assert_eq!(
        search(&json!([{}, {}]), &format!("map(&{literal}, @)")).unwrap(),
        json!([expected.clone(), expected.clone()])
    );
    let data = json!({"literal": expected.clone()});
    assert_eq!(
        search(&data, &format!("literal == {literal}")).unwrap(),
        json!("True")
    );
    assert_eq!(search(&data, "literal").unwrap(), expected);
}

#[test]
fn intermediate_limits_apply_even_when_the_final_result_would_be_tiny() {
    // This is a safe admission-only rejection, not the 24-stage OOM example.
    // Native evaluation is never entered for a denied query.
    let stages = std::iter::repeat_n("[@,@]", 14)
        .collect::<Vec<_>>()
        .join(" | ");
    let data = json!("x".repeat(1024));
    for expression in [
        stages.clone(),
        format!("{stages} | length(@)"),
        format!("{stages} | `[0]`"),
    ] {
        assert_eq!(
            search(&data, &expression).unwrap_err().kind(),
            QueryErrorKind::ResourceLimit
        );
    }
    assert_eq!(search(&data, "[@,@] | length(@)").unwrap(), json!("2"));
    assert_eq!(
        validate(&" ".repeat(64 * 1024 + 1)).unwrap_err().kind(),
        QueryErrorKind::ResourceLimit
    );
}

#[test]
fn expression_nesting_is_a_typed_error_not_a_panic() {
    let expression = std::iter::repeat_n("a", 150).collect::<Vec<_>>().join(".");
    assert_eq!(
        search(&json!({}), &expression).unwrap_err().kind(),
        QueryErrorKind::ResourceLimit
    );
}

#[test]
fn structural_depth_limit_is_rejected_by_offline_preflight() {
    // The pinned parser accepts 128 links, but those links form 129 AST levels.
    let expression = vec!["a"; 129].join(".");
    assert_eq!(
        validate(&expression).unwrap_err().kind(),
        QueryErrorKind::ResourceLimit
    );
    assert_eq!(
        search(&json!({}), &expression).unwrap_err().kind(),
        QueryErrorKind::ResourceLimit
    );
    validate(&vec!["a"; 128].join(".")).unwrap();
}

#[test]
fn selecting_small_fields_does_not_multiply_unrelated_row_payloads() {
    let mut rows = vec![json!({"id": "a"}); 100];
    rows[0] = json!({"id": "a", "unused": "x".repeat(32_768)});
    let data = json!({"rows": rows});
    for expression in ["rows[*].id", "rows[].id", "map(&id, rows)"] {
        assert_eq!(
            search(&data, expression).unwrap(),
            json!(vec!["a"; 100]),
            "{expression}"
        );
    }
}

#[test]
fn scalar_length_does_not_reserve_an_additional_full_array_copy() {
    let data = json!({"rows": vec![0; 100_000]});
    assert_eq!(search(&data, "length(rows)").unwrap(), json!("100000"));
}
