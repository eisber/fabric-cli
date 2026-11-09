// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure, bounded invocation policy for caller-owned API v2 execution.
//!
//! Keep one budget across authentication, initial requests, retries, operation
//! polling, result retrieval and collection pages. This module does not dispatch,
//! authenticate, sleep, buffer bodies, or grant permission to perform an operation.
//! Hosts must apply these checks at their respective execution boundaries.

use std::{error::Error, fmt, num::NonZeroUsize, time::Duration};

/// Independent caller preferences, not operation permissions or completion proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Controls {
    /// Allow the host to wait for an accepted operation to complete.
    pub wait: bool,
    /// Allow the host to follow collection continuations.
    pub paginate: bool,
    /// Allow retries, subject to explicit replay safety and the retry bound.
    pub retry: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            wait: true,
            paginate: true,
            retry: true,
        }
    }
}

/// Exact-operation replay classification supplied by the operation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaySafety {
    /// Never replay mutations or operations without an explicit safe declaration.
    ///
    /// This includes PUT, POST, PATCH and DELETE, and even GET when its exact
    /// operation has not been declared safe. No HTTP method heuristic is used.
    Never,
    /// The exact operation has explicitly been declared a replay-safe read.
    DeclaredSafeRead,
}

/// Finite limits shared by every phase of one invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Total elapsed time, including authentication, delays and all requests.
    pub timeout: Duration,
    /// Maximum number of fully decoded collection pages.
    pub max_pages: NonZeroUsize,
    /// Maximum number of dispatch admissions, across every request phase.
    pub max_sends: NonZeroUsize,
    /// Maximum decoded bytes in each individual response.
    pub max_response_bytes: NonZeroUsize,
    /// Maximum decoded bytes across all responses, including discarded bodies.
    pub max_total_bytes: NonZeroUsize,
    /// Maximum retries per exchange after its original attempt; zero disables retries.
    pub max_retries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(240),
            max_pages: NonZeroUsize::new(100).expect("nonzero page default"),
            max_sends: NonZeroUsize::new(256).expect("nonzero send default"),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024)
                .expect("nonzero response default"),
            max_total_bytes: NonZeroUsize::new(64 * 1024 * 1024).expect("nonzero total default"),
            max_retries: 2,
        }
    }
}

impl Limits {
    /// Validate the timeout; the numeric resource caps are nonzero by type.
    ///
    /// A total-byte cap below the per-response cap is valid: the smaller remaining
    /// total determines the effective response allowance. Zero retries is valid.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.timeout.is_zero() {
            Err(PolicyError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}

/// Non-secret accounting only; no request, body, operation identifier or token.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    /// Dispatch admissions, debited before dispatch, not proven wire sends.
    pub sends: usize,
    /// Fully decoded collection pages committed by the host.
    pub pages: usize,
    /// Observed decoded bytes, including representable resource-limit overshoot.
    ///
    /// On arithmetic overflow the last representable count is retained and the
    /// invocation is permanently stopped; this value is never reset or wrapped.
    pub decoded_bytes: usize,
}

/// Stable, sanitized invocation-policy failures without raw caller/provider data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// The invocation limits are invalid.
    InvalidLimits,
    /// The invocation reached or exceeded its total deadline.
    TimedOut,
    /// Cancellation has been latched; it does not roll back prior requests.
    Cancelled,
    /// No more dispatch admissions are available.
    SendLimit,
    /// No more collection pages may be fetched or committed.
    PageLimit,
    /// A decoded response exceeded its per-response byte cap.
    ResponseLimit,
    /// The cumulative decoded-byte budget is exhausted or exceeded.
    TotalByteLimit,
    /// The caller disabled retries.
    RetryDisabled,
    /// The exact operation lacks an explicit replay-safe read declaration.
    RetryUnsafe,
    /// The allowed number of retries has already been used.
    RetryLimit,
    /// A supplied clock reading precedes invocation start.
    InvalidTime,
    /// An accounting value could not be represented without overflow.
    ArithmeticOverflow,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLimits => "invalid invocation limits",
            Self::TimedOut => "invocation deadline reached",
            Self::Cancelled => "invocation cancelled; cancellation is not rollback",
            Self::SendLimit => "invocation send limit reached",
            Self::PageLimit => "invocation page limit reached",
            Self::ResponseLimit => "response decoded-byte limit exceeded",
            Self::TotalByteLimit => "invocation decoded-byte limit reached",
            Self::RetryDisabled => "invocation retries are disabled",
            Self::RetryUnsafe => "operation is not declared replay safe",
            Self::RetryLimit => "invocation retry limit reached",
            Self::InvalidTime => "clock reading precedes invocation start",
            Self::ArithmeticOverflow => "invocation accounting overflow",
        })
    }
}

