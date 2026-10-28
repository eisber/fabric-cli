// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[allow(dead_code)]
mod base {
    include!("../support/mod.rs");
}

pub(crate) use base::{Credential, Reply, reply, sdk};

use async_trait::async_trait;
use azure_core::http::{
    AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fmt,
    future::{Future, pending},
    sync::{Arc, Mutex},
    time::Duration,
};

pub(crate) struct Http {
    replies: Mutex<VecDeque<Reply>>,
    pub(crate) requests: Mutex<Vec<String>>,
    pub(crate) headers: Mutex<Vec<Headers>>,
    pub(crate) hold_after: Option<usize>,
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfflineHttp").finish_non_exhaustive()
    }
}

impl Http {
    pub(crate) fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(vec![]),
            headers: Mutex::new(vec![]),
            hold_after: None,
        }
    }

    pub(crate) fn assert_consumed(&self) {
        assert!(
            self.replies.lock().unwrap().is_empty(),
            "unused HTTP replies"
        );
    }
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.method(), Method::Get);
        assert_eq!(request.url().scheme(), "https");
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            format!("Bearer {}", "offline-command-test")
        );
        assert_eq!(
            request.headers().get_str(&"content-type".into()).unwrap(),
            "application/json"
        );
        self.requests
            .lock()
            .unwrap()
            .push(request.url().as_str().into());
        self.headers.lock().unwrap().push(request.headers().clone());
        if self.hold_after == Some(self.requests.lock().unwrap().len()) {
            pending::<()>().await;
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP");
        let mut headers = Headers::new();
        for (key, value) in reply.headers {
            headers.insert(key, value);
        }
        let bytes = match reply.body {
            Value::String(raw) => raw.into_bytes(),
            value => serde_json::to_vec(&value).unwrap(),
        };
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(reply.status),
            headers,
            bytes,
        ))
    }
}

#[derive(Default)]
pub(crate) struct Timer(pub(crate) Mutex<Vec<Duration>>);

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.0.lock().unwrap().push(delay);
        Box::pin(async move {
            if delay == Duration::from_secs(240) {
                pending::<()>().await;
            }
        })
    }
}

pub(crate) fn executor(http: Arc<Http>, timer: Arc<Timer>) -> HttpExecutor {
    HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap()
}

pub(crate) fn complete<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}
