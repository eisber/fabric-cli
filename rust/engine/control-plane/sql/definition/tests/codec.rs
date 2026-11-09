// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public native codec, structural admission and redaction cases.

#![deny(unnameable_test_items)]

mod support;

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, SqlDatabaseDefinitionFormat as Format,
    parse_sql_database_definition as parse, parse_sql_database_definition_response as response,
    sql_database_definition_from_parts as from_parts,
};
use serde_json::{Value, json};
use support::{assert_kind, limits, part, wire_part};

#[test]
fn binary_dacpac_roundtrips_all_bytes_and_retains_native_path() {
    for path in ["sqldb.dacpac", "definition.dacpac", "nested/任意.dacpac"] {
        let bytes: Vec<u8> = (0..=255).collect();
        let definition = from_parts(Format::Dacpac, vec![part(path, &bytes)], limits()).unwrap();
        let encoded = serde_json::to_vec(&definition).unwrap();
        let decoded = parse(&encoded, limits()).unwrap();
        assert_eq!(decoded.format(), Format::Dacpac);
        assert_eq!(decoded.parts()[0].path, path);
        assert_eq!(decoded.parts()[0].bytes, bytes);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    }
}

#[test]
fn json_looking_and_bom_content_is_not_reformatted_or_newline_translated() {
    for bytes in [
        &b"{ \"model\" : [1, 2] }\r\n"[..],
        &b"\xef\xbb\xbfmodel\r\n\n"[..],
        &b"PK\x03\x04\0\xff"[..],
    ] {
        let body = json!({"parts": [wire_part("definition.dacpac", bytes)]});
        let definition = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap();
        assert_eq!(definition.parts()[0].bytes, bytes);
        let again = parse(&serde_json::to_vec(&definition).unwrap(), limits()).unwrap();
        assert_eq!(again.parts()[0].bytes, bytes);
    }
}

#[test]
fn multipart_project_preserves_order_shared_queries_bom_and_non_utf8() {
    let input = vec![
        part(".sharedqueries/query.sql", b"select 1;\r\n"),
        part(
            "source/project.sqlproj",
            b"\xef\xbb\xbf<Project>\r\n</Project>",
        ),
        part("dbo/Tables/z.sql", b"{} \r\n\n"),
        part("dbo/Tables/a.sql", b"\xff\xfe\x00"),
        part("dbo/Empty.sql", b""),
        part(".platform", b"  { \"opaque\": true }\r\n"),
    ];
    let definition = from_parts(Format::Sqlproj, input, limits()).unwrap();
    let before: Vec<_> = definition
        .parts()
        .iter()
        .map(|part| (part.path.clone(), part.bytes.clone()))
        .collect();
    let decoded = parse(&serde_json::to_vec(&definition).unwrap(), limits()).unwrap();
    let after: Vec<_> = decoded
        .parts()
        .iter()
        .map(|part| (part.path.clone(), part.bytes.clone()))
        .collect();
    assert_eq!(after, before);
}

#[test]
fn default_format_is_only_the_native_dacpac_default() {
    assert_eq!(Format::default(), Format::Dacpac);
    assert_eq!(Format::try_from("dacpac").unwrap().as_str(), "dacpac");
    assert_eq!(Format::try_from("sqlproj").unwrap().as_str(), "sqlproj");
    let body = json!({"parts": [wire_part("definition.dacpac", b"x")]});
    let definition = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap();
    assert_eq!(definition.format(), Format::Dacpac);
    assert_eq!(
        serde_json::to_value(definition).unwrap()["format"],
        "dacpac"
    );
    for selector in ["", "default", ".dacpac", "Dacpac", "SQLPROJ", "bak"] {
        assert_eq!(
            Format::try_from(selector).unwrap_err().kind,
            Kind::UnsupportedFormat
        );
    }
}

#[test]
fn response_inherits_requested_project_format_and_serializes_it_explicitly() {
    let body = json!({"definition": {"parts": [wire_part("sqldb.sqlproj", b"<Project/>")]}});
    let definition = response(
        &serde_json::to_vec(&body).unwrap(),
        Format::Sqlproj,
        limits(),
    )
    .unwrap();
    assert_eq!(definition.format(), Format::Sqlproj);
    let bare = serde_json::to_vec(&definition).unwrap();
    assert_eq!(parse(&bare, limits()).unwrap().format(), Format::Sqlproj);
    assert_eq!(
        serde_json::from_slice::<Value>(&bare).unwrap()["format"],
        "sqlproj"
    );
}

