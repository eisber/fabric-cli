// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    RunClock, RunDeadline, RunError, RunExchange, RunFailure, RunOutput, RunOwnerKind,
    RunPreflight, RunResponseRecorder, RunTarget, RunTargetKind,
};
use crate::{
    EndpointBinding, JobClient, JobExecutionError, JobFuture, JobRequest, JobResponse,
    ServiceOrigin,
};
use azure_core::http::{Method, Url};
use fabric_sdk_operations::polling::{
    ObservationDriver, ObservationStep, ObserverFuture, WorkDomain, drive, work_state,
};
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

/// Remote state, independent of local wait/output success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    /// No mutation has been dispatched.
    NotSubmitted,
    /// Provider accepted execution, not completed it.
    Accepted,
    /// Binding verified completed execution and requested result.
    Completed,
    /// Provider verified execution failure.
    Failed,
    /// Known execution/result has incomplete coverage.
    Partial,
    /// Dispatch outcome or a future native state is not verified.
    Unknown,
}

/// Native execution identity, never a UDF correlation identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunHandleKind {
    /// Native item scheduler instance.
    Job(String),
    /// Actual generic operation, not a scheduler instance.
    Operation(String),
    /// Borrowed Livy session and exact statement; zero IDs remain valid.
    Livy {
        /// Original explicitly selected borrowed session.
        session: u64,
        /// Actual statement returned by that session.
        statement: u64,
    },
}

/// Validated native monitor on the original exact target and selected endpoint.
#[derive(Clone, Eq, PartialEq)]
pub struct RunHandle {
    target: RunTarget,
    endpoint: EndpointBinding,
    kind: RunHandleKind,
    url: Url,
}

impl RunHandle {
    /// Validate a native Notebook job-instance Location, never a collection URL.
    pub fn job(
        target: &RunTarget,
        endpoint: &EndpointBinding,
        location: &str,
    ) -> Result<Self, RunError> {
        if target.kind() != RunTargetKind::Notebook {
            return Err(RunError::WrongOwner);
        }
        let prefix = format!(
            "/v1/workspaces/{}/items/{}/jobs/instances/",
            target.owner().workspace(),
            target.owner().item(),
        );
        Self::location(target, endpoint, location, &prefix, false)
    }

    /// Validate an actual generic-operation Location supplied by an adapter.
    ///
    /// UDF targets reject this: invocationId is synchronous correlation only.
    pub fn operation(
        target: &RunTarget,
        endpoint: &EndpointBinding,
        location: &str,
    ) -> Result<Self, RunError> {
        if target.kind() == RunTargetKind::Udf {
            return Err(RunError::UnsupportedCapability);
        }
        Self::location(target, endpoint, location, "/v1/operations/", true)
    }

    fn location(
        target: &RunTarget,
        endpoint: &EndpointBinding,
        location: &str,
        prefix: &str,
        operation: bool,
    ) -> Result<Self, RunError> {
        let url = Url::parse(location).map_err(|_| RunError::Protocol)?;
        let id = url.path().strip_prefix(prefix).ok_or(RunError::Protocol)?;
        crate::identifier(id).map_err(|_| RunError::Protocol)?;
        endpoint
            .validate_link(location, &format!("{prefix}{id}"))
            .map_err(|_| RunError::Protocol)?;
        if endpoint.origin() != ServiceOrigin::Fabric {
            return Err(RunError::Context);
        }
        let kind = if operation {
            RunHandleKind::Operation(id.into())
        } else {
            RunHandleKind::Job(id.into())
        };
        Ok(Self {
            target: target.clone(),
            endpoint: endpoint.clone(),
            kind,
            url,
        })
    }