impl Error for PolicyError {}

/// A single invocation's caller-clock deadline and finite resource accounting.
///
/// The caller supplies nondecreasing readings from one monotonic clock, including
/// the start reading. Readings before start are rejected without latching an error.
/// Inter-call clock monotonicity is the caller's responsibility. No wall clock,
/// runtime, timer or absolute `start + timeout` arithmetic is used.
///
/// Resource failures detected by mutating methods are latched. Subsequent checks
/// return that original failure, even after cancellation. Otherwise cancellation
/// wins over time errors, including simultaneous deadline expiry. Reaching an
/// exact send/page/byte cap is valid; attempting more work is not.
///
/// This type is intentionally not cloneable: copying it must not replenish an
/// invocation's accounting. Progress is observable through a shared reference.
#[derive(Debug)]
pub struct InvocationBudget {
    limits: Limits,
    controls: Controls,
    start: Duration,
    progress: Progress,
    cancelled: bool,
    stopped: Option<PolicyError>,
}

impl InvocationBudget {
    /// Start one budget for the entire invocation using a caller clock reading.
    pub fn new(limits: Limits, controls: Controls, start: Duration) -> Result<Self, PolicyError> {
        limits.validate()?;
        Ok(Self {
            limits,
            controls,
            start,
            progress: Progress::default(),
            cancelled: false,
            stopped: None,
        })
    }

    /// Return time remaining; equality with the deadline is already timed out.
    ///
    /// Returns any latched resource failure first, then cancellation, then clock
    /// validation/deadline errors. Subtraction is checked, including near
    /// [`Duration::MAX`], so a large start reading cannot wrap the deadline.
    pub fn remaining(&self, now: Duration) -> Result<Duration, PolicyError> {
        if let Some(error) = &self.stopped {
            return Err(error.clone());
        }
        if self.cancelled {
            return Err(PolicyError::Cancelled);
        }
        let elapsed = now
            .checked_sub(self.start)
            .ok_or(PolicyError::InvalidTime)?;
        self.limits
            .timeout
            .checked_sub(elapsed)
            .filter(|remaining| !remaining.is_zero())
            .ok_or(PolicyError::TimedOut)
    }

    /// Check the invocation's latched stop, cancellation and deadline.
    pub fn check(&self, now: Duration) -> Result<(), PolicyError> {
        self.remaining(now).map(|_| ())
    }

    /// Permanently latch cancellation without clearing any accounting or failure.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Read accounting without exposing bodies or credentials.
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Read the three independent host preferences.
    pub fn controls(&self) -> Controls {
        self.controls
    }

    /// Read the immutable limits for this invocation.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Debit one send immediately before dispatch, after policy checks pass.
    ///
    /// Every retry, poll, result and page request uses the same counter. Admission
    /// is conservative, not proof that a request reached the wire; the debit is
    /// not refunded if dispatch subsequently fails. Page fetches must additionally
    /// call [`Self::before_page`] before dispatch.
    pub fn before_send(&mut self, now: Duration) -> Result<(), PolicyError> {
        self.check(now)?;
        if self.progress.sends >= self.limits.max_sends.get() {
            return Err(self.stop(PolicyError::SendLimit));
        }
        let Some(sends) = self.progress.sends.checked_add(1) else {
            return Err(self.stop(PolicyError::ArithmeticOverflow));
        };
        self.progress.sends = sends;
        Ok(())
    }

    /// Return the smaller of the per-response cap and remaining cumulative bytes.
    ///
    /// Exact cumulative exhaustion returns [`PolicyError::TotalByteLimit`].
    /// This is a fresh-response allowance; within a response the host must also
    /// subtract bytes already decoded from the per-response cap.
    pub fn response_allowance(&self, now: Duration) -> Result<usize, PolicyError> {
        self.check(now)?;
        let remaining = self
            .limits
            .max_total_bytes
            .get()
            .checked_sub(self.progress.decoded_bytes)
            .filter(|remaining| *remaining != 0)
            .ok_or(PolicyError::TotalByteLimit)?;
        Ok(remaining.min(self.limits.max_response_bytes.get()))
    }

