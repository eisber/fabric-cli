// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Lossless source numeric spelling, independent from numeric value bounds.

/// Return a Python Unicode decimal digit, not the broader numeric category.
///
/// These 68 ten-digit blocks are measured against the offline source oracle's
/// Unicode 15.1 database. Superscripts, fractions and non-ASCII signs are not
/// decimal digits. The caller still enforces its JSON/native value domain.
pub fn source_decimal_digit(value: char) -> Option<u8> {
    const STARTS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
        0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60,
        0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
        0x1e950, 0x1fbf0,
    ];
    let value = u32::from(value);
    let index = STARTS
        .partition_point(|start| *start <= value)
        .checked_sub(1)?;
    let digit = value - STARTS[index];
    (digit < 10).then_some(digit as u8)
}

/// Translate decimal digits without parsing through floating point.
///
/// Other characters remain unchanged so the caller can apply its exact integer,
/// float or regex grammar rather than broadening source syntax.
pub fn source_decimal_text(value: &str) -> String {
    value
        .chars()
        .map(|value| source_decimal_digit(value).map_or(value, |digit| char::from(b'0' + digit)))
        .collect()
}

/// Normalize Python's signed base-ten integer spelling without converting value.
///
/// Only decimal digits, one optional ASCII sign and underscores between digits
/// are accepted. Whitespace is trimmed. `None` is a lexical failure; a returned
/// string may still exceed i64/u64 or duration bounds, which callers must check.
pub fn source_integer_text(value: &str) -> Option<String> {
    let normalized = source_decimal_text(value.trim());
    let digits = normalized.strip_prefix(['+', '-']).unwrap_or(&normalized);
    let bytes = digits.as_bytes();
    if bytes.is_empty()
        || !bytes.iter().enumerate().all(|(index, byte)| {
            byte.is_ascii_digit()
                || (*byte == b'_'
                    && index > 0
                    && index + 1 < bytes.len()
                    && bytes[index - 1].is_ascii_digit()
                    && bytes[index + 1].is_ascii_digit())
        })
    {
        return None;
    }
    Some(normalized.replace('_', ""))
}
