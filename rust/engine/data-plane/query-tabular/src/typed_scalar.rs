// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use fabric_sdk_aihub_query::NativeValue;

pub(crate) fn integer<T: TryFrom<i128>>(value: &NativeValue) -> Result<T, &'static str> {
    let NativeValue::Integer(text) = value else {
        return Err("expected native integer");
    };
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty()
        || !digits.bytes().all(|v| v.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return Err("invalid exact integer token");
    }
    let value = text
        .parse::<i128>()
        .map_err(|_| "integer exceeds declared width")?;
    T::try_from(value).map_err(|_| "integer exceeds declared width or sign")
}

fn number(value: &NativeValue) -> Result<&str, &'static str> {
    let NativeValue::Number(text) = value else {
        return Err("expected native approximate number");
    };
    if !text
        .as_bytes()
        .first()
        .is_some_and(|v| v.is_ascii_digit() || *v == b'-')
        || serde_json::from_str::<serde::de::IgnoredAny>(text).is_err()
    {
        return Err("invalid native numeric token");
    }
    Ok(text)
}

pub(crate) fn float64(value: &NativeValue) -> Result<f64, &'static str> {
    let text = number(value)?;
    let value = text.parse::<f64>().map_err(|_| "invalid native Float64")?;
    if !value.is_finite() || (value == 0.0 && nonzero_mantissa(text)) {
        return Err("non-finite or oversized native Float64");
    }
    Ok(value)
}

pub(crate) fn float32(value: &NativeValue) -> Result<f32, &'static str> {
    let text = number(value)?;
    let value = text.parse::<f32>().map_err(|_| "invalid native Float32")?;
    if !value.is_finite() || (value == 0.0 && nonzero_mantissa(text)) {
        return Err("non-finite or oversized native Float32");
    }
    Ok(value)
}

fn nonzero_mantissa(text: &str) -> bool {
    text.split(['e', 'E'])
        .next()
        .is_some_and(|mantissa| mantissa.bytes().any(|value| matches!(value, b'1'..=b'9')))
}

pub(crate) fn boolean(value: &NativeValue) -> Result<bool, &'static str> {
    match value {
        NativeValue::Boolean(value) => Ok(*value),
        _ => Err("expected native boolean"),
    }
}

pub(crate) fn text(value: &NativeValue) -> Result<&str, &'static str> {
    match value {
        NativeValue::Text(value) => Ok(value),
        _ => Err("expected native text"),
    }
}

pub(crate) fn guid(value: &NativeValue) -> Result<&str, &'static str> {
    let NativeValue::Guid(value) = value else {
        return Err("expected native GUID");
    };
    if value.len() != 36 || uuid::Uuid::parse_str(value).is_err() {
        return Err("invalid native GUID");
    }
    Ok(value)
}

pub(crate) fn binary(value: &NativeValue) -> Result<Vec<u8>, &'static str> {
    let NativeValue::Binary(value) = value else {
        return Err("expected native binary");
    };
    STANDARD.decode(value).map_err(|_| "invalid native base64")
}

pub(crate) fn date(value: &NativeValue) -> Result<i32, &'static str> {
    let NativeValue::Date(value) = value else {
        return Err("expected native date");
    };
    let value = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| "invalid native date")?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).ok_or("invalid date epoch")?;
    i32::try_from(value.signed_duration_since(epoch).num_days())
        .map_err(|_| "date exceeds Arrow Date32 range")
}

fn fraction(text: &str) -> Result<(), &'static str> {
    if let Some((_, digits)) = text.split_once('.')
        && (digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|v| v.is_ascii_digit()))
    {
        return Err("temporal value exceeds exact nanosecond precision or has an offset");
    }
    Ok(())
}

pub(crate) fn datetime(value: &NativeValue) -> Result<i64, &'static str> {
    let NativeValue::DateTime(value) = value else {
        return Err("expected native datetime");
    };
    if value.len() < 19 || value.as_bytes().get(10) != Some(&b'T') {
        return Err("expected naive native datetime");
    }
    fraction(value)?;
    let value = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| "offset-bearing datetime has no declared column timezone")?;
    if value.nanosecond() >= 1_000_000_000 {
        return Err("leap seconds cannot be represented without normalization");
    }
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .ok_or("invalid naive epoch")?;
    value
        .signed_duration_since(epoch)
        .num_nanoseconds()
        .ok_or("datetime exceeds exact Arrow nanosecond range")
}