    /// Bind a real statement ID to the adapter's verified exact LH-12 monitor.
    ///
    /// The route/version/casing come from the Lakehouse binding, not from this
    /// foundation. The supplied path must end in this session/statement route.
    pub fn livy(
        target: &RunTarget,
        endpoint: &EndpointBinding,
        monitor: &str,
        statement: u64,
    ) -> Result<Self, RunError> {
        if target.owner().kind() != RunOwnerKind::Lakehouse
            || target.kind() != RunTargetKind::LivyStatement
        {
            return Err(RunError::WrongOwner);
        }
        let session = target.session().ok_or(RunError::InvalidTarget)?;
        let url = Url::parse(monitor).map_err(|_| RunError::Protocol)?;
        let owner_prefix = format!(
            "/v1/workspaces/{}/lakehouses/{}/",
            target.owner().workspace(),
            target.owner().item(),
        );
        let suffix = format!("/sessions/{session}/statements/{statement}");
        if !url.path().starts_with(&owner_prefix)
            || !url.path().ends_with(&suffix)
            || url.path().contains('%')
        {
            return Err(RunError::Protocol);
        }
        endpoint
            .validate_link(monitor, url.path())
            .map_err(|_| RunError::Protocol)?;
        if endpoint.origin() != ServiceOrigin::Fabric {
            return Err(RunError::Context);
        }
        Ok(Self {
            target: target.clone(),
            endpoint: endpoint.clone(),
            kind: RunHandleKind::Livy { session, statement },
            url,
        })
    }
    /// Exact scoped target.
    pub fn target(&self) -> &RunTarget {
        &self.target
    }
    /// Original endpoint, never inferred from the response hostname.
    pub fn endpoint(&self) -> &EndpointBinding {
        &self.endpoint
    }
    /// Typed native identity.
    pub fn kind(&self) -> &RunHandleKind {
        &self.kind
    }
    /// Validated monitor; use only in the original context.
    pub fn url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.kind {
            RunHandleKind::Job(_) => "job",
            RunHandleKind::Operation(_) => "operation",
            RunHandleKind::Livy { .. } => "livy",
        };
        f.debug_struct("RunHandle")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

/// Actual bounded evidence survives local phase failure; raw fields are untrusted.
#[derive(Clone)]
pub struct RunEvidence {
    /// Original target.
    pub target: RunTarget,
    /// Original caller-selected endpoint.
    pub endpoint: EndpointBinding,
    /// One executor mutation attempt began, not proof of a remote side effect.
    pub mutation_may_have_occurred: bool,
    /// Remote state remains separate from any local failure.
    pub outcome: RunOutcome,
    /// Validated native identity, when actually received.
    pub handle: Option<RunHandle>,
    /// UDF correlation only, never an asynchronous monitor.
    pub correlation_id: Option<String>,
    /// Original complete submit response, retained even on parse/status failure.
    pub initial_response: Option<JobResponse>,
    /// Whether the initial response completed streaming, rather than a prefix.
    pub initial_response_complete: bool,
    /// Most recent complete observation/result response.
    pub last_response: Option<JobResponse>,
    /// Whether the latest observation completed streaming.
    pub last_response_complete: bool,
    /// A received body exceeded the bound and was omitted, not a complete empty body.
    pub response_body_omitted: bool,
    /// Binding-interpreted native result; false, zero and null remain distinct.
    pub result: Option<Value>,
    /// Whether local output commit succeeded.
    pub output_committed: bool,
}

impl fmt::Debug for RunEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunEvidence")
            .field("outcome", &self.outcome)
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .field("handle", &self.handle)
            .field("response_body_omitted", &self.response_body_omitted)
            .field("output_committed", &self.output_committed)
            .finish_non_exhaustive()
    }
}

/// Adapter-interpreted native observation; HTTP 200 alone is not completion.
pub struct RunObservation {
    /// Remote execution/result coverage.
    pub outcome: RunOutcome,
    /// Whether the binding permits another observation.
    pub terminal: bool,
    /// Native result, retaining null/false/zero when present.
    pub result: Option<Value>,
}

/// Pure native decoder consumed by the shared v2 observation bridge.
pub trait RunObserver: Send + Sync {
    /// Validate identity and decode native status/output, with no I/O.
    fn decode(
        &self,
        handle: &RunHandle,
        response: &JobResponse,
    ) -> Result<RunObservation, RunError>;
}

/// Native scheduler status decoder; unknown/Deduped/Cancelled are not success.
pub struct SchedulerObserver;

impl RunObserver for SchedulerObserver {
    fn decode(
        &self,
        handle: &RunHandle,
        response: &JobResponse,
    ) -> Result<RunObservation, RunError> {
        let RunHandleKind::Job(id) = handle.kind() else {
            return Err(RunError::WrongOwner);
        };
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| RunError::Protocol)?;
        if !value.is_object()
            || value
                .get("id")
                .is_some_and(|value| value.as_str() != Some(id))
            || value
                .get("itemId")
                .is_some_and(|value| value.as_str() != Some(handle.target.owner().item()))
        {
            return Err(RunError::Protocol);
        }
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .ok_or(RunError::Protocol)?;
        let (outcome, terminal) = match work_state(WorkDomain::Job, status) {
            Some((false, _)) => (RunOutcome::Accepted, false),
            Some((true, true)) => (RunOutcome::Completed, true),
            Some((true, false)) => (RunOutcome::Failed, true),
            None => (RunOutcome::Unknown, true),
        };
        Ok(RunObservation {
            outcome,
            terminal,
            result: Some(value),
        })
    }
}

