// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public facade contracts for SQL metadata and native definition consumers.

#![deny(unnameable_test_items)]

use fabric_sdk::{sql_databases, sql_definitions};
use std::{any::TypeId, num::NonZeroUsize};

#[test]
fn metadata_facade_reexports_the_focused_request_and_result_types() {
    assert_eq!(
        TypeId::of::<sql_databases::SqlRequest<sql_databases::SqlDatabase>>(),
        TypeId::of::<fabric_sdk_sql_database::SqlRequest<fabric_sdk_sql_database::SqlDatabase>>(),
    );
    assert_eq!(
        TypeId::of::<sql_databases::SqlDatabasePage>(),
        TypeId::of::<fabric_sdk_sql_database::SqlDatabasePage>(),
    );
}

#[test]
fn definition_facade_preserves_binary_parts_through_the_focused_codec() {
    let limits = sql_definitions::DefinitionLimits {
        max_wire_bytes: NonZeroUsize::new(1024).unwrap(),
        max_decoded_bytes: NonZeroUsize::new(64).unwrap(),
        max_part_bytes: NonZeroUsize::new(64).unwrap(),
        max_parts: NonZeroUsize::new(4).unwrap(),
        max_path_bytes: NonZeroUsize::new(64).unwrap(),
        max_depth: NonZeroUsize::new(4).unwrap(),
    };
    let bytes = vec![0, 0xff, 0xfe, 0x80];
    let facade = sql_definitions::sql_database_definition_from_parts(
        sql_definitions::SqlDatabaseDefinitionFormat::Dacpac,
        vec![sql_definitions::NativeDefinitionPart {
            path: "definition.dacpac".to_owned(),
            bytes: bytes.clone(),
        }],
        limits,
    )
    .unwrap();
    let direct: fabric_sdk_sql_definition::NativeDefinition = facade;
    let wire = serde_json::to_vec(&direct).unwrap();
    let decoded = sql_definitions::parse_sql_database_definition(&wire, limits).unwrap();

    assert_eq!(
        decoded.format(),
        sql_definitions::SqlDatabaseDefinitionFormat::Dacpac,
    );
    assert_eq!(decoded.parts().len(), 1);
    assert_eq!(decoded.parts()[0].path, "definition.dacpac");
    assert_eq!(decoded.parts()[0].bytes, bytes);
}
