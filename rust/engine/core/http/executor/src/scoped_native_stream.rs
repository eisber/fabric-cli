// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Single-dispatch native streams with an explicit HTTPS endpoint and audience.

use crate::{
    ExecutionFuture, ExecutionTimer,
    native_runtime::{MAX_URL_BYTES, prepare_native_request},
    native_stream::{DispatchState, before_deadline, response_error, send_stream, stream_response},
};
use azure_core::{
    credentials::TokenCredential,
    http::{HttpClient, Url},
};
use fabric_sdk::runtime::{
    NativeDeadline, NativeEndpoint, NativeError, NativeRequest, NativeStreamExecutor,
    NativeStreamLimits, NativeStreamResponse,
};
use std::{fmt, sync::Arc};

const MAX_SCOPE_BYTES: usize = 4096;

/// Caller-owned service policy checked before credentials and again before send.
///
/// Validation can reject an immutable request and its original deadline, but
/// cannot replace its endpoint, body, headers, token or authorization policy.
/// Service-specific route, permission and server-timeout rules belong here;
/// the generic executor never infers query policy from a URL or payload.
pub trait NativeStreamPreflight: Send + Sync {
    /// Validate service policy against the remaining original observation budget.
    ///
    /// # Errors
    /// Return the actual policy failure without logging request or identity data.
    /// An explicit server timeout that exceeds the remaining budget must fail,
    /// not be clamped or replayed after credential acquisition.
    fn validate(
        &self,
        request: &NativeRequest,
        deadline: &NativeDeadline,
    ) -> Result<(), NativeError>;
}

/// Opens bounded native streams for one explicit HTTPS endpoint and literal scope.
///
/// Credentials, HTTP client, asynchronous timer and service policy are injected.
/// Construction performs no I/O. The client must already disable redirects and
/// retries, stream bounded transport fragments, and release local I/O on drop.
/// No credential discovery, query policy, runtime or transport is installed.
///
/// One absolute deadline covers acquisition, headers, body reads and caller
/// pauses. Dropping a future or body stops local observation, not remote work.
#[derive(Clone)]
pub struct NativeScopeStreamExecutor {
    endpoint: NativeEndpoint,
    audience_scope: String,
    credential: Arc<dyn TokenCredential>,
    client: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
    preflight: Arc<dyn NativeStreamPreflight>,
}

impl NativeScopeStreamExecutor {
    /// Bind one explicit HTTPS endpoint, literal HTTPS scope and caller resources.
    ///
    /// The scope is retained verbatim, never derived from the endpoint or parsed
    /// as token claims. Scope syntax does not establish service permissions.
    ///
    /// # Errors
    /// Rejects non-HTTPS endpoints, ambiguous repeated trailing path separators,
    /// excessive URLs, and empty, nonliteral, whitespace-containing or excessive
    /// scopes without accessing credentials.
    pub fn new(
        endpoint: NativeEndpoint,
        audience_scope: String,
        credential: Arc<dyn TokenCredential>,
        client: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
        preflight: Arc<dyn NativeStreamPreflight>,
    ) -> Result<Self, NativeError> {
        if endpoint.base().scheme() != "https" || endpoint.base().path().ends_with("//") {
            return Err(NativeError::InvalidInput);
        }
        if endpoint.base().as_str().len() > MAX_URL_BYTES {
            return Err(NativeError::Limit);
        }
        validate_scope(&audience_scope)?;
        Ok(Self {
            endpoint,
            audience_scope,
            credential,
            client,
            timer,
            preflight,
        })
    }

    fn validate_target(&self, request: &NativeRequest) -> Result<(), NativeError> {
        let url = request.url();
        if url.as_str().len() > MAX_URL_BYTES {
            return Err(NativeError::Limit);
        }
        let base = self.endpoint.base();
        let prefix = base.path().strip_suffix('/').unwrap_or(base.path());
        if url.scheme() != "https"
            || url.origin() != base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || (url.path() != prefix
                && !url
                    .path()
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/')))
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }
}

impl fmt::Debug for NativeScopeStreamExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeScopeStreamExecutor")
            .field("endpoint", &"<explicit>")
            .field("audience_scope", &"<literal>")
            .field("credential", &"<injected>")
            .field("client", &"<injected>")
            .field("timer", &"<injected>")
            .field("preflight", &"<injected>")
            .finish()
    }
}

impl NativeStreamExecutor for NativeScopeStreamExecutor {
    fn open<'a>(
        &'a self,
        request: &'a NativeRequest,
        limits: NativeStreamLimits,
        deadline: &'a NativeDeadline,
    ) -> ExecutionFuture<'a, Result<NativeStreamResponse, NativeError>> {
        Box::pin(async move {
            let limits = limits.validate()?;
            deadline.remaining()?;
            self.validate_target(request)?;
            let prepared = prepare_native_request(request)?;
            self.preflight.validate(request, deadline)?;
            deadline.remaining()?;
            let mut state = DispatchState::default();
            let exchange = async {
                let token = self
                    .credential
                    .get_token(&[self.audience_scope.as_str()], None)
                    .await
                    .map_err(|_| NativeError::InvalidResponse)?;
                let raw = send_stream(
                    self.client.as_ref(),
                    &prepared,
                    request.url(),
                    &token,
                    deadline,
                    &mut state,
                    || self.preflight.validate(request, deadline),
                )
                .await?;
                stream_response(
                    raw,
                    limits,
                    deadline,
                    Arc::clone(&self.timer),
                    prepared.method,
                    &mut state,
                )
            };
            match before_deadline(self.timer.as_ref(), deadline, None, exchange).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) | Err(error) => Err(response_error(error, prepared.method, state)),
            }
        })
    }
}

fn validate_scope(scope: &str) -> Result<(), NativeError> {
    if scope.len() > MAX_SCOPE_BYTES {
        return Err(NativeError::Limit);
    }
    if scope.is_empty() || !scope.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(NativeError::InvalidInput);
    }
    let url = Url::parse(scope).map_err(|_| NativeError::InvalidInput)?;
    if url.scheme() != "https"
        || url.as_str() != scope
        || url.path() == "/"
        || NativeEndpoint::new(url).is_err()
    {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}
