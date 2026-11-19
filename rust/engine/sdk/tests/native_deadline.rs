// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The real runtime budget, with caller-controlled time instead of wall sleeps.

#![deny(unnameable_test_items)]

use fabric_core::execution::{ExecutionClock, ExecutionFuture};
use fabric_sdk::runtime::{NativeDeadline, NativeError};
use std::{
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Clock(Mutex<Duration>);

impl ExecutionClock for Clock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }

    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

#[test]
fn injected_deadline_has_exact_boundary_and_shared_original_budget() {
    let clock = Arc::new(Clock::default());
    *clock.0.lock().unwrap() = Duration::from_secs(17);
    let deadline = NativeDeadline::with_clock(Duration::from_secs(10), clock.clone()).unwrap();
    let body = deadline.clone();
    assert!(deadline.clock().is_some());
    *clock.0.lock().unwrap() = Duration::from_secs(26);
    assert_eq!(deadline.remaining(), Ok(Duration::from_secs(1)));
    *clock.0.lock().unwrap() = Duration::from_secs(27) - Duration::from_nanos(1);
    assert_eq!(body.remaining(), Ok(Duration::from_nanos(1)));
    *clock.0.lock().unwrap() = Duration::from_secs(27);
    assert_eq!(body.remaining(), Err(NativeError::TimedOut));
    // Neither resuming the original nor correcting a bad clock restarts expiry.
    *clock.0.lock().unwrap() = Duration::from_secs(18);
    assert_eq!(deadline.remaining(), Err(NativeError::TimedOut));
}

#[test]
fn rollback_is_rejected_across_clones_without_extending_observation() {
    let clock = Arc::new(Clock::default());
    let deadline = NativeDeadline::with_clock(Duration::from_secs(10), clock.clone()).unwrap();
    *clock.0.lock().unwrap() = Duration::from_secs(4);
    assert_eq!(deadline.remaining(), Ok(Duration::from_secs(6)));
    *clock.0.lock().unwrap() = Duration::from_secs(3);
    assert_eq!(deadline.clone().remaining(), Err(NativeError::TimedOut));
    *clock.0.lock().unwrap() = Duration::from_secs(5);
    assert_eq!(deadline.remaining(), Err(NativeError::TimedOut));
}

#[test]
fn duration_policy_and_clock_arithmetic_fail_before_any_work() {
    let clock = Arc::new(Clock::default());
    assert!(NativeDeadline::with_clock(Duration::ZERO, clock.clone()).is_err());
    assert!(NativeDeadline::with_clock(Duration::from_secs(86_401), clock.clone()).is_err());
    assert!(NativeDeadline::with_clock(Duration::from_secs(86_400), clock.clone()).is_ok());
    *clock.0.lock().unwrap() = Duration::MAX;
    assert!(NativeDeadline::with_clock(Duration::from_nanos(1), clock).is_err());
}

#[test]
fn legacy_constructor_keeps_its_external_alarm_boundary() {
    let deadline = NativeDeadline::new(Duration::from_secs(10)).unwrap();
    assert!(deadline.clock().is_none());
}
