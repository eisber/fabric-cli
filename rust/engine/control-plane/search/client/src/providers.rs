// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Candidate, CandidateIdentity, CandidateMetadata, Coverage, Guid, ProviderCapabilities,
    ProviderError, ProviderErrorCode, ProviderRequest, SearchError, SearchFuture, SearchProvider,
    SearchRequest, Source, SourceObservation, canonical_type,
};
use fabric_sdk_api::{ApiError, ExecutionError};
use fabric_sdk_catalog::{CatalogClient, CatalogFilter, CatalogRequest};
use fabric_sdk_relevance::{
    ArtifactType, DiscoveryCoverage, DiscoveryRequest, DiscoveryScope, RelevanceClient,
    RelevanceError,
};
use serde_json::Value;
use std::{future::pending, num::NonZeroUsize};

/// One-page native Catalog adapter over an already caller-bound SDK client.
///
/// The explicit tenant is selection evidence supplied by the caller, not proof
/// that token claims or response rows independently establish a tenant. This
/// adapter never changes credentials, hydrates hits or follows additional pages.
pub struct CatalogProvider {
    client: CatalogClient,
    capabilities: ProviderCapabilities,
}

impl CatalogProvider {
    /// Store the caller's client and explicit tenant binding without I/O.
    pub fn new(client: CatalogClient, bound_tenant: Guid) -> Self {
        Self {
            client,
            capabilities: ProviderCapabilities {
                source: Source::Catalog,
                bound_tenant,
                tenant_scope: true,
                workspace_scope: true,
                include_types: true,
                exclude_types: true,
                effective_types: None,
                catalog_resume: true,
            },
        }
    }

    fn native_request(&self, request: ProviderRequest<'_>) -> Result<CatalogRequest, ApiError> {
        if request.source() != Source::Catalog {
            return Err(ApiError::InvalidInput);
        }
        match request.request() {
            SearchRequest::Initial(initial) => {
                let workspace_ids = initial
                    .scope()
                    .workspace_id()
                    .map(|id| vec![id.as_str().to_owned()])
                    .unwrap_or_default();
                let filter = CatalogFilter::new(
                    initial.filters().include().to_vec(),
                    initial.filters().exclude().to_vec(),
                    workspace_ids,
                )?;
                CatalogRequest::initial(
                    initial.query().map(str::to_owned),
                    filter,
                    Some(initial.limits().page_size()),
                )
            }
            SearchRequest::CatalogResume(resume) => {
                CatalogRequest::resume(resume.cursor().token().to_owned())
            }
        }
    }
}

impl SearchProvider for CatalogProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn validate(&self, request: ProviderRequest<'_>) -> Result<(), SearchError> {
        self.native_request(request)
            .map(|_| ())
            .map_err(|_| SearchError::ProviderInput {
                provider: Source::Catalog,
                field: "Catalog request",
            })
    }

    fn search<'a>(&'a self, request: ProviderRequest<'a>) -> SearchFuture<'a, SourceObservation> {
        Box::pin(async move {
            let native = self
                .native_request(request)
                .map_err(|_| ProviderError::new(ProviderErrorCode::UnsupportedCapability))?;
            let limit = response_limit(request)?;
            let observation = self
                .client
                .search_v2_observed(&native, limit, pending())
                .await
                .map_err(|failure| ProviderError {
                    code: failure
                        .http_status
                        .and_then(classify_status)
                        .unwrap_or_else(|| classify_api(&failure.error)),
                    http_status: failure.http_status,
                    response_bytes: failure.response_bytes,
                    retry_after_seconds: failure.retry_after_seconds,
                })?;
            let invalid = || {
                measured_error(
                    ProviderErrorCode::InvalidResponse,
                    observation.http_status,
                    observation.response_bytes,
                )
            };
            let candidates = observation
                .page
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    catalog_candidate(row, &self.capabilities.bound_tenant, index + 1)
                        .map_err(|_| invalid())
                })
                .collect::<Result<Vec<_>, _>>()?;
            SourceObservation::usable(
                Source::Catalog,
                candidates,
                Coverage::CatalogPage {
                    continuation: observation.page.next_token,
                },
                observation.response_bytes,
                Some(observation.http_status),
            )
            .map_err(|_| invalid())
        })
    }
}

/// Native relevance adapter over a caller-injected, origin-bound SDK client.
///
/// Default admission is Report and SemanticModel only. The optional explicit
/// effective-type constructor declares already admitted server/toolset policy;
/// it never turns on a feature, chooses an audience or bypasses service policy.
pub struct RelevanceProvider {
    client: RelevanceClient,
    capabilities: ProviderCapabilities,
    effective_types: Vec<ArtifactType>,
}

impl RelevanceProvider {
    /// Store a client with the explicit base Report/SemanticModel subset.
    pub fn new(client: RelevanceClient, bound_tenant: Guid) -> Self {
        Self::with_effective_types(
            client,
            bound_tenant,
            vec![ArtifactType::Report, ArtifactType::SemanticModel],
        )
        .expect("constant nonempty direct discovery type subset")
    }

