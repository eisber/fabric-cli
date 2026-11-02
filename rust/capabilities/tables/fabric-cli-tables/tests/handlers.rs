// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual command -> TABLE SDK -> output contracts, independent of the front door.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_tables::*;
use fabric_sdk_tables::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{pending, ready},
    sync::{Arc, Mutex},
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

struct Resolver {
    item_type: &'static str,
    requests: Mutex<Vec<(Vec<String>, bool)>>,
}
impl TableResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        _cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            if self.item_type == "Notebook" {
                return Err(CommandError::new(
                    Some("not supported for command 'ls'".into()),
                    Some("UnsupportedCommand".into()),
                ));
            }
            self.requests
                .lock()
                .unwrap()
                .push((path.into(), must_exist));
            Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path.join(" ")).unwrap(),
                item_type: ItemType::parse(self.item_type).unwrap(),
            })
        })
    }
}
struct Executor {
    accepted: bool,
    requests: Mutex<Vec<String>>,
}
impl TableExecutor for Executor {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.method().into());
            let body = if request.method() == "GET" {
                if request.url().query().is_some() {
                    serde_json::to_vec(
                        &json!({"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}),
                    )
                    .unwrap()
                } else {
                    serde_json::to_vec(&json!({"metaData":{"schemaString":"{\"fields\":[{\"name\":\"x\",\"type\":\"long\"}]}"}})).unwrap()
                }
            } else {
                Vec::new()
            };
            Ok(TableResponse {
                status: 200,
                body,
                headers: BTreeMap::from([("x-ms-resource-type".into(), "file".into())]),
            })
        })
    }
    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.method().into());
            assert_eq!(
                request.url().path(),
                "/v1/workspaces/ws/lakehouses/item/tables/sales/load"
            );
            if self.accepted {
                Ok(LoadOutcome::Accepted { operation_id: None })
            } else {
                Ok(LoadOutcome::Completed)
            }
        })
    }
}
struct Jobs {
    requests: Mutex<Vec<Value>>,
    outcome: MaintenanceOutcome,
}
impl MaintenanceExecutor for Jobs {
    fn run<'a>(
        &'a self,
        _: &'a TablePath,
        config: Value,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(config);
            Ok(self.outcome.clone())
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

fn parsed(args: &[&str]) -> TableCommand {
    TableCommand::from_matches(&command().try_get_matches_from(args).unwrap()).unwrap()
}

#[tokio::test]
async fn complete_and_accepted_loads_remain_distinct_through_real_handler_and_writer() {
    for accepted in [false, true] {
        let sdk = sdk();
        let executor = Executor {
            accepted,
            requests: Mutex::new(Vec::new()),
        };
        let resolver = Resolver {
            item_type: "Lakehouse",
            requests: Mutex::new(Vec::new()),
        };
        let jobs = Jobs {
            requests: Mutex::new(Vec::new()),
            outcome: MaintenanceOutcome::Completed {
                instance_id: "job".into(),
            },
        };
        let host = TableHost {
            sdk: &sdk,
            executor: &executor,
            jobs: &jobs,
            resolver: &resolver,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let command = parsed(&["table", "load", "Tables/sales", "--file", "Files/input.csv"]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            host.run(&command, pending(), &mut out, &mut err)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "Loading 'sales' table. It may take some time...\n"
        );
        let result: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["command"], "table");
        assert_eq!(result["status"], "Success");
        assert_eq!(
            result["result"]["message"],
            if accepted {
                "Load table operation started"
            } else {
                "Table 'sales' loaded successfully"
            }
        );
        assert_eq!(*executor.requests.lock().unwrap(), ["HEAD", "POST"]);
        assert_eq!(
            resolver
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.1)
                .collect::<Vec<_>>(),
            [false, false]
        );
    }
}

#[tokio::test]
async fn invalid_format_still_heads_source_but_never_submits_load() {
    let sdk = sdk();
    let executor = Executor {
        accepted: false,
        requests: Mutex::new(Vec::new()),
    };
    let resolver = Resolver {
        item_type: "Lakehouse",
        requests: Mutex::new(Vec::new()),
    };
    let jobs = Jobs {
        requests: Mutex::new(Vec::new()),
        outcome: MaintenanceOutcome::Completed {
            instance_id: "job".into(),
        },
    };
    let host = TableHost {
        sdk: &sdk,
        executor: &executor,
        jobs: &jobs,
        resolver: &resolver,
        output_format: "json",
        text: None,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        limits: TableLimits::default(),
    };
    let command = parsed(&[
        "table",
        "load",
        "Tables/sales",
        "--file",
        "Files/a",
        "--format",
        "csv",
    ]);
    let plan = host.dispatch(&command, pending()).await.unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["command"], "table load");
    assert_eq!(value["result"]["error_code"], "InvalidInput");
    assert_eq!(plan.exit_code, 1);
    assert_eq!(*executor.requests.lock().unwrap(), ["HEAD"]);
}

