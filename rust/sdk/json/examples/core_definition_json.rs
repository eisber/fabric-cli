// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Core-layer direct consumer; no CLI or SDK dependency.

use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/definition-cases.json"
    ))?;
    let observed: Vec<_> = cases.iter().map(|case| {
        json!({"name":case["name"],"encoded":fabric_core_json::json_dumps(&case["data"])})
    }).collect();
    println!("{}", serde_json::to_string(&observed)?);
    Ok(())
}
