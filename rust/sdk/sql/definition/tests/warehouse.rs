// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse fidelity and shared admission without changing SQLDatabase behavior.

#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, SqlDatabaseDefinitionFormat,
    parse_sql_database_definition_response,
    warehouse::{
        NativeMediaType, WarehouseDefinitionFormat as Format,
        parse_warehouse_definition_response as parse,
    },
};
use serde_json::{Value, json};
use support::{limits, nz, wire_part};

fn body(parts: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"definition":{"parts":parts}})).unwrap()
}

#[test]
fn warehouse_default_is_sqlproj_without_changing_sql_database_dacpac_default() {
    assert_eq!(Format::default(), Format::Sqlproj);
    assert_eq!(
        SqlDatabaseDefinitionFormat::default(),
        SqlDatabaseDefinitionFormat::Dacpac
    );
    for (name, format) in [("sqlproj", Format::Sqlproj), ("dacpac", Format::Dacpac)] {
        assert_eq!(Format::try_from(name).unwrap(), format);
        assert_eq!(format.as_str(), name);
    }
    for invalid in ["", "default", ".sqlproj", "SQLPROJ", "DACPAC", "future"] {
        assert_eq!(
            Format::try_from(invalid).unwrap_err().kind,
            Kind::UnsupportedFormat
        );
    }
    let response = body(json!([wire_part("sqldw.sqlproj", b"x")]));
    let definition = parse(&response, Format::default(), limits()).unwrap();
    assert_eq!(definition.declared_format(), None);
    assert_eq!(definition.format(), Format::Sqlproj);
    support::assert_kind(
        parse_sql_database_definition_response(
            &response,
            SqlDatabaseDefinitionFormat::default(),
            limits(),
        ),
        Kind::UnsupportedPart,
    );
    let legacy = fabric_sdk_sql_definition::sql_database_definition_from_parts(
        SqlDatabaseDefinitionFormat::default(),
        vec![support::part("legacy.dacpac", b"x")],
        limits(),
    )
    .unwrap();
    assert_eq!(legacy.format(), SqlDatabaseDefinitionFormat::Dacpac);
}

#[test]
fn complete_envelope_native_bytes_media_and_unknown_fields_are_immutable_fidelity() {
    let mut project = wire_part("source/sqldw.sqlproj", b"\xef\xbb\xbf<Project/>\r\n");
    project["mediaType"] = json!("application/x-future-project; version=3");
    project["future"] = json!({"model":9007199254740993_u64,"opaque":[null, true]});
    let mut query = wire_part(".sharedqueries/query.sql", b"\xff\0\r\n");
    query["mediaType"] = Value::Null;
    let mut platform = wire_part(".platform", b"  opaque platform bytes\r\n");
    platform["mediaType"] = json!("");
    let response = serde_json::to_vec_pretty(&json!({
        "futureEnvelope":{"keep":true},
        "definition":{"futureDefinition":[1,2], "parts":[
            project, query, wire_part("dbo/Tables/T.sql", b""), platform
        ]}
    }))
    .unwrap();
    let definition = parse(&response, Format::Sqlproj, limits()).unwrap();
    assert_eq!(definition.response_bytes(), response);
    assert!(definition.has_unknown_fields());
    assert_eq!(definition.parts()[0].path, "source/sqldw.sqlproj");
    assert_eq!(definition.parts()[0].bytes, b"\xef\xbb\xbf<Project/>\r\n");
    assert_eq!(definition.parts()[1].path, ".sharedqueries/query.sql");
    assert_eq!(definition.parts()[1].bytes, b"\xff\0\r\n");
    assert_eq!(definition.parts()[2].bytes, b"");
    assert_eq!(definition.parts()[3].bytes, b"  opaque platform bytes\r\n");
    assert_eq!(
        definition.media_types(),
        [
            NativeMediaType::Value("application/x-future-project; version=3".into()),
            NativeMediaType::Null,
            NativeMediaType::Missing,
            NativeMediaType::Value("".into()),
        ]
    );
    let display = format!(
        "{definition:?} {:?} {:?}",
        definition.parts(),
        definition.media_types()
    );
    for secret in ["source/sqldw", "opaque platform", "future-project"] {
        assert!(!display.contains(secret));
    }
    // The existing strict SQLDatabase profile still rejects these extra fields.
    assert_eq!(
        parse_sql_database_definition_response(
            &response,
            SqlDatabaseDefinitionFormat::Sqlproj,
            limits()
        )
        .unwrap_err()
        .kind,
        Kind::InvalidShape,
    );
}

