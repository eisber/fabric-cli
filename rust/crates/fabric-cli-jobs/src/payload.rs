// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{JobCommand, ScheduleArgs, parser::invalid};
use fabric_cli_output::CommandError;
use fabric_sdk_jobs::JobType;
use serde_json::{Map, Value, json};

/// Caller-owned input-file boundary; libraries never open arbitrary supplied paths.
pub trait JsonInput: Send + Sync {
    /// Read a requested JSON file, preserving source errors for the command envelope.
    fn read_json(&self, path: &str) -> Result<Value, CommandError>;
}

/// An explicit host policy rejecting file input, without ambient filesystem access.
pub struct NoFiles;

impl JsonInput for NoFiles {
    fn read_json(&self, _: &str) -> Result<Value, CommandError> {
        Err(not_runnable(
            "JSON file input requires a caller-supplied file reader",
        ))
    }
}

pub(crate) fn not_runnable(message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some("NotRunnable".into()))
}

/// Translate input token joining, quote stripping and inline single-quote replacement.
pub fn json_input(tokens: &[String], files: &dyn JsonInput) -> Result<Value, CommandError> {
    let joined = tokens.join(" ");
    let normalized = joined.trim_matches(['\'', '"']);
    if normalized.starts_with('{') && normalized.ends_with('}') {
        serde_json::from_str(&normalized.replace('\'', "\""))
            .map_err(|_| CommandError::unexpected(Some("Invalid JSON content".into())))
    } else {
        files.read_json(normalized)
    }
}