    /// Supply an explicit finite subset admitted by the caller's provider policy.
    ///
    /// # Errors
    /// Rejects empty/duplicate sets and nonrequestable related-metadata kinds.
    pub fn with_effective_types(
        client: RelevanceClient,
        bound_tenant: Guid,
        effective_types: Vec<ArtifactType>,
    ) -> Result<Self, SearchError> {
        let mut unique = Vec::new();
        for kind in &effective_types {
            if !ArtifactType::ALL.contains(kind) || unique.contains(kind) {
                return Err(SearchError::ProviderInput {
                    provider: Source::Relevance,
                    field: "effective types",
                });
            }
            unique.push(*kind);
        }
        if effective_types.is_empty() {
            return Err(SearchError::ProviderInput {
                provider: Source::Relevance,
                field: "effective types",
            });
        }
        let names = effective_types
            .iter()
            .map(|kind| canonical_type(kind.as_str()).to_owned())
            .collect();
        Ok(Self {
            client,
            capabilities: ProviderCapabilities {
                source: Source::Relevance,
                bound_tenant,
                tenant_scope: true,
                workspace_scope: true,
                include_types: true,
                exclude_types: true,
                effective_types: Some(names),
                catalog_resume: false,
            },
            effective_types,
        })
    }

    fn native_request(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<DiscoveryRequest, RelevanceError> {
        if request.source() != Source::Relevance {
            return Err(RelevanceError::UnsupportedScope);
        }
        let SearchRequest::Initial(initial) = request.request() else {
            return Err(RelevanceError::UnsupportedScope);
        };
        let filters = initial.filters();
        if filters
            .include()
            .iter()
            .chain(filters.exclude())
            .any(|value| {
                !self
                    .effective_types
                    .iter()
                    .any(|kind| canonical_type(kind.as_str()) == value)
            })
        {
            return Err(RelevanceError::UnsupportedType);
        }
        let types = self
            .effective_types
            .iter()
            .copied()
            .filter(|kind| {
                let name = canonical_type(kind.as_str());
                (filters.include().is_empty()
                    || filters.include().iter().any(|value| value == name))
                    && !filters.exclude().iter().any(|value| value == name)
            })
            .collect();
        DiscoveryRequest::new(
            initial.query().ok_or(RelevanceError::InvalidText)?,
            initial.limits().max_results(),
            match initial.scope().workspace_id() {
                Some(id) => DiscoveryScope::Workspace {
                    id: id.as_str().to_owned(),
                },
                None => DiscoveryScope::Tenant,
            },
            types,
        )
    }
}

impl SearchProvider for RelevanceProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn validate(&self, request: ProviderRequest<'_>) -> Result<(), SearchError> {
        self.native_request(request)
            .map(|_| ())
            .map_err(|error| SearchError::ProviderInput {
                provider: Source::Relevance,
                field: match error {
                    RelevanceError::InvalidText => "query",
                    RelevanceError::InvalidMaxResults => "maxResults",
                    RelevanceError::InvalidWorkspaceId => "workspaceId",
                    RelevanceError::UnsupportedScope => "scope",
                    _ => "effective types",
                },
            })
    }

