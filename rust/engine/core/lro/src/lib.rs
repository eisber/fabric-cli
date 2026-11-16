// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric/ARM LRO and job reconciliation of the initial response; never repeats a mutation.
#![forbid(unsafe_code)]
use async_trait::async_trait;
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

/// First Fabric non-job polling delay; subsequent fallbacks are seven then twenty seconds.
pub const INITIAL_DELAY: Duration = Duration::from_secs(2);
/// Job-style polling interval for jobs and ARM when Retry-After is absent or invalid.
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
    /// Observe validated reconciliation before its first wait, not a service status.
    ///
    /// Uses the same cancellation, deadline and error handling as status observations.
    async fn started(&self, _kind: Strategy) -> Result<(), CoreError> {
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
        return Err(protocol("Operation response exceeds allocation bound"));
    }
    fabric_core_json::decode(&response.body).map_err(|_| protocol("Malformed operation JSON"))
}
fn error_code(json: &Value) -> Option<&str> {
    let error = json.get("error");
    error
        .and_then(|value| value.get("errorCode"))
        .and_then(Value::as_str)
        .or_else(|| {
            error
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str)
        })
        .or_else(|| json.get("errorCode").and_then(Value::as_str))
        .or_else(|| json.get("code").and_then(Value::as_str))
}
fn service_failure(response: &ApiResponse, code: Option<&str>, message: &str) -> CoreError {
    let code = code
        .or_else(|| response.header("x-ms-public-api-error-code"))
        .or_else(|| response.header("x-ms-error-code"));
    CoreError::service_failure(message, code)
}
enum State {
    Pending,
    Success,
    Failed,
    Cancelled,
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
        _ => Err(protocol("Unknown operation status")),
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
        _ => Err(protocol("Unknown job instance status")),
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
    settings: (LroConfig, &dyn PollObserver),
) -> Result<ApiResponse, CoreError> {
    let (config, observer) = settings;
    bounded_with_clock(clock, context, observer.started(handle.kind())).await??;
    let mut previous = initial.clone();
    let mut delay = INITIAL_DELAY;
    let mut warned_percentage = false;
    loop {
        context.remaining_at(clock.now())?;
        // The parser exposes safe diagnostics to callers; invalid server hints fall back
        // to the fixed client policy and never reveal header contents.
        let job = matches!(handle.kind(), Strategy::FabricJob);
        let server_interval =
            job || matches!(handle.kind(), Strategy::ArmLocation | Strategy::ArmStatus);
        let fallback = config.poll_interval.unwrap_or(if server_interval {
            JOB_POLLING_INTERVAL
        } else {
            delay
        });
        let server = match retry_after(&previous, clock.wall_now()) {
            Ok(delay) => delay,
            Err(_) => {
                clock.diagnostic(if config.poll_interval.is_some() {
                    "Invalid Retry-After header; using configured polling interval"
                } else if job {
                    "Invalid Retry-After header; using 10-second job polling interval"
                } else if server_interval {
                    "Invalid Retry-After header; using 10-second ARM polling interval"
                } else {
                    "Invalid Retry-After header; using client polling schedule"
                });
                None
            }
        };
        let wait = match (config.poll_interval, server_interval, server) {
            (None, true, Some(server)) => server,
            _ => fallback.max(server.unwrap_or_default()),
        };
        sleep_with_budget(clock, wait, context).await?;
        bounded_with_clock(clock, context, observer.polling(handle.kind())).await??;
        let response = fetch(executor, clock, audience, handle.url().clone(), context).await?;
        if !(200..300).contains(&response.status) {
            let json = body(&response);
            let message = if json.is_ok() {
                "Operation status request failed"
            } else {
                "Operation status request failed; error response was not valid JSON"
            };
            return Err(service_failure(
                &response,
                json.as_ref().ok().and_then(error_code),
                message,
            ));
        }
        let (current, status, percentage, failure_code) = match handle.kind() {
            Strategy::ArmLocation => match response.status {
                200 => (State::Success, "Succeeded", None, None),
                201 | 202 => (State::Pending, "Running", None, None),
                _ => return Err(protocol("Unexpected ARM Location response status")),
            },
            kind => {
                if kind == Strategy::FabricJob && response.status != 200 {
                    return Err(protocol("Unexpected job instance response status"));
                }
                let json = body(&response)?;
                let (current, status) = if job {
                    job_state(&json)?
                } else {
                    state(&json)?
                };
                let failure_code = if matches!(current, State::Failed | State::Cancelled) {
                    error_code(&json).map(str::to_owned)
                } else {
                    None
                };
                (
                    current,
                    status,
                    percentage(&json, clock, &mut warned_percentage),
                    failure_code,
                )
            }
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
        match current {
            State::Failed | State::Cancelled => {
                let message = if job {
                    "Backend job failed or was cancelled"
                } else {
                    "Backend operation failed or was cancelled"
                };
                return Err(service_failure(&response, failure_code.as_deref(), message));
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
                    return Ok(response);
                }
                if !matches!(handle.kind(), Strategy::Fabric) {
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
                    return Ok(response);
                };
                if &result_url == handle.url() {
                    return Err(protocol("Fabric result points to its status endpoint"));
                }
                // A terminal status can advertise a result before it is ready.
                // Its Retry-After is still a not-before constraint, sharing the
                // original reconciliation deadline (not a fresh result budget).
                let result_delay = match retry_after(&response, clock.wall_now()) {
                    Ok(delay) => delay.unwrap_or_default(),
                    Err(_) => {
                        clock.diagnostic(
                            "Invalid Retry-After header; using client polling schedule",
                        );
                        INITIAL_DELAY
                    }
                };
                if !result_delay.is_zero() {
                    sleep_with_budget(clock, result_delay, context).await?;
                }
                let result_response = fetch(executor, clock, audience, result_url, context).await?;
                if (200..300).contains(&result_response.status) {
                    return Ok(result_response);
                }
                let error_body = body(&result_response);
                if error_body.as_ref().ok().and_then(error_code) == Some("OperationHasNoResult") {
                    return Ok(response);
                }
                let message = if error_body.is_ok() {
                    "Operation result retrieval failed"
                } else {
                    "Operation result retrieval failed; error response was not valid JSON"
                };
                return Err(service_failure(
                    &result_response,
                    error_body.as_ref().ok().and_then(error_code),
                    message,
                ));
            }
        }
    }
}
/// Reconcile the actual initial response without ever replaying the initial request.
///
/// Raw non-LRO responses are returned unchanged. ARM and Fabric job-instance Locations
/// poll using Retry-After or ten seconds; jobs return terminal job JSON.
/// Fabric terminal status is returned when no result is advertised.
/// ARM completion preserves initial body/headers and
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
    reconcile(
        executor,
        clock,
        audience,
        initial,
        &handle,
        &context,
        (config, observer),
    )
    .await
    .map_err(|error| handle.attach(error))
}
