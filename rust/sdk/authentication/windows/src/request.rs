// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::BrokerError;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    thread::Thread,
    time::{Duration, Instant},
};

const WORKER_LIMIT: usize = 4;

struct Lifecycle {
    cancelled: AtomicBool,
    ended: AtomicBool,
    confirmed: AtomicBool,
    failed: AtomicBool,
    uncertain: AtomicBool,
}

/// Secret-free control of cancellation and actual worker teardown.
///
/// Result delivery is not teardown. A cancelled or timed-out worker retains its
/// native capacity until its operation, window and apartment have been released.
#[derive(Clone)]
pub struct OperationControl(Arc<Lifecycle>);

impl OperationControl {
    fn new() -> Self {
        Self(Arc::new(Lifecycle {
            cancelled: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            confirmed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
        }))
    }

    /// Requests cancellation without pretending that native cleanup is complete.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }

    /// Whether the worker has reached its final teardown action.
    pub fn has_ended(&self) -> bool {
        self.0.ended.load(Ordering::Acquire)
    }

    /// Whether operation/window/apartment cleanup was confirmed.
    pub fn cleanup_confirmed(&self) -> bool {
        self.has_ended() && self.0.confirmed.load(Ordering::Acquire)
    }

    /// Current non-secret process worker counts, including pending cleanup.
    pub fn worker_counts() -> (usize, usize) {
        let pool = worker_pool();
        (pool.active.load(Ordering::Acquire), pool.limit)
    }

    pub(crate) fn cleanup_uncertain(&self, uncertain: bool) {
        self.0.uncertain.store(uncertain, Ordering::Release);
    }
}

pub(crate) struct WorkerPool {
    active: AtomicUsize,
    limit: usize,
}

impl WorkerPool {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            limit,
        })
    }

    pub(crate) fn reserve(self: &Arc<Self>) -> Result<WorkerPermit, BrokerError> {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .map(|_| {
                WorkerPermit(Arc::new(Reservation {
                    pool: Arc::clone(self),
                    finished: AtomicBool::new(false),
                }))
            })
            .map_err(|active| BrokerError::CapacityExhausted {
                active,
                limit: self.limit,
            })
    }
}

struct Reservation {
    pool: Arc<WorkerPool>,
    finished: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct WorkerPermit(Arc<Reservation>);

impl WorkerPermit {
    fn finish(self, confirmed: bool) {
        if confirmed && !self.0.finished.swap(true, Ordering::AcqRel) {
            self.0.pool.active.fetch_sub(1, Ordering::AcqRel);
        }
        // An uncertain worker's reservation is intentionally not recycled.
    }
}

pub(crate) fn worker_pool() -> &'static Arc<WorkerPool> {
    static POOL: OnceLock<Arc<WorkerPool>> = OnceLock::new();
    POOL.get_or_init(|| WorkerPool::new(WORKER_LIMIT))
}

struct State<T> {
    result: Option<Result<T, BrokerError>>,
    delivered: bool,
    waker: Option<Waker>,
    worker: Option<Thread>,
}

pub(crate) struct Shared<T>(Mutex<State<T>>, OperationControl, Instant);

