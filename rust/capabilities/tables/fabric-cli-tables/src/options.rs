// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::CommandError;
use fabric_sdk_tables::{LoadMode, LoadOptions, Maintenance};
use serde_json::Value;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

pub(crate) fn error(code: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some(code.into()))
}

/// Translate Python's comma-separated format argument, not a CSV parser.
///
/// Quotes are stripped only around individual values. Duplicate keys use the
/// last value. Boolean-looking values become booleans even for the delimiter.
pub fn load_options(
    mode: &str,
    format: Option<&str>,
    extension: Option<&str>,
) -> Result<LoadOptions, CommandError> {
    let mode = match mode {
        "overwrite" => LoadMode::Overwrite,
        "append" => LoadMode::Append,
        _ => return Err(error("InvalidInput", "Invalid table load mode")),
    };
    let mut options = LoadOptions::default();
    if let Some(format) = format.filter(|value| !value.is_empty()) {
        let mut values = BTreeMap::new();
        for part in format.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = part.split_once('=').ok_or_else(|| {
                error(
                    "InvalidInput",
                    format!("Invalid format argument: '{part}' (missing '=')"),
                )
            })?;
            let key = key.trim();
            if !matches!(key, "format" | "header" | "delimiter") {
                return Err(error(
                    "InvalidInput",
                    format!(
                        "Invalid key: '{key}'. Allowed keys are: {{'format', 'header', 'delimiter'}}"
                    ),
                ));
            }
            let mut value = value.trim();
            if (value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"'))
            {
                value = if value.len() == 1 {
                    ""
                } else {
                    &value[1..value.len() - 1]
                };
            }
            let value = match value.to_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::String(value.into()),
            };
            values.insert(key, value);
        }
        let value = values.get("format").cloned().unwrap_or("".into());
        let value = value.as_str().ok_or_else(|| {
            CommandError::unexpected(Some("'bool' object has no attribute 'capitalize'".into()))
        })?;
        let mut chars = value.chars();
        let format = match chars.next() {
            Some(first) => {
                first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
            }
            None => String::new(),
        };
        options = match format.as_str() {
            "Csv" => LoadOptions::csv(values.remove("header"), values.remove("delimiter")),
            "Parquet" => LoadOptions::parquet(),
            _ => {
                return Err(error(
                    "UnknownError",
                    format!("Invalid format: '{format}'. Allowed formats are: 'Csv', 'Parquet'"),
                ));
            }
        };
    }
    options.mode = mode;
    options.extension = extension.map(str::to_owned);
    Ok(options)
}

/// Translate signed integer hours using Python's floor division, without a new minimum.
///
/// Decimal string arithmetic avoids narrowing Python's integers to a machine
/// integer. There is no dry-run flag in the pinned Python parser.
pub fn vacuum(retain_n_hours: &str) -> Result<Maintenance, CommandError> {
    vacuum_with_digit_limit(retain_n_hours, NonZeroUsize::new(4300))
}

/// Translate retention with an explicit Python integer-string conversion limit.
///
/// The default [`vacuum`] uses the oracle's 4300 decimal digits. `None` models a
/// caller that explicitly disabled Python's limit; this never reads process globals.
/// Decimal character semantics are pinned to the oracle's Unicode 15.1 data.
pub fn vacuum_with_digit_limit(
    retain_n_hours: &str,
    max_digits: Option<NonZeroUsize>,
) -> Result<Maintenance, CommandError> {
    let value = retain_n_hours.trim();
    let digits = if value.starts_with(['+', '-']) {
        &value[1..]
    } else {
        value
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits
            .chars()
            .all(|c| crate::decimal::digit(c).is_some() || c == '_')
    {
        return Err(CommandError::unexpected(Some(format!(
            "invalid literal for int() with base 10: '{retain_n_hours}'"
        ))));
    }
    let digit_count = digits.chars().filter(|ch| *ch != '_').count();
    if let Some(limit) = max_digits
        && digit_count > limit.get()
    {
        return Err(CommandError::unexpected(Some(format!(
            "Exceeds the limit ({} digits) for integer string conversion: value has {digit_count} digits; use sys.set_int_max_str_digits() to increase the limit",
            limit.get()
        ))));
    }
    let mut days = String::new();
    let mut remainder = 0_u16;
    for digit in digits.chars().filter_map(crate::decimal::digit) {
        let current = remainder * 10 + u16::from(digit);
        let quotient = current / 24;
        if quotient != 0 || !days.is_empty() {
            days.push(char::from(b'0' + quotient as u8));
        }
        remainder = current % 24;
    }
    if days.is_empty() {
        days.push('0');
    }
    if value.starts_with('-') && remainder != 0 {
        let mut bytes = days.into_bytes();
        let mut carry = true;
        for digit in bytes.iter_mut().rev() {
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            bytes.insert(0, b'1');
        }
        days = String::from_utf8(bytes).expect("decimal ASCII");
        remainder = 24 - remainder;
    }
    if value.starts_with('-') && days != "0" {
        days.insert(0, '-');
    }
    Ok(Maintenance::Vacuum {
        retention_period: format!("{days}:{remainder:02}:00:00"),
    })
}

/// Preserve Python's whitespace trimming and empty comma-separated Z-Order entries.
pub fn optimize(vorder: bool, zorder: Option<&str>) -> Maintenance {
    Maintenance::Optimize {
        vorder,
        zorder: zorder
            .filter(|value| !value.is_empty())
            .map(|value| value.split(',').map(|v| v.trim().into()).collect()),
    }
}
