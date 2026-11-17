// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
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
    let (ticks, offset) = datetime_value(value)?;
    if offset.is_some() {
        return Err("offset datetime does not match the naive output column");
    }
    Ok(ticks)
}

pub(crate) fn offset_datetime(value: &NativeValue, expected: i32) -> Result<i64, &'static str> {
    let (ticks, offset) = datetime_value(value)?;
    if offset != Some(expected) {
        return Err("datetime offset does not match the fixed output column timezone");
    }
    Ok(ticks)
}

pub(crate) fn datetime_value(value: &NativeValue) -> Result<(i64, Option<i32>), &'static str> {
    let (value, offset_required) = match value {
        NativeValue::DateTime(value) => (value, false),
        NativeValue::DateTimeOffset(value) => (value, true),
        _ => return Err("expected native datetime"),
    };
    let (local, aware) = match value.strip_suffix('Z') {
        Some(local) => (local, true),
        None => match value.get(value.len().saturating_sub(6)..) {
            Some(offset) if offset.starts_with(['+', '-']) => {
                if offset == "-00:00" {
                    return Err("unknown RFC3339 offset cannot be represented as UTC");
                }
                (&value[..value.len() - 6], true)
            }
            _ => (value.as_str(), false),
        },
    };
    if local.len() < 19 || local.as_bytes().get(10) != Some(&b'T') {
        return Err("expected native datetime");
    }
    fraction(local)?;
    if aware {
        let parsed =
            DateTime::parse_from_rfc3339(value).map_err(|_| "invalid native datetime offset")?;
        if parsed.nanosecond() >= 1_000_000_000 {
            return Err("leap seconds cannot be represented without normalization");
        }
        let ticks = parsed
            .timestamp_nanos_opt()
            .ok_or("datetime exceeds exact Arrow nanosecond range")?;
        return Ok((ticks, Some(parsed.offset().local_minus_utc())));
    }
    if offset_required {
        return Err("native DateTimeOffset requires an explicit offset");
    }
    let parsed = NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| "invalid naive native datetime")?;
    if parsed.nanosecond() >= 1_000_000_000 {
        return Err("leap seconds cannot be represented without normalization");
    }
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .ok_or("invalid naive epoch")?;
    let ticks = parsed
        .signed_duration_since(epoch)
        .num_nanoseconds()
        .ok_or("datetime exceeds exact Arrow nanosecond range")?;
    Ok((ticks, None))
}

pub(crate) fn timezone(offset: i32) -> String {
    if offset == 0 {
        return "UTC".into();
    }
    let seconds = offset.unsigned_abs();
    format!(
        "{}{:02}:{:02}",
        if offset < 0 { '-' } else { '+' },
        seconds / 3600,
        seconds % 3600 / 60
    )
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

struct DecimalParts<'a> {
    negative: bool,
    integer: &'a str,
    fraction: &'a str,
    exponent: i64,
}

impl DecimalParts<'_> {
    fn scale(&self) -> Result<i64, &'static str> {
        i64::try_from(self.fraction.len())
            .map_err(|_| "decimal scale overflow")?
            .checked_sub(self.exponent)
            .ok_or("decimal scale overflow")
    }

    fn coefficient_digits(&self) -> usize {
        self.integer
            .bytes()
            .chain(self.fraction.bytes())
            .skip_while(|digit| *digit == b'0')
            .count()
    }
}

fn decimal_parts(value: &NativeValue) -> Result<DecimalParts<'_>, &'static str> {
    let NativeValue::Decimal(text) = value else {
        return Err("expected native decimal");
    };
    let (negative, text) = match text.strip_prefix('-') {
        Some(text) => (true, text),
        None => (false, text),
    };
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
                return Err("invalid exact decimal exponent");
            }
            (
                mantissa,
                exponent
                    .parse::<i64>()
                    .map_err(|_| "decimal exponent exceeds supported range")?,
            )
        }
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
    Ok(DecimalParts {
        negative,
        integer,
        fraction,
        exponent,
    })
}

pub(crate) fn decimal_layout(value: &NativeValue) -> Result<(i64, usize), &'static str> {
    let parts = decimal_parts(value)?;
    Ok((parts.scale()?, parts.coefficient_digits()))
}

pub(crate) fn money<T: TryFrom<i128>>(
    value: &NativeValue,
    precision: u8,
) -> Result<i128, &'static str> {
    let coefficient = decimal(value, precision, 4)?;
    T::try_from(coefficient).map_err(|_| "monetary value exceeds declared scaled integer range")?;
    Ok(coefficient)
}

pub(crate) fn decimal(value: &NativeValue, precision: u8, scale: i8) -> Result<i128, &'static str> {
    let parts = decimal_parts(value)?;
    if parts.coefficient_digits() == 0 {
        return Ok(0);
    }
    let shift = i64::from(scale)
        .checked_sub(parts.scale()?)
        .ok_or("decimal scale overflow")?;
    let mut digits = String::with_capacity(parts.integer.len() + parts.fraction.len());
    digits.push_str(parts.integer);
    digits.push_str(parts.fraction);
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
    Ok(if parts.negative {
        -coefficient
    } else {
        coefficient
    })
}
