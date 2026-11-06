// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pull-based native Fabric bodies over the caller's existing HTTP stack.

use crate::{
    ExecutionFuture, ExecutionTimer, HttpExecutor,
    native_runtime::{
        PreparedRequest, prepare_request, response_metadata, uncertain_response, validate_binding,
    },
};
use azure_core::{
    Bytes,
    credentials::AccessToken,
    error::ErrorKind,
    http::{AsyncRawResponse, AsyncResponseBody, Method, Url},
};
use fabric_core::{AuthenticatedExecution, RequestError};
use fabric_sdk::{
    RequestContext, ServiceOrigin,
    runtime::{
        NativeDeadline, NativeError, NativeRequest, NativeStreamBody, NativeStreamExecutor,
        NativeStreamLimits, NativeStreamResponse,
    },
};
use futures_util::StreamExt;
use std::{
    fmt,
    future::{Future, pending, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};

/// Opens single-dispatch native Fabric streams without collecting their bodies.
///
/// The explicit context and existing HTTP executor must select identical Fabric
/// bindings and the Fabric audience, as for [`super::native_runtime::NativeFabricExecutor`].
/// Request preflight is shared with that adapter, including its JSON allocation
/// bounds. The caller's Azure client must stream and disable retries/redirects.
///
/// One supplied absolute deadline covers acquisition, headers, body reads and
/// caller pauses. The existing timer enforces pending waits; no runtime, TLS,
/// credential implementation, retry or remote-cancellation engine is installed.
#[derive(Clone)]
pub struct NativeFabricStreamExecutor {
    context: RequestContext,
    http: HttpExecutor,
}

impl NativeFabricStreamExecutor {
    /// Composes an explicit caller context and HTTP executor without I/O.
    ///
    /// # Errors
    /// Rejects non-Fabric services as unsupported, and audience/binding mismatches
    /// as invalid input, before accessing credentials or networking.
    pub fn new(context: RequestContext, http: HttpExecutor) -> Result<Self, NativeError> {
        validate_binding(&context, &http)?;
        Ok(Self { context, http })
    }
}

impl fmt::Debug for NativeFabricStreamExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFabricStreamExecutor")
            .field("context", &"<explicit>")
            .field("http", &"<injected>")
            .finish()
    }
}

impl NativeStreamExecutor for NativeFabricStreamExecutor {
    fn open<'a>(
        &'a self,
        request: &'a NativeRequest,
        limits: NativeStreamLimits,
        deadline: &'a NativeDeadline,
    ) -> ExecutionFuture<'a, Result<NativeStreamResponse, NativeError>> {
        Box::pin(async move {
            let limits = limits.validate()?;
            deadline.remaining()?;
            let prepared = prepare_request(&self.context, &self.http, request)?;
            let mut state = DispatchState::default();
            let exchange = async {
                let raw = {
                    let mut dispatch = StreamDispatch {
                        http: &self.http,
                        prepared: &prepared,
                        state: &mut state,
                        deadline,
                    };
                    self.context
                        .execute(
                            ServiceOrigin::Fabric,
                            request.url(),
                            &mut dispatch,
                            pin!(pending()),
                        )
                        .await
                        .map_err(authentication_error)?
                };
                let (status, headers, body) = raw.deconstruct();
                let status = u16::from(status);
                if !(100..=599).contains(&status) {
                    return Err(NativeError::InvalidResponse);
                }
                state.status = Some(status);
                let (headers, content_type) = response_metadata(
                    headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                )?;
                Ok(NativeStreamResponse {
                    status,
                    content_type,
                    headers,
                    body: Box::new(FabricBody {
                        stream: Some(body),
                        remainder: None,
                        terminal: None,
                        received: 0,
                        limits,
                        deadline: deadline.clone(),
                        timer: Arc::clone(&self.http.timer),
                        method: prepared.method,
                        state,
                    }),
                })
            };
            match before_deadline(self.http.timer.as_ref(), deadline, None, exchange).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) | Err(error) => Err(response_error(error, prepared.method, state)),
            }
        })
    }
}

#[derive(Clone, Copy, Default)]
struct DispatchState {
    sent: bool,
    status: Option<u16>,
}

struct StreamDispatch<'a, 'request> {
    http: &'a HttpExecutor,
    prepared: &'a PreparedRequest<'request>,
    state: &'a mut DispatchState,
    deadline: &'a NativeDeadline,
}

impl AuthenticatedExecution for StreamDispatch<'_, '_> {
    type Response = AsyncRawResponse;
    type Error = NativeError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> ExecutionFuture<'a, Result<Self::Response, Self::Error>> {
        Box::pin(async move {
            self.deadline.remaining()?;
            HttpExecutor::validate_token(token).map_err(|_| NativeError::InvalidInput)?;
            let mut request = HttpExecutor::authenticated_request(
                self.prepared.method,
                url,
                token,
                self.prepared.body.as_ref(),
            );
            for (name, value) in &self.prepared.headers {
                request.insert_header(name.clone(), value.clone());
            }
            self.deadline.remaining()?;
            self.state.sent = true;
            self.http
                .client
                .execute_request(&request)
                .await
                .map_err(stream_error)
        })
    }
}

fn stream_error(error: azure_core::Error) -> NativeError {
    if matches!(error.kind(), ErrorKind::Io) {
        NativeError::Transport
    } else {
        NativeError::InvalidResponse
    }
}

fn authentication_error(error: RequestError<NativeError>) -> NativeError {
    match error {
        RequestError::InvalidAudience | RequestError::InvalidEndpoint => NativeError::InvalidInput,
        RequestError::Credential => NativeError::InvalidResponse,
        RequestError::Cancelled => NativeError::Cancelled,
        RequestError::Execution(error) => error,
    }
}

