// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ordered workspace metadata and independently observed optional enrichments.

use crate::{
    WorkspaceClient,
    workspaces::{
        PageProgress, WorkspaceExecutor, WorkspaceListErrorKind, WorkspaceListLimits, header,
        or_cancel,
    },
};
use azure_core::http::Url;
use fabric_core::Audience;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    future::{Future, ready},
    pin::{Pin, pin},
};

/// Validated metadata retaining the parsed service object's complete key order.
///
/// This is additive: the existing typed workspace LIST model is unchanged.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct WorkspaceMetadata {
    value: Value,
}

impl WorkspaceMetadata {
    /// Returns the validated opaque service ID.
    pub fn id(&self) -> &str {
        self.value["id"].as_str().expect("validated workspace id")
    }

    /// Returns the validated service display name.
    pub fn display_name(&self) -> &str {
        self.value["displayName"]
            .as_str()
            .expect("validated workspace display name")
    }

    /// Borrows metadata without typed reserialization or field reordering.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Consumes the original ordered metadata and completed augmentations.
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl<'de> Deserialize<'de> for WorkspaceMetadata {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if !value.get("id").is_some_and(Value::is_string)
            || !value.get("displayName").is_some_and(Value::is_string)
        {
            return Err(serde::de::Error::custom(
                "workspace metadata requires string id and displayName",
            ));
        }
        Ok(Self { value })
    }
}

impl fmt::Debug for WorkspaceMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceMetadata")
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A finite step in Python's workspace GET sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceMetadataStep {
    /// Primary workspace metadata, whose failures are never optional.
    Primary,
    /// Complete managed private endpoint collection.
    ManagedPrivateEndpoints,
    /// Spark settings object.
    SparkSettings,
    /// Complete workspace role assignment collection.
    RoleAssignments,
}

impl WorkspaceMetadataStep {
    fn segments(self) -> &'static [&'static str] {
        match self {
            Self::Primary => &[],
            Self::ManagedPrivateEndpoints => &["managedPrivateEndpoints"],
            Self::SparkSettings => &["spark", "settings"],
            Self::RoleAssignments => &["roleAssignments"],
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Primary => unreachable!("primary is not an augmentation"),
            Self::ManagedPrivateEndpoints => "managedPrivateEndpoints",
            Self::SparkSettings => "sparkSettings",
            Self::RoleAssignments => "roleAssignments",
        }
    }
}

/// Redacted, typed reason for preserving Python's optional-field omission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceOmission {
    /// An optional service request failed, possibly after earlier pages.
    ServiceStatus(u16),
    /// The source handler ignores this completed non-200 response.
    IgnoredStatus(u16),
    /// The optional response was not a valid object or collection.
    InvalidResponse,
}

/// One completed optional step, in source order; no provider text or IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceEnrichmentOutcome {
    /// Optional step attempted.
    pub step: WorkspaceMetadataStep,
    /// `None` means the entire value was inserted; otherwise it was not inserted.
    pub omission: Option<WorkspaceOmission>,
}

/// Successful primary metadata with explicit observations of every enrichment.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceMetadataResult {
    /// Ordered primary metadata and only complete successful augmentations.
    pub metadata: WorkspaceMetadata,
    /// Exactly three outcomes in managed-endpoint, Spark, role order.
    pub enrichments: Vec<WorkspaceEnrichmentOutcome>,
}

/// Workspace metadata failure classification, retaining structured service data.
#[derive(Clone, PartialEq)]
pub enum WorkspaceMetadataErrorKind {
    /// Invalid opaque path segment; no credential or request was acquired.
    InvalidIdentifier,
    /// Shared workspace read, response, cancellation or traversal failure.
    Read(WorkspaceListErrorKind),
}

impl fmt::Debug for WorkspaceMetadataErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier => f.write_str("InvalidIdentifier"),
            Self::Read(WorkspaceListErrorKind::Http { status, .. }) => {
                f.debug_tuple("Http").field(status).finish()
            }
            Self::Read(kind) => f.debug_tuple("Read").field(kind).finish(),
        }
    }
}

/// Explicit incomplete operation; completed observations are not partial success.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceMetadataError {
    /// Step at which the operation stopped.
    pub step: WorkspaceMetadataStep,
    /// Typed failure; Debug never echoes service codes/messages or resource data.
    pub kind: WorkspaceMetadataErrorKind,
    /// Earlier completed optional steps, if any.
    pub enrichments: Vec<WorkspaceEnrichmentOutcome>,
}

