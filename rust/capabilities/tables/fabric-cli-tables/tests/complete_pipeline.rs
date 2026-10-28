// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real command/SDK/released-storage/shared-operation/output consumers, network offline.

#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_output::{
    CommandError, OutputError, OutputInput, OutputStatus, RenderPlan, Timestamp,
};
use fabric_cli_tables::*;
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_operations::{OperationLimits, OperationWarning};
use fabric_sdk_tables::*;
use serde_json::{Value, json};
use std::{
    future::pending,
    sync::{Arc, Mutex},
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
            token: "offline-complete-pipeline".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}
struct Context {
    kind: &'static str,
}
impl TableResolver for Context {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        _: bool,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            if self.kind == "Notebook" {
                return Err(CommandError::new(
                    Some("not supported for command 'ls'".into()),
                    Some("UnsupportedCommand".into()),
                ));
            }
            Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path.join(" ")).unwrap(),
                item_type: ItemType::parse(self.kind).unwrap(),
            })
        })
    }
}
struct UnexpectedRawStorage;
impl StorageReads for UnexpectedRawStorage {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("Released generic storage adapter must handle actual GET/HEAD") })
    }
}
struct Warnings;
impl TableOperationObserver for Warnings {
    fn warning(&self, warning: OperationWarning) -> Result<(), TableError> {
        panic!("unexpected fixture warning: {warning:?}")
    }
}

async fn run(
    network: Arc<ScriptedHttp>,
    command: &TableCommand,
    kind: &'static str,
) -> (u8, Vec<u8>, Vec<u8>, Arc<Credential>) {
    run_limits(network, command, kind, TableLimits::default()).await
}

async fn run_limits(
    network: Arc<ScriptedHttp>,
    command: &TableCommand,
    kind: &'static str,
    limits: TableLimits,
) -> (u8, Vec<u8>, Vec<u8>, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    let sdk = TableClient::new(
        credential.clone(),
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap();
    let http = Arc::new(
        HttpExecutor::new(
            network.clone(),
            Arc::new(TestTimer::default()),
            ExecutionOptions::default(),
        )
        .unwrap(),
    );
    let storage = OneLakeHttpExecutor::new(http.clone());
    let completion = FabricLoadCompletion {
        http: &http,
        limits: OperationLimits::default(),
        observer: &Warnings,
    };
    let loads = SharedTableExecutor {
        http: &http,
        storage: &UnexpectedRawStorage,
        operations: &completion,
    };
    let executor = TableWithOneLake {
        tables: &loads,
        storage: &storage,
    };
    let jobs = support::sdk(network, Arc::new(TestTimer::default()));
    let maintenance = JobMaintenance {
        jobs: &jobs,
        wait: Default::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    let context = Context { kind };
    let host = TableHost {
        sdk: &sdk,
        executor: &executor,
        jobs: &maintenance,
        resolver: &context,
        output_format: "json",
        text: None,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        limits,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = host
        .run(command, pending(), &mut out, &mut err)
        .await
        .unwrap();
    (exit, out, err, credential)
}

#[tokio::test]
async fn source_load_option_inventory_runs_actual_source_head_and_post_not_a_preflight_stub() {
    let original: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/cases.json"
    ))
    .unwrap();
    let extra: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/branches.json"
    ))
    .unwrap();
    for case in original["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(extra["cases"].as_array().unwrap().iter())
        .filter(|case| case["operation"] == "load")
    {
        let resource_type = case["source_kind"].as_str().unwrap_or("file");
        let mut replies = vec![response(200, "", &[("x-ms-resource-type", resource_type)])];
        if case.get("error_code").is_none() {
            replies.push(response(
                case["load_status"].as_u64().unwrap_or(200) as u16,
                "",
                &[],
            ));
        }
        let network = ScriptedHttp::new(replies);
        let command = TableCommand {
            operation: TableOperation::Load {
                file: vec!["Files/input.csv".into()],
                mode: case["mode"].as_str().unwrap_or("overwrite").into(),
                format: case["format"].as_str().map(str::to_owned),
                extension: case["extension"].as_str().map(str::to_owned),
            },
            path: vec!["Tables/sales".into()],
            output_format: None,
        };
        let (exit, out, err, credential) = run(network.clone(), &command, "Lakehouse").await;
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests[0].method(), azure_core::http::Method::Head);
        assert_eq!(
            requests[0].url().as_str(),
            "https://onelake.dfs.fabric.microsoft.com/ws/item/Files/input.csv"
        );
        if case.get("error_code").is_some() {
            let output: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(exit, 1, "{}", case["id"]);
            assert_eq!(output["result"]["error_code"], case["error_code"]);
            if case["error_code"] == "UnexpectedError" {
                assert!(output.get("command").is_none());
            }
            assert_eq!(requests.len(), 1, "invalid options must not POST");
        } else {
            assert_eq!(exit, 0, "{}", case["id"]);
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests[1].url().as_str(),
                "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/item/tables/sales/load"
            );
            if let Some(expected) = case.get("expected") {
                match requests[1].body() {
                    azure_core::http::Body::Bytes(bytes) => assert_eq!(
                        &serde_json::from_slice::<Value>(bytes).unwrap(),
                        expected,
                        "{}",
                        case["id"]
                    ),
                    _ => panic!("expected JSON request"),
                }
            }
            if case["expected_no_output"] == true {
                assert!(out.is_empty());
            } else {
                let output: Value = serde_json::from_slice(&out).unwrap();
                assert_eq!(
                    output["result"]["message"],
                    case["expected_message"]
                        .as_str()
                        .unwrap_or("Table 'sales' loaded successfully")
                );
            }
        }
        let scopes = credential.0.lock().unwrap();
        assert_eq!(scopes[0], Audience::OneLake.scope());
        assert_eq!(
            scopes.len(),
            requests.len(),
            "one credential acquisition per actual request"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "Loading 'sales' table. It may take some time...\n"
        );
    }
}

