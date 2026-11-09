// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{RunError, RunTargetKind};
use crate::JobFuture;
use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

/// Caller-owned monotonic clock and asynchronous timer; no new runtime is made.
pub trait RunClock: Send + Sync {
    /// Time since an arbitrary fixed monotonic origin, never wall-clock time.
    fn now(&self) -> Duration;
    /// Complete after at least the delay; drop releases the local timer.
    fn sleep(&self, delay: Duration) -> JobFuture<'_, ()>;
}

/// Validated positive integer duration, bounded inclusively by 24 hours.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunTimeout(Duration);

impl RunTimeout {
    /// Parse ms/s/m/h with checked arithmetic, rejecting unitless/fractional input.
    pub fn parse(text: &str) -> Result<Self, RunError> {
        let (digits, multiplier) = if let Some(digits) = text.strip_suffix("ms") {
            (digits, 1)
        } else if let Some(digits) = text.strip_suffix('s') {
            (digits, 1_000)
        } else if let Some(digits) = text.strip_suffix('m') {
            (digits, 60_000)
        } else if let Some(digits) = text.strip_suffix('h') {
            (digits, 3_600_000)
        } else {
            return Err(RunError::InvalidTimeout);
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(RunError::InvalidTimeout);
        }
        let milliseconds = digits
            .parse::<u64>()
            .ok()
            .and_then(|value| value.checked_mul(multiplier))
            .filter(|value| (1..=86_400_000).contains(value))
            .ok_or(RunError::InvalidTimeout)?;
        Ok(Self(Duration::from_millis(milliseconds)))
    }

    /// Default total budget for the selected mode, not an HTTP exchange limit.
    pub const fn for_target(target: RunTargetKind, wait: bool) -> Self {
        Self(Duration::from_secs(match target {
            RunTargetKind::Udf => 120,
            RunTargetKind::LivyOneShot => 600,
            _ if wait => 300,
            _ => 30,
        }))
    }

    /// Inspect the exact validated duration.
    pub const fn duration(self) -> Duration {
        self.0
    }
}

/// One immutable deadline starting after pure local preflight.
#[derive(Clone, Copy, Debug)]
pub struct RunDeadline {
    shared: fabric_sdk_operations::deadline::MonotonicDeadline,
}

impl RunDeadline {
    /// Start before remote resolution, credential acquisition or submission.
    pub fn start(clock: &dyn RunClock, timeout: RunTimeout) -> Result<Self, RunError> {
        Ok(Self {
            shared: fabric_sdk_operations::deadline::MonotonicDeadline::new(
                clock.now(),
                timeout.duration(),
            )
            .map_err(|_| RunError::InvalidTimeout)?,
        })
    }

    /// Strict remaining time: at now >= deadline no work may start.
    pub fn remaining(self, clock: &dyn RunClock) -> Result<Duration, RunError> {
        self.shared
            .remaining(clock.now())
            .map_err(|_| RunError::Deadline)
    }

    /// Guard one phase with the same deadline and reusable cancellation signal.
    ///
    /// Cancellation and deadline are polled before the operation on every wake.
    /// Ties never poll a simultaneously ready remote future. Callers retain
    /// evidence in the phase itself before returning it through this guard.
    pub async fn guard<T>(
        self,
        clock: &dyn RunClock,
        mut cancelled: Pin<&mut (dyn Future<Output = ()> + Send + '_)>,
        operation: impl Future<Output = T>,
    ) -> Result<T, RunError> {
        let remaining = self.remaining(clock)?;
        let mut timer = clock.sleep(remaining);
        let mut operation = pin!(operation);
        poll_fn(|cx| {
            if cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(RunError::Interrupted));
            }
            if self.remaining(clock).is_err() || timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(RunError::Deadline));
            }
            match operation.as_mut().poll(cx) {
                Poll::Ready(value) => {
                    if cancelled.as_mut().poll(cx).is_ready() {
                        Poll::Ready(Err(RunError::Interrupted))
                    } else if self.remaining(clock).is_err() {
                        Poll::Ready(Err(RunError::Deadline))
                    } else {
                        Poll::Ready(Ok(value))
                    }
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
}
