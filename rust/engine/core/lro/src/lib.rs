// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric/Azure/Storage LRO and job reconciliation; never repeats a mutation.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use fabric_core::diagnostics::{
    DelaySource, DiagnosticDuration, DiagnosticEvent, DiagnosticOutcome, OperationOutcome,
    PollPurpose, PollingStrategy, RequestId, ResultAction,
};
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, MAX_RESPONSE_BYTES, OperationContext, OperationHandle as Handle,
    OperationKind as Strategy, operation_handle, resolve_operation_url, validate_response,
};
use fabric_sdk_http::{bounded_with_clock, retry_after, sleep_with_budget};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use url::Url;

/// First Fabric operation/job delay; next is seven, then server hints capped at twenty seconds.
pub const INITIAL_DELAY: Duration = Duration::from_secs(2);
/// Azure/Storage fallback interval; the legacy public name is retained for compatibility.
pub const JOB_POLLING_INTERVAL: Duration = Duration::from_secs(10);
const POLL_REQUEST_TIMEOUT: Duration = Duration::from_secs(240);

/// Safe status observed on an actual successful status GET, never raw service data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PollProgress {
    status: &'static str,
    job: bool,
    percentage: Option<f64>,
}
impl PollProgress {
    /// Canonical recognized status; excludes URLs, headers, body fields and errors.
    pub fn status(self) -> &'static str {
        self.status
    }
    /// Whether this observation uses the Fabric job-instance protocol.
    pub fn is_job(self) -> bool {
        self.job
    }
    /// Finite service-supplied numeric percentage from zero through one hundred.
    /// Missing, null, or invalid metadata is never exposed as raw service text.
    pub fn percentage_complete(self) -> Option<f64> {
        self.percentage
    }
}

/// Caller-owned observation boundary. The engine never writes console output.
#[async_trait]
pub trait PollObserver: Send + Sync {
    /// Observe sanitized diagnostics independently of service progress.
    ///
    /// The default is silent. Diagnostic delivery is bounded by the same
    /// cancellation/deadline and its errors never trigger HTTP retries.
    async fn diagnostic(&self, _event: DiagnosticEvent) -> Result<(), CoreError> {
        Ok(())
    }

    /// Observe validated reconciliation before its first wait, not a service status.
    ///
    /// Uses the same cancellation, deadline and error handling as status observations.
    async fn started(&self, _kind: Strategy) -> Result<(), CoreError> {
        Ok(())
    }

    /// Observe the initial HTTP acknowledgement, not a backend execution state.
    /// Defaults to the existing start callback for compatibility.
    async fn accepted(&self, kind: Strategy, _http_status: u16) -> Result<(), CoreError> {
        self.started(kind).await
    }

    /// Observe the selected next status-check delay, including server hints.
    /// Callback time counts toward the delay and the reconciliation deadline.
    async fn waiting(&self, _kind: Strategy, _delay: Duration) -> Result<(), CoreError> {
        Ok(())
    }

    /// Observe a status check after its required wait, before credentials or HTTP.
    ///
    /// Called once per logical status GET, not for individual transport retries.
    /// Uses the same cancellation, deadline and error handling as status observations.
    async fn polling(&self, _kind: Strategy) -> Result<(), CoreError> {
        Ok(())
    }

    /// Observe a recognized status before waiting again or returning its outcome.
    ///
    /// Observation shares the operation cancellation/deadline. Return only safe
    /// local errors; observation failure stops local polling, not backend work.
    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError>;
}

struct NoProgress;
#[async_trait]
impl PollObserver for NoProgress {
    async fn observe(&self, _: PollProgress) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Per-reconciliation timing overrides. Defaults impose no overall time limit.
#[derive(Clone, Copy, Debug, Default)]
pub struct LroConfig {
    poll_interval: Option<Duration>,
    timeout: Option<Duration>,
}
impl LroConfig {
    /// Validate optional positive polling interval and total timeout durations.
    pub fn new(
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
    ) -> Result<Self, CoreError> {
        for (name, duration) in [("poll_interval", poll_interval), ("timeout", timeout)] {
            if let Some(duration) = duration
                && (duration.is_zero() || Instant::now().checked_add(duration).is_none())
            {
                return Err(CoreError::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid {name}: must be positive and within clock range"),
                ));
            }
        }
        Ok(Self {
            poll_interval,
            timeout,
        })
    }
}