#[tokio::test]
async fn real_head_then_load_poll_and_result_preserves_structured_failure_messages_and_evidence() {
    for detail in [
        json!({"z":null,"a":[true,false,"x'y",1.0]}),
        json!(["bad", null]),
        Value::Null,
        json!("plain"),
    ] {
        for cancelled in [false, true] {
            let state = if cancelled { "Cancelled" } else { "Failed" };
            let network = ScriptedHttp::new([
                response(200, "", &[("x-ms-resource-type", "file")]),
                response(202, "", &[("x-ms-operation-id", OP)]),
                response(
                    200,
                    &json!({"status":state,"error":detail}).to_string(),
                    &[],
                ),
            ]);
            let command = TableCommand {
                operation: TableOperation::Load {
                    file: vec!["Files/a".into()],
                    mode: "overwrite".into(),
                    format: None,
                    extension: None,
                },
                path: vec!["Tables/sales".into()],
                output_format: None,
            };
            let (exit, out, _, credential) = run(network.clone(), &command, "Lakehouse").await;
            assert_eq!(exit, 1);
            let output: Value = serde_json::from_slice(&out).unwrap();
            let reference = fabric_sdk_operations::OperationError {
                kind: fabric_sdk_operations::OperationErrorKind::OperationFailed {
                    status: state.into(),
                    error: None,
                },
                raw_service_error: Some(Box::new(detail.clone())),
                operation_id: Some(OP.into()),
                mutation_may_have_occurred: true,
            };
            let expected = fabric_cli_mutations::operation_error_output(&reference);
            assert_eq!(output["result"]["message"], expected.message.unwrap());
            assert_eq!(
                output["result"]["error_code"],
                if cancelled {
                    "LongRunningOperationCancelled"
                } else {
                    "LongRunningOperationFailed"
                }
            );
            let requests = network.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(
                credential.0.lock().unwrap().as_slice(),
                [
                    Audience::OneLake.scope(),
                    Audience::Fabric.scope(),
                    Audience::Fabric.scope()
                ]
            );
        }
    }
}

