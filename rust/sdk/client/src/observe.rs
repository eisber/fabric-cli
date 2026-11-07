// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded observation of an already-existing Fabric operation or item job.
//!
//! This is not submission reconciliation: every request is a GET of the exact
//! supplied handle (or its explicitly advertised operation result). Credentials,
//! HTTP retries and asynchronous timers remain owned by the shared executor.

use crate::{Audience, Client, ServiceOrigin};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::{
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
    time::Duration,
};

/// An existing service handle, never an arbitrary URL or a submission request.
///
/// Identifiers must be 1–256 ASCII letters, digits, hyphens or underscores.
/// Validation precedes credential acquisition, including when constructed directly.
#[derive(Clone, Eq, PartialEq)]
pub enum ReadHandle {
    /// A generic Fabric operation.
    Operation {
        /// Existing operation identifier.
        operation_id: String,
    },
    /// An existing job instance on one specific item.
    Job {
        /// Workspace containing the item.
        workspace_id: String,
        /// Item containing the job instance.
        item_id: String,
        /// Existing job instance identifier.
        job_id: String,
    },
}

impl ReadHandle {
    fn path(&self) -> Result<String, OperationErrorKind> {
        let valid = |id: &str| {
            !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        };
        match self {
            Self::Operation { operation_id } if valid(operation_id) => {
                Ok(format!("/v1/operations/{operation_id}"))
            }
            Self::Job {
                workspace_id,
                item_id,
                job_id,
            } if valid(workspace_id) && valid(item_id) && valid(job_id) => Ok(format!(
                "/v1/workspaces/{workspace_id}/items/{item_id}/jobs/instances/{job_id}"
            )),
            _ => Err(OperationErrorKind::InvalidInput),
        }
    }
}

impl fmt::Debug for ReadHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Operation { .. } => "Operation",
            Self::Job { .. } => "Job",
        };
        f.debug_struct("ReadHandle")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

/// Explicit observation bounds; requesting a result never implicitly enables wait.
#[derive(Clone, Copy, Debug)]
pub struct ObserveOptions {
    /// Poll active work until terminal; false performs exactly one status GET.
    pub wait: bool,
    /// Require an advertised generic operation result; item jobs are unsupported.
    pub result: bool,
    /// Total logical GET dispatches, including initial status, polls and result.
    ///
    /// Transport retries inside one dispatch belong to the injected executor and
    /// its own finite retry budget; they are not observable through this trait.
    pub max_requests: NonZeroUsize,
    /// Streaming and post-response byte bound for each GET.
    pub max_response_bytes: NonZeroUsize,
    /// Largest permitted wait. Must be nonzero when `wait` is true.
    ///
    /// Missing Retry-After uses one second. Present values must contain only
    /// decimal digits and represent positive integer seconds. Zero, dates,
    /// whitespace, signed values and integer overflow fail explicitly, never
    /// busy-loop. Values above this bound fail instead of being clamped.
    pub max_delay: Duration,
}

/// Exact bytes and optional Content-Type returned by the operation result GET.
#[derive(Clone, Eq, PartialEq)]
pub struct ObservedResult {
    /// Uninterpreted result payload, not restricted to JSON.
    pub bytes: Vec<u8>,
    /// Exact Content-Type value, if supplied; no default media type is invented.
    pub media_type: Option<String>,
}

impl fmt::Debug for ObservedResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservedResult")
            .field("bytes", &"<service data>")
            .field("media_type", &"<service data>")
            .finish()
    }
}

/// The latest status response, distinct from successful execution of the work.
#[derive(Clone, PartialEq)]
pub struct Observation {
    /// Exact caller-supplied handle, retained even on unsuccessful work.
    pub handle: ReadHandle,
    /// Original parsed service JSON, including unknown fields and service errors.
    pub raw_status: Value,
    /// True only for a recognized terminal status in this handle's status domain.
    pub terminal: bool,
    /// True only for generic Succeeded or job Completed.
    ///
    /// A Deduped job is terminal but does not establish successful execution.
    pub work_succeeded: bool,
    /// Present only after an explicitly requested result GET succeeds.
    pub result: Option<ObservedResult>,
    /// Number of logical GETs dispatched, including any result retrieval.
    pub requests: usize,
}

impl fmt::Debug for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observation")
            .field("handle", &self.handle)
            .field("raw_status", &"<service data>")
            .field("terminal", &self.terminal)
            .field("work_succeeded", &self.work_succeeded)
            .field("result", &self.result)
            .field("requests", &self.requests)
            .finish()
    }
}