    /// Account exactly one decoded chunk, without retaining its body.
    ///
    /// The host supplies the bytes previously decoded in this response, resetting
    /// that argument only for a new response. Every body counts, including retry,
    /// poll, service-error and discarded bodies. A nonzero chunk must not be
    /// submitted twice.
    ///
    /// Stop/time/cancellation checks run first; rejected submissions do not change
    /// accounting. Otherwise the chunk's full byte count is recorded, even if it
    /// exceeds a cap. Response overflow/limits take precedence over a total-limit
    /// violation. If the cumulative addition overflows, the previous representable
    /// count remains. Any arithmetic or byte-cap violation permanently stops this
    /// budget, so a failed chunk cannot be retried as unobserved work.
    pub fn record_chunk(
        &mut self,
        response_bytes_so_far: usize,
        chunk_bytes: usize,
        now: Duration,
    ) -> Result<(), PolicyError> {
        self.check(now)?;
        let Some(total) = self.progress.decoded_bytes.checked_add(chunk_bytes) else {
            return Err(self.stop(PolicyError::ArithmeticOverflow));
        };
        self.progress.decoded_bytes = total;
        let Some(response) = response_bytes_so_far.checked_add(chunk_bytes) else {
            return Err(self.stop(PolicyError::ArithmeticOverflow));
        };
        if response > self.limits.max_response_bytes.get() {
            return Err(self.stop(PolicyError::ResponseLimit));
        }
        if total > self.limits.max_total_bytes.get() {
            return Err(self.stop(PolicyError::TotalByteLimit));
        }
        Ok(())
    }

    /// Check before fetching each page, preventing an N+1 fetch at the page cap.
    ///
    /// Call [`Self::commit_page`] once after each full page is decoded. A final
    /// page exactly at the cap is complete if it has no continuation: do not call
    /// this method again unless another page is actually required. The `paginate`
    /// preference is a separate host disposition, not a hard-cap error.
    pub fn before_page(&self, now: Duration) -> Result<(), PolicyError> {
        self.check(now)?;
        if self.progress.pages >= self.limits.max_pages.get() {
            Err(PolicyError::PageLimit)
        } else {
            Ok(())
        }
    }

    /// Commit one fully decoded page; partial or failed pages are not committed.
    ///
    /// The host must serialize page fetch/commit pairs. This accounting helper
    /// does not reserve pages for concurrent requests or deduplicate host calls.
    pub fn commit_page(&mut self, now: Duration) -> Result<(), PolicyError> {
        self.check(now)?;
        if self.progress.pages >= self.limits.max_pages.get() {
            return Err(self.stop(PolicyError::PageLimit));
        }
        let Some(pages) = self.progress.pages.checked_add(1) else {
            return Err(self.stop(PolicyError::ArithmeticOverflow));
        };
        self.progress.pages = pages;
        Ok(())
    }

    /// Check retry preference, exact-operation safety and retries already used.
    ///
    /// The original attempt is not a retry. The host supplies a nondecreasing
    /// count within this exchange and debits each admitted dispatch with
    /// [`Self::before_send`]. A new exchange starts its own retry count but never
    /// resets the shared send/byte/deadline budgets. Polling is not replay of the original request,
    /// and `wait`/`paginate` do not affect retry admission. This check neither
    /// schedules a retry nor classifies service failures as retryable.
    pub fn retry_allowed(
        &self,
        safety: ReplaySafety,
        retries_so_far: usize,
        now: Duration,
    ) -> Result<(), PolicyError> {
        self.check(now)?;
        if !self.controls.retry {
            return Err(PolicyError::RetryDisabled);
        }
        if safety != ReplaySafety::DeclaredSafeRead {
            return Err(PolicyError::RetryUnsafe);
        }
        if retries_so_far >= self.limits.max_retries {
            return Err(PolicyError::RetryLimit);
        }
        Ok(())
    }

    /// Check a provider-required delay without sleeping or shortening it.
    ///
    /// Zero delay is valid while time remains. A delay equal to or longer than
    /// remaining time returns [`PolicyError::TimedOut`]; there would be no time
    /// left for subsequent work. The host must check again after any actual wait.
    pub fn check_delay(&self, delay: Duration, now: Duration) -> Result<(), PolicyError> {
        if delay >= self.remaining(now)? {
            Err(PolicyError::TimedOut)
        } else {
            Ok(())
        }
    }

    fn stop(&mut self, error: PolicyError) -> PolicyError {
        self.stopped = Some(error.clone());
        error
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("nonzero test limit")
    }

    fn limits() -> Limits {
        Limits {
            timeout: Duration::from_secs(10),
            max_pages: nonzero(2),
            max_sends: nonzero(3),
            max_response_bytes: nonzero(4),
            max_total_bytes: nonzero(6),
            max_retries: 2,
        }
    }

    fn budget() -> InvocationBudget {
        InvocationBudget::new(limits(), Controls::default(), Duration::ZERO)
            .expect("valid test budget")
    }

