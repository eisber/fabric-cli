// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_core_json::{
    DecimalInteger, Format, Indent, Options, PythonDict, PythonString, PythonValue,
    PythonValueError, encode_python, encode_python_with_options,
};
use serde_json::{Value, json};
use std::error::Error;

fn string(points: &Value) -> Result<PythonString, Box<dyn Error>> {
    let points = points
        .as_array()
        .ok_or("missing codepoints")?
        .iter()
        .map(|point| Ok(u32::try_from(point.as_u64().ok_or("invalid codepoint")?)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(PythonString::from_codepoints(points)?)
}

pub fn value(spec: &Value) -> Result<PythonValue, Box<dyn Error>> {
    Ok(match spec["kind"].as_str().ok_or("missing value kind")? {
        "none" => PythonValue::None,
        "bool" => PythonValue::Bool(spec["value"].as_bool().ok_or("invalid bool")?),
        "int" => PythonValue::Integer(DecimalInteger::parse(
            spec["value"].as_str().ok_or("invalid integer")?,
        )?),
        "float" => PythonValue::Float(f64::from_bits(u64::from_str_radix(
            spec["bits"].as_str().ok_or("invalid float bits")?,
            16,
        )?)),
        "string" => PythonValue::String(string(&spec["codepoints"])?),
        "list" => PythonValue::List(
            spec["items"]
                .as_array()
                .ok_or("invalid list")?
                .iter()
                .map(value)
                .collect::<Result<_, _>>()?,
        ),
        "dict" => {
            let entries = spec["items"]
                .as_array()
                .ok_or("invalid dict")?
                .iter()
                .map(|entry| Ok((string(&entry["key"])?, value(&entry["value"])?)))
                .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
            PythonValue::Dict(PythonDict::from_entries(entries)?)
        }
        _ => return Err("unsupported native fixture kind".into()),
    })
}

pub fn observe(case: &Value) -> Result<Value, Box<dyn Error>> {
    let value = value(&case["value"])?;
    let unicode = match encode_python_with_options(
        &value,
        Options {
            format: Format::Default,
            ensure_ascii: false,
        },
    ) {
        Ok(encoded) => json!({"encoded":encoded}),
        Err(PythonValueError::Surrogate { .. }) => json!({"error":"surrogate"}),
        Err(error) => return Err(error.into()),
    };
    Ok(json!({
        "name": case["name"],
        "json": encode_python(&value,Format::Default)?,
        "pretty0": encode_python(&value,Format::Pretty(Indent::Zero))?,
        "pretty2": encode_python(&value,Format::Pretty(Indent::Two))?,
        "pretty4": encode_python(&value,Format::Pretty(Indent::Four))?,
        "truth": value.is_truthy(),
        "unicode": unicode,
    }))
}
