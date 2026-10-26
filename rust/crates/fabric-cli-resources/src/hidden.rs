// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue, python_truthy};
use fabric_sdk_api::Limits;
use fabric_sdk_catalog::{CatalogClient, HiddenCollection, HiddenMetadata, HiddenResource};
use serde_json::Value;
use std::future::Future;

/// Source virtual-item suffix for exact name lookup; no context tree is invented.
#[derive(Clone, Copy, Debug)]
pub enum HiddenNameKind {
    /// Tenant domain.
    Domain,
    /// Tenant connection.
    Connection,
    /// Tenant gateway.
    Gateway,
}

impl HiddenNameKind {
    fn suffix(self) -> &'static str {
        match self {
            Self::Domain => "Domain",
            Self::Connection => "Connection",
            Self::Gateway => "Gateway",
        }
    }
    fn collection(self) -> HiddenCollection {
        match self {
            Self::Domain => HiddenCollection::Domains,
            Self::Connection => HiddenCollection::Connections,
            Self::Gateway => HiddenCollection::Gateways,
        }
    }
}

/// Source case-sensitive first full-name match after stripping boundary slashes.
/// Rows come from the existing catalog/context; this helper performs no I/O.
pub fn find_hidden_name(
    rows: &[Value],
    kind: HiddenNameKind,
    name: &str,
) -> Result<String, CommandError> {
    let requested = name.trim_matches('/');
    for row in rows {
        let display = row
            .get("displayName")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CommandError::new(
                    "InvalidEntriesFormat",
                    "Invalid hidden resource entries format",
                )
            })?;
        let id = row.get("id").and_then(Value::as_str).ok_or_else(|| {
            CommandError::new(
                "InvalidEntriesFormat",
                "Invalid hidden resource entries format",
            )
        })?;
        if format!("{display}.{}", kind.suffix()) == requested {
            if id.is_empty() {
                break;
            }
            return Ok(id.into());
        }
    }
    Err(CommandError::new(
        "NotFound",
        format!("The {} '{name}' could not be found", kind.suffix()),
    ))
}

/// Resolves through the existing complete catalog collection, never a shadow GET,
/// ambient cache or case-insensitive/name-fallback search.
pub async fn resolve_hidden_name(
    client: &CatalogClient,
    kind: HiddenNameKind,
    name: &str,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<String, CommandError> {
    let rows = client
        .list_hidden(&kind.collection(), limits, cancelled)
        .await
        .map_err(|e| CommandError::from(e.error))?
        .rows;
    find_hidden_name(&rows, kind, name)
}

/// Source first-ID-to-short-name projection. Missing ID/match produces the
/// source empty string; malformed rows remain explicit errors.
pub fn domain_name_by_id(rows: &[Value], id: Option<&str>) -> Result<String, CommandError> {
    let Some(id) = id else {
        return Ok(String::new());
    };
    for row in rows {
        let row_id = row.get("id").and_then(Value::as_str).ok_or_else(|| {
            CommandError::new("InvalidEntriesFormat", "Invalid domain entries format")
        })?;
        if row_id == id {
            return row
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    CommandError::new("InvalidEntriesFormat", "Invalid domain entries format")
                });
        }
    }
    Ok(String::new())
}

/// Both the internal unprojected metadata and optional public GET output.
#[derive(Clone, Debug, PartialEq)]
pub struct HiddenRead {
    /// Actual ordered SDK metadata and retained source-suppressed diagnostics.
    pub metadata: HiddenMetadata,
    /// Projection for the front door's existing output/stream layer.
    pub output: CommandValue,
}

/// Composes source singleton/enrichment SDK reads and GET query/default path
/// discovery. `verbose=false` retains metadata without printing an envelope.
/// Storage export remains the existing explicitly injected host boundary.
pub async fn read_hidden_metadata(
    client: &CatalogClient,
    resource: &HiddenResource,
    query: Option<&str>,
    verbose: bool,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<HiddenRead, CommandError> {
    let mut output = CommandValue::default();
    if matches!(resource, HiddenResource::Domain(_)) {
        output.warnings.push("Requires Fabric administrator".into());
    }
    let metadata = client
        .get_metadata(resource, limits, cancelled)
        .await
        .map_err(|e| CommandError::from(e).with_context(&output))?;
    let value = fabric_cli_query::search(&metadata.value, query.unwrap_or("")).map_err(|_| {
        CommandError::new(
            "InvalidInput",
            "Invalid jmespath query (https://jmespath.org)",
        )
        .with_context(&output)
    })?;
    if verbose && python_truthy(&value) {
        output.data = Some(value);
    }
    Ok(HiddenRead { metadata, output })
}
