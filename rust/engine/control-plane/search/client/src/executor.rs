// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    ProviderError, SearchError, SearchMode, SearchReceipt, SearchRequest, Source,
    SourceObservation, canonical_type, federate,
};
use futures_util::future::{join, poll_fn};
use std::{
    future::Future,
    pin::{Pin, pin},
    task::Poll,
};

/// Borrowed provider work on the caller's async runtime.
pub type SearchFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

/// Explicit configured-provider capabilities, not discovered authority or consent.
///
/// A relevance adapter must supply effective capabilities from its supported
/// contract/admission policy. This crate does not infer a native RE wire route.
#[derive(Clone, Debug)]
pub struct ProviderCapabilities {
    /// Exact provider source.
    pub source: Source,
    /// Tenant to which the caller explicitly bound this provider's credentials.
    pub bound_tenant: crate::Guid,
    /// Tenant-wide accessible discovery is supported.
    pub tenant_scope: bool,
    /// A native exact workspace restriction is supported.
    pub workspace_scope: bool,
    /// Native type inclusion is supported.
    pub include_types: bool,
    /// Native type exclusion is supported.
    pub exclude_types: bool,
    /// Effective admitted types; `None` means no finite type list is declared.
    ///
    /// Values are explicit canonical type strings. This declaration never enables
    /// a server feature or widens the authenticated caller's access.
    pub effective_types: Option<Vec<String>>,
    /// Native Catalog continuation is supported; relevance must declare false.
    pub catalog_resume: bool,
}

impl ProviderCapabilities {
    fn validate(&self, source: Source, request: &SearchRequest) -> Result<(), SearchError> {
        let unsupported = |feature| SearchError::UnsupportedCapability {
            provider: source,
            feature,
        };
        if self.source != source
            || &self.bound_tenant != request.scope().tenant_id()
            || (source == Source::Relevance && self.catalog_resume)
            || self.effective_types.as_ref().is_some_and(|types| {
                types.len() > 500
                    || types.iter().any(|value| {
                        value.trim().is_empty()
                            || value.chars().count() > 50
                            || value.chars().any(char::is_control)
                            || canonical_type(value) != value
                    })
            })
        {
            return Err(unsupported("provider declaration"));
        }
        if request.scope().workspace_id().is_some() {
            if !self.workspace_scope {
                return Err(unsupported("workspace scope"));
            }
        } else if !self.tenant_scope {
            return Err(unsupported("tenant scope"));
        }
        match request {
            SearchRequest::CatalogResume(_) => {
                if source != Source::Catalog || !self.catalog_resume {
                    return Err(unsupported("native Catalog resume"));
                }
            }
            SearchRequest::Initial(initial) => {
                let filters = initial.filters();
                if !filters.include().is_empty() && !self.include_types {
                    return Err(unsupported("type inclusion"));
                }
                if !filters.exclude().is_empty() && !self.exclude_types {
                    return Err(unsupported("type exclusion"));
                }
                if let Some(types) = &self.effective_types
                    && filters
                        .include()
                        .iter()
                        .chain(filters.exclude())
                        .any(|requested| !types.contains(requested))
                {
                    return Err(unsupported("effective type"));
                }
            }
        }
        Ok(())
    }
}

/// Validated source-specific view of an immutable operation request.
#[derive(Clone, Copy, Debug)]
pub struct ProviderRequest<'a> {
    request: &'a SearchRequest,
    source: Source,
}

impl<'a> ProviderRequest<'a> {
    /// Original validated input; text and native cursor must not be rewritten.
    pub const fn request(self) -> &'a SearchRequest {
        self.request
    }

    /// Selected source.
    pub const fn source(self) -> Source {
        self.source
    }

    /// One-page/top-k network allowance; no hidden traversal or retry loop.
    pub const fn max_requests(self) -> usize {
        1
    }

    /// Streaming acquisition ceiling that the injected adapter must enforce.
    pub const fn max_response_bytes(self) -> usize {
        self.request.limits().max_bytes()
            / if matches!(self.request.mode(), SearchMode::Hybrid) {
                2
            } else {
                1
            }
    }
}

/// Caller-owned provider, with no identity, runtime or global-state discovery.
///
/// `search` must return work that is cancelled by dropping its future: no detached
/// tasks, blocking calls or hidden retry/traversal. The operation races both
/// borrowed provider futures against one caller-owned cancellation/deadline future.
/// Native authorization remains the provider/host's responsibility.
pub trait SearchProvider: Send + Sync {
    /// Return already configured capabilities without network/credential access.
    fn capabilities(&self) -> &ProviderCapabilities;

    /// Pure provider-specific preflight before any selected provider is dispatched.
    ///
    /// This must not access credentials/network or perform effects. It supplements
    /// common capability checks, for example native text limits or empty effective
    /// type sets. Returning an error prevents both hybrid requests.
    ///
    /// # Errors
    /// Returns sanitized provider input/capability evidence.
    fn validate(&self, _request: ProviderRequest<'_>) -> Result<(), SearchError> {
        Ok(())
    }