impl fmt::Display for WorkspaceMetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("workspace metadata operation did not complete")
    }
}

impl std::error::Error for WorkspaceMetadataError {}

impl WorkspaceClient {
    /// Reads workspace metadata and independently attempts all three enrichments.
    ///
    /// Credentials, Fabric selection, HTTP/retry/deadline policy and cancellation
    /// are caller-owned. Optional service/shape failures retain source omission
    /// semantics as typed observations, never logged provider diagnostics.
    /// Cancellation, identity/execution failures, unsafe continuations and finite
    /// limits abort explicitly, even on optional steps. Existing metadata keys
    /// are replaced in place, not removed/reappended.
    ///
    /// # Errors
    ///
    /// Returns the failed step and prior observations, never apparently complete
    /// metadata after a safety failure. Dropping this future drops in-flight work.
    pub async fn get_workspace_metadata(
        &self,
        executor: &dyn WorkspaceExecutor,
        workspace_id: &str,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceMetadataResult, WorkspaceMetadataError> {
        let mut cancelled = pin!(cancelled);
        let primary = async {
            or_cancel(ready(()), cancelled.as_mut()).await?;
            if self.selection().audience() != Audience::Fabric {
                return Err(WorkspaceListErrorKind::InvalidAudience.into());
            }
            let url = metadata_url(workspace_id, WorkspaceMetadataStep::Primary)?;
            let value = self
                .metadata_value(executor, url, limits, false, cancelled.as_mut())
                .await?;
            serde_json::from_value(value)
                .map_err(|_| WorkspaceListErrorKind::InvalidResponse.into())
        }
        .await
        .map_err(|kind| WorkspaceMetadataError {
            step: WorkspaceMetadataStep::Primary,
            kind,
            enrichments: Vec::new(),
        })?;
        self.enrich_workspace_metadata(executor, workspace_id, primary, limits, cancelled)
            .await
    }

    /// Enrich already-completed, validated primary metadata without reading it again.
    ///
    /// Explicit compatibility callers may obtain the primary value through the
    /// shared operation policy first. This reuses exactly the strict metadata
    /// API's optional-step ordering, omissions, bounds and cancellation behavior;
    /// it never interprets an accepted response as completed primary metadata.
    ///
    /// # Errors
    /// Rejects invalid selection/identifiers and preserves failed-step evidence.
    pub async fn enrich_workspace_metadata(
        &self,
        executor: &dyn WorkspaceExecutor,
        workspace_id: &str,
        mut metadata: WorkspaceMetadata,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceMetadataResult, WorkspaceMetadataError> {
        let mut cancelled = pin!(cancelled);
        let mut step = WorkspaceMetadataStep::Primary;
        let mut enrichments = Vec::new();
        let result = async {
            metadata_url(workspace_id, step)?;
            if self.selection().audience() != Audience::Fabric {
                return Err(WorkspaceListErrorKind::InvalidAudience.into());
            }
            for next in [
                WorkspaceMetadataStep::ManagedPrivateEndpoints,
                WorkspaceMetadataStep::SparkSettings,
                WorkspaceMetadataStep::RoleAssignments,
            ] {
                step = next;
                or_cancel(ready(()), cancelled.as_mut()).await?;
                let value = self
                    .metadata_value(
                        executor,
                        metadata_url(workspace_id, step)?,
                        limits,
                        step != WorkspaceMetadataStep::SparkSettings,
                        cancelled.as_mut(),
                    )
                    .await;
                let omission = match value {
                    Ok(value) => {
                        metadata.value[step.field()] = value;
                        None
                    }
                    Err(kind) => Some(match kind {
                        WorkspaceListErrorKind::Http { status, .. } => {
                            WorkspaceOmission::ServiceStatus(status)
                        }
                        WorkspaceListErrorKind::UnsupportedStatus(status) => {
                            WorkspaceOmission::IgnoredStatus(status)
                        }
                        WorkspaceListErrorKind::InvalidResponse => {
                            WorkspaceOmission::InvalidResponse
                        }
                        safety => return Err(safety.into()),
                    }),
                };
                enrichments.push(WorkspaceEnrichmentOutcome { step, omission });
            }
            or_cancel(ready(()), cancelled.as_mut()).await?;
            Ok(metadata)
        }
        .await;
        match result {
            Ok(metadata) => Ok(WorkspaceMetadataResult {
                metadata,
                enrichments,
            }),
            Err(kind) => Err(WorkspaceMetadataError {
                step,
                kind,
                enrichments,
            }),
        }
    }

    async fn metadata_value(
        &self,
        executor: &dyn WorkspaceExecutor,
        mut url: Url,
        limits: WorkspaceListLimits,
        collection: bool,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Value, WorkspaceListErrorKind> {
        let path = url.path().to_owned();
        let mut progress = PageProgress::default();
        let mut saw_ok = false;
        loop {
            let response = self
                .read_workspace(
                    executor,
                    url.clone(),
                    limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await
                .map_err(|kind| {
                    if progress.pages_received != 0
                        && matches!(kind, WorkspaceListErrorKind::UnsupportedStatus(_))
                    {
                        WorkspaceListErrorKind::InvalidContinuation
                    } else {
                        kind
                    }
                })?;
            let possible_continuation = header(&response, "x-ms-continuation")
                .is_some_and(|token| !token.is_empty())
                || response
                    .body
                    .windows(b"continuationToken".len())
                    .any(|bytes| bytes == b"continuationToken");
            if response.status == 201 && !possible_continuation && progress.pages_received == 0 {
                return Err(WorkspaceListErrorKind::UnsupportedStatus(response.status));
            }
            let value: Value = serde_json::from_slice(&response.body)
                .map_err(|_| WorkspaceListErrorKind::InvalidResponse)?;
            let next = continuation(&response, &value)?;
            if !collection {
                if response.status != 200 {
                    return Err(WorkspaceListErrorKind::UnsupportedStatus(response.status));
                }
                if next.is_some() {
                    return Err(WorkspaceListErrorKind::InvalidContinuation);
                }
                if !value.is_object() {
                    return Err(WorkspaceListErrorKind::InvalidResponse);
                }
                return Ok(value);
            }
            let page: crate::Page<Value> = serde_json::from_value(value)
                .map_err(|_| WorkspaceListErrorKind::InvalidResponse)?;
            saw_ok |= response.status == 200;
            if !progress.append(page, &response, limits, &path)? {
                if response.status != 200 {
                    // Python discards a trailing 201 subtree. Do not expose the
                    // earlier pages as an apparently complete collection.
                    return Err(if saw_ok {
                        WorkspaceListErrorKind::InvalidContinuation
                    } else {
                        WorkspaceListErrorKind::UnsupportedStatus(response.status)
                    });
                }
                return Ok(Value::Array(progress.value));
            }
            url.query_pairs_mut().clear().append_pair(
                "continuationToken",
                progress
                    .next_token
                    .as_deref()
                    .expect("validated next token"),
            );
        }
    }
}

impl From<WorkspaceListErrorKind> for WorkspaceMetadataErrorKind {
    fn from(kind: WorkspaceListErrorKind) -> Self {
        Self::Read(kind)
    }
}

fn metadata_url(id: &str, step: WorkspaceMetadataStep) -> Result<Url, WorkspaceMetadataErrorKind> {
    if id.is_empty()
        || matches!(id, "." | "..")
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
    {
        return Err(WorkspaceMetadataErrorKind::InvalidIdentifier);
    }
    let mut url = Url::parse("https://api.fabric.microsoft.com")
        .map_err(|_| WorkspaceMetadataErrorKind::InvalidIdentifier)?;
    url.path_segments_mut()
        .map_err(|_| WorkspaceMetadataErrorKind::InvalidIdentifier)?
        .extend(["v1", "workspaces", id])
        .extend(step.segments().iter().copied());
    Ok(url)
}

fn continuation(
    response: &crate::WorkspaceResponse,
    value: &Value,
) -> Result<Option<String>, WorkspaceListErrorKind> {
    match header(response, "x-ms-continuation") {
        Some(token) => Ok((!token.is_empty()).then(|| token.to_owned())),
        None => match value.get("continuationToken") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(token)) => Ok((!token.is_empty()).then(|| token.clone())),
            _ => Err(WorkspaceListErrorKind::InvalidResponse),
        },
    }
}
