// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Limits, QueryErrorKind, json::RawValue, media, protocol};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{collections::BTreeSet, fmt};

/// Closed catalog views; neither accepts an expression or invokes a template.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CatalogView {
    /// Fixed template names and descriptions navigation.
    #[default]
    Summary,
    /// Fixed full catalog entries navigation, including native parameter metadata.
    FullEntries,
}

impl CatalogView {
    pub(crate) const fn expression(self) -> &'static str {
        match self {
            Self::Summary => "Templates[].{Name:Name,Description:Description}",
            Self::FullEntries => "Templates[]",
        }
    }
}

/// One untrusted native MCP catalog block, retained without inferred row schema.
///
/// The JSON preserves the entire block, including annotations and metadata.
/// Text is not interpreted as JSON, Markdown, SQL, a typed rowset or instructions.
pub struct NativeCatalogContent {
    /// Native block discriminator; never inferred from its contents.
    pub kind: String,
    /// Exact block JSON as received within the bounded MCP result.
    pub json: String,
}

impl fmt::Debug for NativeCatalogContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeCatalogContent(<untrusted, redacted>)")
    }
}

/// Native catalog navigation response, deliberately not a typed query result.
///
/// This attests to an observed tool response, not engine inactivity, read-only
/// enforcement, catalog completeness, or a completed dataset. Caller owns the
/// sensitive and untrusted text, native queries, metadata and resource contents.
pub struct NativeCatalogResponse {
    /// Every native content block, in order, without schema inference or selection.
    pub content: Vec<NativeCatalogContent>,
    /// Exact native result JSON, including top-level metadata.
    pub result_json: String,
    /// Actual negotiated MCP protocol version.
    pub protocol_version: String,
    /// The actual catalog tool HTTP call was dispatched and its response observed.
    pub tool_dispatched: bool,
}

impl fmt::Debug for NativeCatalogResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeCatalogResponse")
            .field("content_blocks", &self.content.len())
            .field("tool_dispatched", &self.tool_dispatched)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogResult {
    content: Vec<RawValue>,
    #[serde(default)]
    is_error: bool,
    task: Option<RawValue>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    resource: Option<Resource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    uri: String,
    #[serde(default, deserialize_with = "present_mime")]
    mime_type: Option<String>,
    text: Option<String>,
    blob: Option<String>,
}

fn present_mime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

pub(crate) fn decode(
    raw: &RawValue,
    limits: Limits,
    protocol_version: String,
) -> Result<NativeCatalogResponse, QueryErrorKind> {
    let result: CatalogResult = protocol::parse(raw.get())?;
    if result.is_error {
        return Err(QueryErrorKind::ToolError);
    }
    if result.task.is_some() {
        return Err(QueryErrorKind::Protocol);
    }
    if result.content.len() > limits.content_blocks {
        return Err(QueryErrorKind::Limit);
    }
    let mut uris = BTreeSet::new();
    let mut bytes = 0_usize;
    let mut content = Vec::with_capacity(result.content.len());
    for raw_block in result.content {
        let block: Block = protocol::parse(raw_block.get())?;
        if block.kind.is_empty() || block.kind.len() > 256 {
            return Err(QueryErrorKind::Protocol);
        }
        match block.kind.as_str() {
            "text" if block.text.is_some() && block.resource.is_none() => {}
            "resource" if block.text.is_none() => {
                let resource = block.resource.ok_or(QueryErrorKind::Protocol)?;
                if resource.uri.is_empty()
                    || resource.uri.len() > 4096
                    || resource.uri.chars().any(char::is_control)
                    || resource.text.is_some() == resource.blob.is_some()
                    || (resource.mime_type.is_none() && resource.text.is_none())
                {
                    return Err(QueryErrorKind::Protocol);
                }
                if let Some(mime) = &resource.mime_type {
                    media::essence(mime)?;
                }
                if !uris.insert(resource.uri) {
                    return Err(QueryErrorKind::DuplicateResource);
                }

                if let Some(blob) = &resource.blob {
                    STANDARD
                        .decode(blob)
                        .map_err(|_| QueryErrorKind::Protocol)?;
                }
            }
            "text" => return Err(QueryErrorKind::Protocol),
            // Retain other native blocks verbatim. No URI, image or audio
            // content is fetched, decoded into rows, or executed.
            _ => {}
        }
        bytes = bytes
            .checked_add(raw_block.get().len())
            .ok_or(QueryErrorKind::Limit)?;
        if bytes > limits.resource_bytes {
            return Err(QueryErrorKind::Limit);
        }
        content.push(NativeCatalogContent {
            kind: block.kind,
            json: raw_block.get().to_owned(),
        });
    }
    Ok(NativeCatalogResponse {
        content,
        result_json: raw.get().to_owned(),
        protocol_version,
        tool_dispatched: true,
    })
}