fn protocol(message: &str) -> CoreError {
    CoreError::new(ErrorKind::Protocol, message)
}

fn body(response: &ApiResponse) -> Result<Value, CoreError> {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(protocol(&format!(
            "Operation response exceeds the {MAX_RESPONSE_BYTES}-byte size limit"
        )));
    }
    fabric_core_json::decode(&response.body).map_err(|_| protocol("Malformed operation JSON"))
}
/// Error objects in precedence order: Fabric/ARM `error`, Fabric job `failureReason`, top level.
fn error_objects(json: &Value) -> impl Iterator<Item = &Value> {
    [json.get("error"), json.get("failureReason"), Some(json)]
        .into_iter()
        .flatten()
        .filter(|value| value.is_object())
}
fn error_code(json: &Value) -> Option<&str> {
    error_objects(json).find_map(|error| {
        error
            .get("errorCode")
            .and_then(Value::as_str)
            .or_else(|| error.get("code").and_then(Value::as_str))
    })
}
fn error_message(json: &Value) -> Option<&str> {
    error_objects(json).find_map(|error| error.get("message").and_then(Value::as_str))
}
fn service_failure(response: &ApiResponse, json: Option<&Value>, summary: &str) -> CoreError {
    service_failure_parts(
        response,
        json.and_then(error_code),
        json.and_then(error_message),
        summary,
    )
}
fn service_failure_parts(
    response: &ApiResponse,
    code: Option<&str>,
    detail: Option<&str>,
    summary: &str,
) -> CoreError {
    let code = code
        .or_else(|| response.header("x-ms-public-api-error-code"))
        .or_else(|| response.header("x-ms-error-code"));
    let mut message = summary.to_owned();
    if let Some(detail) = detail.and_then(fabric_core::sanitize_service_message) {
        message.push_str(": ");
        message.push_str(&detail);
    }
    if let Some(request_id) = ["requestId", "x-ms-request-id"]
        .into_iter()
        .find_map(|name| response.header(name).and_then(RequestId::parse))
    {
        message.push_str(&format!(" (request ID: {request_id})"));
    }
    CoreError::service_failure(message, code)
}
/// Name an unrecognized status only when it is a short identifier.
fn unknown_status(kind: &str, status: &str) -> CoreError {
    if !status.is_empty()
        && status.len() <= 64
        && status
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        protocol(&format!("Unrecognized {kind} status '{status}'"))
    } else {
        protocol(&format!(
            "Unrecognized {kind} status (value omitted because it is not a short identifier)"
        ))
    }
}
#[derive(Clone, Copy)]
enum State {
    Pending,
    Success,
    Failed,
    Cancelled,
}
fn polling_strategy(strategy: Strategy) -> PollingStrategy {
    match strategy {
        Strategy::Fabric => PollingStrategy::Fabric,
        Strategy::FabricJob => PollingStrategy::FabricJob,
        Strategy::ArmStatus => PollingStrategy::ArmStatus,
        Strategy::ArmLocation => PollingStrategy::ArmLocation,
    }
}
async fn diagnostic(
    observer: &dyn PollObserver,
    clock: &dyn Clock,
    context: &OperationContext,
    event: DiagnosticEvent,
    terminal: Option<OperationOutcome>,
) -> Result<(), CoreError> {
    bounded_with_clock(clock, context, observer.diagnostic(event))
        .await?
        .map_err(|_| {
            CoreError::diagnostic_failure(
                terminal.map_or(DiagnosticOutcome::Accepted, DiagnosticOutcome::Terminal),
            )
        })
}
fn state(json: &Value) -> Result<(State, &'static str), CoreError> {
    let status = json
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("Missing operation status"))?;
    match status.to_ascii_lowercase().as_str() {
        "succeeded" => Ok((State::Success, "Succeeded")),
        "completed" => Ok((State::Success, "Completed")),
        "notstarted" => Ok((State::Pending, "NotStarted")),
        "running" => Ok((State::Pending, "Running")),
        "inprogress" => Ok((State::Pending, "InProgress")),
        "accepted" => Ok((State::Pending, "Accepted")),
        "creating" => Ok((State::Pending, "Creating")),
        "updating" => Ok((State::Pending, "Updating")),
        "deleting" => Ok((State::Pending, "Deleting")),
        "failed" => Ok((State::Failed, "Failed")),
        "cancelled" | "canceled" => Ok((State::Cancelled, "Cancelled")),
        _ => Err(unknown_status("operation", status)),
    }
}
fn job_state(json: &Value) -> Result<(State, &'static str), CoreError> {
    let status = json
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("Missing job instance status"))?;
    match status {
        "NotStarted" => Ok((State::Pending, "NotStarted")),
        "InProgress" => Ok((State::Pending, "InProgress")),
        "Completed" => Ok((State::Success, "Completed")),
        "Deduped" => Ok((State::Success, "Deduped")),
        "Failed" => Ok((State::Failed, "Failed")),
        "Cancelled" => Ok((State::Cancelled, "Cancelled")),
        _ => Err(unknown_status("job instance", status)),
    }
}
fn percentage(json: &Value, clock: &dyn Clock, warned: &mut bool) -> Option<f64> {
    let value = json.get("percentageComplete")?;
    if value.is_null() {
        return None;
    }
    let percentage = value
        .as_f64()
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value));
    if percentage.is_none() && !*warned {
        clock.diagnostic("Invalid percentageComplete; omitting polling percentage");
        *warned = true;
    }
    percentage
}
fn get(url: Url) -> ApiRequest {
    ApiRequest {
        method: "GET".into(),
        url,
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}
async fn fetch(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    url: Url,
    context: &OperationContext,
) -> Result<ApiResponse, CoreError> {
    let now = clock.now();
    context.remaining_at(now)?;
    let deadline = now
        .checked_add(POLL_REQUEST_TIMEOUT)
        .ok_or_else(|| protocol("Polling request deadline exceeds clock range"))?;
    let request_context = OperationContext {
        deadline: Some(
            context
                .deadline
                .map_or(deadline, |overall| overall.min(deadline)),
        ),
        cancellation: context.cancellation.clone(),
    };
    let response = bounded_with_clock(
        clock,
        &request_context,
        executor.execute_poll(get(url), audience, &request_context),
    )
    .await??;
    context.remaining_at(clock.now())?;
    validate_response(&response)?;
    if (300..400).contains(&response.status) {
        return Err(protocol("Operation redirects are not followed"));
    }
    Ok(response)
}
async fn reconcile(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    initial: ApiResponse,
    handle: &Handle,
    context: &OperationContext,
    settings: (LroConfig, &dyn PollObserver, &mut Option<OperationOutcome>),
) -> Result<ApiResponse, CoreError> {
    let (config, observer, terminal) = settings;
    diagnostic(
        observer,
        clock,
        context,
        DiagnosticEvent::Polling {
            strategy: polling_strategy(handle.kind()),
        },
        *terminal,
    )
    .await?;
    bounded_with_clock(
        clock,
        context,
        observer.accepted(handle.kind(), initial.status),
    )
    .await??;
    let mut previous = initial.clone();
    let mut delay = INITIAL_DELAY;
    let mut warned_percentage = false;
    loop {
        context.remaining_at(clock.now())?;
        // Invalid hints fall back to the client policy without revealing header contents.
        let job = matches!(handle.kind(), Strategy::FabricJob);
        let server_interval = matches!(handle.kind(), Strategy::ArmLocation | Strategy::ArmStatus);
        let fallback = config.poll_interval.unwrap_or(if server_interval {
            JOB_POLLING_INTERVAL
        } else {
            delay
        });
        let mut source = if config.poll_interval.is_some() {
            DelaySource::Configured
        } else {
            DelaySource::Client
        };
        let server = match retry_after(&previous, clock.wall_now()) {
            Ok(delay) => delay,
            Err(_) => {
                source = DelaySource::InvalidServerHint;
                clock.diagnostic(if config.poll_interval.is_some() {
                    "Invalid Retry-After header; using configured polling interval"
                } else if server_interval {
                    "Invalid Retry-After header; using 10-second Azure/Storage polling interval"
                } else {
                    "Invalid Retry-After header; using client polling schedule"
                });
                None
            }
        };
        let wait = match (config.poll_interval, server_interval, server) {
            (None, false, _) if delay < Duration::from_secs(20) => delay,
            (None, false, Some(server)) => delay.min(server),
            (None, true, Some(server)) => server,
            _ => fallback.max(server.unwrap_or_default()),
        };
        if server.is_some_and(|server| server == wait) {
            source = DelaySource::Server;
        }
        let before = clock.now();
        bounded_with_clock(clock, context, observer.waiting(handle.kind(), wait)).await??;
        diagnostic(
            observer,
            clock,
            context,
            DiagnosticEvent::PollWait {
                delay: DiagnosticDuration::from_duration(wait),
                purpose: PollPurpose::Status,
                source,
            },
            *terminal,
        )
        .await?;
        let remaining_wait = wait.saturating_sub(clock.now().duration_since(before));
        sleep_with_budget(clock, remaining_wait, context).await?;
        bounded_with_clock(clock, context, observer.polling(handle.kind())).await??;
        let response = fetch(executor, clock, audience, handle.url().clone(), context).await?;
        if !(200..300).contains(&response.status) {
            let json = body(&response).ok();
            let message = if json.is_some() {
                format!(
                    "Operation status request failed with HTTP {}",
                    response.status
                )
            } else {
                format!(
                    "Operation status request failed with HTTP {}; error response was not valid JSON",
                    response.status
                )
            };
            return Err(service_failure(&response, json.as_ref(), &message));
        }
        let (current, status, percentage, failure_json, failure_message) = match handle.kind() {
            Strategy::ArmLocation => match response.status {
                200 => (State::Success, "Succeeded", None, None, None),
                201 | 202 => (State::Pending, "Running", None, None, None),
                _ => {
                    return Err(protocol(&format!(
                        "Unexpected HTTP {} from Azure/Storage Location status poll; expected 200, 201, or 202",
                        response.status
                    )));
                }
            },
            kind => {
                if kind == Strategy::FabricJob && response.status != 200 {
                    return Err(protocol(&format!(
                        "Unexpected HTTP {} from job instance status poll; expected 200",
                        response.status
                    )));
                }
                let json = body(&response)?;
                let (current, status) = if job {
                    job_state(&json)?
                } else {
                    state(&json)?
                };
                let failed = matches!(current, State::Failed | State::Cancelled);
                let failure_message = if job && failed {
                    json.get("failureReason")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .filter(|message| !message.is_empty())
                        .map(str::to_owned)
                } else {
                    None
                };
                let percentage = percentage(&json, clock, &mut warned_percentage);
                (
                    current,
                    status,
                    percentage,
                    failed.then_some(json),
                    failure_message,
                )
            }
        };
        *terminal = match current {
            State::Pending => None,
            State::Success => Some(OperationOutcome::Succeeded),
            State::Failed => Some(OperationOutcome::Failed),
            State::Cancelled => Some(OperationOutcome::Cancelled),
        };
        bounded_with_clock(
            clock,
            context,
            observer.observe(PollProgress {
                status,
                job,
                percentage,
            }),
        )
        .await??;
        if let Some(outcome) = *terminal {
            diagnostic(
                observer,
                clock,
                context,
                DiagnosticEvent::PollCompleted { outcome },
                *terminal,
            )
            .await?;
        }
        match current {
            State::Failed | State::Cancelled => {
                let message = match (job, current) {
                    (true, State::Cancelled) => "Backend job was cancelled",
                    (true, _) => "Backend job failed",
                    (false, State::Cancelled) => "Backend operation was cancelled",
                    (false, _) => "Backend operation failed",
                };
                if !job {
                    return Err(service_failure(&response, failure_json.as_ref(), message));
                }
                // Job failure text is untrusted and is exposed only in final JSON.
                let code = failure_json.as_ref().and_then(|json| {
                    json.get("failureReason")
                        .and_then(error_code)
                        .or_else(|| error_code(json))
                });
                let mut error = service_failure_parts(&response, code, None, message);
                error.failure_reason =
                    failure_message.map(|message| fabric_core::JobFailureReason { message });
                return Err(error);
            }
            State::Pending => {
                previous = response;
                if !server_interval {
                    delay = if delay == INITIAL_DELAY {
                        Duration::from_secs(7)
                    } else {
                        Duration::from_secs(20)
                    };
                }
            }
            State::Success => {
                if job {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollResult {
                            action: ResultAction::StatusResponse,
                        },
                        *terminal,
                    )
                    .await?;
                    return Ok(response);
                }
                if !matches!(handle.kind(), Strategy::Fabric) {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollResult {
                            action: ResultAction::InitialResponse,
                        },
                        *terminal,
                    )
                    .await?;
                    let mut completed = initial;
                    completed.status = 200;
                    return Ok(completed);
                }
                let json = body(&response)?;
                let result_url = if let Some(location) = response.header("location") {
                    handle.fabric_result_url(location)?
                } else if let Some(resource) = json.get("resourceLocation").and_then(Value::as_str)
                {
                    resolve_operation_url(audience, resource, handle.url())?
                } else {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollResult {
                            action: ResultAction::StatusResponse,
                        },
                        *terminal,
                    )
                    .await?;
                    return Ok(response);
                };
                if &result_url == handle.url() {
                    return Err(protocol("Fabric result points to its status endpoint"));
                }
                // A terminal status can advertise a result before it is ready.
                // Its Retry-After is still a not-before constraint, sharing the
                // original reconciliation deadline (not a fresh result budget).
                let mut source = DelaySource::Server;
                let result_delay = match retry_after(&response, clock.wall_now()) {
                    Ok(delay) => delay.unwrap_or_default(),
                    Err(_) => {
                        source = DelaySource::InvalidServerHint;
                        clock.diagnostic(
                            "Invalid Retry-After header; using client polling schedule",
                        );
                        INITIAL_DELAY
                    }
                };
                diagnostic(
                    observer,
                    clock,
                    context,
                    DiagnosticEvent::PollResult {
                        action: ResultAction::Fetch,
                    },
                    *terminal,
                )
                .await?;
                if !result_delay.is_zero() {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollWait {
                            delay: DiagnosticDuration::from_duration(result_delay),
                            purpose: PollPurpose::Result,
                            source,
                        },
                        *terminal,
                    )
                    .await?;
                    sleep_with_budget(clock, result_delay, context).await?;
                }
                let result_response = fetch(executor, clock, audience, result_url, context).await?;
                if (200..300).contains(&result_response.status) {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollResult {
                            action: ResultAction::Retrieved,
                        },
                        *terminal,
                    )
                    .await?;
                    return Ok(result_response);
                }
                let error_body = body(&result_response).ok();
                if error_body.as_ref().and_then(error_code) == Some("OperationHasNoResult") {
                    diagnostic(
                        observer,
                        clock,
                        context,
                        DiagnosticEvent::PollResult {
                            action: ResultAction::NoResult,
                        },
                        *terminal,
                    )
                    .await?;
                    return Ok(response);
                }
                let message = if error_body.is_some() {
                    format!(
                        "Operation result retrieval failed with HTTP {}",
                        result_response.status
                    )
                } else {
                    format!(
                        "Operation result retrieval failed with HTTP {}; error response was not valid JSON",
                        result_response.status
                    )
                };
                return Err(service_failure(
                    &result_response,
                    error_body.as_ref(),
                    &message,
                ));
            }
        }
    }
}
/// Reconcile the actual initial response without ever replaying the initial request.
///
/// Raw non-LRO responses are returned unchanged. Fabric operations and jobs use
/// two- then seven-second delays, followed by Retry-After capped at twenty seconds
/// (twenty seconds if absent/invalid). Azure/Storage use
/// Retry-After or ten seconds; jobs return terminal job JSON.
/// Fabric terminal status is returned when no result is advertised.
/// Azure/Storage completion preserves initial body/headers and
/// normalizes only its status to 200. Waiting has no overall deadline by default;
/// individual status/result requests are bounded to 240 seconds.
/// Recovery errors describe local uncertainty, never backend rollback.
pub async fn complete(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    initial: ApiResponse,
    cancellation: CancellationToken,
) -> Result<ApiResponse, CoreError> {
    complete_with_config(
        executor,
        clock,
        audience,
        initial,
        cancellation,
        LroConfig::default(),
    )
    .await
}

