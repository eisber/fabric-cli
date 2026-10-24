// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared HTTP execution for handwritten workspace and Notebook operations.
//!
//! The caller supplies an Azure HTTP client and asynchronous timer. This crate
//! installs no transport, TLS backend, credential provider, or runtime. The HTTP
//! client must stream bodies, disable redirects and automatic retries, and stop
//! local work when its future is dropped.

use azure_core::{
    credentials::AccessToken,
    http::{HttpClient, Method, Request, Url},
};
use fabric_sdk::{
    notebooks::{NotebookExecutionError, NotebookExecutor, NotebookRequest, NotebookResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use futures_util::StreamExt;
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::Duration,
};

/// A caller-runtime future; dropping it must release local timer/request resources.
pub type ExecutionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Asynchronous time supplied by the consumer's runtime, never a blocking sleep.
pub trait ExecutionTimer: Send + Sync {
    /// Completes after at least `delay`; dropping the future cancels the local wait.
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()>;
}

/// Finite execution policy shared across an HTTP exchange and its read retries.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionOptions {
    /// Overall deadline, including response streaming and read retry waits.
    pub timeout: Duration,
    /// Maximum accepted retry or Notebook polling delay.
    pub max_delay: Duration,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(240),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Sanitized executor failures, without URLs, bodies, tokens, or provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    /// A zero deadline or unsupported request configuration was supplied.
    InvalidInput,
    /// The caller's HTTP client or response stream failed.
    Transport,
    /// The overall HTTP deadline elapsed.
    TimedOut,
    /// The decoded response stream exceeded the operation's byte bound.
    ResponseLimit,
    /// A read exhausted its finite retry budget.
    RetriesExhausted,
    /// A retry interval was invalid or exceeded the caller's bound.
    RetryDelay,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid HTTP execution configuration",
            Self::Transport => "HTTP transport or response streaming failed",
            Self::TimedOut => "HTTP execution deadline elapsed",
            Self::ResponseLimit => "HTTP response exceeded its byte bound",
            Self::RetriesExhausted => "HTTP read retry budget exhausted",
            Self::RetryDelay => "HTTP retry interval is invalid or exceeds its bound",
        })
    }
}

impl std::error::Error for ExecutionError {}

mod items;
#[cfg(test)]
mod tests;

/// One reusable client implementing both existing SDK executor interfaces.
///
/// Only GETs are retried. POSTs execute once, including on 429 or a lost response.
/// Read status/backoff behavior follows Python's HTTPS adapter and outer 429
/// loop for integer Retry-After values. Redirects and I/O retries are deliberately
/// not installed; see the crate README for the precise compatibility boundary.
#[derive(Clone)]
pub struct HttpExecutor {
    client: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
    options: ExecutionOptions,
}

impl HttpExecutor {
    /// Stores caller-owned HTTP/time implementations without performing I/O.
    ///
    /// # Errors
    /// Rejects a zero deadline or maximum delay.
    pub fn new(
        client: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
        options: ExecutionOptions,
    ) -> Result<Self, ExecutionError> {
        if options.timeout.is_zero() || options.max_delay.is_zero() {
            return Err(ExecutionError::InvalidInput);
        }
        Ok(Self {
            client,
            timer,
            options,
        })
    }

    async fn execute(
        &self,
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
        limit: NonZeroUsize,
    ) -> Result<WorkspaceResponse, ExecutionError> {
        if url.scheme() != "https"
            || url.host_str() != Some("api.fabric.microsoft.com")
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || token.token.secret().is_empty()
            || token.token.secret().chars().any(char::is_control)
        {
            return Err(ExecutionError::InvalidInput);
        }
        let mut request = Request::new(url.clone(), method);
        request.insert_header("authorization", format!("Bearer {}", token.token.secret()));
        request.insert_header("content-type", "application/json");
        request.set_body(body.to_vec());
        self.before_deadline(async {
            // Python's outer 429 loop has four iterations. Its HTTPS adapter
            // independently allows three status retries on each iteration.
            for _ in 0..4 {
                let response = self.read_with_retries(&request, limit).await?;
                if method != Method::Get || response.status != 429 {
                    return Ok(response);
                }
                let delay = retry_after(&response.headers)?.unwrap_or(Duration::from_secs(10));
                self.wait_bounded(delay).await?;
            }
            Err(ExecutionError::RetriesExhausted)
        })
        .await
    }

