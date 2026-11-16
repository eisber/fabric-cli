// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk::maps::MAX_OUTPUT_BYTES;
use serde_json::Value;

const UNSUPPORTED: &str = "Maps queries support bounded field/index/wildcard paths only; functions, filters, pipes and value constructors are unsupported.";

pub(super) fn validate(query: &str) -> Result<usize, &'static str> {
    if query.len() > 1024 || fabric_cli_query::validate_json_projection(query).is_err() {
        return Err(UNSUPPORTED);
    }
    if matches!(query, "@" | ".") {
        return Ok(1);
    }
    let bytes = query.as_bytes();
    let mut index = if bytes.starts_with(b"@.") { 2 } else { 0 };
    let mut selectors = 0;
    let mut needs_field = true;
    while index < bytes.len() {
        if needs_field {
            if !(bytes[index].is_ascii_alphabetic() || bytes[index] == b'_') {
                return Err(UNSUPPORTED);
            }
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            selectors += 1;
            needs_field = false;
        } else if bytes[index] == b'.' {
            index += 1;
            needs_field = true;
        } else if bytes[index] == b'[' {
            index += 1;
            if bytes.get(index) == Some(&b'*') {
                index += 1;
            } else if bytes.get(index) != Some(&b']') {
                if bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                if start == index {
                    return Err(UNSUPPORTED);
                }
            }
            if bytes.get(index) != Some(&b']') {
                return Err(UNSUPPORTED);
            }
            index += 1;
            selectors += 1;
        } else {
            return Err(UNSUPPORTED);
        }
        if selectors > 64 {
            return Err(UNSUPPORTED);
        }
    }
    if needs_field || selectors == 0 {
        return Err(UNSUPPORTED);
    }
    Ok(selectors)
}

pub(super) fn project(data: &Value, query: &str) -> Result<Value, &'static str> {
    let selectors = validate(query)?;
    let input_bytes = serde_json::to_vec(data)
        .map_err(|_| "Maps query input serialization failed.")?
        .len();
    // This subset never duplicates a source node. Conservatively charge one
    // complete input clone per selector plus the evaluator's input/output.
    if input_bytes
        .checked_mul(selectors + 2)
        .is_none_or(|bytes| bytes > MAX_OUTPUT_BYTES)
    {
        return Err(
            "Maps query intermediate-value budget exceeded; select a smaller response or shorter path.",
        );
    }
    fabric_cli_query::project_json(data, query).map_err(|_| "Invalid Maps query projection.")
}
