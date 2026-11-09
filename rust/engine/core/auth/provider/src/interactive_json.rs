// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::InteractiveError;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

struct Bounded(usize);

impl<'de> DeserializeSeed<'de> for Bounded {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.0 > 16 {
            return Err(serde::de::Error::custom("depth"));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Bounded {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("number"))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Bounded(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate"));
            }
            let value = map.next_value_seed(Bounded(self.0 + 1))?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Value, InteractiveError> {
    if bytes.len() > super::BODY_LIMIT {
        return Err(InteractiveError::Response);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Bounded(0)
        .deserialize(&mut deserializer)
        .map_err(|_| InteractiveError::Response)?;
    deserializer.end().map_err(|_| InteractiveError::Response)?;
    if !value.is_object() {
        return Err(InteractiveError::Response);
    }
    Ok(value)
}