    fn assert_stopped(budget: &mut InvocationBudget, expected: PolicyError) {
        let progress = budget.progress().clone();
        let now = Duration::ZERO;
        assert_eq!(budget.check(now), Err(expected.clone()));
        assert_eq!(budget.remaining(now), Err(expected.clone()));
        assert_eq!(budget.before_send(now), Err(expected.clone()));
        assert_eq!(budget.before_page(now), Err(expected.clone()));
        assert_eq!(budget.commit_page(now), Err(expected.clone()));
        assert_eq!(budget.response_allowance(now), Err(expected.clone()));
        assert_eq!(budget.record_chunk(0, 1, now), Err(expected.clone()));
        assert_eq!(
            budget.retry_allowed(ReplaySafety::DeclaredSafeRead, 0, now),
            Err(expected.clone())
        );
        assert_eq!(
            budget.check_delay(Duration::ZERO, now),
            Err(expected.clone())
        );
        budget.cancel();
        assert_eq!(budget.check(Duration::MAX), Err(expected));
        assert_eq!(budget.progress(), &progress);
    }

    #[test]
    fn defaults_are_finite_and_enabled() {
        let defaults = Limits::default();
        assert_eq!(defaults.timeout, Duration::from_secs(240));
        assert_eq!(defaults.max_pages.get(), 100);
        assert_eq!(defaults.max_sends.get(), 256);
        assert_eq!(defaults.max_response_bytes.get(), 4 * 1024 * 1024);
        assert_eq!(defaults.max_total_bytes.get(), 64 * 1024 * 1024);
        assert_eq!(defaults.max_retries, 2);
        assert_eq!(defaults.validate(), Ok(()));
        assert_eq!(
            Controls::default(),
            Controls {
                wait: true,
                paginate: true,
                retry: true
            }
        );
        assert_eq!(budget().progress(), &Progress::default());
        assert_eq!(budget().limits(), &limits());
    }

    #[test]
    fn validates_timeout_without_imposing_byte_cap_order() {
        let mut limits = limits();
        limits.timeout = Duration::ZERO;
        assert_eq!(limits.validate(), Err(PolicyError::InvalidLimits));
        assert!(matches!(
            InvocationBudget::new(limits.clone(), Controls::default(), Duration::ZERO),
            Err(PolicyError::InvalidLimits)
        ));
        limits.timeout = Duration::from_nanos(1);
        limits.max_response_bytes = nonzero(100);
        limits.max_total_bytes = nonzero(1);
        limits.max_retries = 0;
        assert_eq!(limits.validate(), Ok(()));
        let budget = InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
        assert_eq!(budget.response_allowance(Duration::ZERO), Ok(1));
    }

    #[test]
    fn all_eight_control_combinations_are_independent() {
        for wait in [false, true] {
            for paginate in [false, true] {
                for retry in [false, true] {
                    let controls = Controls {
                        wait,
                        paginate,
                        retry,
                    };
                    let mut budget =
                        InvocationBudget::new(limits(), controls, Duration::ZERO).unwrap();
                    assert_eq!(budget.controls(), controls);
                    assert_eq!(budget.before_send(Duration::ZERO), Ok(()));
                    assert_eq!(budget.before_page(Duration::ZERO), Ok(()));
                    assert_eq!(budget.commit_page(Duration::ZERO), Ok(()));
                    assert_eq!(
                        budget.retry_allowed(ReplaySafety::DeclaredSafeRead, 0, Duration::ZERO),
                        if retry {
                            Ok(())
                        } else {
                            Err(PolicyError::RetryDisabled)
                        }
                    );
                    assert_eq!(budget.progress().sends, 1);
                    assert_eq!(budget.progress().pages, 1);
                }
            }
        }
    }

    #[test]
    fn undeclared_operations_never_replay_with_either_retry_preference() {
        for retry in [false, true] {
            let budget = InvocationBudget::new(
                limits(),
                Controls {
                    retry,
                    ..Controls::default()
                },
                Duration::ZERO,
            )
            .unwrap();
            assert_eq!(
                budget.retry_allowed(ReplaySafety::Never, 0, Duration::ZERO),
                Err(if retry {
                    PolicyError::RetryUnsafe
                } else {
                    PolicyError::RetryDisabled
                })
            );
            assert_eq!(budget.progress(), &Progress::default());
        }
    }