#[test]
fn explicit_response_format_must_match_requested_format() {
    let body = json!({"definition": {
        "format": "dacpac", "parts": [wire_part("sqldb.dacpac", b"x")]
    }});
    assert_kind(
        response(
            &serde_json::to_vec(&body).unwrap(),
            Format::Sqlproj,
            limits(),
        ),
        Kind::FormatMismatch,
    );
}

#[test]
fn unknown_empty_and_null_formats_are_not_omission() {
    for value in [json!(""), json!("DACPAC"), json!("bacpac")] {
        let body = json!({"format": value, "parts": [wire_part("a.dacpac", b"x")]});
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::UnsupportedFormat,
        );
    }
    for value in [Value::Null, json!(true), json!([]), json!({}), json!(1)] {
        let body = json!({"format": value, "parts": [wire_part("a.dacpac", b"x")]});
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::InvalidShape,
        );
    }
}

#[test]
fn missing_multiple_empty_and_mixed_primary_parts_are_rejected() {
    assert_kind(
        from_parts(Format::Dacpac, vec![], limits()),
        Kind::MissingPrimaryPart,
    );
    assert_kind(
        from_parts(Format::Sqlproj, vec![part("a.sql", b"x")], limits()),
        Kind::MissingPrimaryPart,
    );
    for (format, suffix) in [(Format::Dacpac, "dacpac"), (Format::Sqlproj, "sqlproj")] {
        assert_kind(
            from_parts(format, vec![part(&format!("a.{suffix}"), b"")], limits()),
            Kind::InvalidShape,
        );
        assert_kind(
            from_parts(
                format,
                vec![
                    part(&format!("a.{suffix}"), b"x"),
                    part(&format!("nested/b.{suffix}"), b"y"),
                ],
                limits(),
            ),
            Kind::MultiplePrimaryParts,
        );
    }
    for format in [Format::Dacpac, Format::Sqlproj] {
        assert_kind(
            from_parts(
                format,
                vec![part("a.dacpac", b"x"), part("b.sqlproj", b"y")],
                limits(),
            ),
            Kind::UnsupportedPart,
        );
    }
}

#[test]
fn project_only_is_admitted_but_unrelated_extensions_are_not() {
    from_parts(
        Format::Sqlproj,
        vec![part("project.sqlproj", b"<Project/>")],
        limits(),
    )
    .unwrap();
    for path in [
        "database.bak",
        "database.bacpac",
        "database.DACPAC",
        "project.SQLPROJ",
        "content.json",
        "nested/.platform",
    ] {
        assert_kind(
            from_parts(Format::Dacpac, vec![part(path, b"x")], limits()),
            Kind::UnsupportedPart,
        );
    }
}

#[test]
fn platform_is_opaque_and_content_never_resolves_external_references() {
    let definition = from_parts(
        Format::Sqlproj,
        vec![
            part(
                "project.sqlproj",
                b"<Project><Import Project=\"../../outside\"/>$(Secret)</Project>",
            ),
            part(
                "query.sql",
                b":r C:\\outside.sql\nhttps://untrusted.invalid",
            ),
            part(".platform", b"not-json\xff"),
        ],
        limits(),
    )
    .unwrap();
    let again = parse(&serde_json::to_vec(&definition).unwrap(), limits()).unwrap();
    assert_eq!(again.parts(), definition.parts());
}

#[test]
fn strict_base64_rejects_noncanonical_spellings_and_preserves_valid_payload() {
    for payload in [
        "Zg", "Zg=", "Zg==\n", " Zg==", "Zh==", "Zm9=", "====", "Z=g=", "_w==", "-w==", "💥",
        "Zg===",
    ] {
        let body = json!({"parts": [{
            "path": "a.dacpac", "payload": payload, "payloadType": "InlineBase64"
        }]});
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::InvalidBase64,
        );
    }
    for bytes in [&b"f"[..], &b"fo"[..], &b"foo"[..], &b"\0\xff"[..]] {
        let payload = STANDARD.encode(bytes);
        let body = json!({"parts": [{
            "path": "a.dacpac", "payload": payload, "payloadType": "InlineBase64"
        }]});
        let definition = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap();
        assert_eq!(definition.parts()[0].bytes, bytes);
        assert_eq!(
            serde_json::to_value(&definition).unwrap()["parts"][0]["payload"],
            payload
        );
    }
}

