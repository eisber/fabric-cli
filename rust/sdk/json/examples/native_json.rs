// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-owned native value consumer; emits observations, not generated expectations.

#[path = "../tests/support/native.rs"]
mod support;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/native-cases.json"
    ))?;
    let observed = cases
        .iter()
        .map(support::observe)
        .collect::<Result<Vec<_>, _>>()?;
    println!("{}", serde_json::to_string(&observed)?);
    Ok(())
}