/// Build Python's `executionData` value before the SDK adds its request envelope.
pub fn execution_data(
    command: &JobCommand,
    job_type: JobType,
    files: &dyn JsonInput,
) -> Result<Option<Value>, CommandError> {
    if command.input.is_some() && (command.params.is_some() || command.config.is_some()) {
        return Err(not_runnable(
            "Cannot use input in combination with params or config arguments at the same time.",
        ));
    }
    if let Some(input) = &command.input {
        return json_input(input, files).map(Some);
    }
    let mut result = Map::new();
    if let Some(tokens) = &command.params {
        let params = parameters(&tokens.join(" "), job_type)?;
        result.insert("parameters".into(), Value::Object(params));
    }
    if let Some(config) = &command.config {
        if job_type != JobType::Notebook {
            return Err(not_runnable(format!(
                "Configuration payload not supported for job type: {}.",
                job_type.as_str()
            )));
        }
        result.insert("configuration".into(), json_input(config, files)?);
    }
    Ok((!result.is_empty()).then_some(Value::Object(result)))
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// Python's unanchored pattern finds name:type=value spans and splits only at
// comma + optional whitespace + the next word:word=, not at JSON commas.
fn parameter_start(text: &str, offset: usize) -> Option<(usize, usize, usize)> {
    for (relative, ch) in text[offset..].char_indices() {
        if !word(ch) {
            continue;
        }
        let start = offset + relative;
        let mut end = start + ch.len_utf8();
        while end < text.len() {
            let ch = text[end..].chars().next()?;
            if !word(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        if text.as_bytes().get(end) != Some(&b':') {
            continue;
        }
        let colon = end;
        end += 1;
        let type_start = end;
        while end < text.len() {
            let ch = text[end..].chars().next()?;
            if !word(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        if end > type_start && text.as_bytes().get(end) == Some(&b'=') {
            return Some((start, colon, end));
        }
    }
    None
}

fn parameters(text: &str, job_type: JobType) -> Result<Map<String, Value>, CommandError> {
    if !matches!(job_type, JobType::Notebook | JobType::Pipeline) {
        return Err(not_runnable(format!(
            "Parameters not supported for job type: {}. Please use the input argument instead",
            job_type.as_str()
        )));
    }
    let mut result = Map::new();
    let mut offset = 0;
    while let Some((start, colon, equal)) = parameter_start(text, offset) {
        let mut end = text.len();
        for (relative, _) in text[equal + 1..].match_indices(',') {
            let comma = equal + 1 + relative;
            let following = &text[comma + 1..];
            let trimmed = following.trim_start();
            let next = text.len() - trimmed.len();
            if parameter_start(text, next).is_some_and(|(start, _, _)| start == next) {
                end = comma;
                break;
            }
        }
        let raw = &text[equal + 1..end];
        if raw.is_empty() || raw.contains('\n') {
            return Err(not_runnable(format!("Invalid parameter format: {text}")));
        }
        let name = &text[start..colon];
        let kind = &text[colon + 1..equal];
        let value = parameter_value(kind, raw, job_type)?;
        let value = if job_type == JobType::Notebook {
            json!({"type": kind, "value": value})
        } else {
            value
        };
        result.insert(name.into(), value);
        offset = end;
        if end == text.len() {
            break;
        }
    }
    if result.is_empty() {
        return Err(not_runnable(format!("Invalid parameter format: {text}")));
    }
    Ok(result)
}

fn parameter_value(kind: &str, raw: &str, job_type: JobType) -> Result<Value, CommandError> {
    match kind {
        "string" => Ok(Value::String(raw.trim_matches(['\'', '"']).into())),
        "secureString" if job_type == JobType::Pipeline => {
            Ok(Value::String(raw.trim_matches(['\'', '"']).into()))
        }
        "bool" => Ok(Value::Bool(raw.to_lowercase() == "true")),
        "int" => {
            let value = raw.trim();
            let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
            if digits.is_empty()
                || !valid_numeric_underscores(digits)
                || !digits.bytes().all(|c| c.is_ascii_digit() || c == b'_')
            {
                return Err(not_runnable("Invalid integer parameter value"));
            }
            value
                .replace('_', "")
                .parse::<i64>()
                .map(|n| json!(n))
                .map_err(|_| {
                    not_runnable("Integer parameter exceeds the supported JSON integer bound")
                })
        }
        "float" => {
            if !valid_numeric_underscores(raw.trim()) {
                return Err(not_runnable("Invalid float parameter value"));
            }
            let number = raw
                .trim()
                .replace('_', "")
                .parse::<f64>()
                .map_err(|_| not_runnable("Invalid float parameter value"))?;
            serde_json::Number::from_f64(number)
                .map(Value::Number)
                .ok_or_else(|| not_runnable("Non-finite JSON parameters are unsupported"))
        }
        "array" | "object" if job_type == JobType::Pipeline => {
            serde_json::from_str(raw).map_err(|_| not_runnable("Invalid JSON parameter value"))
        }
        _ => Err(not_runnable(format!(
            "Invalid parameter type: {kind}. Supported types: {}.",
            if job_type == JobType::Notebook {
                "string, int, float, bool"
            } else {
                "string, secureString, int, float, bool, array, object"
            }
        ))),
    }
}

fn valid_numeric_underscores(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().all(|(i, byte)| {
        *byte != b'_'
            || (i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit())
    })
}

/// Source schedule configuration; `None` means update enabled state only.
pub fn schedule_configuration(
    args: &ScheduleArgs,
    input: Option<&[String]>,
    update: bool,
    files: &dyn JsonInput,
) -> Result<Option<Value>, CommandError> {
    let supplied = args.schedule_type.is_some()
        || args.start.is_some()
        || args.end.is_some()
        || args.interval.is_some()
        || args.days.is_some();
    if let Some(input) = input {
        let config = json_input(input, files)?;
        if args.enable || (update && args.disable) || supplied {
            return Err(not_runnable(
                "Cannot use input in combination with schedule parameters",
            ));
        }
        return Ok(Some(config));
    }
    if supplied {
        let (Some(kind), Some(start), Some(end), Some(interval)) =
            (&args.schedule_type, &args.start, &args.end, &args.interval)
        else {
            return Err(not_runnable(
                "`type`, `interval`, `start`, and `end` are required parameters for schedule creation.",
            ));
        };
        let mut config = Map::new();
        let capitalized = match kind.as_str() {
            "cron" => "Cron",
            "daily" => "Daily",
            "weekly" => "Weekly",
            _ => return Err(invalid("Invalid schedule type")),
        };
        config.insert("type".into(), json!(capitalized));
        match kind.as_str() {
            "cron" => {
                if interval.is_empty() || !interval.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(not_runnable(format!(
                        "Invalid format for interval: {interval}. Must be an integer."
                    )));
                }
                let number = interval.parse::<u64>().map_err(|_| {
                    not_runnable("Schedule interval exceeds the JSON integer bound")
                })?;
                config.insert("interval".into(), json!(number));
            }
            "daily" | "weekly" => {
                let times: Vec<_> = interval.trim_matches(['[', ']']).split(',').collect();
                for time in &times {
                    let bytes = time.as_bytes();
                    if bytes.len() < 5
                        || bytes[2] != b':'
                        || ![0, 1, 3, 4].iter().all(|&i| bytes[i].is_ascii_digit())
                    {
                        return Err(not_runnable(format!(
                            "Invalid time format: {time}. Must be in the format HH:mm"
                        )));
                    }
                }
                config.insert("times".into(), json!(times));
                if kind == "weekly" {
                    let days = args
                        .days
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            not_runnable("Weekly schedule requires the days parameter.")
                        })?;
                    let days: Vec<_> = days.trim_matches(['[', ']']).split(',').collect();
                    for day in &days {
                        if ![
                            "Monday",
                            "Tuesday",
                            "Wednesday",
                            "Thursday",
                            "Friday",
                            "Saturday",
                            "Sunday",
                        ]
                        .contains(day)
                        {
                            return Err(not_runnable(format!(
                                "Invalid day: {day}. Must be one of Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday"
                            )));
                        }
                    }
                    config.insert("weekdays".into(), json!(days));
                }
            }
            _ => unreachable!(),
        }
        for (key, value) in [("startDateTime", start), ("endDateTime", end)] {
            let b = value.strip_suffix('\n').unwrap_or(value).as_bytes();
            if b.len() != 19
                || !b.iter().enumerate().all(|(i, c)| match i {
                    4 | 7 => *c == b'-',
                    10 => *c == b'T',
                    13 | 16 => *c == b':',
                    _ => c.is_ascii_digit(),
                })
            {
                return Err(not_runnable(format!(
                    "Invalid timestamp format: {value}. Must be in the format yyyy-MM-ddTHH:mm:ss"
                )));
            }
            config.insert(key.into(), json!(value));
        }
        config.insert("localTimeZoneId".into(), json!("UTC"));
        Ok(Some(Value::Object(config)))
    } else if args.enable || (update && args.disable) {
        Ok(None)
    } else {
        Err(not_runnable(
            "Schedule configuration is required for schedule creation.",
        ))
    }
}

pub(crate) fn schedule_payload(config: Value, enabled: bool) -> Result<Value, CommandError> {
    let contains = |key: &str| -> Result<bool, CommandError> {
        match &config {
            Value::Object(values) => Ok(values.contains_key(key)),
            Value::Array(values) => Ok(values.iter().any(|value| value.as_str() == Some(key))),
            Value::String(value) => Ok(value.contains(key)),
            other => Err(CommandError::unexpected(Some(format!(
                "argument of type '{}' is not iterable",
                match other {
                    Value::Null => "NoneType",
                    Value::Bool(_) => "bool",
                    Value::Number(number) if number.is_i64() || number.is_u64() => "int",
                    _ => "float",
                }
            )))),
        }
    };
    if contains("enabled")? && contains("configuration")? {
        Ok(config)
    } else {
        Ok(json!({"configuration": config, "enabled": enabled}))
    }
}
