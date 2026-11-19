// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Dataflow query-name discovery from the public definition metadata.
//!
//! Never evaluates M, inspects source credentials, writes a definition, or falls
//! back to parsing Mashup code. The returned names are untrusted service data.

use crate::{
    Client,
    observe::{ObserveError, ObserveOptions, observe},
    representations::{
        DefinitionRead, DefinitionTarget, RepresentationError, RepresentationLimits,
        read_definition,
    },
};
use fabric_sdk_operations::OperationExecutor;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, future::Future, num::NonZeroUsize, time::Duration};

const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_QUERIES: usize = 4096;

/// One authoritative metadata entry, not proof that its M evaluation succeeds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedQuery {
    /// Opaque service query identifier.
    pub query_id: String,
    /// Exact Power Query name, without normalization.
    pub query_name: String,
    /// Hidden queries remain visible in explicit discovery.
    #[serde(default)]
    pub is_hidden: bool,
    /// Source load setting; this is not execution permission or table type.
    #[serde(default = "default_load_enabled")]
    pub load_enabled: bool,
}

fn default_load_enabled() -> bool {
    true
}

/// Bounded query metadata, with explicit absence of the optional query index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryInventory {
    /// Whether `queriesMetadata` was present; false never proves an empty Mashup.
    pub query_metadata_present: bool,
    /// Every entry in the metadata index, including hidden/non-loaded queries.
    pub queries: Vec<NamedQuery>,
    /// Actual definition-operation ID, when retrieval required an LRO.
    pub operation_id: Option<String>,
}

/// Discovery fails explicitly; raw service errors stay in their typed sources.
#[derive(Debug)]
pub enum QueryDiscoveryError {
    /// Invalid explicit target, finite limits, metadata shape, version or names.
    InvalidMetadata,
    /// Definition retrieval, part selection or canonical Base64 decoding failed.
    Definition(RepresentationError),
    /// An actual definition operation failed or did not return its result.
    Observation(Box<ObserveError>),
}

impl fmt::Display for QueryDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dataflow query discovery failed or is incomplete")
    }
}

impl std::error::Error for QueryDiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidMetadata => None,
            Self::Definition(error) => Some(error),
            Self::Observation(error) => Some(error.as_ref()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    format_version: String,
    #[serde(default)]
    queries_metadata: QueryMap,
}

#[derive(Default)]
struct QueryMap {
    present: bool,
    values: BTreeMap<String, NamedQuery>,
}

impl<'de> Deserialize<'de> for QueryMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = QueryMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded object with unique query metadata keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<QueryMap, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, NamedQuery>()? {
                    if values.len() >= MAX_QUERIES || values.insert(key, value).is_some() {
                        return Err(de::Error::custom("ambiguous or oversized query index"));
                    }
                }
                Ok(QueryMap {
                    present: true,
                    values,
                })
            }
        }
        deserializer.deserialize_map(Unique)
    }
}

/// Decode only the authoritative `queryMetadata.json` inline part.
///
/// Unknown fields are not echoed. Duplicate metadata keys/names, malformed
/// optional booleans, unsupported versions, encodings and part aliases fail.
/// Missing `queriesMetadata` is reported explicitly, never inferred from M text.
///
/// # Errors
/// Returns a bounded metadata or definition error without any network operation.
pub fn from_definition(read: &DefinitionRead) -> Result<QueryInventory, QueryDiscoveryError> {
    let invalid = || QueryDiscoveryError::InvalidMetadata;
    let DefinitionRead::Completed { definition } = read else {
        return Err(invalid());
    };
    let parts = definition["definition"]["parts"]
        .as_array()
        .ok_or_else(invalid)?;
    if parts.len() > MAX_QUERIES {
        return Err(invalid());
    }
    let matches: Vec<_> = parts
        .iter()
        .filter(|part| {
            part["path"]
                .as_str()
                .is_some_and(|path| path.eq_ignore_ascii_case("queryMetadata.json"))
        })
        .collect();
    if matches.len() != 1
        || matches[0]["path"].as_str() != Some("queryMetadata.json")
        || matches[0]["payload"]
            .as_str()
            .is_none_or(|value| value.len() > MAX_METADATA_BYTES * 4 / 3 + 4)
    {
        return Err(invalid());
    }
    let bytes = read
        .part_bytes("queryMetadata.json")
        .map_err(QueryDiscoveryError::Definition)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(invalid());
    }
    let metadata: Metadata = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if metadata.format_version != "202502" {
        return Err(invalid());
    }
    let mut names = std::collections::BTreeSet::new();
    for query in metadata.queries_metadata.values.values() {
        if query.query_name.trim().is_empty()
            || query.query_name.len() > 4096
            || query.query_name.chars().any(char::is_control)
            || query.query_id.trim().is_empty()
            || query.query_id.len() > 256
            || query.query_id.chars().any(char::is_control)
            || !names.insert(&query.query_name)
        {
            return Err(invalid());
        }
    }
    Ok(QueryInventory {
        query_metadata_present: metadata.queries_metadata.present,
        queries: metadata.queries_metadata.values.into_values().collect(),
        operation_id: None,
    })
}

/// Fetch an explicit Dataflow definition once and discover its named queries.
///
/// Reuses the existing definition reader and generic LRO observer. No execute,
/// refresh, definition update, connection discovery or credential fallback runs.
/// Caller-owned transport/cancellation must impose the total wall-clock budget.
///
/// # Errors
/// Rejects invalid targets/limits before credentials and preserves typed native
/// failures. The optional query index's absence is not a successful empty Mashup.
pub async fn list_queries(
    client: &Client,
    executor: &dyn OperationExecutor,
    target: &DefinitionTarget,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<QueryInventory, QueryDiscoveryError> {
    let valid_id = |id: &str| {
        id.len() == 36
            && id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    };
    if !valid_id(&target.workspace_id)
        || !valid_id(&target.item_id)
        || target.native_format.is_some()
        || limits.max_response_bytes.get() > 16 * 1024 * 1024
        || limits.max_result_bytes.get() > 16 * 1024 * 1024
    {
        return Err(QueryDiscoveryError::InvalidMetadata);
    }
    let mut cancelled = Box::pin(cancelled);
    let mut read = read_definition(client, executor, target, limits, cancelled.as_mut())
        .await
        .map_err(QueryDiscoveryError::Definition)?;
    let mut operation_id = None;
    if let DefinitionRead::Accepted {
        operation_id: actual,
    } = &read
    {
        operation_id = Some(actual.clone());
        let handle = read
            .accepted_handle()
            .ok_or(QueryDiscoveryError::InvalidMetadata)?;
        let observation = observe(
            client,
            executor,
            &handle,
            ObserveOptions {
                wait: true,
                result: true,
                max_requests: NonZeroUsize::new(100).expect("positive request limit"),
                max_response_bytes: limits.max_response_bytes,
                max_delay: Duration::from_secs(60),
            },
            cancelled.as_mut(),
        )
        .await
        .map_err(|error| QueryDiscoveryError::Observation(Box::new(error)))?;
        if !observation.work_succeeded {
            return Err(QueryDiscoveryError::InvalidMetadata);
        }
        let result = observation
            .result
            .ok_or(QueryDiscoveryError::InvalidMetadata)?;
        read = DefinitionRead::from_completed_result(&result.bytes, limits)
            .map_err(QueryDiscoveryError::Definition)?;
    }
    let mut inventory = from_definition(&read)?;
    inventory.operation_id = operation_id;
    Ok(inventory)
}