fn response_error(error: NativeError, method: Method, state: DispatchState) -> NativeError {
    if !state.sent {
        return error;
    }
    if matches!(
        error,
        NativeError::InvalidResponse | NativeError::Limit | NativeError::InvalidInput
    ) {
        return uncertain_response(error, method);
    }
    if let Some(status) = state.status
        && !(200..=299).contains(&status)
    {
        return NativeError::Http(status);
    }
    uncertain_response(error, method)
}

fn remaining(
    original: &NativeDeadline,
    supplied: Option<&NativeDeadline>,
) -> Result<Duration, NativeError> {
    let remaining = original.remaining()?;
    supplied.map_or(Ok(remaining), |deadline| {
        deadline.remaining().map(|limit| limit.min(remaining))
    })
}

async fn before_deadline<T>(
    timer: &dyn ExecutionTimer,
    original: &NativeDeadline,
    supplied: Option<&NativeDeadline>,
    future: impl Future<Output = T> + Send,
) -> Result<T, NativeError> {
    let mut timeout = timer.sleep(remaining(original, supplied)?);
    let mut future = pin!(future);
    poll_fn(|cx| {
        if remaining(original, supplied).is_err() || timeout.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(NativeError::TimedOut));
        }
        let result = future.as_mut().poll(cx);
        // A provider can return late in the same poll, even without yielding.
        if remaining(original, supplied).is_err() || timeout.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(NativeError::TimedOut));
        }
        result.map(Ok)
    })
    .await
}

struct FabricBody {
    stream: Option<AsyncResponseBody>,
    remainder: Option<Bytes>,
    terminal: Option<NativeError>,
    received: u64,
    limits: NativeStreamLimits,
    deadline: NativeDeadline,
    timer: Arc<dyn ExecutionTimer>,
    method: Method,
    state: DispatchState,
}

impl FabricBody {
    fn stop(&mut self, error: NativeError) -> NativeError {
        let error = response_error(error, self.method, self.state);
        self.stream = None;
        self.remainder = None;
        self.terminal = Some(error);
        error
    }

    async fn fragment(
        &mut self,
        max_bytes: usize,
        original: &NativeDeadline,
        supplied: &NativeDeadline,
    ) -> Result<Option<Vec<u8>>, NativeError> {
        let chunk = if let Some(chunk) = self.remainder.take() {
            chunk
        } else {
            let next = match self.stream.as_mut() {
                Some(stream) => stream.next().await,
                None => None,
            };
            let Some(chunk) = next else {
                self.stream = None;
                return Ok(None);
            };
            let chunk = chunk.map_err(stream_error)?;
            let bytes = u64::try_from(chunk.len()).map_err(|_| NativeError::Limit)?;
            if bytes > self.limits.max_body_bytes - self.received {
                return Err(NativeError::Limit);
            }
            self.received += bytes;
            chunk
        };
        if chunk.is_empty() {
            return Err(NativeError::InvalidResponse);
        }
        remaining(original, Some(supplied))?;
        let length = chunk
            .len()
            .min(max_bytes)
            .min(self.limits.max_fragment_bytes);
        let exposed = chunk[..length].to_vec();
        if length < chunk.len() {
            self.remainder = Some(chunk.slice(length..));
        }
        Ok(Some(exposed))
    }

    async fn read_next(
        &mut self,
        max_bytes: usize,
        supplied: &NativeDeadline,
    ) -> Result<Option<Vec<u8>>, NativeError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if max_bytes == 0 {
            return Err(self.stop(NativeError::InvalidInput));
        }
        if self.stream.is_none() {
            return Ok(None);
        }
        let original = self.deadline.clone();
        let timer = Arc::clone(&self.timer);
        let read = self.fragment(max_bytes, &original, supplied);
        match before_deadline(timer.as_ref(), &original, Some(supplied), read).await {
            Ok(Ok(fragment)) => Ok(fragment),
            Ok(Err(error)) | Err(error) => Err(self.stop(error)),
        }
    }
}

impl NativeStreamBody for FabricBody {
    fn read<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> ExecutionFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            let mut guard = ReadGuard {
                body: self,
                completed: false,
            };
            let result = guard.body.read_next(max_bytes, deadline).await;
            guard.completed = true;
            result
        })
    }
}

struct ReadGuard<'a> {
    body: &'a mut FabricBody,
    completed: bool,
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.body.stop(NativeError::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn only_explicit_io_evidence_maps_to_transport_not_auth_or_protocol() {
        for (kind, expected) in [
            (ErrorKind::Io, NativeError::Transport),
            (ErrorKind::DataConversion, NativeError::InvalidResponse),
            (ErrorKind::Other, NativeError::InvalidResponse),
            (ErrorKind::Credential, NativeError::InvalidResponse),
            (ErrorKind::Connection, NativeError::InvalidResponse),
        ] {
            assert_eq!(
                stream_error(azure_core::Error::with_message(kind, "private")),
                expected
            );
        }
        assert_eq!(
            authentication_error(RequestError::Credential),
            NativeError::InvalidResponse
        );
    }

    #[test]
    fn metadata_rejects_all_observable_duplicate_case_variants_and_controls() {
        for fields in [
            vec![("ETag", "first"), ("etag", "second")],
            vec![("x-native", "first"), ("x-native", "second")],
            vec![("content-type", "bad\r\nmedia")],
            vec![("x invalid", "value")],
        ] {
            assert_eq!(response_metadata(fields), Err(NativeError::InvalidResponse));
        }
    }
}
