// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Export-only Python JSON parsing; formatting reuses the shared native encoder.

use super::{DefinitionErrorKind as Kind, ExportStorage};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::{
    DecimalInteger, Format, Indent, MAX_NATIVE_DEPTH, MAX_NATIVE_NODES, Options, PythonDict,
    PythonString, PythonValue, PythonValueError, encode_python_with_options,
};
use serde_json::Value;

pub(super) fn payload(
    original: &Value,
    path: &str,
    notebook: bool,
    storage: ExportStorage,
    remaining: &mut usize,
) -> Result<(Vec<u8>, bool), Kind> {
    let mut value = PythonValue::try_from_json(original).map_err(native_error)?;
    if let Value::String(encoded) = original
        && !encoded.is_empty()
    {
        if encoded.len() / 4 * 3 > remaining.saturating_add(2) {
            return Err(Kind::Limit);
        }
        let decoded = match STANDARD.decode(encoded) {
            Ok(bytes) => Some(bytes),
            Err(base64::DecodeError::InvalidLength(_) | base64::DecodeError::InvalidPadding)
                if !encoded.contains('=')
                    && encoded.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
                    }) =>
            {
                None
            }
            Err(_) => return Err(Kind::InvalidDefinition),
        };
        if let Some(bytes) = decoded {
            if STANDARD.encode(&bytes) != *encoded {
                return Err(Kind::InvalidDefinition);
            }
            *remaining = remaining.checked_sub(bytes.len()).ok_or(Kind::Limit)?;
            let Ok(text) = std::str::from_utf8(&bytes) else {
                if notebook {
                    return Err(Kind::InvalidDefinition);
                }
                return Ok((bytes, false));
            };
            if [".json", ".ipynb", ".pbir", ".platform", ".pbism"]
                .iter()
                .any(|suffix| path.ends_with(suffix))
            {
                if let Some(parsed) = parse(text, true)? {
                    value = parsed;
                }
                // A JSON decode error leaves the original base64 payload.
            } else {
                value = PythonValue::String(PythonString::from_text(text).map_err(native_error)?);
            }
        }
        // A base64 decode error also leaves the original payload, never a
        // permissively decoded or silently omitted part.
    }
    if notebook {
        value = clean_notebook(value)?;
    }
    Ok((store(&value, storage)?, true))
}

fn native_error(error: PythonValueError) -> Kind {
    match error {
        PythonValueError::Limit { .. } => Kind::Limit,
        _ => Kind::InvalidDefinition,
    }
}

fn key_is(key: &PythonString, expected: &str) -> bool {
    key.codepoints()
        .iter()
        .copied()
        .eq(expected.chars().map(u32::from))
}

fn clean_notebook(value: PythonValue) -> Result<PythonValue, Kind> {
    let dict = match value {
        PythonValue::Dict(dict) => dict,
        PythonValue::String(ref text) => {
            if text
                .codepoints()
                .windows(5)
                .any(|window| window == [99, 101, 108, 108, 115])
            {
                return Err(Kind::InvalidDefinition);
            }
            return Ok(value);
        }
        PythonValue::List(ref values) => {
            if values
                .iter()
                .any(|value| matches!(value, PythonValue::String(text) if key_is(text, "cells")))
            {
                return Err(Kind::InvalidDefinition);
            }
            return Ok(value);
        }
        _ => return Err(Kind::InvalidDefinition),
    };
    let mut entries = dict.entries().to_vec();
    for (key, value) in &mut entries {
        if !key_is(key, "cells") {
            continue;
        }
        let PythonValue::List(cells) = value else {
            return Err(Kind::InvalidDefinition);
        };
        for cell in cells {
            let PythonValue::Dict(dict) = cell else {
                return Err(Kind::InvalidDefinition);
            };
            let entries = dict.entries().iter().map(|(key, value)| {
                (
                    key.clone(),
                    if key_is(key, "outputs") {
                        PythonValue::List(Vec::new())
                    } else {
                        value.clone()
                    },
                )
            });
            *dict = PythonDict::from_entries(entries).map_err(native_error)?;
        }
    }
    Ok(PythonValue::Dict(
        PythonDict::from_entries(entries).map_err(native_error)?,
    ))
}