impl<T> Shared<T> {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_timeout(Duration::from_secs(600))
    }

    pub(crate) fn with_timeout(timeout: Duration) -> Arc<Self> {
        Arc::new(Self(
            Mutex::new(State {
                result: None,
                delivered: false,
                waker: None,
                worker: None,
            }),
            OperationControl::new(),
            Instant::now() + timeout,
        ))
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.0.lock().unwrap_or_else(|poisoned| {
            self.1.0.failed.store(true, Ordering::Release);
            self.1.cancel();
            poisoned.into_inner()
        })
    }

    pub(crate) fn control(&self) -> OperationControl {
        self.1.clone()
    }

    pub(crate) fn expired(&self) -> bool {
        Instant::now() >= self.2
    }

    pub(crate) fn check_deadline(&self) {
        if self.expired() {
            self.complete(Err(BrokerError::TimedOut));
            self.cancel();
        }
    }

    pub(crate) fn uncertain_cleanup(&self) {
        self.1.0.uncertain.store(true, Ordering::Release);
    }

    pub(crate) fn no_worker(&self) {
        self.1.0.confirmed.store(true, Ordering::Release);
        self.1.0.ended.store(true, Ordering::Release);
    }

    pub(crate) fn attach_worker(&self, worker: Thread) {
        self.lock().worker = Some(worker);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.1.0.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        let worker = {
            let mut state = self.lock();
            if state.delivered {
                return;
            }
            self.1.cancel();
            if state.result.as_ref().is_some_and(Result::is_ok) {
                state.result = Some(Err(BrokerError::Cancelled));
            }
            state.worker.clone()
        };
        if let Some(worker) = worker {
            worker.unpark();
        }
    }

    pub(crate) fn complete(&self, result: Result<T, BrokerError>) {
        let waker = {
            let mut state = self.lock();
            if state.delivered || state.result.is_some() {
                return;
            }
            state.result = Some(match result {
                _ if self.1.0.failed.load(Ordering::Acquire) => {
                    Err(BrokerError::CoordinationFailed)
                }
                Ok(_) if self.is_cancelled() => Err(BrokerError::Cancelled),
                result => result,
            });
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

pub(crate) struct WorkerExit<T> {
    shared: Arc<Shared<T>>,
    permit: Option<WorkerPermit>,
}

impl<T> WorkerExit<T> {
    pub(crate) fn new(shared: Arc<Shared<T>>, permit: WorkerPermit) -> Self {
        Self {
            shared,
            permit: Some(permit),
        }
    }

    pub(crate) fn finish(mut self) {
        let confirmed = !self.shared.1.0.uncertain.load(Ordering::Acquire)
            && !self.shared.1.0.failed.load(Ordering::Acquire);
        self.shared
            .1
            .0
            .confirmed
            .store(confirmed, Ordering::Release);
        self.shared.1.0.ended.store(true, Ordering::Release);
        if let Some(permit) = self.permit.take() {
            permit.finish(confirmed);
        }
    }
}

impl<T> Drop for WorkerExit<T> {
    fn drop(&mut self) {
        if self.permit.is_some() {
            self.shared.1.0.failed.store(true, Ordering::Release);
            self.shared.complete(Err(BrokerError::CoordinationFailed));
            self.shared.1.0.ended.store(true, Ordering::Release);
            // Dropping the permit without finish quarantines its capacity.
        }
    }
}

pub(crate) struct RequestFuture<T>(pub(crate) Arc<Shared<T>>);

impl<T> Future for RequestFuture<T> {
    type Output = Result<T, BrokerError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock();
        if self.0.1.0.failed.load(Ordering::Acquire) {
            state.delivered = true;
            state.result = None;
            return Poll::Ready(Err(BrokerError::CoordinationFailed));
        }
        if let Some(result) = state.result.take() {
            state.delivered = true;
            Poll::Ready(match result {
                Ok(_) if self.0.is_cancelled() => Err(BrokerError::Cancelled),
                result => result,
            })
        } else {
            assert!(!state.delivered, "broker request polled after completion");
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl<T> Drop for RequestFuture<T> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn context(counter: &Arc<WakeCount>) -> Waker {
        Waker::from(Arc::clone(counter))
    }

    #[test]
    fn completion_wakes_the_current_waiter_without_a_runtime() {
        let shared = Shared::new();
        shared.attach_worker(std::thread::current());
        let mut request = RequestFuture(Arc::clone(&shared));
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = context(&counter);
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut request).poll(&mut cx).is_pending());
        shared.complete(Ok(42));
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(Pin::new(&mut request).poll(&mut cx), Poll::Ready(Ok(42)));
    }

    #[test]
    fn cancellation_before_completion_rejects_a_late_success() {
        let shared = Shared::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        shared.cancel();
        shared.complete(Ok(42));
        let waker = Waker::noop();
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(waker)),
            Poll::Ready(Err(BrokerError::Cancelled))
        );
    }

    #[test]
    fn cancellation_after_completion_but_before_delivery_rejects_success() {
        let shared = Shared::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        shared.complete(Ok(42));
        shared.cancel();
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(BrokerError::Cancelled))
        );
    }

    #[test]
    fn dropping_request_requests_native_cleanup_without_a_login() {
        let shared = Shared::<u8>::new();
        drop(RequestFuture(Arc::clone(&shared)));
        assert!(shared.is_cancelled());
    }

    #[test]
    fn native_failures_are_preserved_and_do_not_echo_provider_text() {
        let shared = Shared::<u8>::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        let error = BrokerError::Native {
            stage: crate::NativeStage::Completion,
            code: -1,
        };
        shared.complete(Err(error));
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(error))
        );
        assert_eq!(
            error.to_string(),
            "Windows broker call failed at Completion (HRESULT 0xffffffff)"
        );
    }

    #[test]
    fn cancellation_does_not_hide_native_cleanup_failure() {
        let shared = Shared::<u8>::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        shared.cancel();
        let error = BrokerError::Native {
            stage: crate::NativeStage::Cancellation,
            code: -1,
        };
        shared.complete(Err(error));
        shared.cancel();
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(error))
        );
    }

    #[test]
    fn delivering_success_finishes_without_triggering_cancellation_on_drop() {
        let shared = Shared::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        shared.complete(Ok(42));
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Ok(42))
        );
        drop(request);
        assert!(!shared.is_cancelled());
    }

    #[test]
    fn only_the_latest_waiter_is_woken() {
        let shared = Shared::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        let first = Arc::new(WakeCount(AtomicUsize::new(0)));
        let second = Arc::new(WakeCount(AtomicUsize::new(0)));
        for counter in [&first, &second] {
            let waker = context(counter);
            assert!(
                Pin::new(&mut request)
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
        }
        shared.complete(Ok(42));
        assert_eq!(first.0.load(Ordering::SeqCst), 0);
        assert_eq!(second.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn late_cleanup_cannot_replace_an_already_reported_failure() {
        let shared = Shared::new();
        let mut request = RequestFuture(Arc::clone(&shared));
        let error = BrokerError::CompletionUnconfirmed { code: -1 };
        shared.complete(Err(error));
        shared.complete(Ok(42));
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(error))
        );
    }

    #[test]
    fn four_pending_workers_keep_capacity_after_consumer_drop_and_late_exit_recovers_it() {
        let pool = WorkerPool::new(4);
        let mut workers = Vec::new();
        for _ in 0..4 {
            let shared = Shared::<u8>::new();
            let permit = pool.reserve().unwrap();
            let exit = WorkerExit::new(Arc::clone(&shared), permit);
            shared.complete(Err(BrokerError::CleanupPending {
                active: 4,
                limit: 4,
            }));
            drop(RequestFuture(Arc::clone(&shared)));
            assert!(!shared.control().has_ended());
            workers.push(exit);
        }
        for _ in 0..3 {
            assert!(matches!(
                pool.reserve(),
                Err(BrokerError::CapacityExhausted {
                    active: 4,
                    limit: 4
                })
            ));
        }
        workers.pop().unwrap().finish();
        assert_eq!(pool.active.load(Ordering::Acquire), 3);
        let replacement = pool.reserve().unwrap();
        assert_eq!(pool.active.load(Ordering::Acquire), 4);
        replacement.finish(true);
        for worker in workers {
            worker.finish();
        }
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unconfirmed_cleanup_never_recycles_capacity() {
        let pool = WorkerPool::new(1);
        let shared = Shared::<u8>::new();
        let control = shared.control();
        let exit = WorkerExit::new(Arc::clone(&shared), pool.reserve().unwrap());
        shared.uncertain_cleanup();
        exit.finish();
        assert!(control.has_ended());
        assert!(!control.cleanup_confirmed());
        assert!(matches!(
            pool.reserve(),
            Err(BrokerError::CapacityExhausted {
                active: 1,
                limit: 1
            })
        ));
    }

    #[test]
    fn confirmed_late_cleanup_retry_recovers_the_worker_reservation() {
        let pool = WorkerPool::new(1);
        let shared = Shared::<u8>::new();
        let exit = WorkerExit::new(Arc::clone(&shared), pool.reserve().unwrap());
        shared.uncertain_cleanup();
        shared.control().cleanup_uncertain(false);
        exit.finish();
        assert!(shared.control().cleanup_confirmed());
        pool.reserve().unwrap().finish(true);
    }

    #[test]
    fn rejected_admission_has_no_phantom_worker_to_drain() {
        let shared = Shared::<u8>::new();
        shared.complete(Err(BrokerError::CapacityExhausted {
            active: 4,
            limit: 4,
        }));
        shared.no_worker();
        let mut request = RequestFuture(Arc::clone(&shared));
        assert!(shared.control().cleanup_confirmed());
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(BrokerError::CapacityExhausted {
                active: 4,
                limit: 4,
            }))
        );
    }

    #[test]
    fn absolute_deadline_suppresses_callback_success_without_releasing_worker() {
        let pool = WorkerPool::new(1);
        let shared = Shared::<u8>::with_timeout(Duration::ZERO);
        let exit = WorkerExit::new(Arc::clone(&shared), pool.reserve().unwrap());
        let mut request = RequestFuture(Arc::clone(&shared));
        shared.check_deadline();
        shared.complete(Ok(42));
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(BrokerError::TimedOut))
        );
        assert!(!shared.control().has_ended());
        assert!(pool.reserve().is_err());
        exit.finish();
        assert!(shared.control().cleanup_confirmed());
        pool.reserve().unwrap().finish(true);
    }

    #[test]
    fn poisoned_shared_state_fails_closed_without_repeated_panics() {
        let shared = Shared::<u8>::new();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = shared.0.lock().unwrap();
            panic!("synthetic lock poison");
        }));
        assert!(poisoned.is_err());
        shared.complete(Ok(42));
        shared.cancel();
        shared.complete(Ok(43));
        let mut request = RequestFuture(Arc::clone(&shared));
        assert_eq!(
            Pin::new(&mut request).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(BrokerError::CoordinationFailed))
        );
        drop(request);
        assert!(shared.is_cancelled());
    }
}