    #[test]
    fn retry_bound_counts_only_retries_after_the_original_attempt() {
        for maximum in [0, 1, 2, 4] {
            let mut limits = limits();
            limits.max_retries = maximum;
            limits.max_sends = nonzero(10);
            let mut budget =
                InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
            budget.before_send(Duration::ZERO).unwrap();
            for used in 0..maximum {
                assert_eq!(
                    budget.retry_allowed(ReplaySafety::DeclaredSafeRead, used, Duration::ZERO),
                    Ok(())
                );
                budget.before_send(Duration::ZERO).unwrap();
            }
            assert_eq!(budget.progress().sends, maximum + 1);
            for used in [maximum, usize::MAX] {
                assert_eq!(
                    budget.retry_allowed(ReplaySafety::DeclaredSafeRead, used, Duration::ZERO),
                    Err(PolicyError::RetryLimit)
                );
            }
        }
    }

    #[test]
    fn phase_transitions_never_reset_the_total_deadline() {
        let start = Duration::from_secs(100);
        let mut limits = limits();
        limits.max_sends = nonzero(10);
        let mut budget = InvocationBudget::new(limits, Controls::default(), start).unwrap();
        // Authentication, initial send, retry, poll, result and collection page.
        budget.check(Duration::from_secs(101)).unwrap();
        budget.before_send(Duration::from_secs(102)).unwrap();
        budget
            .retry_allowed(ReplaySafety::DeclaredSafeRead, 0, Duration::from_secs(103))
            .unwrap();
        budget.before_send(Duration::from_secs(103)).unwrap();
        budget.before_send(Duration::from_secs(105)).unwrap();
        budget.before_send(Duration::from_secs(107)).unwrap();
        budget.record_chunk(0, 2, Duration::from_secs(108)).unwrap();
        budget.before_page(Duration::from_secs(109)).unwrap();
        budget.before_send(Duration::from_secs(109)).unwrap();
        assert_eq!(
            budget.remaining(Duration::from_secs(109)),
            Ok(Duration::from_secs(1))
        );
        let progress = budget.progress().clone();
        assert_eq!(
            budget.commit_page(Duration::from_secs(110)),
            Err(PolicyError::TimedOut)
        );
        assert_eq!(budget.progress(), &progress);
        assert_eq!(budget.check(Duration::MAX), Err(PolicyError::TimedOut));
    }

    #[test]
    fn deadline_is_exclusive_at_nanosecond_precision() {
        let budget = budget();
        assert_eq!(
            budget.remaining(Duration::ZERO),
            Ok(Duration::from_secs(10))
        );
        assert_eq!(
            budget.remaining(Duration::from_nanos(9_999_999_999)),
            Ok(Duration::from_nanos(1))
        );
        assert_eq!(
            budget.remaining(Duration::from_secs(10)),
            Err(PolicyError::TimedOut)
        );
    }

    #[test]
    fn pre_start_time_is_invalid_without_a_permanent_stop() {
        let start = Duration::from_secs(2);
        let before = Duration::from_secs(1);
        let mut budget = InvocationBudget::new(limits(), Controls::default(), start).unwrap();
        assert_eq!(budget.remaining(before), Err(PolicyError::InvalidTime));
        assert_eq!(budget.before_send(before), Err(PolicyError::InvalidTime));
        assert_eq!(budget.before_page(before), Err(PolicyError::InvalidTime));
        assert_eq!(budget.commit_page(before), Err(PolicyError::InvalidTime));
        assert_eq!(
            budget.record_chunk(0, 1, before),
            Err(PolicyError::InvalidTime)
        );
        assert_eq!(
            budget.response_allowance(before),
            Err(PolicyError::InvalidTime)
        );
        assert_eq!(
            budget.retry_allowed(ReplaySafety::DeclaredSafeRead, 0, before),
            Err(PolicyError::InvalidTime)
        );
        assert_eq!(
            budget.check_delay(Duration::ZERO, before),
            Err(PolicyError::InvalidTime)
        );
        assert_eq!(budget.progress(), &Progress::default());
        assert_eq!(budget.before_send(start), Ok(()));
    }

    #[test]
    fn time_math_never_adds_start_and_timeout() {
        let start = Duration::MAX.checked_sub(Duration::from_secs(1)).unwrap();
        let budget = InvocationBudget::new(limits(), Controls::default(), start).unwrap();
        assert_eq!(budget.remaining(start), Ok(Duration::from_secs(10)));
        assert_eq!(budget.remaining(Duration::MAX), Ok(Duration::from_secs(9)));
        assert_eq!(budget.check(Duration::ZERO), Err(PolicyError::InvalidTime));
        let mut limits = limits();
        limits.timeout = Duration::MAX;
        let budget = InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
        assert_eq!(budget.check(Duration::MAX), Err(PolicyError::TimedOut));
        assert_eq!(
            budget.check_delay(Duration::MAX, Duration::from_secs(1)),
            Err(PolicyError::TimedOut)
        );
    }

