// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric-only native runtime composition over the existing bounded HTTP engine.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor, bound};
use azure_core::{
    credentials::AccessToken,
    http::{Method, Url},
};
use fabric_core::{AuthenticatedExecution, RequestError};
use fabric_sdk::{
    Audience, EndpointBinding, RequestContext, ServiceOrigin,
    runtime::{
        NativeEffect, NativeError, NativeExecutor, NativeMethod, NativeRequest, NativeResponse,
    },
    workspaces::WorkspaceResponse,
};
use serde_json::Value;
use std::{borrow::Cow, fmt, future::pending, io, num::NonZeroUsize, pin::pin};

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;

/// Executes native Fabric requests with an explicit caller credential and HTTP stack.
///
/// The immutable context and HTTP executor must select the identical Fabric
/// endpoint binding. Only that HTTPS origin and its literal `/v1` API prefix
/// are accepted. Native/workload origins outside the existing endpoint policy
/// are unsupported; this adapter supplies no MWC bootstrap or authorization.
///
/// Authentication borrows the context's credential through core's authenticated
/// execution primitive. All request validation precedes credential access.
/// The existing HTTP executor owns deadlines, streaming bounds and GET-only
/// retries. Non-GET/HEAD methods are conservatively treated as mutations when
/// classifying lost responses, even if the request declares a read effect.
///
/// Dropping the returned future stops local observation only. It never requests
/// remote cancellation, proves rollback or permits replaying a mutation.
#[derive(Clone)]
pub struct NativeFabricExecutor {
    context: RequestContext,
    http: HttpExecutor,
}

impl NativeFabricExecutor {
    /// Composes an explicit context and an existing HTTP executor without I/O.
    ///
    /// # Errors
    /// Returns [`NativeError::UnsupportedCapability`] for a non-Fabric service,
    /// or [`NativeError::InvalidInput`] for a non-Fabric credential audience or
    /// unequal context/executor bindings. Host selection never infers an audience.
    pub fn new(context: RequestContext, http: HttpExecutor) -> Result<Self, NativeError> {
        validate_binding(&context, &http)?;
        Ok(Self { context, http })
    }
}

impl fmt::Debug for NativeFabricExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFabricExecutor")
            .field("context", &"<explicit>")
            .field("http", &"<injected>")
            .finish()
    }
}

impl NativeExecutor for NativeFabricExecutor {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> ExecutionFuture<'a, Result<NativeResponse, NativeError>> {
        Box::pin(async move {
            let limit = NonZeroUsize::new(max_response_bytes).ok_or(NativeError::InvalidInput)?;
            validate_binding(&self.context, &self.http)?;
            validate_url(self.context.endpoint(), request.url())?;
            let method = method(request.method());
            if matches!(method, Method::Get | Method::Head)
                && request.effect() != NativeEffect::Read
            {
                // GET is replayable in the shared engine, never for a declared mutation.
                return Err(NativeError::InvalidInput);
            }
            let headers = request_headers(request)?;
            bound::validate_request(method, request.url(), &[], &headers)
                .map_err(|_| NativeError::InvalidInput)?;
            let body = request_body(request)?;
            let mut dispatch = Dispatch {
                http: &self.http,
                method,
                body: body.as_ref(),
                headers: &headers,
                limit,
            };
            let response = self
                .context
                .execute(
                    ServiceOrigin::Fabric,
                    request.url(),
                    &mut dispatch,
                    pin!(pending()),
                )
                .await
                .map_err(|error| match error {
                    RequestError::InvalidAudience | RequestError::InvalidEndpoint => {
                        NativeError::InvalidInput
                    }
                    RequestError::Credential => NativeError::InvalidResponse,
                    RequestError::Cancelled => NativeError::Cancelled,
                    RequestError::Execution(error) => execution_error(error, method),
                })?;
            let mut response = NativeResponse {
                status: response.status,
                body: response.body,
                content_type: None,
                headers: response.headers,
            };
            response.content_type = response
                .header("content-type")
                .map_err(|error| uncertain_response(error, method))?
                .map(str::to_owned);
            Ok(response)
        })
    }
}