#[test]
fn binary_dacpac_uses_received_filename_not_conflicting_documentation_examples() {
    let bytes: Vec<_> = (0..=255).collect();
    for path in ["sqldw.dacpac", "definition.dacpac", "native/custom.dacpac"] {
        let response = body(json!([
            wire_part(path, &bytes),
            wire_part(".platform", b"\0\xff")
        ]));
        let native = parse(&response, Format::Dacpac, limits()).unwrap();
        assert_eq!(native.parts()[0].path, path);
        assert_eq!(native.parts()[0].bytes, bytes);
        assert_eq!(native.response_bytes(), response);
    }
}

#[test]
fn format_missing_null_unknown_mismatch_and_known_declaration_remain_distinct() {
    for (format, expected) in [
        (Value::Null, Kind::InvalidShape),
        (json!("future"), Kind::UnsupportedFormat),
        (json!(""), Kind::UnsupportedFormat),
        (json!("dacpac"), Kind::FormatMismatch),
    ] {
        let response = serde_json::to_vec(&json!({"definition":{
            "format":format,"parts":[wire_part("x.sqlproj", b"x")]
        }}))
        .unwrap();
        assert_eq!(
            parse(&response, Format::Sqlproj, limits())
                .unwrap_err()
                .kind,
            expected
        );
    }
    let response = serde_json::to_vec(&json!({"definition":{
        "format":"sqlproj","parts":[wire_part("x.sqlproj", b"x")]
    }}))
    .unwrap();
    assert_eq!(
        parse(&response, Format::Sqlproj, limits())
            .unwrap()
            .declared_format(),
        Some(Format::Sqlproj)
    );
}

#[test]
fn duplicate_structural_keys_nulls_unknown_payloads_and_missing_content_do_not_succeed() {
    for response in [
        r#"{"definition":{"parts":[],"parts":[]}}"#,
        r#"{"definition":{"format":"sqlproj","format":"sqlproj","parts":[]}}"#,
        r#"{"definition":{},"definition":{}}"#,
        r#"{"future":1,"future":2,"definition":{}}"#,
        r#"{"definition":{"future":1,"future":2,"parts":[]}}"#,
        r#"{"definition":{"parts":[{"path":"x.sqlproj","path":"y.sqlproj","payload":"eA==","payloadType":"InlineBase64"}]}}"#,
        r#"{"definition":{"parts":[{"path":"x.sqlproj","payload":"eA==","payloadType":"InlineBase64","mediaType":null,"mediaType":null}]}}"#,
        r#"{"definition":{"parts":[{"path":"x.sqlproj","payload":"eA==","payloadType":"InlineBase64","mediaType":1}]}}"#,
        r#"{"definition":null}"#,
        r#"{"definition":{"parts":null}}"#,
        r#"{"definition":{"parts":[null]}}"#,
    ] {
        assert_eq!(
            parse(response.as_bytes(), Format::Sqlproj, limits())
                .unwrap_err()
                .kind,
            Kind::InvalidShape,
            "{response}"
        );
    }
    for response in [
        b"{}".as_slice(),
        b"{\"status\":\"Running\"}",
        b"{\"errorCode\":\"Denied\"}",
    ] {
        assert_eq!(
            parse(response, Format::Sqlproj, limits()).unwrap_err().kind,
            Kind::MissingDefinition
        );
    }
    let mut part = wire_part("x.sqlproj", b"x");
    part["payloadType"] = json!("FutureEncoding");
    assert_eq!(
        parse(&body(json!([part])), Format::Sqlproj, limits())
            .unwrap_err()
            .kind,
        Kind::UnsupportedPayloadType
    );
    for response in [b"{".as_slice(), b"{} trailing", b"{\"future\":}"] {
        assert_eq!(
            parse(response, Format::Sqlproj, limits()).unwrap_err().kind,
            Kind::InvalidJson
        );
    }
}

