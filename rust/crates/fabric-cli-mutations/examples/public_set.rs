// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure public SET helper probe; no service, identity, context, UI or runtime.

use fabric_cli_mutations::update_fabric_element;
use serde_json::{Value, json};
use std::io::{self, BufRead};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        let result = update_fabric_element(
            input["metadata"].clone(),
            input["query"].as_str().ok_or("missing query")?,
            input["input"].as_str().ok_or("missing input")?,
        );
        println!(
            "{}",
            match result {
                Ok(value) => json!({"value":value,"error":null}),
                Err(error) => json!({"value":null,"error":error.code}),
            }
        );
    }
    Ok(())
}
