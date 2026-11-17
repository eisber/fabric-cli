// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private exact-span decoding without changing serde_json's workspace features.

use crate::QueryErrorKind;
use serde::{
    Deserialize, Deserializer,
    de::{
        self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor,
    },
    forward_to_deserialize_any,
};
use std::{collections::BTreeSet, fmt};

const RAW_SPAN: &str = "__fabric_sdk_aihub_query_exact_span_v1";
type Error = de::value::Error;
type Result<T> = std::result::Result<T, Error>;

pub(crate) struct RawValue(Box<str>);

impl RawValue {
    pub(crate) fn get(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RawValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct RawVisitor;
        impl Visitor<'_> for RawVisitor {
            type Value = RawValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an SDK-private exact JSON span")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<RawValue, E> {
                Ok(RawValue(value.into()))
            }
        }
        deserializer.deserialize_newtype_struct(RAW_SPAN, RawVisitor)
    }
}

pub(crate) fn parse<T: DeserializeOwned>(text: &str) -> std::result::Result<T, QueryErrorKind> {
    let mut syntax = serde_json::Deserializer::from_str(text);
    de::IgnoredAny::deserialize(&mut syntax).map_err(|_| QueryErrorKind::Protocol)?;
    syntax.end().map_err(|_| QueryErrorKind::Protocol)?;
    let text = text.trim();
    validate(Span(text), 0, &mut 0)?;
    T::deserialize(Span(text)).map_err(|_| QueryErrorKind::Protocol)
}

#[derive(Clone, Copy)]
struct Span<'a>(&'a str);

fn invalid() -> Error {
    de::Error::custom("invalid exact JSON shape")
}

// Syntax has already been checked by serde's IgnoredAny parser. This only finds
// the original token's boundary; it never parses numbers or copies a subtree.
fn token(text: &str) -> Result<(&str, &str)> {
    let text = text.trim_start();
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
                if depth == 0 {
                    return Ok((&text[..=index], &text[index + 1..]));
                }
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&text[..=index], &text[index + 1..]));
                }
            }
            b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n' if depth == 0 => {
                return Ok((&text[..index], &text[index..]));
            }
            _ => {}
        }
    }
    if !text.is_empty() && !quoted && depth == 0 {
        Ok((text, ""))
    } else {
        Err(invalid())
    }
}

struct Sequence<'a> {
    remaining: &'a str,
}

impl<'a> Sequence<'a> {
    fn next_span(&mut self) -> Result<Option<Span<'a>>> {
        self.remaining = self.remaining.trim_start();
        if self.remaining.is_empty() {
            return Ok(None);
        }
        let (value, rest) = token(self.remaining)?;
        self.remaining = rest
            .trim_start()
            .strip_prefix(',')
            .unwrap_or(rest.trim_start());
        Ok(Some(Span(value)))
    }
}

impl<'de> SeqAccess<'de> for Sequence<'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        self.next_span()?
            .map(|span| seed.deserialize(span))
            .transpose()
    }
}

struct Mapping<'a> {
    remaining: &'a str,
    value: Option<Span<'a>>,
}

impl<'a> Mapping<'a> {
    fn next_pair(&mut self) -> Result<Option<(Span<'a>, Span<'a>)>> {
        self.remaining = self.remaining.trim_start();
        if self.remaining.is_empty() {
            return Ok(None);
        }
        let (key, rest) = token(self.remaining)?;
        let rest = rest.trim_start().strip_prefix(':').ok_or_else(invalid)?;
        let (value, rest) = token(rest)?;
        self.remaining = rest
            .trim_start()
            .strip_prefix(',')
            .unwrap_or(rest.trim_start());
        Ok(Some((Span(key), Span(value))))
    }
}

impl<'de> MapAccess<'de> for Mapping<'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        let Some((key, value)) = self.next_pair()? else {
            return Ok(None);
        };
        self.value = Some(value);
        seed.deserialize(key).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(self.value.take().ok_or_else(invalid)?)
    }
}