fn store(value: &PythonValue, storage: ExportStorage) -> Result<Vec<u8>, Kind> {
    let options = Options {
        format: if storage == ExportStorage::Local {
            Format::Pretty(Indent::Four)
        } else {
            Format::Default
        },
        ensure_ascii: storage == ExportStorage::Lakehouse,
    };
    let encoded = match value {
        PythonValue::String(text) => {
            let text = text.try_to_string().map_err(native_error)?;
            match parse(&text, false)? {
                Some(value) => encode_python_with_options(&value, options).map_err(native_error)?,
                None => text,
            }
        }
        PythonValue::Dict(_) | PythonValue::List(_) => {
            encode_python_with_options(value, options).map_err(native_error)?
        }
        _ if storage == ExportStorage::Lakehouse => return Err(Kind::InvalidDefinition),
        PythonValue::None => "None".into(),
        PythonValue::Bool(true) => "True".into(),
        PythonValue::Bool(false) => "False".into(),
        PythonValue::Integer(integer) => integer.as_str().into(),
        PythonValue::Float(value) if value.is_nan() => "nan".into(),
        PythonValue::Float(value) if *value == f64::INFINITY => "inf".into(),
        PythonValue::Float(value) if *value == f64::NEG_INFINITY => "-inf".into(),
        PythonValue::Float(_) => {
            encode_python_with_options(value, options).map_err(native_error)?
        }
    };
    Ok(encoded.into_bytes())
}

#[derive(Debug)]
enum ParseError {
    Syntax,
    Native(PythonValueError),
    Limit,
}

impl From<PythonValueError> for ParseError {
    fn from(error: PythonValueError) -> Self {
        Self::Native(error)
    }
}

fn parse(text: &str, decoder_fallback: bool) -> Result<Option<PythonValue>, Kind> {
    let mut parser = Parser {
        text,
        offset: 0,
        nodes: 0,
    };
    let result = parser.value(0).and_then(|value| {
        parser.whitespace();
        if parser.offset == text.len() {
            Ok(value)
        } else {
            Err(ParseError::Syntax)
        }
    });
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ParseError::Syntax) => Ok(None),
        Err(ParseError::Native(PythonValueError::Limit {
            kind: fabric_core_json::LimitKind::DecimalDigits,
            ..
        })) if decoder_fallback => Ok(None),
        Err(ParseError::Native(error)) => Err(native_error(error)),
        Err(ParseError::Limit) => Err(Kind::Limit),
    }
}

