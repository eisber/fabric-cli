// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{DiscoveryRequest, DiscoveryResult, RelevanceError, error::require_success, models};
use fabric_core::{EndpointBinding, ServiceOrigin};
use fabric_sdk_api::{
    ApiClient, ApiError, Origin, RawRequestOptions, Request, RequestBounds, cancellable, header,
};
use fabric_sdk_workspaces::workspaces::WorkspaceResponse;
use std::{
    future::{Future, pending},
    num::NonZeroUsize,
    pin::Pin,
    sync::Arc,
};

/// One operation-specific future over the shared response and error primitives.
pub type DiscoveryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkspaceResponse, ApiError>> + Send + 'a>>;

/// Decoded discovery together with measurements of the actual complete response.
#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryObservation {
    /// Validated service result, retaining its explicit top-k coverage.
    pub result: DiscoveryResult,
    /// Actual completed HTTP response status, not a synthesized success marker.
    pub http_status: u16,
    /// Actual response-body bytes supplied by the executor, before decoding.
    pub response_bytes: usize,
}

/// Failure with only those response facts actually observed by this client.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{error}")]
pub struct DiscoveryObservationError {
    /// Sanitized typed cause; structured service metadata remains inspectable.
    #[source]
    pub error: RelevanceError,
    /// Actual status when a complete raw response reached the client.
    pub http_status: Option<u16>,
    /// Actual complete raw body length, never a reserialization or stream estimate.
    pub response_bytes: Option<usize>,
    /// Parsed nonnegative Retry-After delta-seconds, if supplied and valid.
    /// HTTP-date values cannot be converted without a caller clock and remain None.
    pub retry_after_seconds: Option<u64>,
}

/// Compatible neutral-provider name for an observed discovery failure.
pub type RelevanceObservationError = DiscoveryObservationError;

/// Narrow injection seam for the verified direct discovery operation only.
///
/// The integration adapter must bind the pinned route and explicitly selected
/// service audience through shared core authentication and `fabric-sdk-api`.
/// The existing Power BI/Fabric-audience policy alone is not evidence that the
/// direct RE gateway accepts that resource. No credential binding is supplied here.
/// An owner-authorized inherited binding must not claim verified live acceptance.
/// It must not discover credentials, choose a fallback audience, accept arbitrary
/// endpoints, follow redirects, replay POST or create an HTTP/runtime/retry engine.
/// The byte limit must be enforced while reading. Dropping the returned future
/// must release local work, not claim server-side cancellation.
pub trait DiscoveryExecutor: Send + Sync {
    /// Performs exactly one bounded direct POST using the supplied typed input.
    fn discover<'a>(
        &'a self,
        request: &'a DiscoveryRequest,
        max_response_bytes: NonZeroUsize,
    ) -> DiscoveryFuture<'a>;
}

/// Thin, runtime-neutral direct discovery client over injected shared execution.
#[derive(Clone)]
pub struct RelevanceClient {
    executor: Arc<dyn DiscoveryExecutor>,
}

struct ApiDiscoveryExecutor {
    api: ApiClient,
    endpoint: EndpointBinding,
}

impl DiscoveryExecutor for ApiDiscoveryExecutor {
    fn discover<'a>(
        &'a self,
        request: &'a DiscoveryRequest,
        max_response_bytes: NonZeroUsize,
    ) -> DiscoveryFuture<'a> {
        Box::pin(async move {
            let bound = Request::raw(RawRequestOptions {
                origin: Origin::RelevanceEngine,
                method: request.method(),
                endpoint: request.action().to_owned(),
                query: request.query(),
                headers: Vec::new(),
                body: Vec::new(),
                bounds: RequestBounds::default(),
            })?
            .with_json(&request.body())?
            .with_endpoint(&self.endpoint)?;
            self.api.send(&bound, max_response_bytes, pending()).await
        })
    }
}

impl RelevanceClient {
    /// Stores the caller's narrow shared-API adapter without I/O or discovery.
    pub fn new(executor: Arc<dyn DiscoveryExecutor>) -> Self {
        Self { executor }
    }

    /// Binds the shared authenticated API to an explicitly selected RE endpoint.
    ///
    /// The caller injects credentials, selection, runtime and HTTP execution.
    /// The shared origin inherits the explicit Power BI/Fabric-audience policy;
    /// this is not a claim of live acceptance or verified scope consent.
    /// No default endpoint is selected, and the shared URL construction base is
    /// always rebound to this endpoint before credential acquisition.
    /// The shared API rejects a mismatched credential audience or unsupported
    /// executor before token acquisition. This constructor rejects other service
    /// identities immediately, without rewriting their roots or audiences.
    pub fn from_api(api: ApiClient, endpoint: EndpointBinding) -> Result<Self, RelevanceError> {
        if endpoint.origin() != ServiceOrigin::RelevanceEngine
            || endpoint.audience() != Origin::RelevanceEngine.audience()
        {
            return Err(ApiError::InvalidAudience.into());
        }
        Ok(Self::new(Arc::new(ApiDiscoveryExecutor { api, endpoint })))
    }

    /// Compatible spelling for [`Self::from_api`], requiring the same explicit endpoint.
    pub fn from_api_with_endpoint(
        api: ApiClient,
        endpoint: EndpointBinding,
    ) -> Result<Self, RelevanceError> {
        Self::from_api(api, endpoint)
    }

    /// Executes one finite relevance-ranked POST and decodes the public response.
    ///
    /// There is no paging, query rewriting, LLM invocation or operation polling.
    /// Cancellation drops local work only: the pinned controller does not pass
    /// request cancellation to its backend. Shared execution must not replay POST.
    pub async fn discover(
        &self,
        request: &DiscoveryRequest,
        max_response_bytes: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DiscoveryResult, RelevanceError> {
        self.discover_observed(request, max_response_bytes, cancelled)
            .await
            .map(|observation| observation.result)
            .map_err(|failure| failure.error)
    }

    /// Executes discovery while retaining actual completed HTTP/body measurements.
    ///
    /// Complete-response decode, HTTP-status and local byte-bound failures retain
    /// their exact status/body length. Credential, stream, executor and cancellation
    /// failures before a complete response have no fabricated response metrics.
    /// Retry-After is observed but never causes an automatic retry.
    pub async fn discover_observed(
        &self,
        request: &DiscoveryRequest,
        max_response_bytes: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DiscoveryObservation, DiscoveryObservationError> {
        let response = match cancellable(
            async { self.executor.discover(request, max_response_bytes).await },
            cancelled,
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) | Err(error) => {
                return Err(DiscoveryObservationError {
                    error: error.into(),
                    http_status: None,
                    response_bytes: None,
                    retry_after_seconds: None,
                });
            }
        };
        let http_status = response.status;
        let response_bytes = response.body.len();
        let retry_after_seconds = header(&response, "retry-after").and_then(delta_seconds);
        let result = if response_bytes > max_response_bytes.get() {
            Err(ApiError::Limit.into())
        } else {
            require_success(&response).and_then(|()| models::decode(&response.body, request))
        };
        result
            .map(|result| DiscoveryObservation {
                result,
                http_status,
                response_bytes,
            })
            .map_err(|error| DiscoveryObservationError {
                error,
                http_status: Some(http_status),
                response_bytes: Some(response_bytes),
                retry_after_seconds,
            })
    }
}

fn delta_seconds(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
