// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Emit observed public-API results for the independent, source-executing Python oracle.

#[path = "../tests/support/mod.rs"]
mod support;

use fabric_cli_output::OutputStream;
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/cases.json"
    ))?;
    let observations: Vec<_> = fixtures["cases"]
        .as_array()
        .ok_or("missing cases")?
        .iter()
        .map(|case| {
            let plan = support::plan(case, fixtures["timestamp"].as_str().unwrap());
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            plan.write_to(&mut stdout, &mut stderr).unwrap();
            json!({
                "name": case["name"],
                "stream": match plan.stream {
                    OutputStream::Stdout => "stdout",
                    OutputStream::Stderr => "stderr",
                },
                "exit": plan.exit_code,
                "stdout": String::from_utf8(stdout).unwrap(),
                "stderr": String::from_utf8(stderr).unwrap(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&observations)?);
    Ok(())
}
