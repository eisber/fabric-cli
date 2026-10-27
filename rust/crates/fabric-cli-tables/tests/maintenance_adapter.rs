// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public CLI -> TABLE adapter -> actual JobClient/shared HTTP, with offline I/O.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli_context::ItemType;
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_tables::*;
use fabric_sdk_jobs::WaitOptions;
use fabric_sdk_tables::*;
use serde_json::{Value, json};
use std::{future::pending, sync::Arc};
use support::*;

struct Context;
impl TableResolver for Context {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            assert!(must_exist);
            Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path.join(" ")).unwrap(),
                item_type: ItemType::parse("Lakehouse").unwrap(),
            })
        })
    }
}
struct NoStorageOrLoad;
impl TableExecutor for NoStorageOrLoad {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("maintenance must not call OneLake") })
    }
    fn load<'a>(
        &'a self,
        _: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async { panic!("maintenance must not call load") })
    }
}
fn table_sdk() -> TableClient {
    TableClient::new(
        Arc::new(TestCredential::default()),
        &CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "synthetic", Audience::OneLake).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn vacuum_and_optimize_real_jobs_reach_source_output_and_exact_payloads() {
    for (args, expected, leaf) in [
        (
            vec![
                "table",
                "vacuum",
                "Tables/dbo/sales",
                "--retain_n_hours",
                "182",
            ],
            json!({"vacuumSettings":{"retentionPeriod":"7:14:00:00"},"tableName":"sales","schemaName":"dbo"}),
            "table vacuum",
        ),
        (
            vec![
                "table",
                "optimize",
                "Tables/dbo/sales",
                "--vorder",
                "--zorder",
                "key, date",
            ],
            json!({"optimizeSettings":{"vOrder":true,"zOrderBy":["key","date"]},"tableName":"sales","schemaName":"dbo"}),
            "table optimize",
        ),
    ] {
        let network = ScriptedHttp::new([
            accepted(),
            response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
        ]);
        let jobs = sdk(network.clone(), Arc::new(TestTimer::default()));
        let maintenance = JobMaintenance {
            jobs: &jobs,
            wait: WaitOptions::default(),
            cancel_on_timeout: true,
            operations: None,
        };
        let sdk = table_sdk();
        let host = TableHost {
            sdk: &sdk,
            executor: &NoStorageOrLoad,
            jobs: &maintenance,
            resolver: &Context,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let command =
            TableCommand::from_matches(&command().try_get_matches_from(args).unwrap()).unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            host.run(&command, pending(), &mut stdout, &mut stderr)
                .await
                .unwrap(),
            0
        );
        let output: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(output["command"], leaf);
        assert_eq!(output["result"]["message"], "Job instance 'run' completed");
        assert_eq!(output["result"]["data"], json!([{"id":"run"}]));
        let diagnostics = String::from_utf8(stderr).unwrap();
        assert!(diagnostics.contains(
            "\u{221f} Job instance 'run' created\n\u{221f} Timeout: no timeout specified\n"
        ));
        assert!(diagnostics.ends_with("\u{221f} Job instance status: Completed\n\n"));
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        match requests[0].body() {
            azure_core::http::Body::Bytes(bytes) => assert_eq!(
                serde_json::from_slice::<Value>(bytes).unwrap(),
                json!({"executionData":expected})
            ),
            _ => panic!("expected bytes"),
        }
    }
}

#[tokio::test]
async fn failed_job_and_timeout_cancel_use_actual_engine_without_false_completion() {
    for state in ["Failed", "timeout-cancel", "timeout-leave", "Deduped"] {
        let mut replies = vec![accepted()];
        if state == "timeout-cancel" {
            replies.push(response(202, "", &[]));
        } else if state != "timeout-leave" {
            replies.push(response(
                200,
                &format!(r#"{{"id":"run","status":"{state}"}}"#),
                &[],
            ));
        }
        let network = ScriptedHttp::new(replies);
        let jobs = sdk(network.clone(), Arc::new(TestTimer::default()));
        let maintenance = JobMaintenance {
            jobs: &jobs,
            wait: WaitOptions {
                timeout: state.starts_with("timeout").then_some(0),
                polling_interval: None,
            },
            cancel_on_timeout: state != "timeout-leave",
            operations: None,
        };
        let sdk = table_sdk();
        let host = TableHost {
            sdk: &sdk,
            executor: &NoStorageOrLoad,
            jobs: &maintenance,
            resolver: &Context,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let command = TableCommand::from_matches(
            &command()
                .try_get_matches_from(["table", "vacuum", "Tables/sales"])
                .unwrap(),
        )
        .unwrap();
        let output = host.dispatch(&command, pending()).await.unwrap();
        match state {
            "Failed" => {
                assert_eq!(output.exit_code, 1);
                let value: Value = serde_json::from_slice(&output.payload).unwrap();
                assert_eq!(value["result"]["error_code"], "JobFailed");
                assert_eq!(value["result"]["message"], "Job instance 'run' Failed");
            }
            "timeout-cancel" => {
                let value: Value = serde_json::from_slice(&output.payload).unwrap();
                assert_eq!(
                    value["result"]["message"],
                    "Job instance 'run' cancelled (async)"
                );
                assert_eq!(network.requests.lock().unwrap().len(), 2);
            }
            _ => {
                assert_eq!(output.exit_code, 0);
                assert!(output.payload.is_empty());
            }
        }
    }
}