#[tokio::test]
async fn maintenance_payload_job_output_and_no_output_terminal_states() {
    let sdk = sdk();
    let executor = Executor {
        accepted: false,
        requests: Mutex::new(Vec::new()),
    };
    let resolver = Resolver {
        item_type: "Lakehouse",
        requests: Mutex::new(Vec::new()),
    };
    for outcome in [
        MaintenanceOutcome::Completed {
            instance_id: "job".into(),
        },
        MaintenanceOutcome::TerminalWithoutOutput {
            instance_id: "job".into(),
            status: "Deduped".into(),
        },
        MaintenanceOutcome::CancellationAccepted {
            instance_id: "job".into(),
        },
    ] {
        let jobs = Jobs {
            requests: Mutex::new(Vec::new()),
            outcome: outcome.clone(),
        };
        let host = TableHost {
            sdk: &sdk,
            executor: &executor,
            jobs: &jobs,
            resolver: &resolver,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let command = parsed(&[
            "table",
            "vacuum",
            "Tables/dbo/sales",
            "--retain_n_hours",
            "-1",
        ]);
        let plan = host.dispatch(&command, pending()).await.unwrap();
        assert_eq!(plan.exit_code, 0);
        assert_eq!(
            *jobs.requests.lock().unwrap(),
            [
                json!({"vacuumSettings":{"retentionPeriod":"-1:23:00:00"},"tableName":"sales","schemaName":"dbo"})
            ]
        );
        match outcome {
            MaintenanceOutcome::TerminalWithoutOutput { .. } => assert!(plan.payload.is_empty()),
            MaintenanceOutcome::Completed { .. } => {
                let value: Value = serde_json::from_slice(&plan.payload).unwrap();
                assert_eq!(value["command"], "table vacuum");
                assert_eq!(value["result"]["message"], "Job instance 'job' completed");
            }
            MaintenanceOutcome::CancellationAccepted { .. } => {
                let value: Value = serde_json::from_slice(&plan.payload).unwrap();
                assert_eq!(value["command"], "table");
                assert_eq!(
                    value["result"]["message"],
                    "Job instance 'job' cancelled (async)"
                );
            }
            MaintenanceOutcome::TimedOutWithoutCancellation { .. } => {
                panic!("not a fixture outcome")
            }
        }
    }
}

#[tokio::test]
async fn unsupported_resource_cancellation_and_unconfigured_text_fail_before_mutation() {
    let sdk = sdk();
    let executor = Executor {
        accepted: false,
        requests: Mutex::new(Vec::new()),
    };
    let resolver = Resolver {
        item_type: "Warehouse",
        requests: Mutex::new(Vec::new()),
    };
    let jobs = Jobs {
        requests: Mutex::new(Vec::new()),
        outcome: MaintenanceOutcome::Completed {
            instance_id: "job".into(),
        },
    };
    let host = TableHost {
        sdk: &sdk,
        executor: &executor,
        jobs: &jobs,
        resolver: &resolver,
        output_format: "json",
        text: None,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        limits: TableLimits::default(),
    };
    let command = parsed(&["table", "optimize", "Tables/sales"]);
    for plan in [
        host.dispatch(&command, pending()).await.unwrap(),
        host.dispatch(&command, ready(())).await.unwrap(),
    ] {
        assert_eq!(plan.exit_code, 1);
    }
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(jobs.requests.lock().unwrap().is_empty());
    let mut command = command;
    command.output_format = Some("text".into());
    let plan = host.dispatch(&command, pending()).await.unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["result"]["error_code"], "NotSupported");
}

#[tokio::test]
async fn schema_resource_matrix_and_stdout_stderr_match_source_handler() {
    for item_type in [
        "Lakehouse",
        "Warehouse",
        "KQLDatabase",
        "MirroredDatabase",
        "SemanticModel",
        "SQLDatabase",
        "Notebook",
    ] {
        let sdk = sdk();
        let executor = Executor {
            accepted: false,
            requests: Mutex::new(Vec::new()),
        };
        let resolver = Resolver {
            item_type,
            requests: Mutex::new(Vec::new()),
        };
        let jobs = Jobs {
            requests: Mutex::new(Vec::new()),
            outcome: MaintenanceOutcome::Completed {
                instance_id: "job".into(),
            },
        };
        let host = TableHost {
            sdk: &sdk,
            executor: &executor,
            jobs: &jobs,
            resolver: &resolver,
            output_format: "json",
            text: None,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            limits: TableLimits::default(),
        };
        let command = parsed(&["table", "schema", "Tables/sales"]);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let exit = host
            .run(&command, pending(), &mut stdout, &mut stderr)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        if item_type == "Notebook" {
            assert_eq!(exit, 1);
            assert_eq!(value["result"]["error_code"], "UnsupportedCommand");
            assert!(stderr.is_empty());
            assert!(executor.requests.lock().unwrap().is_empty());
        } else {
            assert_eq!(exit, 0);
            assert_eq!(value["result"]["data"], json!([{"name":"x","type":"long"}]));
            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                "Getting schema for 'sales' table...\nSchema extracted successfully\n"
            );
            assert_eq!(*executor.requests.lock().unwrap(), ["GET", "GET"]);
        }
    }
}

#[tokio::test]
async fn writer_failures_propagate_before_or_after_mutation_without_replay() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture write failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            panic!("Unexpected flush")
        }
    }
    let sdk = sdk();
    let executor = Executor {
        accepted: false,
        requests: Mutex::new(Vec::new()),
    };
    let resolver = Resolver {
        item_type: "Lakehouse",
        requests: Mutex::new(Vec::new()),
    };
    let jobs = Jobs {
        requests: Mutex::new(Vec::new()),
        outcome: MaintenanceOutcome::Completed {
            instance_id: "job".into(),
        },
    };
    let host = TableHost {
        sdk: &sdk,
        executor: &executor,
        jobs: &jobs,
        resolver: &resolver,
        output_format: "json",
        text: None,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        limits: TableLimits::default(),
    };
    let command = parsed(&["table", "load", "Tables/sales", "--file", "Files/input.csv"]);
    assert!(matches!(
        host.run(&command, pending(), &mut Vec::new(), &mut Broken)
            .await,
        Err(WriteError::Io(_))
    ));
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(matches!(
        host.run(&command, pending(), &mut Broken, &mut Vec::new())
            .await,
        Err(WriteError::Io(_))
    ));
    assert_eq!(*executor.requests.lock().unwrap(), ["HEAD", "POST"]);
}
