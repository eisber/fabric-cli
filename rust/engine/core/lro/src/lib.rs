// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric/ARM LRO and job reconciliation of the initial response; never repeats a mutation.
#![forbid(unsafe_code)]
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, MAX_RESPONSE_BYTES, OperationContext, OperationHandle as Handle,
    OperationKind as Strategy, operation_handle, validate_operation_url as checked_url,
    validate_response,
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
        for duration in [poll_interval, timeout].into_iter().flatten() {
            if duration.is_zero() || Instant::now().checked_add(duration).is_none() {
                return Err(CoreError::new(
                    ErrorKind::InvalidInput,
                    "LRO durations must be positive and within clock range",
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
enum State {
    Pending,
    Success,
}
fn state(response: &ApiResponse) -> Result<State, CoreError> {
    let json = body(response)?;
    let status = json
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("Missing operation status"))?;
    match status.to_ascii_lowercase().as_str() {
        "succeeded" | "completed" => Ok(State::Success),
        "notstarted" | "running" | "inprogress" | "accepted" | "creating" | "updating"
        | "deleting" => Ok(State::Pending),
        "failed" | "cancelled" | "canceled" => Err(CoreError::new(
            ErrorKind::Service,
            "Backend operation failed or was cancelled",
        )),
        _ => Err(protocol("Unknown operation status")),
    }
}
fn job_state(response: &ApiResponse) -> Result<State, CoreError> {
    let json = body(response)?;
    let status = json
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("Missing job instance status"))?;
    match status {
        "NotStarted" | "InProgress" => Ok(State::Pending),
        "Completed" | "Deduped" => Ok(State::Success),
        "Failed" | "Cancelled" => Err(CoreError::new(
            ErrorKind::Service,
            "Backend job failed or was cancelled",
        )),
        _ => Err(protocol("Unknown job instance status")),
    }
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
    config: LroConfig,
) -> Result<ApiResponse, CoreError> {
    let mut previous = initial.clone();
    let mut delay = INITIAL_DELAY;
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
        let response = fetch(executor, clock, audience, handle.url().clone(), context).await?;
        if !(200..300).contains(&response.status) {
            return Err(CoreError::new(
                ErrorKind::Service,
                "Operation status request failed",
            ));
        }
        let current = match handle.kind() {
            Strategy::ArmLocation => match response.status {
                200 => State::Success,
                201 | 202 => State::Pending,
                _ => return Err(protocol("Unexpected ARM Location response status")),
            },
            Strategy::Fabric | Strategy::ArmStatus => state(&response)?,
            Strategy::FabricJob => {
                if response.status != 200 {
                    return Err(protocol("Unexpected job instance response status"));
                }
                job_state(&response)?
            }
        };
        match current {
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
                let result = response
                    .header("location")
                    .or_else(|| json.get("resourceLocation").and_then(Value::as_str));
                let Some(result) = result else {
                    return Ok(response);
                };
                let result_url = checked_url(audience, result)?;
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
                let error_body = body(&result_response)?;
                let code = error_body
                    .get("errorCode")
                    .or_else(|| error_body.get("code"))
                    .or_else(|| error_body.get("error").and_then(|v| v.get("code")))
                    .and_then(Value::as_str);
                if code == Some("OperationHasNoResult") {
                    return Ok(response);
                }
                return Err(CoreError::new(
                    ErrorKind::Service,
                    "Operation result retrieval failed",
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
        executor, clock, audience, initial, &handle, &context, config,
    )
    .await
    .map_err(|error| handle.attach(error))
}
