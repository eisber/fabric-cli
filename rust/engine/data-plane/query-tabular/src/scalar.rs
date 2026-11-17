// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use chrono::{DateTime, Timelike};
use serde_json::Value;

pub(crate) fn guid(value: &Value) -> Result<&str, &'static str> {
    let value = text(value)?;
    if value.len() != 36 || uuid::Uuid::parse_str(value).is_err() {
        return Err("expected hyphenated GUID");
    }
    Ok(value)
}

pub(crate) fn signed(value: &Value) -> Result<i64, &'static str> {
    value.as_i64().ok_or("expected exact signed integer")
}

pub(crate) fn int32(value: &Value) -> Result<i32, &'static str> {
    i32::try_from(signed(value)?).map_err(|_| "integer exceeds declared width")
}

pub(crate) fn boolean(value: &Value) -> Result<bool, &'static str> {
    value.as_bool().ok_or("expected boolean")
}

pub(crate) fn text(value: &Value) -> Result<&str, &'static str> {
    value.as_str().ok_or("expected UTF-8 string")
}

pub(crate) fn real(value: &Value) -> Result<f64, &'static str> {
    let number = value.as_f64().ok_or("expected finite real")?;
    if !number.is_finite()
        || value
            .as_i64()
            .is_some_and(|v| number as i128 != i128::from(v))
        || value
            .as_u64()
            .is_some_and(|v| number as i128 != i128::from(v))
    {
        return Err("real is non-finite or integer conversion would round");
    }
    Ok(number)
}

pub(crate) fn datetime(value: &Value) -> Result<i64, &'static str> {
    let value = text(value)?;
    // Native Kusto datetime is UTC with at most seven fractional digits.
    // Reject offsets rather than silently discarding their original timezone.
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(10) != Some(&b'T')
        || !value.ends_with('Z')
        || !matches!(bytes.len(), 20 | 22..=28)
        || (bytes.len() > 20
            && (bytes.get(19) != Some(&b'.')
                || !bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit)))
    {
        return Err("expected UTC datetime with at most 100-nanosecond precision");
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| "invalid UTC datetime")?;
    if parsed.nanosecond() >= 1_000_000_000 {
        return Err("leap seconds are not representable without normalization");
    }
    parsed
        .timestamp_nanos_opt()
        .ok_or("datetime exceeds exact Arrow nanosecond range")
}

pub(crate) fn timespan(value: &Value) -> Result<i64, &'static str> {
    let value = text(value)?;
    timespan_text(value)
}

pub(crate) fn timespan_text(value: &str) -> Result<i64, &'static str> {
    let (negative, value) = match value.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, value),
    };
    let invalid = "expected native [-][d.]hh:mm:ss[.fffffff] timespan";
    let (hours, rest) = value.split_once(':').ok_or(invalid)?;
    let (minutes, seconds) = rest.split_once(':').ok_or(invalid)?;
    let (days, hours) = match hours.split_once('.') {
        Some((days, hours)) => (digits(days)?, hours),
        None => (0, hours),
    };
    let (seconds, fraction) = match seconds.split_once('.') {
        Some((seconds, fraction)) => (seconds, fraction),
        None => (seconds, ""),
    };
    if hours.len() != 2
        || minutes.len() != 2
        || seconds.len() != 2
        || fraction.len() > 7
        || (value.ends_with('.') && fraction.is_empty())
    {
        return Err(invalid);
    }
    let (hours, minutes, seconds) = (digits(hours)?, digits(minutes)?, digits(seconds)?);
    if hours >= 24 || minutes >= 60 || seconds >= 60 {
        return Err(invalid);
    }
    let fractional_ns = if fraction.is_empty() {
        0
    } else {
        digits(fraction)? * 10i128.pow(9 - fraction.len() as u32)
    };
    let ns = days
        .checked_mul(86_400)
        .and_then(|v| v.checked_add(hours * 3600 + minutes * 60 + seconds))
        .and_then(|v| v.checked_mul(1_000_000_000))
        .and_then(|v| v.checked_add(fractional_ns))
        .ok_or("timespan exceeds exact Arrow nanosecond range")?;
    i64::try_from(if negative { -ns } else { ns })
        .map_err(|_| "timespan exceeds exact Arrow nanosecond range")
}

fn digits(value: &str) -> Result<i128, &'static str> {
    if value.is_empty() || !value.bytes().all(|v| v.is_ascii_digit()) {
        return Err("expected unsigned decimal digits");
    }
    value.parse().map_err(|_| "numeric component exceeds range")
}
