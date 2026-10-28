// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CatalogClient, HiddenCollection, HiddenResource};
use fabric_sdk_api::{ApiError, CollectionError, Limits};
use serde_json::Value;
use std::{future::Future, pin::pin};

/// Source-suppressed domain-workspace read failure retained for caller diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataOmission {
    /// Enrichment field omitted rather than fabricated as an empty collection.
    pub field: &'static str,
    /// Actual typed failure, including any earlier complete collection pages.
    pub failure: CollectionError,
}

/// Original ordered singleton metadata plus only completed source enrichments.
#[derive(Clone, Debug, PartialEq)]
pub struct HiddenMetadata {
    /// All primary fields remain intact; successful enrichments replace in place.
    pub value: Value,
    /// Source-suppressed errors remain explicitly observable, never logged globally.
    pub omissions: Vec<MetadataOmission>,
}

impl CatalogClient {
    /// Reads source metadata in primary-then-domain-membership order.
    /// Domain service/shape failures omit only domainWorkspaces and are retained
    /// as diagnostics. Identity, transport, unsafe continuation, limits and
    /// cancellation abort explicitly, consistent with shared metadata safety.
    pub async fn get_metadata(
        &self,
        resource: &HiddenResource,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<HiddenMetadata, ApiError> {
        let mut cancelled = pin!(cancelled);
        let mut metadata = HiddenMetadata {
            value: self
                .get_hidden(resource, limits.max_response_bytes, cancelled.as_mut())
                .await?,
            omissions: Vec::new(),
        };
        if let HiddenResource::Domain(id) = resource {
            match self
                .list_hidden(
                    &HiddenCollection::DomainWorkspaces(id.clone()),
                    limits,
                    cancelled.as_mut(),
                )
                .await
            {
                Ok(workspaces) => {
                    metadata.value["domainWorkspaces"] = Value::Array(workspaces.rows);
                }
                Err(failure)
                    if matches!(
                        failure.error,
                        ApiError::Http { .. }
                            | ApiError::Service { .. }
                            | ApiError::InvalidJson
                            | ApiError::InvalidResponse
                            | ApiError::UnsupportedStatus(_)
                    ) =>
                {
                    metadata.omissions.push(MetadataOmission {
                        field: "domainWorkspaces",
                        failure,
                    });
                }
                Err(failure) => return Err(failure.error),
            }
        }
        Ok(metadata)
    }
}