pub(crate) fn time(value: &NativeValue) -> Result<i64, &'static str> {
    let NativeValue::Time(value) = value else {
        return Err("expected native time");
    };
    fraction(value)?;
    let value =
        NaiveTime::parse_from_str(value, "%H:%M:%S%.f").map_err(|_| "invalid native time")?;
    if value.nanosecond() >= 1_000_000_000 {
        return Err("leap seconds cannot be represented without normalization");
    }
    Ok(
        i64::from(value.num_seconds_from_midnight()) * 1_000_000_000
            + i64::from(value.nanosecond()),
    )
}

pub(crate) fn duration(value: &NativeValue) -> Result<i64, &'static str> {
    let NativeValue::Duration(value) = value else {
        return Err("expected native duration");
    };
    crate::scalar::timespan_text(value)
}

pub(crate) fn decimal_schema(native: &str) -> Result<(u8, i8), &'static str> {
    let args = native
        .strip_prefix("decimal(")
        .or_else(|| native.strip_prefix("numeric("))
        .and_then(|args| args.strip_suffix(')'))
        .ok_or("native schema supplies no explicit decimal precision and scale")?;
    let (precision, scale) = args
        .split_once(',')
        .ok_or("invalid decimal precision and scale")?;
    let precision = precision
        .trim()
        .parse::<u8>()
        .map_err(|_| "invalid decimal precision")?;
    let scale = scale
        .trim()
        .parse::<i8>()
        .map_err(|_| "invalid decimal scale")?;
    if precision == 0 || precision > 38 || scale < 0 || scale as u8 > precision {
        return Err("decimal precision or scale is unsupported by the shared portable codec");
    }
    Ok((precision, scale))
}

pub(crate) fn decimal(value: &NativeValue, precision: u8, scale: i8) -> Result<i128, &'static str> {
    let NativeValue::Decimal(text) = value else {
        return Err("expected native decimal");
    };
    let (negative, text) = match text.strip_prefix('-') {
        Some(text) => (true, text),
        None => (false, text),
    };
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent
                .parse::<i64>()
                .map_err(|_| "decimal exponent exceeds supported range")?,
        ),
        None => (text, 0),
    };
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((_, "")) => return Err("invalid exact decimal token"),
        Some(parts) => parts,
        None => (mantissa, ""),
    };
    if integer.is_empty()
        || (integer.len() > 1 && integer.starts_with('0'))
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|v| v.is_ascii_digit())
    {
        return Err("invalid exact decimal token");
    }
    let shift = i64::from(scale)
        .checked_sub(
            i64::try_from(fraction.len())
                .map_err(|_| "decimal scale overflow")?
                .checked_sub(exponent)
                .ok_or("decimal scale overflow")?,
        )
        .ok_or("decimal scale overflow")?;
    let mut digits = String::with_capacity(integer.len() + fraction.len());
    digits.push_str(integer);
    digits.push_str(fraction);
    if shift < 0 {
        let discard = usize::try_from(shift.checked_neg().ok_or("decimal scale overflow")?)
            .map_err(|_| "decimal scale overflow")?;
        if discard > digits.len() {
            if digits.bytes().any(|v| v != b'0') {
                return Err("decimal would require rounding");
            }
            return Ok(0);
        }
        let end = digits.len() - discard;
        if digits.as_bytes()[end..].iter().any(|v| *v != b'0') {
            return Err("decimal would require rounding");
        }
        digits.truncate(end);
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let append = usize::try_from(shift.max(0)).map_err(|_| "decimal precision overflow")?;
    if digits
        .len()
        .checked_add(append)
        .is_none_or(|length| length > usize::from(precision))
    {
        return Err("decimal exceeds declared precision");
    }
    let coefficient = digits
        .parse::<i128>()
        .map_err(|_| "decimal coefficient overflow")?
        .checked_mul(10i128.pow(append as u32))
        .ok_or("decimal coefficient overflow")?;
    Ok(if negative { -coefficient } else { coefficient })
}