fn validate_binding(context: &RequestContext, http: &HttpExecutor) -> Result<(), NativeError> {
    if context.endpoint().origin() != ServiceOrigin::Fabric
        || http.endpoint().origin() != ServiceOrigin::Fabric
    {
        return Err(NativeError::UnsupportedCapability);
    }
    if context.selection().audience() != Audience::Fabric || context.endpoint() != http.endpoint() {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

fn validate_url(endpoint: &EndpointBinding, url: &Url) -> Result<(), NativeError> {
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(NativeError::Limit);
    }
    if let Some(host) = url.host_str()
        && EndpointBinding::with_host(ServiceOrigin::Fabric, host).is_err()
    {
        return Err(NativeError::UnsupportedCapability);
    }
    endpoint
        .validate_url(url)
        .map_err(|_| NativeError::InvalidInput)?;
    let prefix = endpoint.api_path();
    if url.path() != prefix
        && !url
            .path()
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
    {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

fn method(method: NativeMethod) -> Method {
    match method {
        NativeMethod::Get => Method::Get,
        NativeMethod::Head => Method::Head,
        NativeMethod::Post => Method::Post,
        NativeMethod::Put => Method::Put,
        NativeMethod::Patch => Method::Patch,
        NativeMethod::Delete => Method::Delete,
    }
}

fn request_headers(request: &NativeRequest) -> Result<Vec<(String, String)>, NativeError> {
    if request.headers().len() > MAX_HEADERS - 2 {
        return Err(NativeError::Limit);
    }
    let mut bytes = 0_usize;
    for (name, value) in [
        ("accept", request.accept()),
        ("content-type", request.content_type()),
    ]
    .into_iter()
    .chain(
        request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    ) {
        bytes = bytes
            .checked_add(name.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .and_then(|bytes| bytes.checked_add(4))
            .ok_or(NativeError::Limit)?;
        if bytes > MAX_HEADER_BYTES {
            return Err(NativeError::Limit);
        }
        if name.eq_ignore_ascii_case("keep-alive") || name.eq_ignore_ascii_case("proxy-connection")
        {
            return Err(NativeError::InvalidInput);
        }
    }
    let mut headers = Vec::with_capacity(request.headers().len() + 2);
    headers.push(("accept".into(), request.accept().into()));
    headers.push(("content-type".into(), request.content_type().into()));
    headers.extend(
        request
            .headers()
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    Ok(headers)
}

fn request_body(request: &NativeRequest) -> Result<Cow<'_, [u8]>, NativeError> {
    if let Some(bytes) = request.bytes() {
        if bytes.len() > MAX_BODY_BYTES {
            return Err(NativeError::Limit);
        }
        return Ok(Cow::Borrowed(bytes));
    }
    let Some(value) = request.body() else {
        return Ok(Cow::Borrowed(&[]));
    };
    validate_json(value)?;
    // Measure escaped wire bytes without allocating a serialized body first.
    let mut size = JsonSize(0);
    serde_json::to_writer(&mut size, value).map_err(|error| {
        if error.is_io() {
            NativeError::Limit
        } else {
            NativeError::InvalidInput
        }
    })?;
    let mut bytes = Vec::with_capacity(size.0);
    serde_json::to_writer(&mut bytes, value).map_err(|_| NativeError::InvalidInput)?;
    Ok(Cow::Owned(bytes))
}

fn validate_json(value: &Value) -> Result<(), NativeError> {
    let mut stack = vec![(value, 1_usize)];
    let mut nodes = 1_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err(NativeError::Limit);
        }
        let children = match value {
            Value::Array(values) => values.len(),
            Value::Object(values) => values.len(),
            Value::String(value) if value.len() > MAX_BODY_BYTES => return Err(NativeError::Limit),
            _ => 0,
        };
        nodes = nodes.checked_add(children).ok_or(NativeError::Limit)?;
        if nodes > MAX_JSON_NODES {
            return Err(NativeError::Limit);
        }
        match value {
            Value::Array(values) => stack.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > MAX_BODY_BYTES {
                        return Err(NativeError::Limit);
                    }
                    stack.push((value, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

struct JsonSize(usize);

impl io::Write for JsonSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BODY_BYTES - self.0 {
            return Err(io::Error::other("runtime JSON body bound exceeded"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Dispatch<'a> {
    http: &'a HttpExecutor,
    method: Method,
    body: &'a [u8],
    headers: &'a [(String, String)],
    limit: NonZeroUsize,
}

impl AuthenticatedExecution for Dispatch<'_> {
    type Response = WorkspaceResponse;
    type Error = ExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> ExecutionFuture<'a, Result<Self::Response, Self::Error>> {
        Box::pin(async move {
            self.http
                .execute_bound(
                    ServiceOrigin::Fabric,
                    self.method,
                    url,
                    token,
                    self.body,
                    self.headers,
                    self.limit,
                )
                .await
        })
    }
}

fn execution_error(error: ExecutionError, method: Method) -> NativeError {
    match error {
        ExecutionError::InvalidInput => NativeError::InvalidInput,
        ExecutionError::TimedOut => uncertain_response(NativeError::TimedOut, method),
        ExecutionError::ResponseLimit => uncertain_response(NativeError::Limit, method),
        ExecutionError::Transport => uncertain_response(NativeError::InvalidResponse, method),
        ExecutionError::RetriesExhausted | ExecutionError::RetryDelay => {
            NativeError::InvalidResponse
        }
    }
}

fn uncertain_response(error: NativeError, method: Method) -> NativeError {
    if matches!(method, Method::Get | Method::Head) {
        error
    } else {
        // The shared engine cannot prove whether a lost mutation was applied.
        NativeError::OutcomeUnknown
    }
}