    async fn read_with_retries(
        &self,
        request: &Request,
        limit: NonZeroUsize,
    ) -> Result<WorkspaceResponse, ExecutionError> {
        for retry in 0..=3 {
            let raw = self
                .client
                .execute_request(request)
                .await
                .map_err(|_| ExecutionError::Transport)?;
            let (status, headers, mut stream) = raw.deconstruct();
            let mut body = Vec::new();
            let mut chunks = 0_usize;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| ExecutionError::Transport)?;
                if chunk.len() > limit.get() - body.len() {
                    return Err(ExecutionError::ResponseLimit);
                }
                body.extend_from_slice(&chunk);
                chunks += 1;
                if chunks == 64 {
                    chunks = 0;
                    yield_once().await;
                }
            }
            let response = WorkspaceResponse {
                status: status.into(),
                body,
                headers: headers
                    .iter()
                    .map(|(name, value)| (name.as_str().to_owned(), value.as_str().to_owned()))
                    .collect(),
            };
            let has_retry_after = response.headers.contains_key("retry-after");
            let retryable = request.method() == Method::Get
                && (matches!(response.status, 502..=504)
                    || (has_retry_after && matches!(response.status, 413 | 429)));
            if !retryable {
                return Ok(response);
            }
            if retry == 3 {
                return Err(ExecutionError::RetriesExhausted);
            }
            let delay = retry_after(&response.headers)?
                .filter(|delay| !delay.is_zero())
                .unwrap_or_else(|| {
                    Duration::from_secs(match retry {
                        0 => 0,
                        1 => 2,
                        _ => 4,
                    })
                });
            self.wait_bounded(delay).await?;
        }
        unreachable!("the finite retry loop returns on its last iteration")
    }

    async fn wait_bounded(&self, delay: Duration) -> Result<(), ExecutionError> {
        if delay > self.options.max_delay {
            return Err(ExecutionError::RetryDelay);
        }
        if !delay.is_zero() {
            self.timer.sleep(delay).await;
        }
        Ok(())
    }

    async fn before_deadline<T>(
        &self,
        future: impl Future<Output = Result<T, ExecutionError>> + Send,
    ) -> Result<T, ExecutionError> {
        let mut deadline = self.timer.sleep(self.options.timeout);
        let mut future = pin!(future);
        poll_fn(|cx| {
            if deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(ExecutionError::TimedOut));
            }
            future.as_mut().poll(cx)
        })
        .await
    }
}

impl fmt::Debug for HttpExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpExecutor")
            .field("client", &"<injected>")
            .field("timer", &"<injected>")
            .field("options", &self.options)
            .finish()
    }
}

fn retry_after(headers: &BTreeMap<String, String>) -> Result<Option<Duration>, ExecutionError> {
    headers
        .get("retry-after")
        .map(|value| {
            value
                .trim()
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|_| ExecutionError::RetryDelay)
        })
        .transpose()
}

async fn yield_once() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

impl WorkspaceExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> ExecutionFuture<'a, Result<WorkspaceResponse, WorkspaceExecutionError>> {
        Box::pin(async move {
            self.execute(
                Method::Get,
                request.url(),
                request.token(),
                &[],
                request.max_response_bytes(),
            )
            .await
            .map_err(workspace_error)
        })
    }
}

impl NotebookExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: NotebookRequest<'a>,
    ) -> ExecutionFuture<'a, Result<NotebookResponse, NotebookExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                "GET" => Method::Get,
                "POST" => Method::Post,
                _ => return Err(NotebookExecutionError::Failed),
            };
            self.execute(
                method,
                request.url(),
                request.token(),
                request.body(),
                request.max_response_bytes(),
            )
            .await
            .map(|response| NotebookResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
            .map_err(notebook_error)
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), NotebookExecutionError>> {
        Box::pin(async move {
            self.before_deadline(self.wait_bounded(delay))
                .await
                .map_err(notebook_error)
        })
    }
}

fn workspace_error(error: ExecutionError) -> WorkspaceExecutionError {
    match error {
        ExecutionError::TimedOut => WorkspaceExecutionError::TimedOut,
        ExecutionError::ResponseLimit => WorkspaceExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => WorkspaceExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            WorkspaceExecutionError::Failed
        }
    }
}

fn notebook_error(error: ExecutionError) -> NotebookExecutionError {
    match error {
        ExecutionError::TimedOut => NotebookExecutionError::TimedOut,
        ExecutionError::ResponseLimit => NotebookExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => NotebookExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            NotebookExecutionError::Failed
        }
    }
}
