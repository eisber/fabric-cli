// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, DefinitionLimits,
    NativeDefinitionPart, codec::Admission, warehouse::NativeMediaType,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::{collections::BTreeSet, fmt};

pub(crate) struct ParsedDefinition {
    pub(crate) format: Option<String>,
    pub(crate) parts: Vec<NativeDefinitionPart>,
    pub(crate) payloads: Vec<String>,
    pub(crate) media_types: Vec<NativeMediaType>,
    pub(crate) unknown_fields: bool,
}

struct State {
    admission: Admission,
    limits: DefinitionLimits,
    failure: Option<DefinitionError>,
    part_index: Option<usize>,
    warehouse: bool,
    media_types: Vec<NativeMediaType>,
    unknown_fields: bool,
}

impl State {
    fn record<E: de::Error>(&mut self, error: DefinitionError) -> E {
        self.failure = Some(error);
        E::custom("SQLDatabase definition rejected")
    }
}

pub(crate) fn parse(
    json: &[u8],
    limits: DefinitionLimits,
    response: bool,
) -> Result<ParsedDefinition, DefinitionError> {
    parse_profile(json, limits, response, false)
}

pub(crate) fn parse_profile(
    json: &[u8],
    limits: DefinitionLimits,
    response: bool,
    warehouse: bool,
) -> Result<ParsedDefinition, DefinitionError> {
    let text =
        std::str::from_utf8(json).map_err(|_| DefinitionError::new(Kind::InvalidJson, None))?;
    // Early schema rejection must not hide malformed values or trailing input.
    let mut syntax = serde_json::Deserializer::from_str(text);
    de::IgnoredAny::deserialize(&mut syntax)
        .and_then(|_| syntax.end())
        .map_err(|_| DefinitionError::new(Kind::InvalidJson, None))?;
    let mut state = State {
        admission: Admission::new(limits),
        limits,
        failure: None,
        part_index: None,
        warehouse,
        media_types: Vec::new(),
        unknown_fields: false,
    };
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let result = if response {
        ResponseSeed(&mut state).deserialize(&mut deserializer)
    } else {
        DefinitionSeed(&mut state).deserialize(&mut deserializer)
    }
    .and_then(|parsed| deserializer.end().map(|()| parsed));
    let mut parsed = result.map_err(|error| {
        state.failure.unwrap_or_else(|| {
            DefinitionError::new(
                if error.is_data() {
                    Kind::InvalidShape
                } else {
                    Kind::InvalidJson
                },
                state.part_index,
            )
        })
    })?;
    parsed.media_types = state.media_types;
    parsed.unknown_fields = state.unknown_fields;
    Ok(parsed)
}

struct ResponseSeed<'a>(&'a mut State);

impl<'de> DeserializeSeed<'de> for ResponseSeed<'_> {
    type Value = ParsedDefinition;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for ResponseSeed<'_> {
    type Value = ParsedDefinition;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a SQLDatabase definition response")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut definition = None;
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.0.warehouse && !keys.insert(key.clone()) {
                return Err(de::Error::custom("invalid definition response fields"));
            }
            if key == "definition" && definition.is_none() {
                definition = Some(map.next_value_seed(DefinitionSeed(&mut *self.0))?);
            } else if self.0.warehouse {
                map.next_value::<de::IgnoredAny>()?;
                self.0.unknown_fields = true;
            } else {
                return Err(de::Error::custom("invalid definition response fields"));
            }
        }
        definition.ok_or_else(|| {
            self.0
                .record(DefinitionError::new(Kind::MissingDefinition, None))
        })
    }
}

struct DefinitionSeed<'a>(&'a mut State);