/// Observation-specific classification; shared failures retain their typed error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveErrorKind {
    /// Shared validation, authentication, execution, HTTP, limit or work failure.
    Operation,
    /// Item jobs do not expose this generic operation result contract.
    Unsupported,
    /// A successful HTTP status outside the endpoint's exact 200 contract.
    UnexpectedStatus {
        /// Actual HTTP status, never an invented success or acceptance.
        status: u16,
    },
    /// Status body is not an object containing a string status.
    InvalidState,
    /// String status is outside the supplied handle's finite status domain.
    UnknownState,
    /// Retry-After is malformed, zero or overflows integer seconds.
    InvalidRetryAfter,
    /// Succeeded operation did not advertise its exact result URL in Location.
    ResultNotAdvertised,
    /// Result was requested without waiting, but the operation is still active.
    ResultNotReady,
}

/// A failure retaining the handle, last parsed status and exact dispatch count.
///
/// Debug and Display never print identifiers, raw JSON or provider diagnostics.
/// Access these fields explicitly when displaying service data is appropriate.
#[derive(Clone, PartialEq)]
pub struct ObserveError {
    /// Observation-specific failure classification.
    pub kind: ObserveErrorKind,
    /// Original shared error, or shared classification of a local protocol failure.
    ///
    /// Generic operation IDs are retained independently of acceptance headers.
    /// Observation never marks a mutation as possibly performed.
    pub operation_error: Box<OperationError>,
    /// Exact caller-supplied handle.
    pub handle: ReadHandle,
    /// Last parsed status, including failed or unknown work states when available.
    pub last_observation: Option<Box<Observation>>,
    /// Number of logical GETs actually dispatched, even on cancellation.
    pub requests: usize,
}

impl fmt::Debug for ObserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObserveError")
            .field("kind", &self.kind)
            .field("operation_error", &"<typed evidence>")
            .field("handle", &self.handle)
            .field("last_observation", &self.last_observation)
            .field("requests", &self.requests)
            .finish()
    }
}

impl fmt::Display for ObserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fabric observation failed or is incomplete; inspect classification and retained status")
    }
}

impl std::error::Error for ObserveError {}

