// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TABLE load -> shared HTTP POST -> actual shared operation engine, offline only.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_tables::{
    FabricLoadCompletion, SharedTableExecutor, StorageReads, TableOperationObserver,
};
use fabric_cli_tables::{ResolvedTablePath, TableCommand, TableHost, TableResolver, command};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_operations::{OperationErrorKind, OperationLimits, OperationWarning};
use fabric_sdk_tables::*;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::{Future, pending, poll_fn},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};
use support::{ScriptedHttp, TestTimer, response};

const OP: &str = "22222222-2222-2222-2222-222222222222";
#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|s| s.to_string()));
        Ok(AccessToken {
            token: "offline-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}
fn client(credential: Arc<Credential>) -> TableClient {
    TableClient::new(
        credential,
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}
struct Storage;
impl StorageReads for Storage {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            assert_eq!(request.method(), "HEAD");
            Ok(TableResponse {
                status: 200,
                body: Vec::new(),
                headers: BTreeMap::from([("x-ms-resource-type".into(), "file".into())]),
            })
        })
    }
}
#[derive(Default)]
struct Observer(Mutex<Vec<OperationWarning>>);
impl TableOperationObserver for Observer {
    fn warning(&self, warning: OperationWarning) -> Result<(), TableError> {
        self.0.lock().unwrap().push(warning);
        Ok(())
    }
}

struct Snapshot;
impl TableResolver for Snapshot {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        _: bool,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path.join(" ")).unwrap(),
                item_type: ItemType::parse("Lakehouse").unwrap(),
            })
        })
    }
}
fn path(local: &str) -> TablePath {
    TablePath::new("ws", "item", local).unwrap()
}
fn wire(value: &Value) -> azure_core::http::AsyncRawResponse {
    let body = value.get("body").map(|v| v.to_string()).unwrap_or_default();
    let headers: Vec<(String, String)> = value
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .unwrap()
                    .replace("/operations/op", &format!("/operations/{OP}")),
            )
        })
        .map(|(key, value)| {
            let value = if key.eq_ignore_ascii_case("x-ms-operation-id") {
                OP.to_owned()
            } else {
                value
            };
            (key, value)
        })
        .collect();
    response(
        value["status"].as_u64().unwrap() as u16,
        &body,
        &headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn all_pinned_lifecycle_shapes_execute_actual_load_and_shared_poll_result_engine() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/tables/lifecycle.json"
    ))
    .unwrap();
    let extra: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/tables/lifecycle-extra.json"
    ))
    .unwrap();
    for case in cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(extra["cases"].as_array().unwrap().iter())
    {
        let replies = std::iter::once(wire(&case["initial"])).chain(
            case.get("responses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(wire),
        );
        let network = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let http =
            HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
        let observer = Observer::default();
        let operations = FabricLoadCompletion {
            http: &http,
            limits: OperationLimits::default(),
            observer: &observer,
        };
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &operations,
        };
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let result = sdk
            .load(
                &executor,
                &path("Tables/sales"),
                &path("Files/input.csv"),
                &LoadOptions::default(),
                TableLimits::default(),
                pending(),
            )
            .await;
        if let Some(code) = case["expected"].get("error_code") {
            let error = result.unwrap_err();
            let TableErrorKind::SharedOperation(inner) = error.kind else {
                panic!("missing shared error for{}", case["id"])
            };
            match code.as_str().unwrap() {
                "NotFound" => assert!(matches!(
                    inner.kind,
                    OperationErrorKind::Http { status: 404, .. }
                )),
                "LongRunningOperationFailed" => assert!(
                    matches!(inner.kind,OperationErrorKind::OperationFailed {ref status,..} if status=="Failed")
                ),
                "LongRunningOperationCancelled" => assert!(
                    matches!(inner.kind,OperationErrorKind::OperationFailed {ref status,..} if status=="Cancelled")
                ),
                _ => panic!("unexpected expectation"),
            }
            assert!(error.mutation_may_have_occurred);
        } else {
            assert_eq!(
                result.unwrap(),
                if case["expected"]["status"] == 202 {
                    LoadOutcome::Accepted { operation_id: None }
                } else if case["expected"]["status"] == 204 {
                    LoadOutcome::NoOutput
                } else {
                    LoadOutcome::Completed
                },
                "{}",
                case["id"]
            );
        }
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests[0].method(), azure_core::http::Method::Post);
        assert_eq!(
            requests.len(),
            1 + case["expected"]["requests"].as_array().unwrap().len(),
            "{}",
            case["id"]
        );
        for (request, expected) in requests
            .iter()
            .skip(1)
            .zip(case["expected"]["requests"].as_array().unwrap())
        {
            assert_eq!(
                request.method(),
                azure_core::http::Method::Get,
                "no mutation replay"
            );
            assert_eq!(
                request
                    .url()
                    .path()
                    .trim_start_matches("/v1/")
                    .replace(OP, "op"),
                expected.as_str().unwrap()
            );
        }
        assert_eq!(
            *timer.waits.lock().unwrap(),
            case["expected"]["waits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect::<Vec<_>>()
        );
        let scopes = credential.0.lock().unwrap();
        assert_eq!(scopes[0], Audience::OneLake.scope());
        assert_eq!(scopes.len(), 1 + requests.len());
        assert!(
            scopes[1..]
                .iter()
                .all(|scope| scope == Audience::Fabric.scope())
        );
        if case["id"] == "unknown-state-is-running-with-finite-native-bound-needed" {
            assert_eq!(
                *observer.0.lock().unwrap(),
                [OperationWarning::InvalidRetryAfter]
            );
        }
    }
}

