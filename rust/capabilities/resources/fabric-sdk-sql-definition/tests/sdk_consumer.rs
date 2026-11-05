// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK content consumption without a deploy or filesystem bridge.

#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, NativeDefinition, SqlDatabaseDefinitionFormat as Format,
    parse_sql_database_definition as parse, parse_sql_database_definition_response as response,
    sql_database_definition_from_parts as from_parts,
};
use serde_json::{Value, json};
use support::{assert_kind, limits, part, wire_part};

fn consume_native_definition(definition: NativeDefinition) -> Vec<u8> {
    assert!(!definition.parts().is_empty());
    serde_json::to_vec(&definition).unwrap()
}

#[test]
fn sdk_consumer_passes_the_same_native_type_without_deploy_or_filesystem() {
    let source = json!({"definition": {"parts": [
        wire_part("sqldb.sqlproj", b"<Project/>"),
        wire_part("dbo/Tables/T.sql", b"\xff\0\r\n"),
        wire_part(".sharedqueries/Q.sql", b"select 1;"),
        wire_part(".platform", b"  opaque platform bytes  ")
    ]}});
    let native: NativeDefinition = response(
        &serde_json::to_vec(&source).unwrap(),
        Format::Sqlproj,
        limits(),
    )
    .unwrap();
    let source_paths: Vec<_> = native
        .parts()
        .iter()
        .map(|part| part.path.clone())
        .collect();
    let serialized = consume_native_definition(native);
    let target: NativeDefinition = parse(&serialized, limits()).unwrap();
    assert_eq!(target.format(), Format::Sqlproj);
    assert_eq!(target.parts()[1].bytes, b"\xff\0\r\n");
    assert_eq!(
        target
            .parts()
            .iter()
            .map(|part| part.path.clone())
            .collect::<Vec<_>>(),
        source_paths
    );
    let raw: Value = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(raw["format"], "sqlproj");
    assert_eq!(raw["parts"][0]["payloadType"], "InlineBase64");
}

#[test]
fn local_candidate_content_is_consumed_without_local_file_authority() {
    let candidate = part("definition.dacpac", b"\0\xffbinary");
    let definition = from_parts(Format::Dacpac, vec![candidate], limits()).unwrap();
    let bytes = consume_native_definition(definition);
    let definition = parse(&bytes, limits()).unwrap();
    assert_eq!(definition.parts()[0].bytes, b"\0\xffbinary");
}

#[test]
fn error_or_metadata_bodies_cannot_replace_the_source_definition() {
    for body in [
        json!({"errorCode": "Unauthorized", "status": 401}),
        json!({"errorCode": "Forbidden", "status": 403}),
        json!({"displayName": "Metadata only", "type": "SQLDatabase"}),
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
}
