// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{QueryErrorKind, QueryMode, QueryRequest};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeOwned, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub(crate) const VERSION: &str = "2025-06-18";

// RawValue retains number tokens. This duplicate/depth pass never deserializes a
// row value through serde_json::Value (whose default number path uses f64).
pub(crate) fn parse<T: DeserializeOwned>(text: &str) -> Result<T, QueryErrorKind> {
    let raw: &RawValue = serde_json::from_str(text).map_err(|_| QueryErrorKind::Protocol)?;
    let mut nodes = 0;
    check(raw, 0, &mut nodes)?;
    serde_json::from_str(text).map_err(|_| QueryErrorKind::Protocol)
}

fn check(raw: &RawValue, depth: usize, nodes: &mut usize) -> Result<(), QueryErrorKind> {
    *nodes += 1;
    if depth > 64 || *nodes > 2_100_000 {
        return Err(QueryErrorKind::Limit);
    }
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let object: Object<'_> =
                serde_json::from_str(raw.get()).map_err(|_| QueryErrorKind::Protocol)?;
            for value in object.0.into_values() {
                check(value, depth + 1, nodes)?;
            }
        }
        Some(b'[') => {
            let values: Vec<&RawValue> =
                serde_json::from_str(raw.get()).map_err(|_| QueryErrorKind::Protocol)?;
            for value in values {
                check(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct Object<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object<'de>, M::Error> {
                let mut fields = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON field"));
                    }
                }
                Ok(Object(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

#[derive(Deserialize)]
struct Envelope {
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Box<RawValue>>,
    error: Option<RpcError>,
    method: Option<String>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
}

fn envelope(text: &str, id: u64) -> Result<Option<Box<RawValue>>, QueryErrorKind> {
    let response: Envelope = parse(text)?;
    if response.jsonrpc != "2.0" {
        return Err(QueryErrorKind::Protocol);
    }
    if response.id.is_none() {
        if response.result.is_some() || response.error.is_some() {
            return Err(QueryErrorKind::Protocol);
        }
        return match response.method.as_deref() {
            Some("notifications/progress" | "notifications/message") => Ok(None),
            Some("notifications/cancelled") => Err(QueryErrorKind::Canceled),
            _ => Err(QueryErrorKind::Protocol),
        };
    }
    if response.id != Some(id)
        || response.method.is_some()
        || response.result.is_some() == response.error.is_some()
    {
        return Err(QueryErrorKind::Protocol);
    }
    if let Some(error) = response.error {
        return Err(QueryErrorKind::Rpc(error.code));
    }
    Ok(response.result)
}

pub(crate) fn response(body: &[u8], media: &str, id: u64) -> Result<Box<RawValue>, QueryErrorKind> {
    let text = std::str::from_utf8(body).map_err(|_| QueryErrorKind::Protocol)?;
    match media.split(';').next().map(str::trim) {
        Some("application/json") => envelope(text, id)?.ok_or(QueryErrorKind::Protocol),
        _ => Err(QueryErrorKind::Protocol),
    }
}

#[derive(Default)]
pub(crate) struct SseDecoder {
    scanned: usize,
    line_start: usize,
    frame_start: usize,
    events: usize,
}

impl SseDecoder {
    pub(crate) fn consume(
        &mut self,
        body: &[u8],
        id: u64,
    ) -> Result<Option<Box<RawValue>>, QueryErrorKind> {
        let mut answer = None;
        while self.scanned < body.len() {
            let index = self.scanned;
            self.scanned += 1;
            if body[index] != b'\n' {
                continue;
            }
            let line = &body[self.line_start..index];
            if line.is_empty() || line == b"\r" {
                let frame = std::str::from_utf8(&body[self.frame_start..self.line_start])
                    .map_err(|_| QueryErrorKind::Protocol)?;
                let mut data = Vec::new();
                for line in frame.lines() {
                    if line.starts_with(':') || line.is_empty() {
                        continue;
                    }
                    let (key, value) = line.split_once(':').unwrap_or((line, ""));
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    match key {
                        "data" => data.push(value),
                        "event" if value == "message" || value.is_empty() => {}
                        "id" | "retry" => {} // Never used to reconnect or replay.
                        _ => return Err(QueryErrorKind::Protocol),
                    }
                }
                self.frame_start = index + 1;
                if !data.is_empty() {
                    self.events += 1;
                    if self.events > 256 {
                        return Err(QueryErrorKind::Limit);
                    }
                    if let Some(result) = envelope(&data.join("\n"), id)? {
                        if answer.replace(result).is_some() {
                            return Err(QueryErrorKind::Protocol);
                        }
                    }
                }
            }
            self.line_start = index + 1;
        }
        Ok(answer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Initialize {
    protocol_version: String,
    capabilities: Capabilities,
    server_info: ServerInfo,
}

#[derive(Deserialize)]
struct Capabilities {
    tools: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct ServerInfo {
    name: String,
    version: String,
}

pub(crate) fn negotiate(raw: &RawValue) -> Result<String, QueryErrorKind> {
    let init: Initialize = parse(raw.get())?;
    if !matches!(init.protocol_version.as_str(), VERSION | "2025-03-26")
        || init.capabilities.tools.is_none()
        || init.server_info.name.is_empty()
        || init.server_info.name.len() > 256
        || init.server_info.version.is_empty()
        || init.server_info.version.len() > 256
    {
        return Err(QueryErrorKind::Protocol);
    }
    let tools: BTreeMap<String, Box<RawValue>> = parse(
        init.capabilities
            .tools
            .as_ref()
            .ok_or(QueryErrorKind::Protocol)?
            .get(),
    )?;
    if tools.len() > 32 {
        return Err(QueryErrorKind::Limit);
    }
    Ok(init.protocol_version)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolList {
    pub(crate) tools: Vec<Tool>,
    pub(crate) next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Tool {
    pub(crate) name: String,
    input_schema: Box<RawValue>,
    execution: Option<ToolExecution>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecution {
    task_support: Option<String>,
}

#[derive(Deserialize)]
struct Schema {
    #[serde(rename = "type")]
    kind: String,
    properties: BTreeMap<String, Property>,
    #[serde(default)]
    required: Vec<String>,
}

#[derive(Deserialize)]
struct Property {
    #[serde(rename = "type")]
    kind: Option<Box<RawValue>>,
    #[serde(rename = "enum")]
    values: Option<Vec<serde_json::Value>>,
    description: Option<String>,
    minimum: Option<u32>,
    maximum: Option<u32>,
}

impl Property {
    fn supports(&self, expected: &str) -> bool {
        self.kind.as_ref().is_some_and(|kind| {
            serde_json::from_str::<String>(kind.get()).is_ok_and(|s| s == expected)
                || serde_json::from_str::<Vec<String>>(kind.get()).is_ok_and(|kinds| {
                    kinds.iter().any(|s| s == expected)
                        && kinds.iter().all(|s| s == expected || s == "null")
                })
        })
    }
}

pub(crate) fn validate_tool(tool: &Tool, request: &QueryRequest) -> Result<(), QueryErrorKind> {
    let unsupported = QueryErrorKind::UnsupportedTool;
    if tool
        .execution
        .as_ref()
        .and_then(|e| e.task_support.as_deref())
        == Some("required")
    {
        return Err(unsupported);
    }
    let schema: Schema = parse(tool.input_schema.get()).map_err(|_| unsupported)?;
    if schema.kind != "object"
        || !["artifactId", "query"].into_iter().all(|name| {
            schema.required.iter().any(|s| s == name)
                && schema
                    .properties
                    .get(name)
                    .is_some_and(|p| p.supports("string"))
        })
    {
        return Err(unsupported);
    }
    let supplied: BTreeSet<&str> = ["artifactId", "query", "max_rows"]
        .into_iter()
        .chain(request.filename.as_ref().map(|_| "filename"))
        .chain((request.mode == QueryMode::Template).then_some("language"))
        .collect();
    if schema
        .required
        .iter()
        .any(|name| !supplied.contains(name.as_str()))
    {
        return Err(unsupported);
    }
    let cap = schema.properties.get("max_rows").ok_or(unsupported)?;
    if !cap.supports("integer")
        || cap.minimum.is_some_and(|min| request.max_rows < min)
        || cap.maximum.is_some_and(|max| request.max_rows > max)
        || (request.filename.is_some()
            && !schema
                .properties
                .get("filename")
                .is_some_and(|p| p.supports("string")))
    {
        return Err(unsupported);
    }
    if request.mode == QueryMode::Template {
        let language = schema.properties.get("language").ok_or(unsupported)?;
        let advertised = language.values.as_ref().map_or_else(
            || {
                language
                    .description
                    .as_ref()
                    .is_some_and(|s| s.contains("'jmespath'") || s.contains("`jmespath`"))
            },
            |values| values.iter().any(|s| s.as_str() == Some("jmespath")),
        );
        if !language.supports("string") || !advertised {
            return Err(unsupported);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn near_response_limit_nested_validation_borrows_each_subtree() {
        let text = format!(
            "{{\"padding\":{}\"{}\"{}}}",
            "[".repeat(58),
            "x".repeat(30 * 1024 * 1024),
            "]".repeat(58),
        );
        let object: Object<'_> = serde_json::from_str(&text).unwrap();
        let mut current = object.0["padding"];
        let start = text.as_ptr() as usize;
        let end = start + text.len();
        for _ in 0..58 {
            let values: Vec<&RawValue> = serde_json::from_str(current.get()).unwrap();
            current = values[0];
            let pointer = current.get().as_ptr() as usize;
            assert!(
                pointer >= start && pointer + current.get().len() <= end,
                "validation copied a subtree instead of borrowing the bounded input"
            );
        }
        let raw: &RawValue = serde_json::from_str(&text).unwrap();
        let mut nodes = 0;
        check(raw, 0, &mut nodes).unwrap();
        assert_eq!(nodes, 60);
    }
}
