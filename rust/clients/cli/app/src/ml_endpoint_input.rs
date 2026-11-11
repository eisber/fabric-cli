// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Schema-aware conveniences; native raw bodies deliberately bypass this adapter.

use clap::{
    Arg, ArgGroup, ArgMatches, Command,
    error::{ContextKind, ContextValue, ErrorKind},
};
use fabric_cli_output::{
    input::{self, MAX_INPUT_BYTES},
    tabular::TabularFormat,
};
use fabric_sdk::items::ml_models::{FormatType, Orientation, ScoreDataRequest};
use fabric_sdk_workspaces::mutations::ResourceError;
use serde_json::{Map, Value};
use std::collections::HashSet;

pub(super) const JSON_MESSAGE: &str = "Invalid ML JSON. Supply one UTF-8 native object without duplicate keys; inputs must be an array. In PowerShell, serialize hashtables with ConvertTo-Json -Depth 100 -Compress. PowerShell 7.3+ Standard and Windows PowerShell 5.1/Legacy transport quotes differently; do not apply Legacy escaping in Standard mode. Prefer --input FILE --input-format json for a schema-checked record array.";
pub(super) const BODY_MESSAGE: &str = "Invalid native ML score body. inputs must be an array; optional formatType is dataframe and orientation is split|values|record|index|table (record singular). Unknown fields and explicit null selectors are rejected.";
pub(super) const CONFLICT_MESSAGE: &str = "Ambiguous ML scoring input. Choose exactly one of --body, --input FILE|- or trailing -- key=value. Representation flags cannot replace fields already present in --body. File/record conveniences support record or values orientation; use --body for other native representations.";

#[derive(Clone)]
pub(super) enum Source {
    File {
        path: String,
        format: TabularFormat,
        rows: Option<Vec<Value>>,
    },
    Record(Vec<(String, String)>),
}

#[derive(Clone)]
pub(super) struct Input {
    pub source: Source,
    pub orientation: Orientation,
    pub version: Option<String>,
    pub wait: bool,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScoreInput").finish_non_exhaustive()
    }
}

pub(super) fn command(command: Command) -> Command {
    command
        .args_override_self(false)
        .arg(
            Arg::new("body")
                .long("body")
                .help("Raw native JSON object; at most 1 MiB; preserves native omission semantics"),
        )
        .arg(
            Arg::new("input")
                .long("input")
                .help("Schema-checked record file, or - for stdin; at most 1 MiB"),
        )
        .arg(
            Arg::new("input_format")
                .long("input-format")
                .requires("input")
                .value_parser(["json", "csv", "parquet"])
                .help("Local decoder (default json), not representation or output format"),
        )
        .arg(
            Arg::new("format_type")
                .long("format-type")
                .value_parser(["dataframe"]),
        )
        .arg(
            Arg::new("orientation")
                .long("orientation")
                .value_parser(["split", "values", "record", "index", "table"]),
        )
        .arg(
            Arg::new("record").num_args(1..).last(true).help(
                "Single record after --; key=JSON, or literal text for signature string columns",
            ),
        )
        .group(
            ArgGroup::new("score_input")
                .args(["body", "input", "record"])
                .required(true),
        )
}

pub(super) fn error(message: &'static str) -> clap::Error {
    let mut error = clap::Error::new(ErrorKind::ValueValidation);
    error.insert(ContextKind::Custom, ContextValue::String(message.into()));
    error
}

pub(super) fn diagnostic(error: &clap::Error) -> Option<&'static str> {
    match error.get(ContextKind::Custom) {
        Some(ContextValue::String(value)) => [JSON_MESSAGE, BODY_MESSAGE, CONFLICT_MESSAGE]
            .into_iter()
            .find(|message| value == message),
        _ => None,
    }
}