/// One v2 execution lifecycle on caller-injected jobs HTTP/time primitives.
///
/// Mutation requests are single-attempt, observations are GET-only, and the
/// shared executor alone owns HTTP retries. This bridge does not use the v1
/// interval-override/cancel-on-timeout policies or spawn background tasks.
pub struct RunSession<'a> {
    client: &'a JobClient,
    plan: RunPreflight,
    clock: Arc<dyn RunClock>,
    deadline: RunDeadline,
    cancelled: JobFuture<'a, ()>,
    mutation_consumed: bool,
    evidence: RunEvidence,
}

impl<'a> RunSession<'a> {
    /// Start one total deadline after successful pure preflight.
    pub fn new(
        client: &'a JobClient,
        plan: RunPreflight,
        clock: Arc<dyn RunClock>,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Result<Self, RunError> {
        let deadline = RunDeadline::start(clock.as_ref(), plan.timeout())?;
        Ok(Self {
            client,
            evidence: RunEvidence {
                target: plan.target().clone(),
                endpoint: client.context.endpoint().clone(),
                mutation_may_have_occurred: false,
                outcome: RunOutcome::NotSubmitted,
                handle: None,
                correlation_id: None,
                initial_response: None,
                initial_response_complete: false,
                last_response: None,
                last_response_complete: false,
                response_body_omitted: false,
                result: None,
                output_committed: false,
            },
            plan,
            clock,
            deadline,
            cancelled: Box::pin(LatchedInterruption {
                inner: Box::pin(cancelled),
                completed: false,
            }),
            mutation_consumed: false,
        })
    }
    /// Shared normalized plan.
    pub fn plan(&self) -> &RunPreflight {
        &self.plan
    }
    /// Inspect evidence without emitting provider text or identifiers.
    pub fn evidence(&self) -> &RunEvidence {
        &self.evidence
    }
    /// Exact remaining budget for remote binding resolution and adapter phases.
    pub fn remaining(&self) -> Result<Duration, RunError> {
        self.deadline.remaining(self.clock.as_ref())
    }
    /// Guard remote resolution or result/verdict work in the same total budget.
    pub async fn phase<T>(
        &mut self,
        operation: impl Future<Output = Result<T, RunError>>,
    ) -> Result<T, Box<RunFailure>> {
        let result = self
            .deadline
            .guard(self.clock.as_ref(), self.cancelled.as_mut(), operation)
            .await
            .and_then(|result| result);
        result.map_err(|source| self.failure(source))
    }
    fn failure(&self, source: RunError) -> Box<RunFailure> {
        Box::new(RunFailure {
            source,
            evidence: self.evidence.clone(),
        })
    }

    /// Perform at most one native mutation POST; no auth/status/parse/output replay.
    ///
    /// The adapter supplies an exact endpoint-bound URL and native bytes. The
    /// foundation does not translate legacy/modern Notebook or UDF payloads.
    pub async fn submit(
        &mut self,
        url: Url,
        body: Vec<u8>,
    ) -> Result<JobResponse, Box<RunFailure>> {
        if self.mutation_consumed {
            return Err(self.failure(RunError::Replay));
        }
        self.mutation_consumed = true;
        self.request(Method::Post, url, body, true).await
    }

    /// Observe an exact existing handle with zero submission attempts.
    ///
    /// Uses provider Retry-After as a minimum (default five seconds); if it
    /// cannot fit strictly before the deadline, no early GET is sent. Native
    /// adapters share this bridge, not separate timers/pollers or HTTP retries.
    pub async fn observe(
        &mut self,
        handle: &RunHandle,
        observer: &dyn RunObserver,
    ) -> Result<RunOutcome, Box<RunFailure>> {
        self.bind_handle(handle)?;
        let delay = match self
            .evidence
            .last_response
            .as_ref()
            .or(self.evidence.initial_response.as_ref())
        {
            Some(response) => retry_after(response).map_err(|source| self.failure(source))?,
            None => Duration::ZERO,
        };
        drive(
            &mut RunObservationDriver {
                session: self,
                handle,
                observer,
            },
            Some(delay),
        )
        .await
    }

    /// Retain a validated native handle without conflating acceptance/completion.
    pub fn bind_handle(&mut self, handle: &RunHandle) -> Result<(), Box<RunFailure>> {
        if handle.target() != self.plan.target()
            || handle.endpoint() != self.client.context.endpoint()
        {
            return Err(self.failure(RunError::Context));
        }
        self.evidence.handle = Some(handle.clone());
        Ok(())
    }

    /// Record binding-interpreted native evidence, never infer it from HTTP alone.
    pub fn record(&mut self, outcome: RunOutcome, result: Option<Value>) {
        self.evidence.outcome = outcome;
        self.evidence.result = result;
    }

    /// Interpret a native result/verdict within the original total budget.
    ///
    /// Already decoded evidence survives a deadline/interruption winning the
    /// final local verdict. The decoder is pure and must not perform I/O.
    pub async fn interpret(
        &mut self,
        response: &JobResponse,
        decode: impl FnOnce(&JobResponse) -> Result<RunObservation, RunError> + Send,
    ) -> Result<RunObservation, Box<RunFailure>> {
        let mut decoded = None;
        let result = self
            .deadline
            .guard(self.clock.as_ref(), self.cancelled.as_mut(), async {
                decoded = Some(decode(response)?);
                Ok(())
            })
            .await
            .and_then(|result| result);
        if let Some(observation) = &decoded {
            self.record(observation.outcome, observation.result.clone());
        }
        result.map_err(|source| self.failure(source))?;
        decoded.ok_or_else(|| self.failure(RunError::Protocol))
    }

    /// Retain a synchronous UDF correlation ID without creating a run handle.
    pub fn correlate(&mut self, id: &str) -> Result<(), Box<RunFailure>> {
        if self.plan.target().kind() != RunTargetKind::Udf || id.is_empty() {
            return Err(self.failure(RunError::Protocol));
        }
        self.evidence.correlation_id = Some(id.into());
        Ok(())
    }

    /// No-clobber output commit within the original budget.
    ///
    /// A race, disk error, timeout or interruption preserves remote outcome,
    /// result and handle. The local error is non-success and never reruns work.
    pub async fn commit(
        &mut self,
        output: &dyn RunOutput,
        bytes: &[u8],
    ) -> Result<(), Box<RunFailure>> {
        let mut committed = false;
        let result = self
            .deadline
            .guard(self.clock.as_ref(), self.cancelled.as_mut(), async {
                let result = output.commit(bytes).await;
                committed = result.is_ok();
                result
            })
            .await
            .and_then(|result| result);
        self.evidence.output_committed |= committed;
        result.map_err(|source| self.failure(source))
    }

    async fn delay(&mut self, delay: Duration) -> Result<(), Box<RunFailure>> {
        let remaining = self.remaining().map_err(|source| self.failure(source))?;
        if delay >= remaining {
            return Err(self.failure(RunError::Deadline));
        }
        if !delay.is_zero() {
            let clock = self.clock.clone();
            self.phase(async move {
                clock.sleep(delay).await;
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    async fn request(
        &mut self,
        method: Method,
        url: Url,
        body: Vec<u8>,
        mutation: bool,
    ) -> Result<JobResponse, Box<RunFailure>> {
        let endpoint = self.client.context.endpoint();
        if endpoint.origin() != ServiceOrigin::Fabric
            || self.client.context.selection().audience() != endpoint.audience()
            || endpoint.validate_url(&url).is_err()
        {
            return Err(self.failure(RunError::Context));
        }
        let remaining = self.remaining().map_err(|source| self.failure(source))?;
        let executor = self
            .client
            .executor
            .with_run_budget(remaining)
            .map_err(|_| self.failure(RunError::UnsupportedExecutor))?;
        if !executor.supports_request_headers(&self.client.headers) {
            return Err(self.failure(RunError::UnsupportedExecutor));
        }
        let mut attempted = false;
        let client = self.client;
        let limit = client.limits.max_response_bytes;
        let recorder = RunResponseRecorder::new(limit);
        let scopes = [endpoint.audience().scope()];
        let token = self
            .deadline
            .guard(
                self.clock.as_ref(),
                self.cancelled.as_mut(),
                client.context.credential().get_token(&scopes, None),
            )
            .await
            .map_err(|source| self.failure(source))?
            .map_err(|_| self.failure(RunError::Credential))?;
        // Scope the exchange after acquisition; its own retries/streaming must
        // not get a reset budget or a hidden shorter v1 exchange watchdog.
        let remaining = self.remaining().map_err(|source| self.failure(source))?;
        let executor = client
            .executor
            .with_run_budget(remaining)
            .map_err(|_| self.failure(RunError::UnsupportedExecutor))?;
        let result = self
            .deadline
            .guard(self.clock.as_ref(), self.cancelled.as_mut(), async {
                attempted = true;
                let exchange = executor
                    .execute_run(
                        JobRequest {
                            method,
                            url,
                            token: &token,
                            body,
                            max_response_bytes: limit,
                            headers: client.headers.clone(),
                        },
                        &recorder,
                    )
                    .await;
                let (response, error, complete) = match exchange {
                    RunExchange::Complete(response) => (Some(response), None, true),
                    RunExchange::Failed { error, observed } => (observed, Some(error), false),
                };
                if let Some(response) = &response {
                    recorder.record(response, complete)?;
                }
                if let Some(error) = error {
                    return Err(execution_error(error));
                }
                response.ok_or(RunError::Protocol)
            })
            .await
            .and_then(|result| result);
        if mutation && attempted {
            self.evidence.mutation_may_have_occurred = true;
            self.evidence.outcome = RunOutcome::Unknown;
        }
        let received = recorder.snapshot().map_err(|source| self.failure(source))?;
        if let Some(response) = received.response {
            self.evidence.response_body_omitted |= received.body_omitted;
            if mutation {
                if response.status == 202 {
                    self.evidence.outcome = RunOutcome::Accepted;
                    // Headers may establish a native identity even if streaming
                    // subsequently failed or the deadline won this wake.
                    if self.plan.target().kind() == RunTargetKind::Notebook {
                        let mut locations = response
                            .headers
                            .iter()
                            .filter(|(name, _)| name.eq_ignore_ascii_case("location"));
                        if let Some((_, location)) = locations.next()
                            && locations.next().is_none()
                            && let Ok(handle) = RunHandle::job(
                                self.plan.target(),
                                self.client.context.endpoint(),
                                location,
                            )
                        {
                            self.evidence.handle = Some(handle);
                        }
                    }
                }
                self.evidence.initial_response = Some(response);
                self.evidence.initial_response_complete = received.complete;
            } else {
                self.evidence.last_response = Some(response);
                self.evidence.last_response_complete = received.complete;
            }
        }
        let response = result.map_err(|source| self.failure(source))?;
        if response.status >= 400 {
            return Err(self.failure(RunError::Http(response.status)));
        }
        if !mutation && response.status != 200 {
            return Err(self.failure(RunError::Protocol));
        }
        Ok(response)
    }
}

struct RunObservationDriver<'a, 'session> {
    session: &'a mut RunSession<'session>,
    handle: &'a RunHandle,
    observer: &'a dyn RunObserver,
}

impl ObservationDriver for RunObservationDriver<'_, '_> {
    type Output = RunOutcome;
    type Error = Box<RunFailure>;
    fn step(&mut self) -> ObserverFuture<'_, Result<ObservationStep<RunOutcome>, Box<RunFailure>>> {
        Box::pin(async {
            let response = self
                .session
                .request(Method::Get, self.handle.url().clone(), Vec::new(), false)
                .await?;
            let observer = self.observer;
            let handle = self.handle;
            let observation = self
                .session
                .interpret(&response, move |response| observer.decode(handle, response))
                .await?;
            if observation.terminal {
                Ok(ObservationStep::Complete(observation.outcome))
            } else {
                let delay =
                    retry_after(&response).map_err(|source| self.session.failure(source))?;
                Ok(ObservationStep::Continue(delay))
            }
        })
    }
    fn wait(&mut self, delay: Duration) -> ObserverFuture<'_, Result<(), Box<RunFailure>>> {
        Box::pin(self.session.delay(delay))
    }
}

struct LatchedInterruption<'a> {
    inner: JobFuture<'a, ()>,
    completed: bool,
}
impl Future for LatchedInterruption<'_> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.completed || self.inner.as_mut().poll(cx).is_ready() {
            self.completed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

fn execution_error(error: JobExecutionError) -> RunError {
    match error {
        JobExecutionError::TimedOut => RunError::Deadline,
        JobExecutionError::Cancelled => RunError::Interrupted,
        JobExecutionError::ResponseLimit => RunError::ResponseLimit,
        JobExecutionError::Failed | JobExecutionError::RetriesExhausted => RunError::Transport,
    }
}

fn retry_after(response: &JobResponse) -> Result<Duration, RunError> {
    fabric_sdk_operations::polling::retry_after(&response.headers, Duration::from_secs(5), true)
        .map_err(|_| RunError::Protocol)
}
