// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Virtual-time contracts for the shared execution boundary; no wall sleeps.

#![deny(unnameable_test_items)]

use fabric_core::execution::{
    ExecutionClock, ExecutionContext, ExecutionFuture, Interruption, MonotonicDeadline,
};
use std::{
    future::{Future, pending, ready},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

#[derive(Default)]
struct Clock {
    now: Mutex<Duration>,
    waiters: Mutex<Vec<Waker>>,
    sleeps: Mutex<Vec<Duration>>,
}

impl Clock {
    fn set(&self, now: Duration) {
        *self.now.lock().unwrap() = now;
        for waker in self.waiters.lock().unwrap().drain(..) {
            waker.wake();
        }
    }

    fn advance(&self, seconds: u64) {
        self.set(self.now() + Duration::from_secs(seconds));
    }
}

impl ExecutionClock for Clock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        let end = self.now().checked_add(delay).unwrap();
        self.sleeps.lock().unwrap().push(delay);
        Box::pin(std::future::poll_fn(move |cx| {
            self.waiters.lock().unwrap().push(cx.waker().clone());
            if self.now() >= end {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

#[derive(Default)]
struct Wakes(AtomicUsize);
impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll<T>(future: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    let waker = Waker::from(Arc::new(Wakes::default()));
    future.poll(&mut Context::from_waker(&waker))
}

fn deadline(clock: &Clock) -> MonotonicDeadline {
    MonotonicDeadline::new(clock.now(), Duration::from_secs(10)).unwrap()
}

struct Held {
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl Future for Held {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Poll::Pending
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn finite_arithmetic_and_exact_expiry_preserve_existing_cap() {
    let start = Duration::from_secs(7);
    assert!(MonotonicDeadline::new(start, Duration::ZERO).is_err());
    assert!(MonotonicDeadline::new(start, Duration::from_secs(86_401)).is_err());
    assert!(MonotonicDeadline::new(Duration::MAX, Duration::from_nanos(1)).is_err());
    assert!(MonotonicDeadline::after(start, Duration::MAX).is_err());
    assert!(MonotonicDeadline::after(start, Duration::from_secs(86_401)).is_ok());
    let deadline = MonotonicDeadline::new(start, Duration::from_secs(1)).unwrap();
    assert!(deadline.remaining(start - Duration::from_nanos(1)).is_err());
    assert_eq!(
        deadline
            .remaining(start + Duration::from_secs(1) - Duration::from_nanos(1))
            .unwrap(),
        Duration::from_nanos(1)
    );
    assert!(deadline.remaining(start + Duration::from_secs(1)).is_err());
}

#[test]
fn ready_cancellation_wins_without_polling_effect_and_is_latched() {
    let clock = Clock::default();
    let mut cancelled = pin!(async {});
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    let sends = AtomicUsize::new(0);
    assert_eq!(
        poll(pin!(context.guard(async {
            sends.fetch_add(1, Ordering::SeqCst);
        }))),
        Poll::Ready(Err(Interruption::Cancelled))
    );
    assert_eq!(
        poll(pin!(context.check())),
        Poll::Ready(Err(Interruption::Cancelled))
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert!(clock.sleeps.lock().unwrap().is_empty());
}

#[test]
fn expiry_at_boundary_does_not_poll_simultaneously_ready_send() {
    let clock = Clock::default();
    let deadline = deadline(&clock);
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline, cancelled.as_mut());
    clock.advance(10);
    let sends = AtomicUsize::new(0);
    assert_eq!(
        poll(pin!(context.guard(async {
            sends.fetch_add(1, Ordering::SeqCst);
        }))),
        Poll::Ready(Err(Interruption::Deadline))
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
}

#[test]
fn pending_credential_is_dropped_and_no_transport_phase_starts() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut credential = pin!(context.guard(Held {
            polls: polls.clone(),
            drops: drops.clone(),
        }));
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        assert!(
            credential
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        clock.advance(10);
        assert!(wakes.0.load(Ordering::SeqCst) > 0);
        assert_eq!(
            poll(credential.as_mut()),
            Poll::Ready(Err(Interruption::Deadline))
        );
    }
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        poll(pin!(context.guard(ready("send")))),
        Poll::Ready(Err(Interruption::Deadline))
    );
}

#[test]
fn pending_transport_uses_credentials_remaining_original_budget() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    assert_eq!(
        poll(pin!(context.guard(async { clock.advance(4) }))),
        Poll::Ready(Ok(()))
    );
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut transport = pin!(context.guard(Held {
            polls: Arc::new(AtomicUsize::new(0)),
            drops: drops.clone(),
        }));
        assert!(poll(transport.as_mut()).is_pending());
        clock.advance(6);
        assert_eq!(
            poll(transport.as_mut()),
            Poll::Ready(Err(Interruption::Deadline))
        );
    }
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        [Duration::from_secs(10), Duration::from_secs(6)]
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn late_credential_result_cannot_start_transport() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    assert_eq!(
        poll(pin!(context.guard(async {
            clock.advance(10);
            "token"
        }))),
        Poll::Ready(Err(Interruption::Deadline))
    );
    let sends = AtomicUsize::new(0);
    assert_eq!(
        poll(pin!(context.guard(async {
            sends.fetch_add(1, Ordering::SeqCst);
        }))),
        Poll::Ready(Err(Interruption::Deadline))
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_after_send_keeps_evidence_and_never_replays() {
    let clock = Clock::default();
    let stopped = AtomicBool::new(false);
    let mut cancelled = pin!(std::future::poll_fn(|_| {
        if stopped.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    let sends = AtomicUsize::new(0);
    let accepted = Mutex::new(None);
    assert_eq!(
        poll(pin!(context.guard(async {
            sends.fetch_add(1, Ordering::SeqCst);
            *accepted.lock().unwrap() = Some("accepted-operation-7");
            stopped.store(true, Ordering::SeqCst);
        }))),
        Poll::Ready(Err(Interruption::Cancelled))
    );
    assert_eq!(*accepted.lock().unwrap(), Some("accepted-operation-7"));
    assert_eq!(
        poll(pin!(context.guard(ready(())))),
        Poll::Ready(Err(Interruption::Cancelled))
    );
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_during_body_drops_local_work() {
    let clock = Clock::default();
    let stopped = AtomicBool::new(false);
    let mut cancelled = pin!(std::future::poll_fn(|_| {
        if stopped.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut body = pin!(context.guard(Held {
            polls: Arc::new(AtomicUsize::new(0)),
            drops: drops.clone(),
        }));
        assert!(poll(body.as_mut()).is_pending());
        stopped.store(true, Ordering::SeqCst);
        assert_eq!(
            poll(body.as_mut()),
            Poll::Ready(Err(Interruption::Cancelled))
        );
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn retry_after_is_not_shortened_and_expiry_blocks_next_send() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    {
        let mut wait = pin!(context.wait(Duration::from_secs(7)));
        assert!(poll(wait.as_mut()).is_pending());
        clock.advance(6);
        assert!(poll(wait.as_mut()).is_pending());
        clock.advance(1);
        assert_eq!(poll(wait.as_mut()), Poll::Ready(Ok(())));
    }
    assert_eq!(context.remaining(), Ok(Duration::from_secs(3)));
    {
        let mut wait = pin!(context.wait(Duration::from_secs(4)));
        assert!(poll(wait.as_mut()).is_pending());
        clock.advance(3);
        assert_eq!(
            poll(wait.as_mut()),
            Poll::Ready(Err(Interruption::Deadline))
        );
    }
    assert_eq!(
        poll(pin!(context.guard(ready("retry")))),
        Poll::Ready(Err(Interruption::Deadline))
    );
}

#[test]
fn heartbeat_waits_use_same_budget_and_stop_before_rendering_at_expiry() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    let renders = AtomicUsize::new(0);
    let mut heartbeats = pin!(async {
        for _ in 0..5 {
            context.wait(Duration::from_secs(2)).await?;
            context
                .guard(async {
                    renders.fetch_add(1, Ordering::SeqCst);
                })
                .await?;
        }
        Ok::<(), Interruption>(())
    });
    assert!(poll(heartbeats.as_mut()).is_pending());
    for _ in 0..4 {
        clock.advance(2);
        assert!(poll(heartbeats.as_mut()).is_pending());
    }
    assert_eq!(renders.load(Ordering::SeqCst), 4);
    clock.advance(2);
    assert_eq!(
        poll(heartbeats.as_mut()),
        Poll::Ready(Err(Interruption::Deadline))
    );
    assert_eq!(renders.load(Ordering::SeqCst), 4);
}

#[test]
fn backwards_time_and_unrepresentable_wait_are_terminal() {
    let clock = Clock::default();
    let mut cancelled = pin!(pending());
    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    clock.advance(3);
    assert_eq!(context.remaining(), Ok(Duration::from_secs(7)));
    clock.set(Duration::from_secs(2));
    assert_eq!(context.remaining(), Err(Interruption::Deadline));
    clock.set(Duration::from_secs(4));
    assert_eq!(context.remaining(), Err(Interruption::Deadline));

    let mut context = ExecutionContext::new(&clock, deadline(&clock), cancelled.as_mut());
    assert_eq!(
        poll(pin!(context.wait(Duration::MAX))),
        Poll::Ready(Err(Interruption::Deadline))
    );
    assert_eq!(
        poll(pin!(context.guard(ready("send")))),
        Poll::Ready(Err(Interruption::Deadline))
    );
}