fn selectors(
    values: &ArgMatches,
) -> Result<(Option<FormatType>, Option<Orientation>), clap::Error> {
    let format = values
        .get_one::<String>("format_type")
        .map(|value| {
            serde_json::from_value(Value::String(value.clone())).map_err(|_| error(BODY_MESSAGE))
        })
        .transpose()?;
    let orientation = values
        .get_one::<String>("orientation")
        .map(|value| {
            serde_json::from_value(Value::String(value.clone())).map_err(|_| error(BODY_MESSAGE))
        })
        .transpose()?;
    Ok((format, orientation))
}

pub(super) fn raw(values: &ArgMatches) -> Result<Option<ScoreDataRequest>, clap::Error> {
    let Some(body) = values.get_one::<String>("body") else {
        return Ok(None);
    };
    let parsed = input::strict_json(body.as_bytes()).map_err(|_| error(JSON_MESSAGE))?;
    let mut payload: ScoreDataRequest =
        serde_json::from_value(parsed).map_err(|_| error(BODY_MESSAGE))?;
    let (format, orientation) = selectors(values)?;
    if (format.is_some() && payload.format_type.is_some())
        || (orientation.is_some() && payload.orientation.is_some())
    {
        return Err(error(CONFLICT_MESSAGE));
    }
    if format.is_some() {
        payload.format_type = format;
    }
    if orientation.is_some() {
        payload.orientation = orientation;
    }
    Ok(Some(payload))
}

pub(super) fn convenience(
    values: &ArgMatches,
    version: Option<&str>,
    wait: bool,
) -> Result<Input, clap::Error> {
    let (_, orientation) = selectors(values)?;
    let orientation = orientation.unwrap_or(Orientation::Record);
    if !matches!(orientation, Orientation::Record | Orientation::Values) {
        return Err(error(CONFLICT_MESSAGE));
    }
    let source = if let Some(path) = values.get_one::<String>("input") {
        if path.is_empty() {
            return Err(error(CONFLICT_MESSAGE));
        }
        Source::File {
            path: path.clone(),
            format: TabularFormat::parse(
                values
                    .get_one::<String>("input_format")
                    .map_or("json", String::as_str),
            )
            .map_err(|_| error(CONFLICT_MESSAGE))?,
            rows: None,
        }
    } else {
        let mut pairs = Vec::new();
        let mut keys = HashSet::new();
        let mut bytes = 0_usize;
        for pair in values
            .get_many::<String>("record")
            .ok_or_else(|| error(CONFLICT_MESSAGE))?
        {
            bytes = bytes.saturating_add(pair.len());
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| error(CONFLICT_MESSAGE))?;
            if key.is_empty() || !keys.insert(key) || bytes > MAX_INPUT_BYTES || pairs.len() >= 1024
            {
                return Err(error(CONFLICT_MESSAGE));
            }
            pairs.push((key.to_owned(), value.to_owned()));
        }
        Source::Record(pairs)
    };
    Ok(Input {
        source,
        orientation,
        version: version.map(str::to_owned),
        wait,
    })
}

pub(super) fn local_error(code: &str, message: impl Into<String>) -> ResourceError {
    ResourceError {
        code: code.into(),
        message: message.into(),
        ..ResourceError::input()
    }
}

impl Input {
    pub fn path(&self) -> Option<&str> {
        match &self.source {
            Source::File {
                path, rows: None, ..
            } => Some(path),
            _ => None,
        }
    }

    pub fn prepare(&mut self, bytes: &[u8]) -> Result<(), ResourceError> {
        if let Source::File { format, rows, .. } = &mut self.source {
            *rows = Some(
                input::records(bytes, *format)
                    .map_err(|error| local_error("InvalidScoreInput", error.to_string()))?,
            );
        }
        Ok(())
    }

