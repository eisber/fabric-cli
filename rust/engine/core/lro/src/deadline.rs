// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure monotonic budget shared by native runtime and v2 run consumers.

use std::time::Duration;

/// Invalid duration/arithmetic or expired monotonic budget, without provider data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineError;

impl std::fmt::Display for DeadlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid or expired monotonic deadline")
    }
}
impl std::error::Error for DeadlineError {}

/// An absolute deadline in the caller's monotonic time domain.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicDeadline {
    end: Duration,
}

impl MonotonicDeadline {
    /// Start a positive budget, bounded by 24h, with checked arithmetic.
    pub fn new(now: Duration, timeout: Duration) -> Result<Self, DeadlineError> {
        if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
            return Err(DeadlineError);
        }
        Ok(Self {
            end: now.checked_add(timeout).ok_or(DeadlineError)?,
        })
    }

    /// Remaining original budget; zero is expired, not permission to start work.
    pub fn remaining(self, now: Duration) -> Result<Duration, DeadlineError> {
        self.end
            .checked_sub(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or(DeadlineError)
    }
}
