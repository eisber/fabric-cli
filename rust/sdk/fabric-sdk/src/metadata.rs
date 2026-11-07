// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit metadata retrieval shared by independent execution heads.
//!
//! The caller resolves the target and supplies credentials, HTTP execution,
//! cancellation and finite limits. No definition, query, enrichment, navigation,
//! identity discovery or remote write is selected by this entry.

use crate::{
    Client,
    items::{ItemError, ItemExecutor, ItemGetOptions},
    workspaces::{WorkspaceExecutor, WorkspaceListLimits, WorkspaceMetadataError},
};
use serde_json::Value;
use std::{
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    task::Poll,
};

/// One already-resolved metadata target, never a representation or arbitrary URL.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum MetadataTarget<'a> {
    /// Primary workspace metadata, without optional enrichments.
    Workspace {
        /// Resolved workspace identifier.
        workspace_id: &'a str,
    },
    /// Generic item metadata, without type-specific or definition reads.
    Item {
        /// Resolved owning workspace identifier.
        workspace_id: &'a str,
        /// Resolved item identifier.
        item_id: &'a str,
    },
}

impl fmt::Debug for MetadataTarget<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Workspace { .. } => "MetadataTarget::Workspace(<resolved>)",
            Self::Item { .. } => "MetadataTarget::Item(<resolved>)",
        })
    }
}

/// Caller-selected finite bounds; no draft v2 numeric defaults are installed.
#[derive(Clone, Copy, Debug)]
pub struct MetadataLimits {
    /// Maximum unprojected HTTP response bytes, enforced by the shared executor.
    pub max_response_bytes: NonZeroUsize,
    /// Maximum compact JSON data bytes, before any presentation or file delivery.
    pub max_result_bytes: NonZeroUsize,
}

/// Caller-owned metadata dependencies, shared with the existing SDK consumers.
pub struct MetadataHost<'a> {
    /// SDK holding the immutable endpoint and injected credential selection.
    pub client: &'a Client,
    /// Existing generic item read executor.
    pub items: &'a dyn ItemExecutor,
    /// Existing workspace read executor.
    pub workspaces: &'a dyn WorkspaceExecutor,
}

/// A completed metadata read with its exact bounded JSON data representation.
///
/// This is not a native definition, a recursive graph or an operation receipt.
/// The caller owns observation time, common result envelopes and stream policy.
pub struct MetadataRead<'a> {
    target: MetadataTarget<'a>,
    data: Value,
    json: Vec<u8>,
}

impl<'a> MetadataRead<'a> {
    /// The exact resolved target read by the SDK.
    pub fn target(&self) -> MetadataTarget<'a> {
        self.target
    }

    /// Complete parsed metadata, retaining unknown fields and parsed key order.
    pub fn data(&self) -> &Value {
        &self.data
    }

    /// Compact JSON data bytes, with no envelope, newline or extension rewriting.
    pub fn json_bytes(&self) -> &[u8] {
        &self.json
    }
}

impl fmt::Debug for MetadataRead<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataRead")
            .field("target", &self.target)
            .field("bytes", &self.json.len())
            .finish_non_exhaustive()
    }
}

/// Metadata retrieval failed; no absent/denied/partial read becomes empty success.
#[derive(Debug)]
pub enum MetadataError {
    /// The existing item SDK rejected or failed the read.
    Item(ItemError),
    /// The existing workspace SDK rejected or failed the primary read.
    Workspace(WorkspaceMetadataError),
    /// The complete compact JSON representation exceeded the explicit bound.
    BudgetExceeded,
    /// A valid JSON data representation could not be encoded.
    Encoding,
    /// Caller cancellation won before completed data could be delivered.
    Cancelled,
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Item(_) => "item metadata retrieval failed",
            Self::Workspace(_) => "workspace metadata retrieval failed",
            Self::BudgetExceeded => "metadata exceeded its serialized-data byte budget",
            Self::Encoding => "metadata JSON encoding failed",
            Self::Cancelled => "metadata retrieval was cancelled",
        })
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Item(error) => Some(error),
            Self::Workspace(error) => Some(error),
            _ => None,
        }
    }
}

impl MetadataHost<'_> {
    /// Retrieves exactly one explicit metadata target.
    ///
    /// There is no query parameter that changes acquisition. The existing SDK
    /// validates identifiers and endpoint/audience before credentials and enforces
    /// the response bound before decoding. The result bound applies to the whole
    /// unprojected object. Cancellation never sends a remote cancellation request.
    ///
    /// # Errors
    /// Preserves typed SDK failures, cancellation and serialized-data limits.
    pub async fn read<'target>(
        &self,
        target: MetadataTarget<'target>,
        limits: MetadataLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<MetadataRead<'target>, MetadataError> {
        let mut cancelled = pin!(cancelled);
        let data = match target {
            MetadataTarget::Workspace { workspace_id } => self
                .client
                .get_workspace_primary_metadata(
                    self.workspaces,
                    workspace_id,
                    WorkspaceListLimits {
                        max_response_bytes: limits.max_response_bytes,
                        ..WorkspaceListLimits::default()
                    },
                    cancelled.as_mut(),
                )
                .await
                .map_err(MetadataError::Workspace)?
                .into_value(),
            MetadataTarget::Item {
                workspace_id,
                item_id,
            } => self
                .client
                .get_item_metadata(
                    self.items,
                    workspace_id,
                    item_id,
                    ItemGetOptions {
                        query: Vec::new(),
                        max_response_bytes: limits.max_response_bytes,
                    },
                    cancelled.as_mut(),
                )
                .await
                .map_err(MetadataError::Item)?
                .into_value(),
        };
        let mut json = BoundedJson {
            bytes: Vec::new(),
            limit: limits.max_result_bytes.get(),
            exceeded: false,
        };
        if serde_json::to_writer(&mut json, &data).is_err() {
            return Err(if json.exceeded {
                MetadataError::BudgetExceeded
            } else {
                MetadataError::Encoding
            });
        }
        poll_fn(|cx| {
            Poll::Ready(if cancelled.as_mut().poll(cx).is_ready() {
                Err(MetadataError::Cancelled)
            } else {
                Ok(())
            })
        })
        .await?;
        Ok(MetadataRead {
            target,
            data,
            json: json.bytes,
        })
    }
}

struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            self.exceeded = true;
            return Err(std::io::Error::other("metadata byte budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
