// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TABLE SDK -> actual shared HttpExecutor -> synthetic Azure network boundary.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_tables::{LoadCompletion, SharedTableExecutor, StorageReads};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_tables::*;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken {
            token: "offline-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}
fn sdk() -> TableClient {
    TableClient::new(
        Arc::new(Credential),
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}

#[derive(Debug)]
struct Network {
    responses: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<(String, String)>>,
    pending: bool,
    active: AtomicBool,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests
            .lock()
            .unwrap()
            .push((request.method().to_string(), request.url().to_string()));
        self.active.store(true, Ordering::SeqCst);
        let _active = Active(&self.active);
        if self.pending {
            pending::<()>().await;
        }
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected mutation replay"))
    }
}
#[derive(Default)]
struct Timer {
    expired: AtomicBool,
}
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(poll_fn(move |_| {
            if delay == Duration::from_secs(240) && !self.expired.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}
fn network(status: u16, headers: &[(&str, &str)], body: &str, is_pending: bool) -> Arc<Network> {
    let mut values = Headers::new();
    for &(key, value) in headers {
        values.insert(key.to_string(), value.to_string());
    }
    Arc::new(Network {
        responses: Mutex::new(VecDeque::from([AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            values,
            body.as_bytes().to_vec(),
        )])),
        requests: Mutex::new(Vec::new()),
        pending: is_pending,
        active: AtomicBool::new(false),
    })
}
struct Storage;
impl StorageReads for Storage {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            assert_eq!(request.method(), "HEAD");
            assert_eq!(
                request.url().host_str(),
                Some("onelake.dfs.fabric.microsoft.com")
            );
            Ok(TableResponse {
                status: 200,
                body: Vec::new(),
                headers: BTreeMap::from([("x-ms-resource-type".into(), "file".into())]),
            })
        })
    }
}
struct Completion;
impl LoadCompletion for Completion {
    fn complete<'a>(
        &'a self,
        _request: TableRequest<'a>,
        response: TableResponse,
        _cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            // No fake 202 completion while the shared LRO dependency is pending.
            Err(TableError {
                kind: TableErrorKind::Execution,
                operation_id: response.headers.get("x-ms-operation-id").cloned(),
                mutation_may_have_occurred: true,
            })
        })
    }
}
fn path(local: &str) -> TablePath {
    TablePath::new("ws", "item", local).unwrap()
}

#[tokio::test]
async fn immediate_complete_accepted_nooutput_and_header_lro_are_distinct() {
    for (status, headers, expected) in [
        (200, vec![], Some(LoadOutcome::Completed)),
        (201, vec![], Some(LoadOutcome::Completed)),
        (
            202,
            vec![],
            Some(LoadOutcome::Accepted { operation_id: None }),
        ),
        (
            202,
            vec![(
                "Location",
                "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/job",
            )],
            Some(LoadOutcome::Accepted { operation_id: None }),
        ),
        (204, vec![], Some(LoadOutcome::NoOutput)),
        (202, vec![("x-ms-operation-id", "op")], None),
    ] {
        let raw = network(status, &headers, "", false);
        let http = HttpExecutor::new(
            raw.clone(),
            Arc::new(Timer::default()),
            ExecutionOptions::default(),
        )
        .unwrap();
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &Completion,
        };
        let result = sdk()
            .load(
                &executor,
                &path("Tables/sales"),
                &path("Files/input.csv"),
                &LoadOptions::default(),
                TableLimits::default(),
                pending(),
            )
            .await;
        match expected {
            Some(expected) => assert_eq!(result.unwrap(), expected),
            None => {
                let error = result.unwrap_err();
                assert_eq!(error.operation_id.as_deref(), Some("op"));
                assert!(error.mutation_may_have_occurred);
            }
        }
        let requests = raw.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "POST");
        assert_eq!(
            requests[0].1,
            "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/item/tables/sales/load"
        );
    }
}

#[tokio::test]
async fn post_failure_and_streaming_limit_never_replay_or_become_success() {
    for (status, body, limit, kind) in [
        (
            503,
            "",
            1024,
            TableErrorKind::Http {
                status: 503,
                code: None,
                message: None,
            },
        ),
        (200, "1234", 3, TableErrorKind::Limit),
    ] {
        let raw = network(status, &[], body, false);
        let http = HttpExecutor::new(
            raw.clone(),
            Arc::new(Timer::default()),
            ExecutionOptions::default(),
        )
        .unwrap();
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &Completion,
        };
        let result = sdk()
            .load(
                &executor,
                &path("Tables/sales"),
                &path("Files/input.csv"),
                &LoadOptions::default(),
                TableLimits {
                    max_response_bytes: std::num::NonZeroUsize::new(limit).unwrap(),
                    ..TableLimits::default()
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(result.kind, kind);
        assert!(result.mutation_may_have_occurred);
        assert_eq!(raw.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancelling_dropping_and_timing_out_release_shared_network_resources() {
    for mode in ["cancel", "drop", "timeout"] {
        let raw = network(200, &[], "", true);
        let timer = Arc::new(Timer::default());
        let http =
            HttpExecutor::new(raw.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &Completion,
        };
        let sdk = sdk();
        let table = path("Tables/sales");
        let source = path("Files/input.csv");
        let options = LoadOptions::default();
        let cancel = AtomicBool::new(false);
        let cancelled = poll_fn(|_| {
            if cancel.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut future = Box::pin(sdk.load(
            &executor,
            &table,
            &source,
            &options,
            TableLimits::default(),
            cancelled,
        ));
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(raw.active.load(Ordering::SeqCst));
        if mode == "drop" {
            drop(future);
        } else {
            if mode == "cancel" {
                cancel.store(true, Ordering::SeqCst);
            } else {
                timer.expired.store(true, Ordering::SeqCst);
            }
            let error = future.await.unwrap_err();
            assert_eq!(
                error.kind,
                if mode == "cancel" {
                    TableErrorKind::Cancelled
                } else {
                    TableErrorKind::TimedOut
                }
            );
            assert!(error.mutation_may_have_occurred);
        }
        assert!(!raw.active.load(Ordering::SeqCst));
        assert_eq!(raw.requests.lock().unwrap().len(), 1);
    }
}
