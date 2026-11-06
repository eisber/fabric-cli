// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit inclusive public wire, native-byte and path limits.

#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, DefinitionLimits,
    SqlDatabaseDefinitionFormat as Format, parse_sql_database_definition as parse,
    parse_sql_database_definition_response as response,
    sql_database_definition_from_parts as from_parts,
};
use serde_json::json;
use support::{assert_kind, limits, nz, part, wire_part};

#[test]
fn exact_wire_limit_admits_and_one_less_rejects_input_and_construction() {
    let definition = from_parts(Format::Dacpac, vec![part("a.dacpac", b"abc")], limits()).unwrap();
    let body = serde_json::to_vec(&definition).unwrap();
    let mut budget = limits();
    budget.max_wire_bytes = nz(body.len());
    parse(&body, budget).unwrap();
    from_parts(Format::Dacpac, vec![part("a.dacpac", b"abc")], budget).unwrap();
    budget.max_wire_bytes = nz(body.len() - 1);
    assert_kind(parse(&body, budget), Kind::Limit(Limit::WireBytes));
    assert_kind(
        from_parts(Format::Dacpac, vec![part("a.dacpac", b"abc")], budget),
        Kind::Limit(Limit::WireBytes),
    );
    budget.max_wire_bytes = nz(1);
    assert_kind(parse(b"bad json", budget), Kind::Limit(Limit::WireBytes));
}

#[test]
fn explicit_format_serialization_is_counted_even_if_input_omits_it() {
    let body = serde_json::to_vec(&json!({"parts": [wire_part("a.dacpac", b"x")]})).unwrap();
    let mut budget = limits();
    budget.max_wire_bytes = nz(body.len());
    assert_kind(parse(&body, budget), Kind::Limit(Limit::WireBytes));
}

#[test]
fn response_wire_limit_includes_the_response_wrapper() {
    let body = serde_json::to_vec(&json!({"definition": {
        "format": "dacpac", "parts": [wire_part("a.dacpac", b"x")]
    }}))
    .unwrap();
    let mut budget = limits();
    budget.max_wire_bytes = nz(body.len());
    response(&body, Format::Dacpac, budget).unwrap();
    budget.max_wire_bytes = nz(body.len() - 1);
    assert_kind(
        response(&body, Format::Dacpac, budget),
        Kind::Limit(Limit::WireBytes),
    );
}

#[test]
fn per_part_decoded_limit_is_exact_including_base64_padding_boundaries() {
    for size in 1..=4 {
        let bytes = vec![b'x'; size];
        let body = serde_json::to_vec(&json!({"parts": [wire_part("a.dacpac", &bytes)]})).unwrap();
        let mut budget = limits();
        budget.max_part_bytes = nz(size);
        parse(&body, budget).unwrap();
        from_parts(Format::Dacpac, vec![part("a.dacpac", &bytes)], budget).unwrap();
        let too_many = vec![b'x'; size + 1];
        assert_kind(
            from_parts(Format::Dacpac, vec![part("a.dacpac", &too_many)], budget),
            Kind::Limit(Limit::PartBytes),
        );
        let oversized =
            serde_json::to_vec(&json!({"parts": [wire_part("a.dacpac", &too_many)]})).unwrap();
        assert_kind(parse(&oversized, budget), Kind::Limit(Limit::PartBytes));
    }
}

#[test]
fn aggregate_decoded_limit_counts_all_parts_not_base64_text() {
    let body = serde_json::to_vec(&json!({"format": "sqlproj", "parts": [
        wire_part("a.sqlproj", b"abc"), wire_part("a.sql", b"de")
    ]}))
    .unwrap();
    let mut budget = limits();
    budget.max_decoded_bytes = nz(5);
    parse(&body, budget).unwrap();
    from_parts(
        Format::Sqlproj,
        vec![part("a.sqlproj", b"abc"), part("a.sql", b"de")],
        budget,
    )
    .unwrap();
    budget.max_decoded_bytes = nz(4);
    let error = parse(&body, budget).unwrap_err();
    assert_eq!(error.kind, Kind::Limit(Limit::DecodedBytes));
    assert_eq!(error.part_index, Some(1));
    assert_kind(
        from_parts(
            Format::Sqlproj,
            vec![part("a.sqlproj", b"abc"), part("a.sql", b"de")],
            budget,
        ),
        Kind::Limit(Limit::DecodedBytes),
    );
}

#[test]
fn part_count_is_checked_before_deserializing_an_excess_part() {
    let mut budget = limits();
    budget.max_parts = nz(2);
    let parts = json!([wire_part("a.sqlproj", b"x"), wire_part("a.sql", b"")]);
    let body = serde_json::to_vec(&json!({"format": "sqlproj", "parts": parts})).unwrap();
    parse(&body, budget).unwrap();
    from_parts(
        Format::Sqlproj,
        vec![part("a.sqlproj", b"x"), part("a.sql", b"")],
        budget,
    )
    .unwrap();
    let body = serde_json::to_vec(&json!({"format": "sqlproj", "parts": [
        wire_part("a.sqlproj", b"x"), wire_part("a.sql", b""), null
    ]}))
    .unwrap();
    let error = parse(&body, budget).unwrap_err();
    assert_eq!(error.kind, Kind::Limit(Limit::PartCount));
    assert_eq!(error.part_index, Some(2));
    assert_kind(
        from_parts(
            Format::Sqlproj,
            vec![
                part("a.sqlproj", b"x"),
                part("a.sql", b""),
                part("b.sql", b""),
            ],
            budget,
        ),
        Kind::Limit(Limit::PartCount),
    );
}

#[test]
fn unicode_path_limit_counts_utf8_bytes_and_depth_is_inclusive() {
    let path = "子/é.dacpac";
    let body = serde_json::to_vec(&json!({"parts": [wire_part(path, b"x")]})).unwrap();
    let mut budget = limits();
    budget.max_path_bytes = nz(path.len());
    budget.max_depth = nz(2);
    parse(&body, budget).unwrap();
    from_parts(Format::Dacpac, vec![part(path, b"x")], budget).unwrap();
    budget.max_path_bytes = nz(path.len() - 1);
    assert_kind(parse(&body, budget), Kind::Limit(Limit::PathBytes));
    assert_kind(
        from_parts(Format::Dacpac, vec![part(path, b"x")], budget),
        Kind::Limit(Limit::PathBytes),
    );
    budget.max_path_bytes = nz(path.len());
    budget.max_depth = nz(1);
    assert_kind(parse(&body, budget), Kind::Limit(Limit::Depth));
    assert_kind(
        from_parts(Format::Dacpac, vec![part(path, b"x")], budget),
        Kind::Limit(Limit::Depth),
    );
}

#[test]
fn usize_maximum_budgets_do_not_wrap_small_content_into_failure() {
    let budget = DefinitionLimits {
        max_wire_bytes: nz(usize::MAX),
        max_decoded_bytes: nz(usize::MAX),
        max_part_bytes: nz(usize::MAX),
        max_parts: nz(usize::MAX),
        max_path_bytes: nz(usize::MAX),
        max_depth: nz(usize::MAX),
    };
    let definition = from_parts(Format::Dacpac, vec![part("a.dacpac", b"x")], budget).unwrap();
    parse(&serde_json::to_vec(&definition).unwrap(), budget).unwrap();
}