    fn search<'a>(&'a self, request: ProviderRequest<'a>) -> SearchFuture<'a, SourceObservation> {
        Box::pin(async move {
            let native = self
                .native_request(request)
                .map_err(|_| ProviderError::new(ProviderErrorCode::UnsupportedCapability))?;
            let observation = self
                .client
                .discover_observed(&native, response_limit(request)?, pending())
                .await
                .map_err(|failure| ProviderError {
                    code: failure
                        .http_status
                        .and_then(classify_status)
                        .unwrap_or_else(|| match &failure.error {
                            RelevanceError::Api(error) => classify_api(error),
                            RelevanceError::InvalidResponse => ProviderErrorCode::InvalidResponse,
                            _ => ProviderErrorCode::UnsupportedCapability,
                        }),
                    http_status: failure.http_status,
                    response_bytes: failure.response_bytes,
                    retry_after_seconds: failure.retry_after_seconds,
                })?;
            let measured_failure =
                |code| measured_error(code, observation.http_status, observation.response_bytes);
            if observation.result.coverage == DiscoveryCoverage::DisabledOrUnknown {
                return Err(measured_failure(ProviderErrorCode::Unavailable));
            }
            let candidates = observation
                .result
                .artifacts
                .iter()
                .enumerate()
                .map(|(index, artifact)| {
                    let invalid = || measured_failure(ProviderErrorCode::InvalidResponse);
                    let id = artifact.object_id.as_deref().ok_or_else(invalid)?;
                    let identity =
                        CandidateIdentity::item(self.capabilities.bound_tenant.as_str(), id)
                            .map_err(|_| invalid())?;
                    let workspace_id = artifact
                        .workspace_id
                        .as_deref()
                        .map(Guid::new)
                        .transpose()
                        .map_err(|_| invalid())?;
                    if request
                        .request()
                        .scope()
                        .workspace_id()
                        .is_some_and(|expected| workspace_id.as_ref() != Some(expected))
                    {
                        return Err(invalid());
                    }
                    Candidate::new(
                        identity,
                        artifact.document_type.as_str(),
                        workspace_id,
                        CandidateMetadata {
                            display_name: artifact.display_name.clone(),
                            description: artifact.description.clone(),
                        },
                        Source::Relevance,
                        index + 1,
                    )
                    .map_err(|_| invalid())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let diagnostics = observation
                .result
                .diagnostics
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| measured_failure(ProviderErrorCode::InvalidResponse))?;
            SourceObservation::usable(
                Source::Relevance,
                candidates,
                Coverage::RankedTopK,
                observation.response_bytes,
                Some(observation.http_status),
            )
            .and_then(|result| result.with_native_diagnostics(diagnostics))
            .map_err(|_| measured_failure(ProviderErrorCode::InvalidResponse))
        })
    }
}

fn response_limit(request: ProviderRequest<'_>) -> Result<NonZeroUsize, ProviderError> {
    NonZeroUsize::new(request.max_response_bytes())
        .ok_or_else(|| ProviderError::new(ProviderErrorCode::BudgetExceeded))
}

fn measured_error(code: ProviderErrorCode, status: u16, bytes: usize) -> ProviderError {
    ProviderError {
        code,
        http_status: Some(status),
        response_bytes: Some(bytes),
        retry_after_seconds: None,
    }
}

fn classify_api(error: &ApiError) -> ProviderErrorCode {
    match error {
        ApiError::Credential => ProviderErrorCode::Unauthenticated,
        ApiError::Cancelled | ApiError::Execution(ExecutionError::Cancelled) => {
            ProviderErrorCode::Cancelled
        }
        ApiError::Limit | ApiError::Execution(ExecutionError::ResponseLimit) => {
            ProviderErrorCode::BudgetExceeded
        }
        ApiError::Execution(ExecutionError::TimedOut) => ProviderErrorCode::TimedOut,
        ApiError::Execution(_) => ProviderErrorCode::Unavailable,
        ApiError::InvalidAudience | ApiError::UnsupportedRequest | ApiError::InvalidInput => {
            ProviderErrorCode::UnsupportedCapability
        }
        ApiError::Http { status, .. } | ApiError::Service { status, .. } => {
            classify_status(*status).unwrap_or(ProviderErrorCode::InvalidResponse)
        }
        _ => ProviderErrorCode::InvalidResponse,
    }
}

fn classify_status(status: u16) -> Option<ProviderErrorCode> {
    match status {
        401 => Some(ProviderErrorCode::Unauthenticated),
        403 => Some(ProviderErrorCode::AccessDenied),
        429 => Some(ProviderErrorCode::Throttled),
        404 | 500..=599 => Some(ProviderErrorCode::Unavailable),
        300..=499 => Some(ProviderErrorCode::InvalidResponse),
        _ => None,
    }
}

fn catalog_candidate(row: &Value, tenant: &Guid, rank: usize) -> Result<Candidate, SearchError> {
    let invalid = || SearchError::InvalidObservation;
    let object = row.as_object().ok_or_else(invalid)?;
    let text = |name| object.get(name).and_then(Value::as_str).ok_or_else(invalid);
    let id = text("id")?;
    let native_type = text("type")?;
    let kind = text("catalogEntryType")?;
    let metadata = CandidateMetadata {
        display_name: optional_string(row.get("displayName"))?,
        description: optional_string(row.get("description"))?,
    };
    match kind {
        "FabricItem" => {
            let workspace = row
                .get("hierarchy")
                .and_then(|value| value.get("workspace"))
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            Candidate::new(
                CandidateIdentity::item(tenant.as_str(), id)?,
                native_type,
                Some(Guid::new(workspace)?),
                metadata,
                Source::Catalog,
                rank,
            )
        }
        "Workspace" if native_type == "Workspace" => {
            let identity = CandidateIdentity::workspace(tenant.as_str(), id)?;
            if let Some(hierarchy) = row.get("hierarchy") {
                let hierarchy = hierarchy.as_object().ok_or_else(invalid)?;
                if let Some(workspace) = hierarchy.get("workspace") {
                    let workspace_id = workspace
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid)?;
                    if Guid::new(workspace_id)? != Guid::new(id)? {
                        return Err(SearchError::IdentityConflict);
                    }
                }
            }
            Candidate::new(identity, native_type, None, metadata, Source::Catalog, rank)
        }
        _ => Err(invalid()),
    }
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, SearchError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(SearchError::InvalidObservation),
    }
}