    pub fn payload(&self, metadata: &Value) -> Result<ScoreDataRequest, ResourceError> {
        let schema = signature(metadata)?;
        let rows = match &self.source {
            Source::File {
                rows: Some(rows), ..
            } => rows.clone(),
            Source::File { rows: None, .. } => {
                return Err(local_error(
                    "ScoreInputNotPrepared",
                    "Read the bounded input before dispatch; no inference was submitted.",
                ));
            }
            Source::Record(pairs) => {
                let mut row = Map::new();
                for (key, text) in pairs {
                    let column = schema
                        .iter()
                        .find(|column| column.name == *key)
                        .ok_or_else(schema_error)?;
                    let value =
                        if column.kind == "string" && !text.starts_with('"') && text != "null" {
                            Value::String(text.clone())
                        } else {
                            input::strict_json(text.as_bytes()).map_err(|_| schema_error())?
                        };
                    row.insert(key.clone(), value);
                }
                vec![Value::Object(row)]
            }
        };
        let mut inputs = Vec::new();
        for row in rows {
            let object = row.as_object().ok_or_else(schema_error)?;
            if object.len() != schema.len() {
                return Err(schema_error());
            }
            let mut cells = Vec::new();
            for column in &schema {
                let value = object.get(&column.name).ok_or_else(schema_error)?;
                validate_cell(column, value)?;
                cells.push(value.clone());
            }
            inputs.push(if self.orientation == Orientation::Values {
                Value::Array(cells)
            } else {
                row
            });
        }
        let payload = ScoreDataRequest {
            inputs,
            format_type: Some(FormatType::Dataframe),
            orientation: Some(self.orientation),
        };
        if serde_json::to_vec(&payload)
            .map_err(|_| schema_error())?
            .len()
            > MAX_INPUT_BYTES
        {
            return Err(local_error(
                "ScoreInputLimit",
                "Encoded native score request exceeds 1 MiB; no inference was submitted.",
            ));
        }
        Ok(payload)
    }
}

struct Column {
    name: String,
    kind: String,
    required: bool,
}

fn signature(metadata: &Value) -> Result<Vec<Column>, ResourceError> {
    let entries = metadata
        .get("inputSignature")
        .and_then(Value::as_array)
        .ok_or_else(schema_error)?;
    if entries.is_empty() || entries.len() > 1024 {
        return Err(schema_error());
    }
    let mut names = HashSet::new();
    entries
        .iter()
        .map(|entry| {
            let object = entry.as_object().ok_or_else(schema_error)?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "name" | "type" | "required"))
            {
                return Err(schema_error());
            }
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(schema_error)?;
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(schema_error)?;
            let required = entry
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(schema_error)?;
            if !names.insert(name)
                || !matches!(
                    kind,
                    "boolean" | "integer" | "long" | "float" | "double" | "string"
                )
            {
                return Err(schema_error());
            }
            Ok(Column {
                name: name.into(),
                kind: kind.into(),
                required,
            })
        })
        .collect()
}

fn validate_cell(column: &Column, value: &Value) -> Result<(), ResourceError> {
    let valid = if value.is_null() {
        !column.required
    } else {
        match column.kind.as_str() {
            "boolean" => value.is_boolean(),
            "string" => value.is_string(),
            "integer" => value
                .as_i64()
                .is_some_and(|number| i32::try_from(number).is_ok()),
            "long" => value.as_i64().is_some(),
            "float" | "double" => {
                let exact_integer = if let Some(number) = value.as_i64() {
                    number.unsigned_abs() <= (1_u64 << 53)
                } else if let Some(number) = value.as_u64() {
                    number <= (1_u64 << 53)
                } else {
                    true
                };
                exact_integer
                    && value.as_f64().is_some_and(|number| {
                        number.is_finite()
                            && (column.kind == "double" || f64::from(number as f32) == number)
                    })
            }
            _ => false,
        }
    };
    if !valid {
        return Err(schema_error());
    }
    Ok(())
}

fn schema_error() -> ResourceError {
    local_error(
        "ScoreSchemaMismatch",
        "Input does not match the exact serving-version signature. Supply every named column once, no unknown columns, and exact supported primitive types (boolean, integer, long, float, double, string). Null requires required:false. Tensor/nested/logical schemas and lossy numeric conversions are unsupported. No inference was submitted.",
    )
}
