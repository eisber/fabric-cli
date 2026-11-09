// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared observer driver extracted from the existing native read observer.
//!
//! Policies retain their own typed state, evidence, limits and cancellation.
//! This driver supplies no HTTP retries, timer, credentials, tasks or runtime.

use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};

/// Provider status domain; job and generic operation states are not interchangeable.
#[derive(Clone, Copy)]
pub enum WorkDomain {
    /// Generic Fabric operation.
    Operation,
    /// Native item scheduler job.
    Job,
}

/// Shared native classification; None is an unrecognized status, not success.
pub fn work_state(domain: WorkDomain, status: &str) -> Option<(bool, bool)> {
    match (domain, status) {
        (WorkDomain::Operation, "NotStarted" | "Running")
        | (WorkDomain::Job, "NotStarted" | "InProgress") => Some((false, false)),
        (WorkDomain::Operation, "Succeeded") | (WorkDomain::Job, "Completed") => Some((true, true)),
        (_, "Failed" | "Cancelled") | (WorkDomain::Job, "Deduped") => Some((true, false)),
        _ => None,
    }
}

/// Invalid Retry-After syntax or ambiguous duplicate headers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAfterError;

impl std::fmt::Display for RetryAfterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid observation Retry-After")
    }
}
impl std::error::Error for RetryAfterError {}

/// Parse a provider minimum with explicit legacy/v2 missing and zero policies.
///
/// No clamping, unit conversion, early request, wall-clock inference or timer is
/// installed here. Unsupported HTTP-date values fail rather than being ignored.
pub fn retry_after(
    headers: &BTreeMap<String, String>,
    missing: Duration,
    allow_zero: bool,
) -> Result<Duration, RetryAfterError> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("retry-after"));
    let Some((_, value)) = values.next() else {
        return Ok(missing);
    };
    if values.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(RetryAfterError);
    }
    let seconds = value.parse::<u64>().map_err(|_| RetryAfterError)?;
    if seconds == 0 && !allow_zero {
        return Err(RetryAfterError);
    }
    Ok(Duration::from_secs(seconds))
}

/// Caller-runtime observer future.
pub type ObserverFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One policy-interpreted observation, not a successful HTTP status alone.
pub enum ObservationStep<T> {
    /// Native state/result is terminal according to the selected policy.
    Complete(T),
    /// Observe again only after the provider/policy minimum delay.
    Continue(Duration),
}

/// The existing native observer and run adapters share one driver.
pub trait ObservationDriver {
    /// Policy-specific terminal observation.
    type Output;
    /// Policy-specific failure retaining evidence.
    type Error;
    /// Perform one bounded status read and interpret it.
    fn step(&mut self) -> ObserverFuture<'_, Result<ObservationStep<Self::Output>, Self::Error>>;
    /// Honor the delay with the caller's deadline/cancellation/time policy.
    fn wait(&mut self, delay: Duration) -> ObserverFuture<'_, Result<(), Self::Error>>;
}

/// Drive observation without creating an independent poller per artifact.
///
/// An accepted submission may supply an initial minimum delay; existing-handle
/// reads use None. Each policy checks limits/cancellation before every request.
pub async fn drive<D: ObservationDriver>(
    driver: &mut D,
    initial_delay: Option<Duration>,
) -> Result<D::Output, D::Error> {
    if let Some(delay) = initial_delay {
        driver.wait(delay).await?;
    }
    loop {
        match driver.step().await? {
            ObservationStep::Complete(value) => return Ok(value),
            ObservationStep::Continue(delay) => driver.wait(delay).await?,
        }
    }
}
