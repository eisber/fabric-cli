// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_sql_definition::{
    DefinitionErrorKind, DefinitionLimits, NativeDefinition, NativeDefinitionPart,
};
use serde_json::{Value, json};
use std::num::NonZeroUsize;

/// Make an explicit nonzero synthetic test bound.
pub fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

/// Test-fixture bounds, not production defaults.
pub fn limits() -> DefinitionLimits {
    DefinitionLimits {
        max_wire_bytes: nz(128 * 1024),
        max_decoded_bytes: nz(64 * 1024),
        max_part_bytes: nz(32 * 1024),
        max_parts: nz(32),
        max_path_bytes: nz(1024),
        max_depth: nz(32),
    }
}

/// Build an unvalidated owned native candidate.
pub fn part(path: &str, bytes: &[u8]) -> NativeDefinitionPart {
    NativeDefinitionPart {
        path: path.into(),
        bytes: bytes.to_vec(),
    }
}

/// Build a synthetic InlineBase64 wire part.
pub fn wire_part(path: &str, bytes: &[u8]) -> Value {
    json!({"path": path, "payload": STANDARD.encode(bytes), "payloadType": "InlineBase64"})
}

/// Assert a public content-admission failure classification.
pub fn assert_kind(
    result: Result<NativeDefinition, fabric_sdk_sql_definition::DefinitionError>,
    expected: DefinitionErrorKind,
) {
    assert_eq!(result.unwrap_err().kind, expected);
}
