// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public data encoder consumer; stdout is observations, never generated expectations.

use fabric_cli_output::json_dumps;
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/definition-cases.json"
    ))?;
    let observations = cases
        .iter()
        .map(|case| Ok(json!({"name": case["name"], "encoded": json_dumps(&case["data"])?})))
        .collect::<Result<Vec<_>, fabric_cli_output::OutputError>>()?;
    println!("{}", serde_json::to_string(&observations)?);
    Ok(())
}