    /// Perform one bounded source request on the caller's runtime.
    ///
    /// # Errors
    /// Return classified safe failure evidence; never a successful empty fallback.
    fn search<'a>(&'a self, request: ProviderRequest<'a>) -> SearchFuture<'a, SourceObservation>;
}

/// Source-injected execution, independent of SDK wire clients and host policy.
pub struct SearchOperation<'a> {
    catalog: Option<&'a dyn SearchProvider>,
    relevance: Option<&'a dyn SearchProvider>,
}

impl<'a> SearchOperation<'a> {
    /// Inject providers; construction performs no work and installs no fallback.
    pub const fn new(
        catalog: Option<&'a dyn SearchProvider>,
        relevance: Option<&'a dyn SearchProvider>,
    ) -> Self {
        Self { catalog, relevance }
    }

    /// Validate every selected capability, acquire selected sources and federate.
    ///
    /// Missing relevance or unsupported workspace/type scope fails before **any**
    /// provider request. Selected work runs concurrently without spawning tasks.
    /// Caller cancellation is checked first and drops all in-flight source work;
    /// it does not assert server cancellation, rollback or permission to replay.
    /// The caller can supply a single future combining timeout and cancellation.
    ///
    /// # Errors
    /// Returns explicit configuration/capability/cancellation/budget failures,
    /// or independent all-source failures. One failed source remains partial.
    pub async fn execute(
        &self,
        request: &SearchRequest,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<SearchReceipt, SearchError> {
        let work = async {
            let catalog_selected = request.mode() != SearchMode::Relevance;
            let relevance_selected = request.mode() != SearchMode::Catalog;
            let catalog = if catalog_selected {
                Some(self.catalog.ok_or(SearchError::MissingProvider {
                    provider: Source::Catalog,
                })?)
            } else {
                None
            };
            let relevance = if relevance_selected {
                Some(self.relevance.ok_or(SearchError::MissingProvider {
                    provider: Source::Relevance,
                })?)
            } else {
                None
            };
            let catalog_capabilities = catalog.map(SearchProvider::capabilities);
            let relevance_capabilities = relevance.map(SearchProvider::capabilities);
            for (capabilities, source) in [
                (catalog_capabilities, Source::Catalog),
                (relevance_capabilities, Source::Relevance),
            ] {
                if let Some(capabilities) = capabilities {
                    capabilities.validate(source, request)?;
                }
            }
            for (provider, source) in [(catalog, Source::Catalog), (relevance, Source::Relevance)] {
                if let Some(provider) = provider {
                    provider.validate(ProviderRequest { request, source })?;
                }
            }
            let catalog_work = acquire(catalog, catalog_capabilities, Source::Catalog, request);
            let relevance_work = acquire(
                relevance,
                relevance_capabilities,
                Source::Relevance,
                request,
            );
            let (catalog, relevance) = join(catalog_work, relevance_work).await;
            let catalog = catalog?;
            let relevance = relevance?;
            let sources = [
                (catalog.as_ref(), Source::Catalog),
                (relevance.as_ref(), Source::Relevance),
            ]
            .into_iter()
            .map(|(observation, source)| {
                observation
                    .map(|observation| observation.evidence().clone())
                    .unwrap_or_else(|| crate::SourceEvidence::not_selected(source))
            })
            .collect();
            federate(request, catalog, relevance).map_err(|error| match error {
                error @ SearchError::AllSourcesFailed { .. } => error,
                cause => SearchError::ObservedFailure {
                    cause: Box::new(cause),
                    sources,
                },
            })
        };
        let mut cancelled = pin!(cancelled);
        let mut work = pin!(work);
        poll_fn(move |context| {
            if cancelled.as_mut().poll(context).is_ready() {
                return Poll::Ready(Err(SearchError::Cancelled));
            }
            let result = work.as_mut().poll(context);
            // Synchronous validation/encoding also consumes the caller's budget.
            if cancelled.as_mut().poll(context).is_ready() {
                Poll::Ready(Err(SearchError::Cancelled))
            } else {
                result
            }
        })
        .await
    }
}

async fn acquire(
    provider: Option<&dyn SearchProvider>,
    capabilities: Option<&ProviderCapabilities>,
    source: Source,
    request: &SearchRequest,
) -> Result<Option<SourceObservation>, SearchError> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    let observation = match provider.search(ProviderRequest { request, source }).await {
        Ok(observation) => observation,
        Err(error) => SourceObservation::failed(source, error)?,
    };
    if observation.evidence().source() != source {
        return Err(SearchError::InvalidObservation);
    }
    if let Some(types) = capabilities.and_then(|capabilities| capabilities.effective_types.as_ref())
        && observation
            .candidates()
            .iter()
            .any(|candidate| !types.iter().any(|value| value == candidate.artifact_type()))
    {
        return Ok(Some(observation.reject()));
    }
    Ok(Some(observation))
}
