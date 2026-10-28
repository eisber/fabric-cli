// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public text/table consumer feeding observations to the independent source oracle.

#[path = "../tests/support/text.rs"]
mod support;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/text-cases.json"
    ))?;
    let observed: Vec<_> = cases.iter().map(support::observe).collect();
    println!("{}", serde_json::to_string(&observed)?);
    Ok(())
}
