// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric/ARM reconciliation of the actual initial response; never repeats a mutation.
#![forbid(unsafe_code)]
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, MAX_RESPONSE_BYTES, OperationContext, OperationHandle as Handle,
    OperationKind as Strategy, operation_handle, validate_operation_url as checked_url,
    validate_response,
};
use fabric_sdk_http::{bounded_with_clock, retry_after, sleep_with_budget};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use url::Url;

/// Fixed elapsed polling budget, with no attempt limit.
pub const POLLING_BUDGET: Duration = Duration::from_secs(600);
/// Initial client backoff. Each pending response doubles it without an interval cap.
pub const INITIAL_DELAY: Duration = Duration::from_secs(1);

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
    context.remaining_at(clock.now())?;
    let response = bounded_with_clock(
        clock,
        context,
        executor.execute_poll(get(url), audience, context),
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
) -> Result<ApiResponse, CoreError> {
    let mut previous = initial.clone();
    let mut delay = INITIAL_DELAY;
    loop {
        context.remaining_at(clock.now())?;
        // The parser exposes safe diagnostics to callers; invalid server hints fall back
        // to the fixed client policy and never reveal header contents.
        let server = match retry_after(&previous, clock.wall_now()) {
            Ok(delay) => delay.unwrap_or_default(),
            Err(_) => {
                clock.diagnostic("Invalid Retry-After header; using client backoff");
                Duration::ZERO
            }
        };
        let wait = delay.max(server);
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
        };
        match current {
            State::Pending => {
                previous = response;
                delay = delay.saturating_mul(2);
            }
            State::Success => {
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
                        clock.diagnostic("Invalid Retry-After header; using client backoff");
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
/// Raw non-LRO responses are returned unchanged. Fabric terminal status is returned
/// when no result is advertised. ARM completion preserves initial body/headers and
/// normalizes only its status to 200. A fresh fixed ten-minute budget starts here.
/// Recovery errors describe local uncertainty, never backend rollback.
pub async fn complete(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    audience: Audience,
    initial: ApiResponse,
    cancellation: CancellationToken,
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
        deadline: start + POLLING_BUDGET,
    };
    reconcile(executor, clock, audience, initial, &handle, &context)
        .await
        .map_err(|error| handle.attach(error))
}
