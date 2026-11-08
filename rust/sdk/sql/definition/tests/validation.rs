// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Neutral local validation preserves the existing codec's actual boundaries.
#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, SqlDatabaseDefinitionFormat,
    parse_sql_database_definition, sql_database_definition_from_parts,
    validation::{self, Severity, ValidationError, v2},
};
use serde_json::json;
use support::{assert_kind, limits, nz, part, wire_part};

fn body(path: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format":"dacpac",
        "parts":[wire_part(path, b"opaque-not-a-real-package")]
    }))
    .unwrap()
}

fn report(bytes: &[u8]) -> validation::ValidationReport {
    validation::validate(bytes, "application/json", "dacpac", "1.0", limits()).unwrap()
}

#[test]
fn structural_validity_explicitly_excludes_native_package_and_live_acceptance() {
    let bytes = body("model.dacpac");
    let original = bytes.clone();
    let constructed = sql_database_definition_from_parts(
        SqlDatabaseDefinitionFormat::Dacpac,
        vec![part("model.dacpac", b"opaque-not-a-real-package")],
        limits(),
    )
    .unwrap();
    assert_eq!(serde_json::to_vec(&constructed).unwrap(), bytes);
    let report = report(&bytes);
    assert!(report.valid);
    assert!(report.findings.is_empty());
    assert_eq!(report.operation_id, validation::OPERATION_ID);
    assert_eq!(report.rule_version, "1.0");
    assert_eq!(report.coverage.finding_policy, "first_failure");
    assert!(!report.coverage.complete);
    for exclusion in [
        "dacpac_package_semantics",
        "permissions",
        "service_acceptance",
        "roundtrip",
        "runtime_correctness",
    ] {
        assert!(report.coverage.not_checked.contains(&exclusion));
    }
    assert_eq!(bytes, original);
}

#[test]
fn neutral_registration_is_shared_inert_and_not_a_mutation_binding() {
    let mut registry = v2::Registry::default();
    validation::register(&mut registry).unwrap();
    let target = v2::Target {
        kind: v2::TargetKind::Item,
        owner_type: "SQLDatabase".into(),
        facet: "src".into(),
        representation: validation::REPRESENTATION.into(),
        generation: None,
    };
    let resolved = registry.resolve(v2::Verb::Validate, &target, None).unwrap();
    assert_eq!(resolved.operation.id(), validation::OPERATION_ID);
    assert_eq!(resolved.operation.effects(), &[v2::Effect::LocalRead]);
    assert_eq!(
        resolved.operation.input_schema()["properties"]["format"]["const"],
        "dacpac"
    );
    assert!(registry.resolve(v2::Verb::Update, &target, None).is_err());
    let before = serde_json::to_value(resolved.operation).unwrap();
    assert_eq!(
        validation::register(&mut registry),
        Err(v2::ContractError::DuplicateOperation)
    );
    assert_eq!(registry.operations().count(), 1);
    assert_eq!(registry.bindings().len(), 1);
    assert_eq!(
        serde_json::to_value(registry.operations().next().unwrap()).unwrap(),
        before
    );
}

#[test]
fn malformed_duplicate_and_unknown_keys_are_not_silently_normalized() {
    for bytes in [
        &b"{"[..], b"null", b"[]", b"{} trailing", b"\xff",
        b"{\"format\":\"dacpac\",\"format\":\"dacpac\",\"parts\":[]}",
        b"{\"parts\":[],\"p\\u0061rts\":[]}",
        b"{\"parts\":[{\"path\":\"a.dacpac\",\"p\\u0061th\":\"b.dacpac\",\"payload\":\"eA==\",\"payloadType\":\"InlineBase64\"}]}",
        b"{\"parts\":[{\"path\":\"a.dacpac\",\"payload\":\"eA==\",\"payloadType\":\"InlineBase64\",\"unknown\":true}]}",
        b"{\"definition\":{\"parts\":[]}}",
    ] {
        let result = report(bytes);
        assert!(!result.valid);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Error);
    }
}

#[test]
fn unsupported_profiles_never_return_valid_or_invalid_reports() {
    for (media, format, version) in [
        ("application/json", "dacpac", "future"),
        ("application/json", "sqlproj", "1.0"),
        ("application/octet-stream", "dacpac", "1.0"),
    ] {
        assert_eq!(
            validation::validate(&body("a.dacpac"), media, format, version, limits()),
            Err(ValidationError::Unsupported)
        );
    }
    for input in [
        json!({"format":"future","parts":[]}),
        json!({"parts":[{"path":"a.dacpac","payload":"eA==","payloadType":"future"}]}),
        json!({"parts":[wire_part("a.sqlproj", b"x")]}),
    ] {
        assert_eq!(
            validation::validate(
                &serde_json::to_vec(&input).unwrap(),
                "application/json",
                "dacpac",
                "1.0",
                limits()
            ),
            Err(ValidationError::Unsupported)
        );
    }
}