/// Read or wait for an already-existing operation or item job.
///
/// Every GET uses the client's immutable Fabric endpoint and injected credential
/// through `OperationSession` with `CompletionPolicy::ReturnAccepted`. No 202 is
/// synthesized, followed or reconciled. A status GET must actually return 200.
/// A result requires Succeeded and a Location advertising the exact selected-host
/// `/v1/operations/{id}/result`; advertised URLs are validated, never followed.
/// Result HTTP errors, including OperationHasNoResult, remain errors.
///
/// The caller supplies one cancellation/deadline future covering credentials,
/// HTTP and `executor.wait`. Cancellation drops local work only; it never sends
/// remote cancellation. Request count bounds the loop even without a deadline.
///
/// # Errors
///
/// Rejects invalid IDs, audience/endpoint, unsupported job results and invalid
/// wait bounds before credentials. Non-waiting status-only reads return failed,
/// cancelled or deduped work as an Observation. Waiting (or demanding a result)
/// returns a typed OperationFailed for unsuccessful terminal work. Unknown states,
/// unexpected HTTP statuses, missing result advertisements, malformed delays,
/// exhausted bounds and shared credential/HTTP failures remain explicit errors.
pub async fn observe(
    client: &Client,
    executor: &dyn OperationExecutor,
    handle: &ReadHandle,
    options: ObserveOptions,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Observation, ObserveError> {
    let requests = AtomicUsize::new(0);
    let adapter = CountingExecutor {
        inner: executor,
        requests: &requests,
    };
    let mut session = OperationSession::with_context(
        client.context(),
        adapter,
        OperationLimits {
            max_requests: options.max_requests,
            max_response_bytes: options.max_response_bytes,
            max_poll_delay: options.max_delay,
        },
        cancelled,
    );
    let mut last = None;
    let outcome = observe_inner(
        client,
        &mut session,
        executor,
        handle,
        options,
        &requests,
        &mut last,
    )
    .await;
    let requests = requests.load(Ordering::SeqCst);
    match outcome {
        Ok(mut observation) => {
            observation.requests = requests;
            Ok(observation)
        }
        Err(mut failure) => {
            if let ReadHandle::Operation { operation_id } = handle {
                failure.operation_error.operation_id = Some(operation_id.clone());
            }
            if let Some(observation) = &mut last {
                observation.requests = requests;
            }
            Err(ObserveError {
                kind: failure.kind,
                operation_error: Box::new(failure.operation_error),
                handle: handle.clone(),
                last_observation: last.map(Box::new),
                requests,
            })
        }
    }
}

struct Failure {
    kind: ObserveErrorKind,
    operation_error: OperationError,
}

impl From<OperationError> for Failure {
    fn from(operation_error: OperationError) -> Self {
        Self {
            kind: ObserveErrorKind::Operation,
            operation_error,
        }
    }
}

fn failure(
    session: &OperationSession<'_, CountingExecutor<'_>>,
    kind: ObserveErrorKind,
    operation_kind: OperationErrorKind,
) -> Failure {
    Failure {
        kind,
        operation_error: session.evidence(operation_kind),
    }
}

async fn observe_inner(
    client: &Client,
    session: &mut OperationSession<'_, CountingExecutor<'_>>,
    executor: &dyn OperationExecutor,
    handle: &ReadHandle,
    options: ObserveOptions,
    requests: &AtomicUsize,
    last: &mut Option<Observation>,
) -> Result<Observation, Failure> {
    let path = handle
        .path()
        .map_err(|kind| failure(session, ObserveErrorKind::Operation, kind))?;
    if client.selection().audience() != Audience::Fabric {
        return Err(failure(
            session,
            ObserveErrorKind::Operation,
            OperationErrorKind::InvalidAudience,
        ));
    }
    if client.context().endpoint().origin() != ServiceOrigin::Fabric
        || (options.wait && options.max_delay.is_zero())
    {
        return Err(failure(
            session,
            ObserveErrorKind::Operation,
            OperationErrorKind::InvalidInput,
        ));
    }
    if options.result && matches!(handle, ReadHandle::Job { .. }) {
        return Err(failure(
            session,
            ObserveErrorKind::Unsupported,
            OperationErrorKind::InvalidInput,
        ));
    }
    loop {
        let response = get(session, &path).await?;
        let raw_status: Value = serde_json::from_slice(&response.body).map_err(|_| {
            failure(
                session,
                ObserveErrorKind::InvalidState,
                OperationErrorKind::InvalidResponse,
            )
        })?;
        let status = raw_status.get("status").and_then(Value::as_str);
        let state = match (handle, status) {
            (ReadHandle::Operation { .. }, Some("NotStarted" | "Running"))
            | (ReadHandle::Job { .. }, Some("NotStarted" | "InProgress")) => Some((false, false)),
            (ReadHandle::Operation { .. }, Some("Succeeded"))
            | (ReadHandle::Job { .. }, Some("Completed")) => Some((true, true)),
            (_, Some("Failed" | "Cancelled")) | (ReadHandle::Job { .. }, Some("Deduped")) => {
                Some((true, false))
            }
            _ => None,
        };
        let (terminal, work_succeeded) = state.unwrap_or((false, false));
        let observation = Observation {
            handle: handle.clone(),
            raw_status,
            terminal,
            work_succeeded,
            result: None,
            requests: requests.load(Ordering::SeqCst),
        };
        *last = Some(observation.clone());
        if state.is_none() {
            return Err(failure(
                session,
                if observation
                    .raw_status
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some()
                {
                    ObserveErrorKind::UnknownState
                } else {
                    ObserveErrorKind::InvalidState
                },
                OperationErrorKind::InvalidResponse,
            ));
        }
        if terminal {
            if !work_succeeded && (options.wait || options.result) {
                let raw_error = observation.raw_status.get("error").cloned();
                let mut error = session.evidence(OperationErrorKind::OperationFailed {
                    status: observation.raw_status["status"]
                        .as_str()
                        .expect("recognized status is a string")
                        .to_owned(),
                    error: raw_error
                        .clone()
                        .and_then(|value| serde_json::from_value(value).ok())
                        .map(Box::new),
                });
                error.raw_service_error = raw_error.map(Box::new);
                return Err(error.into());
            }
            if options.result {
                return result(client, session, &path, &response, observation).await;
            }
            return Ok(observation);
        }
        if !options.wait {
            if options.result {
                return Err(failure(
                    session,
                    ObserveErrorKind::ResultNotReady,
                    OperationErrorKind::InvalidResponse,
                ));
            }
            return Ok(observation);
        }
        if requests.load(Ordering::SeqCst) >= options.max_requests.get() {
            return Err(failure(
                session,
                ObserveErrorKind::Operation,
                OperationErrorKind::Limit,
            ));
        }
        let delay = poll_delay(session, &response, options.max_delay)?;
        wait(session, executor, delay).await?;
    }
}

async fn wait(
    session: &mut OperationSession<'_, CountingExecutor<'_>>,
    executor: &dyn OperationExecutor,
    delay: Duration,
) -> Result<(), Failure> {
    let mut waiting = pin!(executor.wait(delay));
    poll_fn(|cx| {
        let cancelled = {
            let mut check = pin!(session.check_cancelled());
            check.as_mut().poll(cx)
        };
        if let Poll::Ready(Err(error)) = cancelled {
            return Poll::Ready(Err(error.into()));
        }
        waiting.as_mut().poll(cx).map(|result| {
            result.map_err(|error| {
                failure(
                    session,
                    ObserveErrorKind::Operation,
                    OperationErrorKind::Execution(error),
                )
            })
        })
    })
    .await
}

async fn get(
    session: &mut OperationSession<'_, CountingExecutor<'_>>,
    path: &str,
) -> Result<OperationResponse, Failure> {
    let operation = FabricOperation::new(
        ResourceMethod::Get,
        path,
        Vec::new(),
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|kind| failure(session, ObserveErrorKind::Operation, kind))?;
    let response = session.execute(operation).await?;
    if response.status != 200 {
        return Err(failure(
            session,
            ObserveErrorKind::UnexpectedStatus {
                status: response.status,
            },
            OperationErrorKind::InvalidResponse,
        ));
    }
    Ok(response)
}

async fn result(
    client: &Client,
    session: &mut OperationSession<'_, CountingExecutor<'_>>,
    path: &str,
    response: &OperationResponse,
    mut observation: Observation,
) -> Result<Observation, Failure> {
    let result_path = format!("{path}/result");
    let advertised = header(response, "Location")
        .map_err(|kind| failure(session, ObserveErrorKind::Operation, kind))?
        .ok_or_else(|| {
            failure(
                session,
                ObserveErrorKind::ResultNotAdvertised,
                OperationErrorKind::InvalidResponse,
            )
        })?;
    client
        .context()
        .endpoint()
        .validate_link(advertised, &result_path)
        .map_err(|_| {
            failure(
                session,
                ObserveErrorKind::Operation,
                OperationErrorKind::InvalidLink,
            )
        })?;
    let response = get(session, &result_path).await?;
    let media_type = header(&response, "Content-Type")
        .map_err(|kind| failure(session, ObserveErrorKind::Operation, kind))?
        .map(str::to_owned);
    observation.result = Some(ObservedResult {
        bytes: response.body,
        media_type,
    });
    Ok(observation)
}

fn poll_delay(
    session: &OperationSession<'_, CountingExecutor<'_>>,
    response: &OperationResponse,
    max_delay: Duration,
) -> Result<Duration, Failure> {
    let value = header(response, "Retry-After")
        .map_err(|kind| failure(session, ObserveErrorKind::Operation, kind))?;
    let invalid = || {
        failure(
            session,
            ObserveErrorKind::InvalidRetryAfter,
            OperationErrorKind::InvalidResponse,
        )
    };
    let delay = match value {
        None => Duration::from_secs(1),
        Some(value) => {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            let seconds = value.parse::<u64>().map_err(|_| invalid())?;
            if seconds == 0 {
                return Err(invalid());
            }
            Duration::from_secs(seconds)
        }
    };
    if delay > max_delay {
        return Err(failure(
            session,
            ObserveErrorKind::Operation,
            OperationErrorKind::Limit,
        ));
    }
    Ok(delay)
}

fn header<'a>(
    response: &'a OperationResponse,
    name: &str,
) -> Result<Option<&'a str>, OperationErrorKind> {
    let mut values = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let first = values.next().map(|(_, value)| value.as_str());
    if values.next().is_some() {
        return Err(OperationErrorKind::InvalidResponse);
    }
    Ok(first)
}

struct CountingExecutor<'a> {
    inner: &'a dyn OperationExecutor,
    requests: &'a AtomicUsize,
}

impl OperationExecutor for CountingExecutor<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(request).await
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.inner.wait(delay)
    }
}
