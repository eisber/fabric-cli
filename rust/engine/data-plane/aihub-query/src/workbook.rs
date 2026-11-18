// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Limits, QueryErrorKind,
    json::{self, RawValue},
    media,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{collections::BTreeSet, fmt};

/// Workload-native Excel media type; extensions and URIs never select content.
pub const WORKBOOK_MIME: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Original workbook bytes, with no ZIP inspection, rewriting or M execution.
///
/// A workbook may be protected/encrypted or data-only. This type does not
/// certify connectivity, labels, refreshability or successful Excel sign-in.
pub struct NativeWorkbook {
    /// Opaque embedded identifier only; never fetched or used as an output path.
    pub uri: String,
    /// Original validated media type, including parameters and spelling.
    pub mime_type: String,
    /// Strictly decoded original service bytes, including any label/protection.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for NativeWorkbook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeWorkbook")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Observed successful terminal tool response, not necessarily a usable workbook.
///
/// Selection/encoding failures remain explicit while completed-response evidence
/// survives. Transport, JSON-RPC, protocol and tool errors are returned separately
/// as `QueryError`; neither case authorizes a fallback execution.
pub struct NativeWorkbookResponse {
    /// Exact bounded result JSON, retaining all resources, annotations and metadata.
    ///
    /// Sensitive untrusted data; do not log it or treat its URIs as downloads.
    pub result_json: String,
    /// Actual negotiated MCP protocol.
    pub protocol_version: String,
    /// Requested cache row cap, not a row count or completeness guarantee.
    pub requested_max_rows: u32,
    /// Exactly one valid native Excel attachment, or its explicit output failure.
    pub workbook: Result<NativeWorkbook, QueryErrorKind>,
}

impl fmt::Debug for NativeWorkbookResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeWorkbookResponse")
            .field("workbook", &self.workbook)
            .finish_non_exhaustive()
    }
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
    requested_max_rows: u32,
) -> Result<NativeWorkbookResponse, QueryErrorKind> {
    let result = json::borrowed(raw.get())?;
    let is_error = result
        .field("isError")?
        .map(|value| value.deserialize::<bool>())
        .transpose()?
        .unwrap_or(false);
    if is_error {
        return Err(QueryErrorKind::ToolError);
    }
    if let Some(task) = result.field("task")?
        && task.deserialize::<Option<serde::de::IgnoredAny>>()?.is_some()
    {
        return Err(QueryErrorKind::Protocol);
    }
    let content = result
        .field("content")?
        .ok_or(QueryErrorKind::Protocol)?
        .elements()?;
    let workbook = select(raw, content, limits);
    Ok(NativeWorkbookResponse {
        result_json: raw.get().to_owned(),
        protocol_version,
        requested_max_rows,
        workbook,
    })
}

fn select(
    raw: &RawValue,
    content: json::BorrowedArray<'_>,
    limits: Limits,
) -> Result<NativeWorkbook, QueryErrorKind> {
    if raw.get().len() > limits.resource_bytes {
        return Err(QueryErrorKind::Limit);
    }
    let mut uris = BTreeSet::new();
    let mut selected = None;
    for (index, raw_block) in content.enumerate() {
        if index >= limits.content_blocks {
            return Err(QueryErrorKind::Limit);
        }
        let block: Block = raw_block?.deserialize()?;
        match block.kind.as_str() {
            "text" if block.text.is_some() && block.resource.is_none() => {}
            "resource" if block.text.is_none() => {
                let resource = block.resource.ok_or(QueryErrorKind::Protocol)?;
                if resource.uri.is_empty()
                    || resource.uri.len() > 4096
                    || resource.uri.chars().any(char::is_control)
                    || resource.text.is_some() == resource.blob.is_some()
                    || (resource.mime_type.is_none() && resource.blob.is_some())
                {
                    return Err(QueryErrorKind::InvalidNativeWorkbook);
                }
                if !uris.insert(resource.uri.clone()) {
                    return Err(QueryErrorKind::DuplicateResource);
                }
                let excel = resource
                    .mime_type
                    .as_deref()
                    .map(media::essence)
                    .transpose()?
                    .is_some_and(|media| media.eq_ignore_ascii_case(WORKBOOK_MIME));
                let bytes = resource
                    .blob
                    .map(|blob| {
                        // The complete encoded result is bounded before decoding;
                        // decoded allocation cannot exceed that encoded budget.
                        STANDARD
                            .decode(blob)
                            .map_err(|_| QueryErrorKind::InvalidNativeWorkbook)
                    })
                    .transpose()?;
                if excel {
                    let bytes = bytes
                        .filter(|bytes| !bytes.is_empty())
                        .ok_or(QueryErrorKind::InvalidNativeWorkbook)?;
                    if selected.is_some() {
                        return Err(QueryErrorKind::MultipleNativeWorkbooks);
                    }
                    selected = Some(NativeWorkbook {
                        uri: resource.uri,
                        mime_type: resource.mime_type.ok_or(QueryErrorKind::InvalidNativeWorkbook)?,
                        bytes,
                    });
                }
            }
            _ => return Err(QueryErrorKind::Protocol),
        }
    }
    selected.ok_or(QueryErrorKind::MissingNativeWorkbook)
}
