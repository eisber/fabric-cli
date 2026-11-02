// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::BrokerError;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    thread::Thread,
};

struct State<T> {
    result: Option<Result<T, BrokerError>>,
    cancelled: bool,
    delivered: bool,
    waker: Option<Waker>,
    worker: Option<Thread>,
}

pub(crate) struct Shared<T>(Mutex<State<T>>);

impl<T> Shared<T> {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(State {
            result: None,
            cancelled: false,
            delivered: false,
            waker: None,
            worker: None,
        })))
    }

    pub(crate) fn attach_worker(&self, worker: Thread) {
        self.0.lock().unwrap().worker = Some(worker);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().cancelled
    }

    pub(crate) fn cancel(&self) {
        let worker = {
            let mut state = self.0.lock().unwrap();
            if state.delivered {
                return;
            }
            state.cancelled = true;
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
            let mut state = self.0.lock().unwrap();
            if state.delivered || state.result.is_some() {
                return;
            }
            state.result = Some(match result {
                Ok(_) if state.cancelled => Err(BrokerError::Cancelled),
                result => result,
            });
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

pub(crate) struct RequestFuture<T>(pub(crate) Arc<Shared<T>>);

impl<T> Future for RequestFuture<T> {
    type Output = Result<T, BrokerError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.0.lock().unwrap();
        if let Some(result) = state.result.take() {
            state.delivered = true;
            Poll::Ready(result)
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
}