/// Reconcile with optional fixed client interval and total polling timeout.
///
/// Valid Retry-After hints cannot be shortened by an interval override.
/// The total timeout starts here and bounds all waits, GET retries and result
/// retrieval. Omitting it allows indefinite waiting, with local cancellation.
pub async fn complete_with_config(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    initial: ApiResponse,
    cancellation: CancellationToken,
    config: LroConfig,
) -> Result<ApiResponse, CoreError> {
    complete_observed(
        executor,
        clock,
        audience,
        initial,
        cancellation,
        config,
        &NoProgress,
    )
    .await
}

/// Reconcile with injected start, status-check and service-status observations.
///
/// Notifies once after validation and before the first wait, before each logical
/// status GET, and for recognized pending/terminal statuses. Initial/result
/// response statuses, malformed/unknown statuses, and failed HTTP attempts do
/// not generate service-status observations. Required waits precede check notifications.
/// Existing unobserved entry points preserve their effect-free default.
pub async fn complete_observed(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    initial: ApiResponse,
    cancellation: CancellationToken,
    config: LroConfig,
    observer: &dyn PollObserver,
) -> Result<ApiResponse, CoreError> {
    let start = clock.now();
    let discovered = operation_handle(audience, initial.status, &initial.headers)?;
    validate_response(&initial).map_err(|error| {
        if let Some(handle) = &discovered {
            handle.attach(error)
        } else {
            error
        }
    })?;
    let Some(handle) = discovered else {
        return Ok(initial);
    };
    let context = OperationContext {
        cancellation,
        deadline: config
            .timeout
            .map(|timeout| {
                start.checked_add(timeout).ok_or_else(|| {
                    handle.attach(CoreError::new(
                        ErrorKind::InvalidInput,
                        "LRO timeout exceeds clock range",
                    ))
                })
            })
            .transpose()?,
    };
    let mut terminal = None;
    let result = reconcile(
        executor,
        clock,
        audience,
        initial,
        &handle,
        &context,
        (config, observer, &mut terminal),
    )
    .await;
    let error = match result {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };
    // Never attempt another event on a failed sink. Cancelled or expired
    // contexts cannot safely await an additional diagnostic acknowledgment.
    let error = if !matches!(
        error.kind,
        ErrorKind::Diagnostic | ErrorKind::Cancelled | ErrorKind::Timeout
    ) {
        match diagnostic(
            observer,
            clock,
            &context,
            DiagnosticEvent::PollFailed { kind: error.kind },
            terminal,
        )
        .await
        {
            Ok(()) => error,
            Err(error) => error,
        }
    } else {
        error
    };
    let mut error = if error.kind == ErrorKind::Diagnostic {
        // A pre-poll failure is not a pre-mutation failure. Retain the already
        // accepted handle and any known response or terminal outcome.
        let outcome = terminal.map_or_else(
            || match error.diagnostic_outcome() {
                Some(DiagnosticOutcome::Received(status)) => DiagnosticOutcome::Received(status),
                _ => DiagnosticOutcome::Accepted,
            },
            DiagnosticOutcome::Terminal,
        );
        let mut reporting = CoreError::diagnostic_failure(outcome);
        if terminal.is_some()
            && let Some(DiagnosticOutcome::Received(status)) = error.diagnostic_outcome()
        {
            reporting
                .message
                .push_str(&format!("; result request returned HTTP status {status}"));
        }
        handle.attach(reporting)
    } else {
        handle.attach(error)
    };
    if terminal.is_some()
        && let Some(recovery) = &mut error.recovery
    {
        recovery.outcome_uncertain = false;
    }
    Err(error)
}