#[tokio::test]
async fn exact_empty_normalized_error_is_not_replaced_by_default_or_normalized_twice() {
    let network = ScriptedHttp::new([
        response(200, "", &[("x-ms-resource-type", "file")]),
        response(202, "", &[("x-ms-operation-id", OP)]),
        response(400, r#"{"message":"..."}"#, &[]),
    ]);
    let command = TableCommand {
        operation: TableOperation::Load {
            file: vec!["Files/a".into()],
            mode: "overwrite".into(),
            format: None,
            extension: None,
        },
        path: vec!["Tables/sales".into()],
        output_format: None,
    };
    let (exit, out, _, _) = run(network, &command, "Lakehouse").await;
    assert_eq!(exit, 1);
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["result"]["message"], "");
    assert_eq!(value["result"]["error_code"], "UnknownError");
}

#[tokio::test]
async fn schema_branch_inventory_runs_released_onelake_adapter_and_preserves_empty_raw_fields() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/branches.json"
    ))
    .unwrap();
    for case in cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["operation"] == "schema")
    {
        let mut replies=vec![response(200,&json!({"paths":case["logs"].as_array().unwrap().iter().map(|log|json!({"name":log["name"]})).collect::<Vec<_>>()} ).to_string(),&[])];
        for path in case["expected_reads"].as_array().unwrap() {
            let name = path.as_str().unwrap().strip_prefix("ws/").unwrap();
            let log = case["logs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|log| log["name"] == name)
                .unwrap();
            replies.push(response(200, log["text"].as_str().unwrap(), &[]));
        }

        let network = ScriptedHttp::new(replies);
        let command = TableCommand {
            operation: TableOperation::Schema,
            path: vec!["Tables/sales".into()],
            output_format: None,
        };
        let (exit, out, err, _) = run(network.clone(), &command, "Warehouse").await;
        let value: Value = serde_json::from_slice(&out).unwrap();
        if let Some(code) = case.get("error_code") {
            assert_eq!(exit, 1);
            assert_eq!(&value["result"]["error_code"], code);
        } else {
            assert_eq!(exit, 0);
            let expected = &case["expected"];
            let data = if expected.is_array() {
                expected.clone()
            } else if expected.is_null() {
                Value::Null
            } else {
                json!([expected])
            };
            assert_eq!(value["result"]["data"], data, "{}", case["id"]);
        }
        let diagnostics = String::from_utf8(err).unwrap();
        assert_eq!(
            diagnostics.contains("Schema extracted successfully"),
            case.get("error_code").is_none() || case["extracted_notice"] == true
        );
        assert_eq!(
            network.requests.lock().unwrap().len(),
            1 + case["expected_reads"].as_array().unwrap().len()
        );
    }
}

#[tokio::test]
async fn source_pinned_full_client_error_messages_match_without_repr_or_double_normalization() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/errors.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let reply = &case["responses"][0];
        let network = ScriptedHttp::new([
            response(200, "", &[("x-ms-resource-type", "file")]),
            response(202, "", &[("x-ms-operation-id", OP)]),
            response(
                reply["status"].as_u64().unwrap() as u16,
                &reply["body"].to_string(),
                &[],
            ),
        ]);
        let command = TableCommand {
            operation: TableOperation::Load {
                file: vec!["Files/a".into()],
                mode: "overwrite".into(),
                format: None,
                extension: None,
            },
            path: vec!["Tables/sales".into()],
            output_format: None,
        };
        let (exit, out, _, _) = run(network, &command, "Lakehouse").await;
        assert_eq!(exit, 1);
        let output: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            output["result"]["error_code"], case["expected"]["error_code"],
            "{}",
            case["id"]
        );
        assert_eq!(
            output["result"]["message"], case["expected_error_message"],
            "{}",
            case["id"]
        );
    }
}

