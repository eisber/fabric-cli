// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline shared-executor consumer support, with synthetic HTTP and credentials.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::{Duration as AzureDuration, OffsetDateTime},
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_jobs::{
    JobClient, JobExecutionError, JobExecutor, JobFuture, JobLimits, JobRequest, JobResponse,
};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
pub struct ScriptedHttp {
    pub requests: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    pub pending: AtomicBool,
    pub fail: AtomicBool,
}

impl ScriptedHttp {
    pub fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into_iter().collect()),
            pending: AtomicBool::new(false),
            fail: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl HttpClient for ScriptedHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(azure_core::Error::new(
                azure_core::error::ErrorKind::Other,
                "synthetic transport failure",
            ));
        }
        if self.pending.load(Ordering::SeqCst) {
            return pending().await;
        }
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or mutation replay"))
    }
}

#[derive(Default)]
pub struct TestTimer {
    pub waits: Mutex<Vec<u64>>,
    pub expired: AtomicBool,
    pub pending_wait: AtomicBool,
}

impl ExecutionTimer for TestTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(async {
                if !self.expired.load(Ordering::SeqCst) {
                    pending::<()>().await;
                }
            })
        } else {
            Box::pin(async move {
                self.waits.lock().unwrap().push(delay.as_secs());
                if self.pending_wait.load(Ordering::SeqCst) {
                    pending::<()>().await;
                }
            })
        }
    }
}

#[derive(Debug, Default)]
pub struct TestCredential {
    pub calls: AtomicUsize,
    pub failed: AtomicBool,
}

#[async_trait]
impl TokenCredential for TestCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        if self.failed.load(Ordering::SeqCst) {
            return Err(azure_core::Error::new(
                azure_core::error::ErrorKind::Other,
                "synthetic credential failure",
            ));
        }
        Ok(AccessToken {
            token: "synthetic-offline-token".into(),
            expires_on: OffsetDateTime::now_utc() + AzureDuration::hours(1),
        })
    }
}

pub fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut result = Headers::new();
    for &(key, value) in headers {
        result.insert(key.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), result, body.as_bytes().to_vec())
}

pub fn accepted() -> AsyncRawResponse {
    response(
        202,
        "",
        &[(
            "Location",
            "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run",
        )],
    )
}

pub fn sdk(http: Arc<ScriptedHttp>, timer: Arc<TestTimer>) -> JobClient {
    client(
        Arc::new(TestCredential::default()),
        http,
        timer,
        JobLimits::default(),
    )
}

pub fn client(
    credential: Arc<dyn TokenCredential>,
    http: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
    limits: JobLimits,
) -> JobClient {
    JobClient::new(
        credential,
        fabric_sdk_jobs::CredentialSelection::new(
            "offline",
            "synthetic",
            fabric_sdk_jobs::Audience::Fabric,
        )
        .unwrap(),
        Arc::new(TestJobExecutor {
            http: HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap(),
        }),
        limits,
    )
}

pub struct TestJobExecutor {
    pub http: HttpExecutor,
}

impl JobExecutor for TestJobExecutor {
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            self.http
                .execute_fabric(
                    request.method(),
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                )
                .await
                .map_err(|error| match error {
                    fabric_sdk_http::ExecutionError::TimedOut => JobExecutionError::TimedOut,
                    fabric_sdk_http::ExecutionError::ResponseLimit => {
                        JobExecutionError::ResponseLimit
                    }
                    fabric_sdk_http::ExecutionError::RetriesExhausted => {
                        JobExecutionError::RetriesExhausted
                    }
                    _ => JobExecutionError::Failed,
                })
        })
    }
    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        Box::pin(async move {
            self.http
                .wait_for(delay)
                .await
                .map_err(|error| match error {
                    fabric_sdk_http::ExecutionError::TimedOut => JobExecutionError::TimedOut,
                    _ => JobExecutionError::Failed,
                })
        })
    }
}