fn validate(
    span: Span<'_>,
    depth: usize,
    nodes: &mut usize,
) -> std::result::Result<(), QueryErrorKind> {
    *nodes += 1;
    if depth > 64 || *nodes > 2_100_000 {
        return Err(QueryErrorKind::Limit);
    }
    match span.0.as_bytes().first() {
        Some(b'{') => {
            let mut fields = Mapping {
                remaining: &span.0[1..span.0.len() - 1],
                value: None,
            };
            let mut names = BTreeSet::new();
            while let Some((key, value)) =
                fields.next_pair().map_err(|_| QueryErrorKind::Protocol)?
            {
                let name: String =
                    serde_json::from_str(key.0).map_err(|_| QueryErrorKind::Protocol)?;
                if !names.insert(name) {
                    return Err(QueryErrorKind::Protocol);
                }
                validate(value, depth + 1, nodes)?;
            }
        }
        Some(b'[') => {
            let mut values = Sequence {
                remaining: &span.0[1..span.0.len() - 1],
            };
            while let Some(value) = values.next_span().map_err(|_| QueryErrorKind::Protocol)? {
                validate(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl<'de> Deserializer<'de> for Span<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.0.as_bytes().first() {
            Some(b'{') => self.deserialize_map(visitor),
            Some(b'[') => self.deserialize_seq(visitor),
            Some(b'"') => self.deserialize_string(visitor),
            Some(b't') if self.0 == "true" => visitor.visit_bool(true),
            Some(b'f') if self.0 == "false" => visitor.visit_bool(false),
            Some(b'n') if self.0 == "null" => visitor.visit_unit(),
            Some(b'-') => visitor.visit_i64(self.0.parse().map_err(|_| invalid())?),
            Some(b'0'..=b'9') => visitor.visit_u64(self.0.parse().map_err(|_| invalid())?),
            _ => Err(invalid()),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let value: String = serde_json::from_str(self.0).map_err(|_| invalid())?;
        visitor.visit_string(value)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.0 == "null" {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        if name == RAW_SPAN {
            visitor.visit_borrowed_str(self.0)
        } else {
            visitor.visit_newtype_struct(self)
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if !self.0.starts_with('[') {
            return Err(invalid());
        }
        visitor.visit_seq(Sequence {
            remaining: &self.0[1..self.0.len() - 1],
        })
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if !self.0.starts_with('{') {
            return Err(invalid());
        }
        visitor.visit_map(Mapping {
            remaining: &self.0[1..self.0.len() - 1],
            value: None,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_map(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let value: String = serde_json::from_str(self.0).map_err(|_| invalid())?;
        visitor.visit_enum(value.into_deserializer())
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str
        bytes byte_buf unit unit_struct identifier
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn nested_near_limit_spans_borrow_input_without_subtree_copies() {
        let text = format!(
            "[{}\"{}\"{}]",
            "[".repeat(58),
            "x".repeat(30 * 1024 * 1024),
            "]".repeat(58)
        );
        let mut current = Span(text.as_str());
        let start = text.as_ptr() as usize;
        for _ in 0..59 {
            let mut sequence = Sequence {
                remaining: &current.0[1..current.0.len() - 1],
            };
            current = sequence.next_span().unwrap().unwrap();
            let pointer = current.0.as_ptr() as usize;
            assert!(pointer >= start && pointer + current.0.len() <= start + text.len());
        }
        let mut nodes = 0;
        validate(Span(&text), 0, &mut nodes).unwrap();
        assert_eq!(nodes, 60);
    }

    #[test]
    fn exact_spans_preserve_large_numbers_and_decoded_duplicate_keys_are_rejected() {
        for text in [
            "18446744073709551615",
            "-9007199254740993",
            "0.1000000000000000000000000001",
            "1.25e-400",
        ] {
            assert_eq!(parse::<RawValue>(text).unwrap().get(), text);
        }
        assert!(parse::<de::IgnoredAny>(r#"{"a":1,"\u0061":2}"#).is_err());
        assert!(parse::<de::IgnoredAny>("null true").is_err());
    }
}
