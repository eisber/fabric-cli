// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct core API consumer emitting observations for the independent Python oracle.

use fabric_core_json::{Format, Indent, encode, encode_unicode};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/unicode-json-cases.json"
    ))?;
    let observed: Vec<_> = cases
        .iter()
        .map(|case| {
            let format = match case.get("indent") {
                Some(Value::Null) => Format::Default,
                Some(value) if value == 0 => Format::Pretty(Indent::Zero),
                Some(value) if value == 2 => Format::Pretty(Indent::Two),
                Some(value) if value == 4 => Format::Pretty(Indent::Four),
                None => Format::Pretty(Indent::Four),
                _ => panic!("unsupported fixture indentation"),
            };
            json!({
                "name":case["name"],
                "encoded":encode_unicode(&case["data"],format),
                "ascii":encode(&case["data"],format),
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&observed)?);
    Ok(())
}