#[test]
fn parts_must_be_objects_in_bare_and_response_definitions() {
    for item in [
        Value::Null,
        json!(false),
        json!(1),
        json!(1.5),
        json!("InlineBase64"),
        json!([]),
        json!(["a.dacpac", "eA==", "InlineBase64"]),
        json!([wire_part("a.dacpac", b"x")]),
    ] {
        let definition = json!({"parts": [item]});
        let bare = serde_json::to_vec(&definition).unwrap();
        let wrapped = serde_json::to_vec(&json!({"definition": definition})).unwrap();
        for result in [
            parse(&bare, limits()),
            response(&wrapped, Format::Dacpac, limits()),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::InvalidShape);
            assert_eq!(error.part_index, Some(0));
        }
    }
}

#[test]
fn unknown_part_fields_fail_in_bare_and_response_definitions() {
    let definition = json!({"parts": [{
        "path": "a.dacpac", "payload": "eA==", "payloadType": "InlineBase64", "extra": null
    }]});
    assert_kind(
        parse(&serde_json::to_vec(&definition).unwrap(), limits()),
        Kind::InvalidShape,
    );
    assert_kind(
        response(
            &serde_json::to_vec(&json!({"definition": definition})).unwrap(),
            Format::Dacpac,
            limits(),
        ),
        Kind::InvalidShape,
    );
}

#[test]
fn unsupported_payload_types_and_nonstring_payloads_fail_closed() {
    for payload_type in [
        "inlinebase64",
        "InlineText",
        "Url",
        "File",
        "",
        " InlineBase64",
        "InlineBase64 ",
        "InlineBase64\0",
    ] {
        let body = json!({"parts": [{
            "path": "a.dacpac", "payload": "eA==", "payloadType": payload_type
        }]});
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::UnsupportedPayloadType,
        );
        assert_kind(
            response(
                &serde_json::to_vec(&json!({"definition": body})).unwrap(),
                Format::Dacpac,
                limits(),
            ),
            Kind::UnsupportedPayloadType,
        );
    }
    for field in ["path", "payload", "payloadType"] {
        for value in [
            Value::Null,
            json!(1),
            json!(true),
            json!([]),
            json!({}),
            json!(["InlineBase64"]),
            json!({"InlineBase64": null}),
            json!([73, 110, 108, 105, 110, 101, 66, 97, 115, 101, 54, 52]),
        ] {
            let mut item = wire_part("a.dacpac", b"x");
            item[field] = value;
            let definition = json!({"parts": [item]});
            assert_kind(
                parse(&serde_json::to_vec(&definition).unwrap(), limits()),
                Kind::InvalidShape,
            );
            assert_kind(
                response(
                    &serde_json::to_vec(&json!({"definition": definition})).unwrap(),
                    Format::Dacpac,
                    limits(),
                ),
                Kind::InvalidShape,
            );
        }
        let mut item = wire_part("a.dacpac", b"x");
        item.as_object_mut().unwrap().remove(field);
        let definition = json!({"parts": [item]});
        assert_kind(
            parse(&serde_json::to_vec(&definition).unwrap(), limits()),
            Kind::InvalidShape,
        );
        assert_kind(
            response(
                &serde_json::to_vec(&json!({"definition": definition})).unwrap(),
                Format::Dacpac,
                limits(),
            ),
            Kind::InvalidShape,
        );
    }
}

#[test]
fn duplicate_json_keys_are_rejected_before_value_conversion() {
    for body in [
        r#"{"format":"dacpac","format":"dacpac","parts":[]}"#,
        r#"{"parts":[],"parts":[]}"#,
        r#"{"parts":[{"path":"a.dacpac","path":"b.dacpac","payload":"eA==","payloadType":"InlineBase64"}]}"#,
        r#"{"parts":[{"path":"a.dacpac","payload":"eA==","payload":"eA==","payloadType":"InlineBase64"}]}"#,
        r#"{"parts":[{"path":"a.dacpac","payload":"eA==","payloadType":"InlineBase64","payloadType":"InlineBase64"}]}"#,
    ] {
        assert_kind(parse(body.as_bytes(), limits()), Kind::InvalidShape);
        let wrapped = format!("{{\"definition\":{body}}}");
        assert_kind(
            response(wrapped.as_bytes(), Format::Dacpac, limits()),
            Kind::InvalidShape,
        );
    }
    let body = br#"{"definition":{"parts":[]},"definition":{"parts":[]}}"#;
    assert_kind(response(body, Format::Dacpac, limits()), Kind::InvalidShape);
}

