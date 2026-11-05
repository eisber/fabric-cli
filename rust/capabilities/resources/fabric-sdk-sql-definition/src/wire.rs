// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, DefinitionLimits,
    NativeDefinitionPart, codec::Admission,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::fmt;

pub(crate) struct ParsedDefinition {
    pub(crate) format: Option<String>,
    pub(crate) parts: Vec<NativeDefinitionPart>,
    pub(crate) payloads: Vec<String>,
}

struct State {
    admission: Admission,
    limits: DefinitionLimits,
    failure: Option<DefinitionError>,
    part_index: Option<usize>,
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
    let mut state = State {
        admission: Admission::new(limits),
        limits,
        failure: None,
        part_index: None,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(json);
    let result = if response {
        ResponseSeed(&mut state).deserialize(&mut deserializer)
    } else {
        DefinitionSeed(&mut state).deserialize(&mut deserializer)
    }
    .and_then(|parsed| deserializer.end().map(|()| parsed));
    result.map_err(|error| {
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
    })
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
        while let Some(key) = map.next_key::<String>()? {
            if key != "definition" || definition.is_some() {
                return Err(de::Error::custom("invalid definition response fields"));
            }
            definition = Some(map.next_value_seed(DefinitionSeed(&mut *self.0))?);
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
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "format" if format.is_none() => format = Some(map.next_value::<String>()?),
                "parts" if parts.is_none() => {
                    parts = Some(map.next_value_seed(PartsSeed(&mut *self.0))?);
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
}

impl<'de> Deserialize<'de> for SourcePart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(SourcePartVisitor)
    }
}

struct SourcePartVisitor;

impl<'de> Visitor<'de> for SourcePartVisitor {
    type Value = SourcePart;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a native definition part object")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut path = None;
        let mut payload = None;
        let mut payload_type = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "path" if path.is_none() => path = Some(map.next_value::<String>()?),
                "payload" if payload.is_none() => payload = Some(map.next_value::<String>()?),
                "payloadType" if payload_type.is_none() => {
                    payload_type = Some(map.next_value::<String>()?);
                }
                _ => return Err(de::Error::custom("invalid definition part fields")),
            }
        }
        Ok(SourcePart {
            path: path.ok_or_else(|| <M::Error as de::Error>::missing_field("path"))?,
            payload: payload.ok_or_else(|| <M::Error as de::Error>::missing_field("payload"))?,
            payload_type: payload_type
                .ok_or_else(|| <M::Error as de::Error>::missing_field("payloadType"))?,
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
        let source = SourcePart::deserialize(deserializer)?;
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
        Ok((part, source.payload))
    }
}