    #[test]
    fn sends_are_debited_at_admission_and_stop_before_n_plus_one() {
        let mut budget = budget();
        for expected in 1..=3 {
            budget.before_send(Duration::ZERO).unwrap();
            assert_eq!(budget.progress().sends, expected);
        }
        assert_eq!(budget.check(Duration::ZERO), Ok(()));
        assert_eq!(
            budget.before_send(Duration::ZERO),
            Err(PolicyError::SendLimit)
        );
        assert_eq!(budget.progress().sends, 3);
        assert_stopped(&mut budget, PolicyError::SendLimit);
    }

    #[test]
    fn page_fetch_guard_prevents_n_plus_one_send() {
        let mut budget = budget();
        let mut fetched = 0;
        while budget.before_page(Duration::ZERO).is_ok() {
            budget.before_send(Duration::ZERO).unwrap();
            fetched += 1;
            budget.record_chunk(0, 1, Duration::ZERO).unwrap();
            budget.commit_page(Duration::ZERO).unwrap();
        }
        assert_eq!(fetched, 2);
        assert_eq!(budget.progress().sends, 2);
        assert_eq!(budget.progress().pages, 2);
        assert_eq!(budget.check(Duration::ZERO), Ok(()));
        assert_eq!(
            budget.before_page(Duration::ZERO),
            Err(PolicyError::PageLimit)
        );
        assert_eq!(
            budget.commit_page(Duration::ZERO),
            Err(PolicyError::PageLimit)
        );
        assert_stopped(&mut budget, PolicyError::PageLimit);
    }

    #[test]
    fn final_page_at_the_cap_is_complete_without_a_continuation() {
        let mut budget = budget();
        for _ in 0..2 {
            budget.before_page(Duration::ZERO).unwrap();
            budget.before_send(Duration::ZERO).unwrap();
            budget.commit_page(Duration::ZERO).unwrap();
        }
        assert_eq!(budget.progress().pages, 2);
        assert_eq!(budget.check(Duration::ZERO), Ok(()));
        assert_eq!(
            budget.remaining(Duration::ZERO),
            Ok(Duration::from_secs(10))
        );
    }

    #[test]
    fn partial_response_keeps_send_and_bytes_but_not_page() {
        let mut budget = budget();
        budget.before_page(Duration::ZERO).unwrap();
        budget.before_send(Duration::ZERO).unwrap();
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        budget.cancel();
        assert_eq!(
            budget.commit_page(Duration::ZERO),
            Err(PolicyError::Cancelled)
        );
        assert_eq!(
            budget.progress(),
            &Progress {
                sends: 1,
                pages: 0,
                decoded_bytes: 3
            }
        );
    }

    #[test]
    fn response_cap_accepts_minus_one_and_exact_and_retains_plus_one() {
        for bytes in [3, 4, 5] {
            let mut budget = budget();
            let result = budget.record_chunk(0, bytes, Duration::ZERO);
            assert_eq!(
                result,
                if bytes <= 4 {
                    Ok(())
                } else {
                    Err(PolicyError::ResponseLimit)
                }
            );
            assert_eq!(budget.progress().decoded_bytes, bytes);
            if bytes > 4 {
                assert_stopped(&mut budget, PolicyError::ResponseLimit);
            }
        }
    }

    #[test]
    fn response_cap_spans_chunks_and_the_failed_chunk_is_not_free() {
        let mut budget = budget();
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        budget.record_chunk(3, 1, Duration::ZERO).unwrap();
        assert_eq!(
            budget.record_chunk(4, 1, Duration::ZERO),
            Err(PolicyError::ResponseLimit)
        );
        assert_eq!(budget.progress().decoded_bytes, 5);
        assert_stopped(&mut budget, PolicyError::ResponseLimit);
    }

    #[test]
    fn cumulative_cap_accepts_minus_one_exact_and_retains_plus_one() {
        for bytes in [5, 6, 7] {
            let mut budget = budget();
            budget.record_chunk(0, 3, Duration::ZERO).unwrap();
            assert_eq!(
                budget.record_chunk(0, bytes - 3, Duration::ZERO),
                if bytes <= 6 {
                    Ok(())
                } else {
                    Err(PolicyError::TotalByteLimit)
                }
            );
            assert_eq!(budget.progress().decoded_bytes, bytes);
            if bytes < 6 {
                assert_eq!(budget.response_allowance(Duration::ZERO), Ok(1));
            } else {
                assert_eq!(
                    budget.response_allowance(Duration::ZERO),
                    Err(PolicyError::TotalByteLimit)
                );
            }
            if bytes > 6 {
                assert_stopped(&mut budget, PolicyError::TotalByteLimit);
            }
        }
    }