struct Parser<'a> {
    text: &'a str,
    offset: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\r' | b'\n' | b'\t'))
        {
            self.offset += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.offset).copied()
    }

    fn take(&mut self, token: &str) -> bool {
        if self.text[self.offset..].starts_with(token) {
            self.offset += token.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<PythonValue, ParseError> {
        self.whitespace();
        self.nodes += 1;
        if self.nodes > MAX_NATIVE_NODES || depth > MAX_NATIVE_DEPTH {
            return Err(ParseError::Limit);
        }
        match self.peek() {
            Some(b'"') => Ok(PythonValue::String(self.string()?)),
            Some(b'[') => self.list(depth),
            Some(b'{') => self.dict(depth),
            Some(b'n') if self.take("null") => Ok(PythonValue::None),
            Some(b't') if self.take("true") => Ok(PythonValue::Bool(true)),
            Some(b'f') if self.take("false") => Ok(PythonValue::Bool(false)),
            Some(b'N') if self.take("NaN") => Ok(PythonValue::Float(f64::NAN)),
            Some(b'I') if self.take("Infinity") => Ok(PythonValue::Float(f64::INFINITY)),
            Some(b'-') if self.take("-Infinity") => Ok(PythonValue::Float(f64::NEG_INFINITY)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(ParseError::Syntax),
        }
    }

    fn string(&mut self) -> Result<PythonString, ParseError> {
        if !self.take("\"") {
            return Err(ParseError::Syntax);
        }
        let mut points = Vec::new();
        loop {
            let character = self.text[self.offset..]
                .chars()
                .next()
                .ok_or(ParseError::Syntax)?;
            self.offset += character.len_utf8();
            match character {
                '"' => return PythonString::from_codepoints(points).map_err(Into::into),
                '\\' => {
                    let escape = self.peek().ok_or(ParseError::Syntax)?;
                    self.offset += 1;
                    let point = match escape {
                        b'"' | b'\\' | b'/' => u32::from(escape),
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => {
                            let high = self.hex()?;
                            if (0xd800..=0xdbff).contains(&high)
                                && self.text[self.offset..].starts_with("\\u")
                            {
                                let saved = self.offset;
                                self.offset += 2;
                                match self.hex() {
                                    Ok(low) if (0xdc00..=0xdfff).contains(&low) => {
                                        0x10000 + ((high - 0xd800) << 10) + low - 0xdc00
                                    }
                                    _ => {
                                        self.offset = saved;
                                        high
                                    }
                                }
                            } else {
                                high
                            }
                        }
                        _ => return Err(ParseError::Syntax),
                    };
                    points.push(point);
                }
                character if character < '\u{20}' => return Err(ParseError::Syntax),
                character => points.push(u32::from(character)),
            }
            if points.len() > fabric_core_json::MAX_STRING_CODEPOINTS {
                return Err(ParseError::Limit);
            }
        }
    }

    fn hex(&mut self) -> Result<u32, ParseError> {
        let bytes = self
            .text
            .as_bytes()
            .get(self.offset..self.offset + 4)
            .ok_or(ParseError::Syntax)?;
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            return Err(ParseError::Syntax);
        }
        self.offset += 4;
        let mut value = 0;
        for byte in bytes {
            value = value * 16 + char::from(*byte).to_digit(16).ok_or(ParseError::Syntax)?;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<PythonValue, ParseError> {
        let start = self.offset;
        self.take("-");
        if self.take("0") {
        } else {
            if !self.peek().is_some_and(|byte| matches!(byte, b'1'..=b'9')) {
                return Err(ParseError::Syntax);
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.offset += 1;
            }
        }
        let mut float = false;
        if self.take(".") {
            float = true;
            self.digits()?;
        }
        if self.take("e") || self.take("E") {
            float = true;
            if !self.take("+") {
                self.take("-");
            }
            self.digits()?;
        }
        let token = &self.text[start..self.offset];
        if float {
            Ok(PythonValue::Float(
                token.parse().map_err(|_| ParseError::Syntax)?,
            ))
        } else {
            Ok(PythonValue::Integer(DecimalInteger::parse(token)?))
        }
    }

    fn digits(&mut self) -> Result<(), ParseError> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        if start == self.offset {
            Err(ParseError::Syntax)
        } else {
            Ok(())
        }
    }

    fn list(&mut self, depth: usize) -> Result<PythonValue, ParseError> {
        self.offset += 1;
        self.whitespace();
        let mut values = Vec::new();
        if self.take("]") {
            return Ok(PythonValue::List(values));
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.whitespace();
            if self.take("]") {
                return Ok(PythonValue::List(values));
            }
            if !self.take(",") {
                return Err(ParseError::Syntax);
            }
        }
    }

    fn dict(&mut self, depth: usize) -> Result<PythonValue, ParseError> {
        self.offset += 1;
        self.whitespace();
        let mut entries = Vec::new();
        if self.take("}") {
            return Ok(PythonValue::Dict(PythonDict::default()));
        }
        loop {
            self.whitespace();
            let key = self.string()?;
            self.whitespace();
            if !self.take(":") {
                return Err(ParseError::Syntax);
            }
            entries.push((key, self.value(depth + 1)?));
            self.whitespace();
            if self.take("}") {
                return Ok(PythonValue::Dict(PythonDict::from_entries(entries)?));
            }
            if !self.take(",") {
                return Err(ParseError::Syntax);
            }
        }
    }
}
