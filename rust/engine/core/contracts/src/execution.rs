// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-owned monotonic scheduling, absolute budgets and local cancellation.
//!
//! Wall-clock timestamps are deliberately absent. An operation's accepted
//! response and other remote evidence belong to its caller, not to this guard:
//! stopping a local future neither rolls back nor authorizes replaying a send.

use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::{Context, Poll},
    time::Duration,
};

/// A future on the caller's runtime; dropping it releases local wait resources.
pub type ExecutionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Caller-owned monotonic clock and asynchronous scheduler, in one time domain.
///
/// `now` never reads wall time. Implementations must wake registered sleeps when
/// their monotonic time advances, and must not complete them before their delay.
/// No runtime, tasks, credential selection or transport is installed here.
pub trait ExecutionClock: Send + Sync {
    /// Time since an arbitrary fixed monotonic origin.
    fn now(&self) -> Duration;
    /// Complete after at least `delay`; dropping the future stops the local wait.
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()>;
}

/// Invalid duration/arithmetic or expired monotonic budget, without provider data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineError;

impl std::fmt::Display for DeadlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid or expired monotonic deadline")
    }
}

impl std::error::Error for DeadlineError {}

/// An immutable absolute deadline in the injected clock's time domain.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicDeadline {
    start: Duration,
    end: Duration,
}

impl MonotonicDeadline {
    /// Start a positive budget, bounded by 24 hours, with checked arithmetic.
    ///
    /// This retains the existing Run and native observation duration policy.
    pub fn new(now: Duration, timeout: Duration) -> Result<Self, DeadlineError> {
        if timeout > Duration::from_secs(86_400) {
            return Err(DeadlineError);
        }
        Self::after(now, timeout)
    }

    /// Start a checked positive budget without imposing a service duration cap.
    ///
    /// HTTP and other callers retain their own existing maximum-duration policy.
    pub fn after(now: Duration, timeout: Duration) -> Result<Self, DeadlineError> {
        if timeout.is_zero() {
            return Err(DeadlineError);
        }
        Ok(Self {
            start: now,
            end: now.checked_add(timeout).ok_or(DeadlineError)?,
        })
    }

    /// Remaining original budget; zero or a clock before the origin is expired.
    pub fn remaining(self, now: Duration) -> Result<Duration, DeadlineError> {
        if now < self.start {
            return Err(DeadlineError);
        }
        self.end
            .checked_sub(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or(DeadlineError)
    }
}

/// A local stop, not evidence that a remote operation was cancelled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interruption {
    /// The caller stopped local observation.
    Cancelled,
    /// The absolute budget expired or the clock became invalid.
    Deadline,
}

impl std::fmt::Display for Interruption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "local execution cancelled",
            Self::Deadline => "local execution deadline expired",
        })
    }
}

impl std::error::Error for Interruption {}

/// One borrowed budget/cancellation boundary across nested asynchronous phases.
///
/// Reuse this context for credentials, sends, body reads, retry/poll waits and
/// output. Cancellation is latched so a completed non-fused future is never
/// polled again. A clock rollback or expiry is terminal for this context.
pub struct ExecutionContext<'a> {
    clock: &'a dyn ExecutionClock,
    deadline: MonotonicDeadline,
    cancelled: Pin<&'a mut (dyn Future<Output = ()> + Send + 'a)>,
    last_now: Option<Duration>,
    stopped: Option<Interruption>,
}

impl<'a> ExecutionContext<'a> {
    /// Borrow an existing absolute deadline and cancellation future without I/O.
    pub fn new(
        clock: &'a dyn ExecutionClock,
        deadline: MonotonicDeadline,
        cancelled: Pin<&'a mut (dyn Future<Output = ()> + Send + 'a)>,
    ) -> Self {
        Self {
            clock,
            deadline,
            cancelled,
            last_now: None,
            stopped: None,
        }
    }

    /// Inspect the same absolute budget, rejecting rollback and terminal stops.
    ///
    /// This synchronous check does not poll cancellation. Use [`Self::check`]
    /// before starting an effect.
    pub fn remaining(&mut self) -> Result<Duration, Interruption> {
        if let Some(stopped) = self.stopped {
            return Err(stopped);
        }
        let now = self.clock.now();
        if self.last_now.is_some_and(|previous| now < previous) {
            self.stopped = Some(Interruption::Deadline);
            return Err(Interruption::Deadline);
        }
        self.last_now = Some(now);
        self.deadline.remaining(now).map_err(|_| {
            self.stopped = Some(Interruption::Deadline);
            Interruption::Deadline
        })
    }

    fn checkpoint(&mut self, cx: &mut Context<'_>) -> Result<Duration, Interruption> {
        if let Some(stopped) = self.stopped {
            return Err(stopped);
        }
        if self.cancelled.as_mut().poll(cx).is_ready() {
            self.stopped = Some(Interruption::Cancelled);
            return Err(Interruption::Cancelled);
        }
        self.remaining()
    }

    /// Poll cancellation and expiry without starting another operation.
    pub async fn check(&mut self) -> Result<(), Interruption> {
        poll_fn(|cx| Poll::Ready(self.checkpoint(cx).map(|_| ()))).await
    }

    /// Guard a phase with the original budget and cancellation, before and after poll.
    ///
    /// Cancellation wins ties, then expiry. Losing futures are dropped locally.
    /// A cooperatively scheduled future cannot interrupt a blocking `poll`, but
    /// a late result is rejected before the next phase can send or publish.
    pub async fn guard<T>(
        &mut self,
        operation: impl Future<Output = T>,
    ) -> Result<T, Interruption> {
        // Check before constructing a host timer, including pre-cancelled work.
        let remaining = poll_fn(|cx| Poll::Ready(self.checkpoint(cx))).await?;
        let clock = self.clock;
        let mut timer = clock.sleep(remaining);
        let mut operation = pin!(operation);
        poll_fn(|cx| {
            self.checkpoint(cx)?;
            if timer.as_mut().poll(cx).is_ready() {
                self.stopped = Some(Interruption::Deadline);
                return Poll::Ready(Err(Interruption::Deadline));
            }
            let result = operation.as_mut().poll(cx);
            self.checkpoint(cx)?;
            if timer.as_mut().poll(cx).is_ready() {
                self.stopped = Some(Interruption::Deadline);
                return Poll::Ready(Err(Interruption::Deadline));
            }
            result.map(Ok)
        })
        .await
    }

    /// Wait at least a delay in the same time domain, under the original budget.
    ///
    /// Suitable for Retry-After, operation polling and progress heartbeats. A wait
    /// does not reset a budget. A delay beyond the budget expires at the deadline;
    /// it is never shortened into permission to send before the requested time.
    pub async fn wait(&mut self, delay: Duration) -> Result<(), Interruption> {
        self.check().await?;
        let clock = self.clock;
        if clock.now().checked_add(delay).is_none() {
            self.stopped = Some(Interruption::Deadline);
            return Err(Interruption::Deadline);
        }
        if delay.is_zero() {
            return self.check().await;
        }
        self.guard(clock.sleep(delay)).await
    }
}