#[test]
fn shared_primary_base64_and_portable_path_admission_are_not_bypassed() {
    let reject = |parts, kind| {
        assert_eq!(
            parse(&body(parts), Format::Sqlproj, limits())
                .unwrap_err()
                .kind,
            kind
        );
    };
    reject(json!([]), Kind::MissingPrimaryPart);
    reject(
        json!([wire_part("x.sqlproj", b"x"), wire_part("y.sqlproj", b"y")]),
        Kind::MultiplePrimaryParts,
    );
    reject(json!([wire_part("x.sqlproj", b"")]), Kind::InvalidShape);
    reject(
        json!([wire_part("x.sqlproj", b"x"), wire_part("x.dacpac", b"y")]),
        Kind::UnsupportedPart,
    );
    for path in [
        "/x.sql",
        "../x.sql",
        "a/../x.sql",
        "a\\x.sql",
        "C:x.sql",
        "CON.sql",
        "a./x.sql",
    ] {
        reject(
            json!([wire_part("x.sqlproj", b"x"), wire_part(path, b"x")]),
            Kind::UnsafePath,
        );
    }
    for (first, second, kind) in [
        ("a.sql", "a.sql", Kind::DuplicatePath),
        ("a.sql", "A.sql", Kind::CaseCollision),
        ("A/a.sql", "a/b.sql", Kind::CaseCollision),
        ("a.sql", "a.sql/b.sql", Kind::FileDirectoryCollision),
    ] {
        reject(
            json!([
                wire_part("x.sqlproj", b"x"),
                wire_part(first, b"x"),
                wire_part(second, b"x")
            ]),
            kind,
        );
    }
    for payload in ["not base64", "eA", "eB==", "_A==", "eA==\n"] {
        let mut part = wire_part("x.sqlproj", b"x");
        part["payload"] = json!(payload);
        reject(json!([part]), Kind::InvalidBase64);
    }
}

#[test]
fn all_six_explicit_limits_apply_inclusively_to_the_complete_native_definition() {
    let response = body(json!([
        wire_part("a/x.sqlproj", b"abc"),
        wire_part("q.sql", b"de")
    ]));
    let exact = fabric_sdk_sql_definition::DefinitionLimits {
        max_wire_bytes: nz(response.len()),
        max_decoded_bytes: nz(5),
        max_part_bytes: nz(3),
        max_parts: nz(2),
        max_path_bytes: nz("a/x.sqlproj".len()),
        max_depth: nz(2),
    };
    parse(&response, Format::Sqlproj, exact).unwrap();
    for (budget, limit) in [
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_wire_bytes: nz(response.len() - 1),
                ..exact
            },
            Limit::WireBytes,
        ),
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_decoded_bytes: nz(4),
                ..exact
            },
            Limit::DecodedBytes,
        ),
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_part_bytes: nz(2),
                ..exact
            },
            Limit::PartBytes,
        ),
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_parts: nz(1),
                ..exact
            },
            Limit::PartCount,
        ),
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_path_bytes: nz("a/x.sqlproj".len() - 1),
                ..exact
            },
            Limit::PathBytes,
        ),
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_depth: nz(1),
                ..exact
            },
            Limit::Depth,
        ),
    ] {
        assert_eq!(
            parse(&response, Format::Sqlproj, budget).unwrap_err().kind,
            Kind::Limit(limit)
        );
    }
}