#[tokio::test]
async fn cancellation_drop_and_limits_after_acceptance_keep_id_without_replaying_post() {
    for mode in ["cancel", "drop", "limit"] {
        let network = ScriptedHttp::new([response(202, "", &[("x-ms-operation-id", OP)])]);
        let timer = Arc::new(TestTimer::default());
        timer.pending_wait.store(true, Ordering::SeqCst);
        let http = HttpExecutor::new(network.clone(), timer, ExecutionOptions::default()).unwrap();
        let observer = Observer::default();
        let operations = FabricLoadCompletion {
            http: &http,
            limits: OperationLimits {
                max_requests: NonZeroUsize::new(if mode == "limit" { 1 } else { 100 }).unwrap(),
                ..OperationLimits::default()
            },
            observer: &observer,
        };
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &operations,
        };
        let sdk = client(Arc::new(Credential::default()));
        let table = path("Tables/sales");
        let source = path("Files/input.csv");
        let options = LoadOptions::default();
        let stop = AtomicBool::new(false);
        let cancelled = poll_fn(|_| {
            if stop.load(Ordering::SeqCst) {
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
        if mode == "limit" {
            let error = future.await.unwrap_err();
            assert!(
                matches!(error.kind,TableErrorKind::SharedOperation(ref inner) if inner.kind==OperationErrorKind::Limit)
            );
            assert_eq!(error.operation_id.as_deref(), Some(OP));
        } else {
            poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            if mode == "drop" {
                drop(future);
            } else {
                stop.store(true, Ordering::SeqCst);
                let error = future.await.unwrap_err();
                assert_eq!(error.operation_id.as_deref(), Some(OP));
                assert!(error.mutation_may_have_occurred);
            }
        }
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn load_reconciliation_and_job_execution_share_the_existing_http_policy() {
    let network = ScriptedHttp::new([
        response(202, "", &[("x-ms-operation-id", OP)]),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
        support::accepted(),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let timer = Arc::new(TestTimer::default());
    let http =
        HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
    let observer = Observer::default();
    let operations = FabricLoadCompletion {
        http: &http,
        limits: OperationLimits::default(),
        observer: &observer,
    };
    let executor = SharedTableExecutor {
        http: &http,
        storage: &Storage,
        operations: &operations,
    };
    let sdk = client(Arc::new(Credential::default()));
    assert_eq!(
        sdk.load(
            &executor,
            &path("Tables/sales"),
            &path("Files/input.csv"),
            &LoadOptions::default(),
            TableLimits::default(),
            pending()
        )
        .await
        .unwrap(),
        LoadOutcome::Completed
    );
    let jobs = support::sdk(network.clone(), timer);
    let maintenance = JobMaintenance {
        jobs: &jobs,
        wait: Default::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    assert_eq!(
        maintenance
            .run(
                &path("Tables/sales"),
                serde_json::json!({"tableName":"sales"}),
                Box::pin(pending())
            )
            .await
            .unwrap(),
        MaintenanceOutcome::Completed {
            instance_id: "run".into()
        }
    );
    assert_eq!(network.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn real_cli_handler_distinguishes_load_completion_and_source_service_cancellation() {
    for status in ["Succeeded", "Cancelled"] {
        let network = ScriptedHttp::new([
            response(202, "", &[("x-ms-operation-id", OP)]),
            response(200, &format!(r#"{{"status":"{status}"}}"#), &[]),
        ]);
        let timer = Arc::new(TestTimer::default());
        let http =
            HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
        let observer = Observer::default();
        let operations = FabricLoadCompletion {
            http: &http,
            limits: OperationLimits::default(),
            observer: &observer,
        };
        let executor = SharedTableExecutor {
            http: &http,
            storage: &Storage,
            operations: &operations,
        };
        let sdk = client(Arc::new(Credential::default()));
        let jobs = support::sdk(network.clone(), timer);
        let maintenance = JobMaintenance {
            jobs: &jobs,
            wait: Default::default(),
            cancel_on_timeout: true,
            operations: None,
        };
        let host = TableHost {
            sdk: &sdk,
            executor: &executor,
            jobs: &maintenance,
            resolver: &Snapshot,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let parsed = TableCommand::from_matches(
            &command()
                .try_get_matches_from(["table", "load", "Tables/sales", "--file", "Files/a"])
                .unwrap(),
        )
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let exit = host
            .run(&parsed, pending(), &mut stdout, &mut stderr)
            .await
            .unwrap();
        let output: Value = serde_json::from_slice(&stdout).unwrap();
        if status == "Succeeded" {
            assert_eq!(exit, 0);
            assert_eq!(
                output["result"]["message"],
                "Table 'sales' loaded successfully"
            );
        } else {
            assert_eq!(exit, 1);
            assert_eq!(output["command"], "table load");
            assert_eq!(
                output["result"]["error_code"],
                "LongRunningOperationCancelled"
            );
            assert_eq!(
                output["result"]["message"],
                "The operation was cancelled: None"
            );
        }
        assert_eq!(network.requests.lock().unwrap().len(), 2);
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "Loading 'sales' table. It may take some time...\n"
        );
    }
}

#[tokio::test]
async fn accepted_operation_wait_deadline_keeps_observed_id_and_never_replays_load() {
    struct DeadlineTimer(AtomicBool);
    impl fabric_sdk_http::ExecutionTimer for DeadlineTimer {
        fn sleep(&self, delay: std::time::Duration) -> fabric_sdk_http::ExecutionFuture<'_, ()> {
            Box::pin(poll_fn(move |_| {
                if delay == std::time::Duration::from_secs(240) && self.0.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }))
        }
    }
    let network = ScriptedHttp::new([response(202, "", &[("x-ms-operation-id", OP)])]);
    let timer = Arc::new(DeadlineTimer(AtomicBool::new(false)));
    let http =
        HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
    let observer = Observer::default();
    let operations = FabricLoadCompletion {
        http: &http,
        limits: OperationLimits::default(),
        observer: &observer,
    };
    let executor = SharedTableExecutor {
        http: &http,
        storage: &Storage,
        operations: &operations,
    };
    let sdk = client(Arc::new(Credential::default()));
    let table = path("Tables/sales");
    let source = path("Files/a");
    let options = LoadOptions::default();
    let mut future = Box::pin(sdk.load(
        &executor,
        &table,
        &source,
        &options,
        TableLimits::default(),
        pending(),
    ));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    timer.0.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    assert!(
        matches!(error.kind,TableErrorKind::SharedOperation(ref inner) if inner.kind==OperationErrorKind::Execution(fabric_sdk_operations::OperationExecutionError::TimedOut))
    );
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}