impl<'de> DeserializeSeed<'de> for DefinitionSeed<'_> {
    type Value = ParsedDefinition;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for DefinitionSeed<'_> {
    type Value = ParsedDefinition;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a native SQLDatabase definition object")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut format = None;
        let mut parts = None;
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.0.warehouse && !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate definition fields"));
            }
            match key.as_str() {
                "format" if format.is_none() => format = Some(map.next_value::<String>()?),
                "parts" if parts.is_none() => {
                    parts = Some(map.next_value_seed(PartsSeed(&mut *self.0))?);
                }
                _ if self.0.warehouse => {
                    map.next_value::<de::IgnoredAny>()?;
                    self.0.unknown_fields = true;
                }
                _ => return Err(de::Error::custom("invalid definition fields")),
            }
        }
        let (parts, payloads) =
            parts.ok_or_else(|| <M::Error as de::Error>::custom("missing definition parts"))?;
        Ok(ParsedDefinition {
            format,
            parts,
            payloads,
            media_types: Vec::new(),
            unknown_fields: false,
        })
    }
}

struct PartsSeed<'a>(&'a mut State);

impl<'de> DeserializeSeed<'de> for PartsSeed<'_> {
    type Value = (Vec<NativeDefinitionPart>, Vec<String>);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for PartsSeed<'_> {
    type Value = (Vec<NativeDefinitionPart>, Vec<String>);

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an ordered native definition part array")
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut sequence: S) -> Result<Self::Value, S::Error> {
        let mut parts = Vec::new();
        let mut payloads = Vec::new();
        while let Some((part, payload)) = sequence.next_element_seed(PartSeed {
            state: &mut *self.0,
            index: parts.len(),
        })? {
            parts.push(part);
            payloads.push(payload);
        }
        self.0.part_index = None;
        Ok((parts, payloads))
    }
}

struct SourcePart {
    path: String,
    payload: String,
    payload_type: String,
    media_type: NativeMediaType,
    unknown_fields: bool,
}

struct SourcePartVisitor {
    warehouse: bool,
}

impl<'de> DeserializeSeed<'de> for SourcePartVisitor {
    type Value = SourcePart;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for SourcePartVisitor {
    type Value = SourcePart;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a native definition part object")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut path = None;
        let mut payload = None;
        let mut payload_type = None;
        let mut media_type = NativeMediaType::Missing;
        let mut unknown_fields = false;
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.warehouse && !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate definition part fields"));
            }
            match key.as_str() {
                "path" if path.is_none() => path = Some(map.next_value::<String>()?),
                "payload" if payload.is_none() => payload = Some(map.next_value::<String>()?),
                "payloadType" if payload_type.is_none() => {
                    payload_type = Some(map.next_value::<String>()?);
                }
                "mediaType" if self.warehouse => {
                    media_type = match map.next_value::<Option<String>>()? {
                        Some(value) => NativeMediaType::Value(value),
                        None => NativeMediaType::Null,
                    };
                }
                _ if self.warehouse => {
                    map.next_value::<de::IgnoredAny>()?;
                    unknown_fields = true;
                }
                _ => return Err(de::Error::custom("invalid definition part fields")),
            }
        }
        Ok(SourcePart {
            path: path.ok_or_else(|| <M::Error as de::Error>::missing_field("path"))?,
            payload: payload.ok_or_else(|| <M::Error as de::Error>::missing_field("payload"))?,
            payload_type: payload_type
                .ok_or_else(|| <M::Error as de::Error>::missing_field("payloadType"))?,
            media_type,
            unknown_fields,
        })
    }
}

struct PartSeed<'a> {
    state: &'a mut State,
    index: usize,
}

impl<'de> DeserializeSeed<'de> for PartSeed<'_> {
    type Value = (NativeDefinitionPart, String);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.state.part_index = Some(self.index);
        if self.index >= self.state.limits.max_parts.get() {
            return Err(self
                .state
                .record(DefinitionError::limit(Limit::PartCount, Some(self.index))));
        }
        let source = SourcePartVisitor {
            warehouse: self.state.warehouse,
        }
        .deserialize(deserializer)?;
        if source.payload_type != "InlineBase64" {
            return Err(self.state.record(DefinitionError::new(
                Kind::UnsupportedPayloadType,
                Some(self.index),
            )));
        }
        let part = self
            .state
            .admission
            .decode(source.path, &source.payload, self.index)
            .map_err(|error| self.state.record(error))?;
        self.state.part_index = None;
        if self.state.warehouse {
            self.state.media_types.push(source.media_type);
            self.state.unknown_fields |= source.unknown_fields;
        }
        Ok((part, source.payload))
    }
}