#[tokio::test]
async fn maintenance_option_inventory_reaches_actual_jobs_and_preserves_no_tempfile_payload() {
    let original: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/cases.json"
    ))
    .unwrap();
    let branches: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/branches.json"
    ))
    .unwrap();
    let decimal: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/retention-unicode.json"
    ))
    .unwrap();
    for case in original["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(branches["cases"].as_array().unwrap().iter())
        .chain(decimal["cases"].as_array().unwrap().iter())
        .filter(|case| matches!(case["operation"].as_str(), Some("vacuum" | "optimize")))
    {
        let replies = if case.get("error_code").is_some() {
            Vec::new()
        } else {
            vec![
                support::accepted(),
                response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
            ]
        };
        let network = ScriptedHttp::new(replies);
        let operation = if case["operation"] == "vacuum" {
            TableOperation::Vacuum {
                retain_n_hours: case["hours"].as_str().unwrap_or("168").into(),
            }
        } else {
            TableOperation::Optimize {
                vorder: case["vorder"].as_bool().unwrap_or(false),
                zorder: case["zorder"].as_str().map(str::to_owned),
            }
        };
        let command = TableCommand {
            operation,
            path: vec![case["local_path"].as_str().unwrap_or("Tables/sales").into()],
            output_format: None,
        };
        let (exit, out, _, _) = run(network.clone(), &command, "Lakehouse").await;
        let value: Value = serde_json::from_slice(&out).unwrap();
        let requests = network.requests.lock().unwrap();
        if let Some(code) = case.get("error_code") {
            assert_eq!(exit, 1);
            assert_eq!(&value["result"]["error_code"], code);
            if code == "UnexpectedError" {
                assert!(value.get("command").is_none());
            }
            assert!(requests.is_empty());
        } else {
            assert_eq!(exit, 0);
            assert_eq!(value["result"]["message"], "Job instance 'run' completed");
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].url().query(), Some("jobType=TableMaintenance"));
            match requests[0].body() {
                azure_core::http::Body::Bytes(bytes) => assert_eq!(
                    serde_json::from_slice::<Value>(bytes).unwrap(),
                    json!({"executionData":case["expected"]}),
                    "{}",
                    case["id"]
                ),
                _ => panic!("expected byte payload"),
            }
        }
    }
}

