// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use arrow_array::{Array, LargeStringArray, StringArray};
use arrow_schema::{DataType, Field};
use serde::Deserialize;

use crate::TabularError;

/// Field metadata declaring validated JSON text or a GUID in a UTF-8 column.
pub const LOGICAL_TYPE_METADATA_KEY: &str = "fabric.tabular.logical_type";

/// Field metadata retaining a bounded native engine type without changing values.
pub const NATIVE_TYPE_METADATA_KEY: &str = "fabric.tabular.native_type";

pub(crate) fn supported(field: &Field, key: &str, value: &str) -> bool {
    match key {
        LOGICAL_TYPE_METADATA_KEY => {
            matches!(field.data_type(), DataType::Utf8 | DataType::LargeUtf8)
                && matches!(value, "json" | "guid")
        }
        NATIVE_TYPE_METADATA_KEY => {
            !value.is_empty()
                && value.len() <= 256
                && value.trim() == value
                && !value.chars().any(char::is_control)
        }
        _ => false,
    }
}

pub(crate) fn encoding(field: &Field) -> Option<&'static str> {
    match field.metadata().get(LOGICAL_TYPE_METADATA_KEY)?.as_str() {
        "json" => Some("json-text"),
        "guid" => Some("guid-string"),
        _ => None,
    }
}

pub(crate) fn validate_value(
    field: &Field,
    array: &dyn Array,
    column: usize,
    row: usize,
    output_row: usize,
) -> Result<(), TabularError> {
    let Some(logical_type) = field.metadata().get(LOGICAL_TYPE_METADATA_KEY) else {
        return Ok(());
    };
    let invalid = |reason| TabularError::UnsupportedValue {
        column,
        row: output_row,
        reason,
    };
    let value = match field.data_type() {
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|values| values.value(row)),
        DataType::LargeUtf8 => array
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|values| values.value(row)),
        _ => None,
    }
    .ok_or_else(|| invalid("logical text column does not match its schema"))?;
    let valid = match logical_type.as_str() {
        "json" => {
            let mut input = serde_json::Deserializer::from_str(value);
            serde::de::IgnoredAny::deserialize(&mut input).is_ok() && input.end().is_ok()
        }
        "guid" => {
            value.len() == 36
                && value.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        }
        _ => false,
    };
    if !valid {
        return Err(invalid(
            "value does not match its declared logical text type",
        ));
    }
    Ok(())
}