    #[test]
    fn three_retry_bytes_plus_four_result_bytes_need_seven_total_bytes() {
        for cap in [6, 7] {
            let mut limits = limits();
            limits.max_total_bytes = nonzero(cap);
            let mut budget =
                InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
            budget.before_send(Duration::ZERO).unwrap();
            budget.record_chunk(0, 3, Duration::ZERO).unwrap();
            budget
                .retry_allowed(ReplaySafety::DeclaredSafeRead, 0, Duration::ZERO)
                .unwrap();
            budget.before_send(Duration::ZERO).unwrap();
            assert_eq!(
                budget.record_chunk(0, 4, Duration::ZERO),
                if cap == 7 {
                    Ok(())
                } else {
                    Err(PolicyError::TotalByteLimit)
                }
            );
            assert_eq!(budget.progress().decoded_bytes, 7);
        }
    }

    #[test]
    fn discarded_poll_and_error_bodies_share_the_same_byte_budget() {
        let mut limits = limits();
        limits.max_sends = nonzero(4);
        let mut budget =
            InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
        for _ in 0..3 {
            budget.before_send(Duration::ZERO).unwrap();
            budget.record_chunk(0, 2, Duration::ZERO).unwrap();
        }
        assert_eq!(budget.progress().decoded_bytes, 6);
        assert_eq!(
            budget.response_allowance(Duration::ZERO),
            Err(PolicyError::TotalByteLimit)
        );
        assert_eq!(
            budget.record_chunk(0, 1, Duration::ZERO),
            Err(PolicyError::TotalByteLimit)
        );
        assert_eq!(budget.progress().decoded_bytes, 7);
    }

    #[test]
    fn allowance_uses_effective_minimum_and_never_resets_cumulative_bytes() {
        let mut budget = budget();
        assert_eq!(budget.response_allowance(Duration::ZERO), Ok(4));
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        assert_eq!(budget.response_allowance(Duration::ZERO), Ok(3));
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        assert_eq!(
            budget.response_allowance(Duration::ZERO),
            Err(PolicyError::TotalByteLimit)
        );
        assert_eq!(budget.record_chunk(3, 0, Duration::ZERO), Ok(()));
        assert_eq!(budget.progress().decoded_bytes, 6);
    }

    #[test]
    fn simultaneous_response_and_total_violations_report_response_limit() {
        let mut budget = budget();
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        assert_eq!(
            budget.record_chunk(0, 5, Duration::ZERO),
            Err(PolicyError::ResponseLimit)
        );
        assert_eq!(budget.progress().decoded_bytes, 8);
        assert_stopped(&mut budget, PolicyError::ResponseLimit);
    }

    #[test]
    fn cumulative_overflow_keeps_the_last_representable_count_and_stops() {
        let mut limits = limits();
        limits.max_response_bytes = nonzero(usize::MAX);
        limits.max_total_bytes = nonzero(usize::MAX);
        let mut budget =
            InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
        budget.record_chunk(0, usize::MAX, Duration::ZERO).unwrap();
        assert_eq!(
            budget.record_chunk(0, 1, Duration::ZERO),
            Err(PolicyError::ArithmeticOverflow)
        );
        assert_eq!(budget.progress().decoded_bytes, usize::MAX);
        assert_stopped(&mut budget, PolicyError::ArithmeticOverflow);
    }

    #[test]
    fn response_counter_overflow_retains_observed_bytes_and_stops() {
        let mut budget = budget();
        assert_eq!(
            budget.record_chunk(usize::MAX, 1, Duration::ZERO),
            Err(PolicyError::ArithmeticOverflow)
        );
        assert_eq!(budget.progress().decoded_bytes, 1);
        assert_stopped(&mut budget, PolicyError::ArithmeticOverflow);
    }

    #[test]
    fn maximum_send_and_page_counts_never_wrap() {
        let mut limits = limits();
        limits.max_sends = nonzero(usize::MAX);
        limits.max_pages = nonzero(usize::MAX);
        let mut send_budget =
            InvocationBudget::new(limits.clone(), Controls::default(), Duration::ZERO).unwrap();
        send_budget.progress.sends = usize::MAX - 1;
        assert_eq!(send_budget.before_send(Duration::ZERO), Ok(()));
        assert_eq!(send_budget.progress().sends, usize::MAX);
        assert_eq!(
            send_budget.before_send(Duration::ZERO),
            Err(PolicyError::SendLimit)
        );
        let mut page_budget =
            InvocationBudget::new(limits, Controls::default(), Duration::ZERO).unwrap();
        page_budget.progress.pages = usize::MAX - 1;
        assert_eq!(page_budget.commit_page(Duration::ZERO), Ok(()));
        assert_eq!(page_budget.progress().pages, usize::MAX);
        assert_eq!(
            page_budget.commit_page(Duration::ZERO),
            Err(PolicyError::PageLimit)
        );
    }