#[tokio::test]
async fn supported_schema_resources_use_real_storage_while_other_resource_mutations_do_not_dispatch()
 {
    for kind in [
        "Lakehouse",
        "Warehouse",
        "KQLDatabase",
        "MirroredDatabase",
        "SemanticModel",
        "SQLDatabase",
        "Notebook",
    ] {
        let supported = kind != "Notebook";
        let network = ScriptedHttp::new(if supported {
            vec![
                response(
                    200,
                    r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}"#,
                    &[],
                ),
                response(
                    200,
                    r#"{"metaData":{"schemaString":"{\"fields\":[]}"}}"#,
                    &[],
                ),
            ]
        } else {
            Vec::new()
        });
        let command = TableCommand {
            operation: TableOperation::Schema,
            path: vec!["Tables/sales".into()],
            output_format: None,
        };
        let (exit, _, _, _) = run(network.clone(), &command, kind).await;
        assert_eq!(exit, if supported { 0 } else { 1 });
        assert_eq!(
            network.requests.lock().unwrap().len(),
            if supported { 2 } else { 0 }
        );
        if kind != "Lakehouse" {
            let network = ScriptedHttp::new(Vec::new());
            let command = TableCommand {
                operation: TableOperation::Optimize {
                    vorder: false,
                    zorder: None,
                },
                path: vec!["Tables/sales".into()],
                output_format: None,
            };
            let (exit, _, _, _) = run(network.clone(), &command, kind).await;
            assert_eq!(exit, 1);
            assert!(network.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn real_source_head_and_external_load_count_against_one_table_lifecycle_budget() {
    let network = ScriptedHttp::new([
        response(200, "", &[("x-ms-resource-type", "file")]),
        response(202, "", &[("x-ms-operation-id", OP)]),
    ]);
    let command = TableCommand {
        operation: TableOperation::Load {
            file: vec!["Files/a".into()],
            mode: "overwrite".into(),
            format: None,
            extension: None,
        },
        path: vec!["Tables/sales".into()],
        output_format: None,
    };
    let (exit, out, _, credential) = run_limits(
        network.clone(),
        &command,
        "Lakehouse",
        TableLimits {
            max_requests: std::num::NonZeroUsize::new(2).unwrap(),
            ..TableLimits::default()
        },
    )
    .await;
    assert_eq!(exit, 1);
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["result"]["error_code"], "Limit");
    assert_eq!(network.requests.lock().unwrap().len(), 2);
    assert_eq!(
        credential.0.lock().unwrap().as_slice(),
        [Audience::OneLake.scope(), Audience::Fabric.scope()]
    );
}

#[tokio::test]
async fn source_properties_failure_precedes_invalid_format_and_never_submits_target_load() {
    let network = ScriptedHttp::new([response(
        403,
        "",
        &[("x-ms-error-code", "AuthorizationFailure")],
    )]);
    let command = TableCommand {
        operation: TableOperation::Load {
            file: vec!["Files/a".into()],
            mode: "overwrite".into(),
            format: Some("csv".into()),
            extension: None,
        },
        path: vec!["Tables/sales".into()],
        output_format: None,
    };
    let (exit, out, _, _) = run(network.clone(), &command, "Lakehouse").await;
    assert_eq!(exit, 1);
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["result"]["error_code"], "AuthorizationFailure");
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn actual_one_lake_hierarchy_support_and_nested_job_type_match_pinned_source() {
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/resources.json"
    ))
    .unwrap();
    for case in manifest["cases"].as_array().unwrap() {
        let kind = match case["item_type"].as_str().unwrap() {
            "Lakehouse" => "Lakehouse",
            "Warehouse" => "Warehouse",
            "KQLDatabase" => "KQLDatabase",
            "MirroredDatabase" => "MirroredDatabase",
            "SemanticModel" => "SemanticModel",
            "SQLDatabase" => "SQLDatabase",
            "CosmosDBDatabase" => "CosmosDBDatabase",
            "SparkJobDefinition" => "SparkJobDefinition",
            _ => panic!("unknown immutable resource case"),
        };
        let target = if kind == "SparkJobDefinition" {
            "Libs/sales"
        } else {
            "Tables/sales"
        };
        let network = ScriptedHttp::new([
            response(200, "", &[("x-ms-resource-type", "file")]),
            response(200, "", &[]),
        ]);
        let load = TableCommand {
            operation: TableOperation::Load {
                file: vec!["Files/a".into()],
                mode: "overwrite".into(),
                format: None,
                extension: None,
            },
            path: vec![target.into()],
            output_format: None,
        };
        let (exit, _, _, _) = run(network.clone(), &load, kind).await;
        assert_eq!(
            exit, 0,
            "the source does not apply yaml supported_items to OneLakeItem: {kind}"
        );
        assert_eq!(
            network.requests.lock().unwrap()[1].url().path(),
            "/v1/workspaces/ws/lakehouses/item/tables/sales/load"
        );
        let allowed = case["job_check"].as_bool().unwrap();
        let network = ScriptedHttp::new(if allowed {
            vec![
                support::accepted(),
                response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
            ]
        } else {
            Vec::new()
        });
        let maintenance = TableCommand {
            operation: TableOperation::Optimize {
                vorder: false,
                zorder: None,
            },
            path: vec![target.into()],
            output_format: None,
        };
        let (exit, out, err, _) = run(network.clone(), &maintenance, kind).await;
        if allowed {
            assert_eq!(exit, 0);
            assert_eq!(
                network.requests.lock().unwrap()[0].url().query(),
                Some(format!("jobType={}", case["job_type"].as_str().unwrap()).as_str())
            );
        } else {
            assert_eq!(exit, 1);
            let value: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(value["result"]["error_code"], "UnsupportedCommand");
            assert_eq!(
                value["result"]["message"],
                "not supported for command 'job run'"
            );
            assert!(network.requests.lock().unwrap().is_empty());
            assert_eq!(
                String::from_utf8(err).unwrap(),
                "Optimizing the 'sales' table. It may take some time...\n"
            );
        }
    }
}

#[tokio::test]
async fn schema_output_uses_existing_strict_integer_guard_instead_of_silent_float_coercion() {
    let network = ScriptedHttp::new([
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}"#,
            &[],
        ),
        response(
            200,
            r#"{"metaData":{"schemaString":"{\"fields\":[{\"name\":\"x\",\"metadata\":{\"wide\":18446744073709551616}}]}"}}"#,
            &[],
        ),
    ]);
    let command = TableCommand {
        operation: TableOperation::Schema,
        path: vec!["Tables/sales".into()],
        output_format: None,
    };
    let (exit, out, _, _) = run(network, &command, "Lakehouse").await;
    assert_eq!(exit, 1);
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["result"]["error_code"], "InvalidJson");
    assert!(value["result"].get("data").is_none());
}

