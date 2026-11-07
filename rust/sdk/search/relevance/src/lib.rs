// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native direct Relevance Engine discovery with explicit finite scope and types.
//!
//! This is a read-only discovery operation, not an LLM/ask pipeline. Credentials,
//! execution, cancellation, response limits and the async runtime are supplied
//! by the caller through shared SDK primitives.

#![cfg_attr(test, deny(unnameable_test_items))]

mod client;
mod error;
mod models;
mod request;

pub use client::{
    DiscoveryExecutor, DiscoveryFuture, DiscoveryObservation, DiscoveryObservationError,
    RelevanceClient, RelevanceObservationError,
};
pub use error::{ErrorDetail, InnerError, ServiceError, ServiceFailure};
pub use models::{
    Artifact, DiscoveryCoverage, DiscoveryDiagnostics, DiscoveryResult, RelevanceConfidence,
    ScoreSource, VectorScore,
};
pub use request::{
    ArtifactType, DISCOVER_ARTIFACTS_ACTION, DISCOVER_ARTIFACTS_PATH, DiscoveryRequest,
    DiscoveryScope, MAX_RESULTS, MAX_SEARCH_TEXT_UTF16, RELEVANCE_ENGINE_PREFIX,
};

/// Sanitized request/response failures; rejected search text is never displayed.
#[derive(Clone, PartialEq, thiserror::Error)]
pub enum RelevanceError {
    /// Blank text or text longer than the verified service limit.
    #[error("discovery requires nonblank text of at most 500 UTF-16 code units")]
    InvalidText,
    /// The caller exceeded this SDK's deliberately smaller top-k bound.
    #[error("discovery max_results must be between 1 and 50")]
    InvalidMaxResults,
    /// An empty list would widen the query; duplicates are also rejected.
    #[error("discovery requires a nonempty list of distinct supported artifact types")]
    InvalidTypes,
    /// Unknown, unverified or deliberately unsupported artifact type.
    #[error("the artifact type is unsupported by direct discovery")]
    UnsupportedType,
    /// Unsupported scopes must never be silently widened.
    #[error("the requested direct discovery scope is unsupported")]
    UnsupportedScope,
    /// Workspace selectors require a canonical-format nonempty GUID.
    #[error("direct discovery requires a nonempty GUID workspace identifier")]
    InvalidWorkspaceId,
    /// Shared endpoint/audience/authentication/execution/status failure.
    #[error("the shared discovery request failed")]
    Api(#[from] fabric_sdk_api::ApiError),
    /// A source-backed non-success service response; metadata remains inspectable.
    #[error("the discovery service returned an HTTP failure")]
    Service(Box<ServiceFailure>),
    /// A nonempty failure body did not match the verified service error contract.
    #[error("invalid discovery error response (HTTP {status})")]
    InvalidErrorResponse {
        /// Actual failing status, even when its error bytes were malformed.
        status: u16,
    },
    /// A successful status did not carry the required, bounded public DTO.
    #[error("invalid discovery response")]
    InvalidResponse,
}

impl std::fmt::Debug for RelevanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidText => "InvalidText",
            Self::InvalidMaxResults => "InvalidMaxResults",
            Self::InvalidTypes => "InvalidTypes",
            Self::UnsupportedType => "UnsupportedType",
            Self::UnsupportedScope => "UnsupportedScope",
            Self::InvalidWorkspaceId => "InvalidWorkspaceId",
            Self::Api(_) => "Api(<redacted>)",
            Self::Service(_) => "Service(<redacted>)",
            Self::InvalidErrorResponse { .. } => "InvalidErrorResponse",
            Self::InvalidResponse => "InvalidResponse",
        })
    }
}