    #[test]
    fn cancellation_is_latched_and_wins_over_simultaneous_expiry() {
        let mut budget = budget();
        budget.before_send(Duration::ZERO).unwrap();
        budget.record_chunk(0, 2, Duration::ZERO).unwrap();
        budget.cancel();
        budget.cancel();
        assert_eq!(
            budget.check(Duration::from_secs(10)),
            Err(PolicyError::Cancelled)
        );
        assert_stopped(&mut budget, PolicyError::Cancelled);
    }

    #[test]
    fn expired_operations_never_advance_accounting() {
        let mut budget = budget();
        let expired = Duration::from_secs(10);
        assert_eq!(budget.before_send(expired), Err(PolicyError::TimedOut));
        assert_eq!(budget.before_page(expired), Err(PolicyError::TimedOut));
        assert_eq!(budget.commit_page(expired), Err(PolicyError::TimedOut));
        assert_eq!(
            budget.record_chunk(0, 1, expired),
            Err(PolicyError::TimedOut)
        );
        assert_eq!(
            budget.response_allowance(expired),
            Err(PolicyError::TimedOut)
        );
        assert_eq!(
            budget.retry_allowed(ReplaySafety::Never, 0, expired),
            Err(PolicyError::TimedOut)
        );
        assert_eq!(budget.progress(), &Progress::default());
    }

    #[test]
    fn provider_delays_are_checked_without_shortening_or_consuming_time() {
        let budget = budget();
        let now = Duration::from_secs(7);
        for delay in [Duration::ZERO, Duration::from_nanos(2_999_999_999)] {
            assert_eq!(budget.check_delay(delay, now), Ok(()));
        }
        for delay in [
            Duration::from_secs(3),
            Duration::from_secs(4),
            Duration::MAX,
        ] {
            assert_eq!(budget.check_delay(delay, now), Err(PolicyError::TimedOut));
        }
        assert_eq!(budget.remaining(now), Ok(Duration::from_secs(3)));
        assert_eq!(
            budget.check_delay(Duration::ZERO, Duration::from_secs(10)),
            Err(PolicyError::TimedOut)
        );
        assert_eq!(budget.progress(), &Progress::default());
    }

    #[test]
    fn retry_policy_errors_do_not_spend_or_permanently_stop_budget() {
        let mut budget = budget();
        assert_eq!(
            budget.retry_allowed(ReplaySafety::Never, 0, Duration::ZERO),
            Err(PolicyError::RetryUnsafe)
        );
        assert_eq!(
            budget.retry_allowed(ReplaySafety::DeclaredSafeRead, 2, Duration::ZERO),
            Err(PolicyError::RetryLimit)
        );
        assert_eq!(budget.progress(), &Progress::default());
        assert_eq!(budget.before_send(Duration::ZERO), Ok(()));
    }

    #[test]
    fn errors_are_sanitized_standard_errors_with_no_nested_source() {
        let cases = [
            (PolicyError::InvalidLimits, "invalid invocation limits"),
            (PolicyError::TimedOut, "invocation deadline reached"),
            (
                PolicyError::Cancelled,
                "invocation cancelled; cancellation is not rollback",
            ),
            (PolicyError::SendLimit, "invocation send limit reached"),
            (PolicyError::PageLimit, "invocation page limit reached"),
            (
                PolicyError::ResponseLimit,
                "response decoded-byte limit exceeded",
            ),
            (
                PolicyError::TotalByteLimit,
                "invocation decoded-byte limit reached",
            ),
            (
                PolicyError::RetryDisabled,
                "invocation retries are disabled",
            ),
            (
                PolicyError::RetryUnsafe,
                "operation is not declared replay safe",
            ),
            (PolicyError::RetryLimit, "invocation retry limit reached"),
            (
                PolicyError::InvalidTime,
                "clock reading precedes invocation start",
            ),
            (
                PolicyError::ArithmeticOverflow,
                "invocation accounting overflow",
            ),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
            assert_eq!(error.clone(), error);
            assert!(Error::source(&error).is_none());
            assert!(!format!("{error:?}").is_empty());
        }
    }
}