#[test]
fn malformed_json_is_not_masked_by_an_earlier_shape_error() {
    for (case, bare, wrapped) in [
        (
            "trailing root after a missing-parts object",
            &b"{}\n{}"[..],
            &br#"{"definition":{}} {}"#[..],
        ),
        (
            "malformed value after an unknown field",
            &br#"{"unknown":}"#[..],
            &br#"{"definition":{"unknown":}}"#[..],
        ),
        (
            "invalid UTF-8 after an unknown field",
            &b"{\"unknown\":\"\xff\"}"[..],
            &b"{\"definition\":{\"unknown\":\"\xff\"}}"[..],
        ),
        (
            "trailing comma after a non-object part",
            &br#"{"parts":[null,]}"#[..],
            &br#"{"definition":{"parts":[null,]}}"#[..],
        ),
    ] {
        assert_eq!(
            parse(bare, limits()).unwrap_err().kind,
            Kind::InvalidJson,
            "bare definition: {case}"
        );
        assert_eq!(
            response(wrapped, Format::Dacpac, limits())
                .unwrap_err()
                .kind,
            Kind::InvalidJson,
            "definition response: {case}"
        );
    }
}

#[test]
fn malformed_unknown_and_metadata_only_shapes_never_become_definitions() {
    for (case, body) in [
        ("empty input", &b""[..]),
        ("unterminated object", &b"{"[..]),
        ("multiple roots", &b"{}\n{}"[..]),
        ("invalid UTF-8 suffix", &b"{\"parts\":[]}\xff"[..]),
        (
            "invalid UTF-8 in a path",
            &b"{\"parts\":[{\"path\":\"\xff\"}]}"[..],
        ),
    ] {
        assert_eq!(
            parse(body, limits()).unwrap_err().kind,
            Kind::InvalidJson,
            "{case}"
        );
    }
    for body in [
        json!({}),
        json!(null),
        json!([]),
        json!("encoded"),
        json!({"parts": null}),
        json!({"parts": {}}),
        json!({"parts": [null]}),
        json!({"parts": [], "extra": 1}),
        json!({"parts": [{
            "path": "a.dacpac", "payload": "eA==", "payloadType": "InlineBase64", "extra": 1
        }]}),
    ] {
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::InvalidShape,
        );
    }
    assert_kind(
        response(b"{}", Format::Dacpac, limits()),
        Kind::MissingDefinition,
    );
    for body in [
        json!({"displayName": "Metadata only"}),
        json!({"status": "Succeeded"}),
        json!({"errorCode": "Forbidden", "status": 403}),
        json!({"errorCode": "Unauthorized", "status": 401}),
        json!({"definition": null}),
        json!({"definition": {"parts": []}, "metadata": {}}),
    ] {
        assert_kind(
            response(
                &serde_json::to_vec(&body).unwrap(),
                Format::Dacpac,
                limits(),
            ),
            Kind::InvalidShape,
        );
    }
    assert_kind(
        response(br#"{"definition":{"parts":[]}}"#, Format::Dacpac, limits()),
        Kind::MissingPrimaryPart,
    );
}

#[test]
fn errors_and_debug_never_include_native_paths_or_content() {
    let secret = "PRIVATE-NATIVE-CONTENT";
    let native = part("PRIVATE-NATIVE-PATH.dacpac", secret.as_bytes());
    let definition = from_parts(Format::Dacpac, vec![native], limits()).unwrap();
    let debug = format!("{definition:?} {:?}", definition.parts());
    assert!(!debug.contains(secret));
    assert!(!debug.contains("PRIVATE-NATIVE-PATH"));
    assert!(!debug.contains(&STANDARD.encode(secret)));
    let body = json!({"parts": [{
        "path": "../PRIVATE-NATIVE-PATH",
        "payload": secret,
        "payloadType": "InlineBase64"
    }]});
    let error = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap_err();
    assert_eq!(error.part_index, Some(0));
    let diagnostic = format!("{error} {error:?}");
    assert!(!diagnostic.contains(secret));
    assert!(!diagnostic.contains("PRIVATE-NATIVE-PATH"));
    assert!(std::error::Error::source(&error).is_none());
    let body = json!({"parts": [{
        "path": "a.dacpac", "payload": secret, "payloadType": "InlineBase64"
    }]});
    let error = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap_err();
    assert_eq!(error.kind, Kind::InvalidBase64);
    assert!(!format!("{error} {error:?}").contains(secret));
}