// Confirm normalized failures are passed to the shared text boundary, not rendered here.
struct TextCapture(Mutex<Vec<OutputInput>>);
impl TableTextOutput for TextCapture {
    fn render(&self, output: &TableOutput) -> Result<RenderPlan, OutputError> {
        if let Some(input) = &output.input {
            self.0.lock().unwrap().push(input.clone());
        }
        Ok(RenderPlan {
            stream: fabric_cli_output::OutputStream::Stdout,
            exit_code: 1,
            payload: Vec::new(),
        })
    }
    fn error(&self, _: &CommandError) -> Result<RenderPlan, OutputError> {
        panic!("normalized operation errors must use the shared normalized boundary")
    }
}

#[test]
fn normalized_text_error_retains_empty_message_and_failure_status() {
    let capture = TextCapture(Mutex::new(Vec::new()));
    let input = OutputInput {
        status: OutputStatus::Failure,
        message: Some("".into()),
        error_code: Some("UnknownError".into()),
        ..OutputInput::default()
    };
    capture.normalized_error(&input).unwrap();
    let observed = capture.0.lock().unwrap();
    assert_eq!(observed[0].message.as_deref(), Some(""));
    assert_eq!(observed[0].status, OutputStatus::Failure);
}

#[tokio::test]
async fn literal_python_wire_bytes_survive_head_submission_and_shared_result_completion() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/wire.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result_location = format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result");
        let network = ScriptedHttp::new([
            response(
                200,
                "",
                &[("x-ms-resource-type", case["source_kind"].as_str().unwrap())],
            ),
            response(202, "", &[("x-ms-operation-id", OP), ("retry-after", "0")]),
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("Location", &result_location)],
            ),
            response(200, r#"{"rows":1}"#, &[]),
        ]);
        let command = TableCommand {
            operation: TableOperation::Load {
                file: vec![
                    case["source_path"]
                        .as_str()
                        .unwrap_or("Files/input.csv")
                        .into(),
                ],
                mode: case["mode"].as_str().unwrap_or("overwrite").into(),
                format: case["format"].as_str().map(str::to_owned),
                extension: case["extension"].as_str().map(str::to_owned),
            },
            path: vec!["Tables/sales".into()],
            output_format: None,
        };
        let (exit, out, err, credential) = run(network.clone(), &command, "Lakehouse").await;
        assert_eq!(exit, 0, "{}", case["id"]);
        let output: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            output["result"]["message"],
            "Table 'sales' loaded successfully"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "Loading 'sales' table. It may take some time...\n"
        );
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests.len(), 4, "{}", case["id"]);
        assert_eq!(requests[0].method(), azure_core::http::Method::Head);
        assert_eq!(
            requests[0].url().host_str(),
            Some("onelake.dfs.fabric.microsoft.com")
        );
        assert_eq!(requests[1].method(), azure_core::http::Method::Post);
        assert_eq!(
            requests[1].url().path(),
            "/v1/workspaces/ws/lakehouses/item/tables/sales/load"
        );
        match requests[1].body() {
            azure_core::http::Body::Bytes(bytes) => {
                assert_eq!(
                    bytes.as_ref(),
                    case["expected_body"].as_str().unwrap().as_bytes(),
                    "{}",
                    case["id"]
                );
                assert!(bytes.is_ascii());
            }
            _ => panic!("expected exact serialized JSON bytes"),
        }
        assert_eq!(requests[2].method(), azure_core::http::Method::Get);
        assert_eq!(requests[3].method(), azure_core::http::Method::Get);
        assert_eq!(requests[3].url().as_str(), result_location);
        assert_eq!(
            credential.0.lock().unwrap().as_slice(),
            [
                Audience::OneLake.scope(),
                Audience::Fabric.scope(),
                Audience::Fabric.scope(),
                Audience::Fabric.scope()
            ]
        );
    }
}