#[test]
fn unsupported_document_format_precedes_format_dependent_admission_failures() {
    for bytes in [
        &br#"{"format":"sqlproj","parts":[]}"#[..],
        br#"{"parts":[],"format":"sqlproj"}"#,
        br#"{"format":"sqlproj","parts":null}"#,
        br#"{"format":"sqlproj","parts":[null]}"#,
        br#"{"format":"sqlproj"}"#,
        br#"{"format":"sqlproj","parts":[{"path":"a.sqlproj","payload":"bad-base64","payloadType":"InlineBase64"}]}"#,
        br#"{"format":"sqlproj","parts":[{"path":"../a.sqlproj","payload":"eA==","payloadType":"InlineBase64"}]}"#,
        br#"{"format":"sqlproj","parts":[{"path":"a.sqlproj","payload":"eA==","payloadType":"InlineBase64"}]}"#,
        br#"{"f\u006frmat":"sqlpr\u006fj","parts":[]}"#,
        br#"{"format":"future","parts":[null]}"#,
    ] {
        assert_eq!(
            validation::validate(bytes, "application/json", "dacpac", "1.0", limits()),
            Err(ValidationError::Unsupported),
            "unsupported native format must not produce a DACPAC report"
        );
    }
    for bytes in [
        &br#"{"format":"dacpac","parts":[]}"#[..],
        br#"{"parts":[]}"#,
        br#"{"format":"dacpac","parts":[null]}"#,
        br#"{"format":"dacpac","format":"sqlproj","parts":[]}"#,
        br#"{"format":null,"parts":[]}"#,
        br#"{"format":123,"parts":[]}"#,
        br#"{"format":"sqlproj","parts":[]} trailing"#,
        br#"{"format":"sqlproj","parts":[}"#,
    ] {
        assert!(!report(bytes).valid);
    }
    let unsupported = br#"{"format":"sqlproj","parts":[]}"#;
    let budget = fabric_sdk_sql_definition::DefinitionLimits {
        max_wire_bytes: nz(unsupported.len() - 1),
        ..limits()
    };
    assert!(matches!(
        validation::validate(unsupported, "application/json", "dacpac", "1.0", budget),
        Err(ValidationError::Budget(error)) if error.kind == Kind::Limit(Limit::WireBytes)
    ));
    assert!(report(&body("model.dacpac")).valid);
}

#[test]
fn precise_ordinal_locations_do_not_echo_paths_or_payloads() {
    let bytes = serde_json::to_vec(&json!({"parts":[
        wire_part("model.dacpac", b"x"),
        {"path":".platform","payload":"do-not-echo-this","payloadType":"InlineBase64"}
    ]}))
    .unwrap();
    let result = report(&bytes);
    assert_eq!(result.findings[0].rule_id, "invalid_base64");
    assert_eq!(result.findings[0].pointer, "/parts/1/payload");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("do-not-echo")
    );
    let unsafe_input = body("../private.dacpac");
    assert_kind(
        parse_sql_database_definition(&unsafe_input, limits()),
        Kind::UnsafePath,
    );
    let result = report(&unsafe_input);
    assert_eq!(result.findings[0].pointer, "/parts/0/path");
    assert_eq!(result.findings[0].rule_id, "unsafe_path");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private.dacpac")
    );
}

#[test]
fn duplicate_case_and_file_directory_paths_remain_rejected() {
    for paths in [
        [".platform", ".platform"],
        ["dir/.platform", "DIR/model.dacpac"],
        ["model.dacpac", "model.dacpac/.platform"],
    ] {
        let bytes = serde_json::to_vec(&json!({"parts":[
            wire_part(paths[0], b"x"), wire_part(paths[1], b"y")
        ]}))
        .unwrap();
        let result = report(&bytes);
        assert!(!result.valid);
        assert_eq!(result.findings[0].rule_id, "path_collision");
        assert_eq!(result.findings[0].pointer, "/parts/1/path");
    }
}

#[test]
fn inclusive_budgets_return_no_verdict_when_exceeded() {
    let bytes = serde_json::to_vec(&json!({"format":"dacpac","parts":[
        wire_part("dir/a.dacpac", b"abc"), wire_part(".platform", b"de")
    ]}))
    .unwrap();
    let exact = fabric_sdk_sql_definition::DefinitionLimits {
        max_wire_bytes: nz(bytes.len()),
        max_decoded_bytes: nz(5),
        max_part_bytes: nz(3),
        max_parts: nz(2),
        max_path_bytes: nz("dir/a.dacpac".len()),
        max_depth: nz(2),
    };
    assert!(
        validation::validate(&bytes, "application/json", "dacpac", "1.0", exact)
            .unwrap()
            .valid
    );
    for (changed, expected) in [
        (
            fabric_sdk_sql_definition::DefinitionLimits {
                max_wire_bytes: nz(bytes.len() - 1),
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
                max_path_bytes: nz("dir/a.dacpac".len() - 1),
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
        let error =
            validation::validate(&bytes, "application/json", "dacpac", "1.0", changed).unwrap_err();
        assert!(
            matches!(error, ValidationError::Budget(error) if error.kind == Kind::Limit(expected))
        );
    }
}

#[test]
fn opaque_platform_bytes_are_not_misrepresented_as_schema_checked() {
    let bytes = serde_json::to_vec(&json!({"parts":[
        wire_part("model.dacpac", b"x"),
        wire_part(".platform", b"not-json-and-not-executed"),
    ]}))
    .unwrap();
    let result = report(&bytes);
    assert!(result.valid);
    assert!(
        result
            .coverage
            .not_checked
            .contains(&"decoded_platform_json")
    );
}
