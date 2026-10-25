// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual public handlers -> SDK -> shared executor -> synthetic HTTP -> output.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_jobs::{
    Confirmation, HttpJobExecutor, JobCommand, JobConfig, JobEvent, JobHost, JobKind, JobUi,
    NoFiles, ScheduleArgs, command_from_matches, execution_data, job_subcommand,
    schedule_configuration,
};
use fabric_cli_output::{CommandError, Timestamp};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{Audience, CredentialSelection, JobClient, JobLimits, JobType};
use serde_json::{Value, json};
use std::{
    future::{pending, poll_fn},
    sync::{Arc, Mutex, atomic::Ordering},
    task::Poll,
};
use support::*;

#[derive(Default)]
struct Ui {
    events: Mutex<Vec<JobEvent>>,
    answer: bool,
    confirmations: Mutex<usize>,
    fail_output: bool,
}

impl JobUi for Ui {
    fn emit(&self, event: JobEvent) -> Result<(), CommandError> {
        if self.fail_output {
            return Err(CommandError::unexpected(Some(
                "synthetic output failure".into(),
            )));
        }
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    fn confirm(&self) -> Confirmation<'_> {
        *self.confirmations.lock().unwrap() += 1;
        Box::pin(async { Ok(self.answer) })
    }
}

struct Harness {
    sdk: JobClient,
    http: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
    current: Arc<Element>,
    catalog: Catalog,
    ui: Ui,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = azure_core::http::AsyncRawResponse>) -> Self {
        Self::resource("DataPipeline", replies)
    }
    fn resource(
        kind: &str,
        replies: impl IntoIterator<Item = azure_core::http::AsyncRawResponse>,
    ) -> Self {
        let tenant = Element::tenant("tenant", "tenant");
        let workspace = Element::child(
            "demo",
            Some("ws".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            tenant,
        )
        .unwrap();
        let item = Element::child(
            "item",
            Some("item".into()),
            NodeKind::Item(ItemType::parse(kind).unwrap()),
            workspace.clone(),
        )
        .unwrap();
        let http = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let sdk = JobClient::new(
            Arc::new(TestCredential::default()),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            Arc::new(HttpJobExecutor::new(
                HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default())
                    .unwrap(),
            )),
            JobLimits::default(),
        );
        Self {
            sdk,
            http,
            timer,
            current: workspace.clone(),
            catalog: Catalog::new(vec![workspace, item]),
            ui: Ui::default(),
        }
    }
    fn host(&self) -> JobHost<'_> {
        JobHost {
            sdk: &self.sdk,
            operations: None,
            lookup: &self.catalog,
            current: self.current.clone(),
            home: HomeContext::Unspecified,
            files: &NoFiles,
            ui: &self.ui,
            config: JobConfig::default(),
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        }
    }
}

fn parse(args: &[&str]) -> JobCommand {
    let args = std::iter::once("job").chain(args.iter().copied());
    command_from_matches(&job_subcommand().try_get_matches_from(args).unwrap()).unwrap()
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../../tests/compatibility/jobs/cases.json")).unwrap()
}

#[tokio::test]
async fn independent_python_command_cases_match_real_handler_outputs_and_request_counts() {
    let fixture = fixtures();
    for resource in fixture["resources"].as_object().unwrap().keys() {
        for case in fixture["cli"].as_array().unwrap() {
            let replies = case["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(|reply| {
                    let headers: Vec<_> = reply
                        .get("headers")
                        .and_then(Value::as_object)
                        .map(|values| {
                            values
                                .iter()
                                .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let body = reply.get("body").map(Value::to_string).unwrap_or_default();
                    response(reply["status"].as_u64().unwrap() as u16, &body, &headers)
                })
                .collect::<Vec<_>>();
            let mut h = Harness::resource(resource, replies);
            h.ui.answer = case["confirmed"] == true;
            let mut arguments = vec![
                case["leaf"].as_str().unwrap().to_owned(),
                format!("item.{resource}"),
            ];
            if let Some(fields) = case.get("args").and_then(Value::as_object) {
                for (name, value) in fields {
                    if name == "disable" && *value == false {
                        arguments.push("--disable".into());
                    } else if *value == true {
                        arguments.push(format!("--{name}"));
                    } else if !value.is_boolean() {
                        arguments.push(format!(
                            "--{}",
                            if name == "schedule_id" { "id" } else { name }
                        ));
                        arguments.push(
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string()),
                        );
                    }
                }
            }
            let borrowed: Vec<_> = arguments.iter().map(String::as_str).collect();
            let command = parse(&borrowed);
            let mut host = h.host();
            host.config.cancel_on_timeout = case
                .get("cancel_on_timeout")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let result = host.execute(&command, pending()).await;
            if let Some(expected) = case.get("error") {
                let value = serde_json::to_value(
                    result.unwrap_err().envelope(host.timestamp.clone(), None),
                )
                .unwrap();
                assert_eq!(value["result"]["error_code"], *expected, "{}", case["name"]);
            } else if let Some(expected) = case.get("output") {
                let output = result.unwrap().unwrap();
                assert_eq!(
                    output.input.command.as_deref(),
                    Some(
                        case.get("output_command")
                            .and_then(Value::as_str)
                            .unwrap_or("job")
                    )
                );
                let mut actual = serde_json::Map::new();
                if let Some(message) = output.input.message {
                    actual.insert("message".into(), json!(message));
                }
                if let Some(data) = output.input.data {
                    actual.insert("data".into(), data);
                }
                assert_eq!(Value::Object(actual), *expected, "{}", case["name"]);
            } else {
                assert!(result.unwrap().is_none(), "{}", case["name"]);
            }
            assert_eq!(
                h.http.requests.lock().unwrap().len(),
                case["responses"].as_array().unwrap().len(),
                "{}",
                case["name"]
            );
            assert_eq!(
                h.ui.events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, JobEvent::Table(_))),
                case["table"] == true,
                "{}",
                case["name"]
            );
        }
    }
}

fn kind(value: &str) -> JobType {
    match value {
        "RunNotebook" => JobType::Notebook,
        "Pipeline" => JobType::Pipeline,
        "sparkjob" => JobType::Spark,
        "TableMaintenance" => JobType::TableMaintenance,
        "refreshGraph" => JobType::RefreshGraph,
        _ => panic!("unknown fixture job type"),
    }
}

fn tokens(value: Option<&Value>) -> Option<Vec<String>> {
    value.map(|v| {
        v.as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect()
    })
}

#[test]
fn every_source_registered_job_argument_is_present_with_the_same_cardinality() {
    let inventory: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/commands/inventory.json"
    ))
    .unwrap();
    let command = job_subcommand();
    let mut checked = 0;
    for source in inventory["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["command"].as_str().unwrap().starts_with("job "))
    {
        let name = source["command"]
            .as_str()
            .unwrap()
            .strip_prefix("job ")
            .unwrap();
        let leaf = command
            .get_subcommands()
            .find(|c| c.get_name() == name)
            .unwrap();
        for arg in source["arguments"].as_array().unwrap() {
            let first = arg["names"][0].as_str().unwrap();
            let native = leaf
                .get_arguments()
                .find(|native| {
                    native.get_id().as_str() == first
                        || arg["names"].as_array().unwrap().iter().any(|name| {
                            name.as_str()
                                .unwrap()
                                .strip_prefix("--")
                                .is_some_and(|long| native.get_long() == Some(long))
                        })
                })
                .unwrap();
            if arg["nargs"] == "+" {
                let range = native.get_num_args().unwrap();
                assert_eq!(range.min_values(), 1);
                assert_eq!(range.max_values(), usize::MAX);
            }
            assert_eq!(
                native.is_required_set(),
                arg["required"] == true || first == "path",
                "{name}: {first}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 8);
}

#[test]
fn source_payload_and_schedule_fixtures_are_independent_of_the_handlers() {
    for case in fixtures()["execution"].as_array().unwrap() {
        let mut command = parse(&["start", "item.DataPipeline"]);
        let args = &case["args"];
        command.params = tokens(args.get("params"));
        command.config = tokens(args.get("config"));
        command.input = tokens(args.get("input"));
        let result = execution_data(&command, kind(case["job_type"].as_str().unwrap()), &NoFiles);
        if let Some(error) = case.get("error") {
            let error_value = serde_json::to_value(
                result
                    .unwrap_err()
                    .envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None),
            )
            .unwrap();
            assert_eq!(
                error_value["result"]["error_code"], *error,
                "{}",
                case["name"]
            );
        } else {
            assert_eq!(
                result.unwrap().unwrap_or(Value::Null),
                case["expected"],
                "{}",
                case["name"]
            );
        }
    }
    for case in fixtures()["schedules"].as_array().unwrap() {
        let raw = &case["args"];
        let args = ScheduleArgs {
            enable: raw["enable"].as_bool().unwrap_or(false),
            disable: raw.get("disable") == Some(&Value::Bool(false)),
            schedule_type: raw["type"].as_str().map(Into::into),
            interval: raw["interval"].as_str().map(Into::into),
            start: raw["start"].as_str().map(Into::into),
            end: raw["end"].as_str().map(Into::into),
            days: raw["days"].as_str().map(Into::into),
        };
        let input = tokens(raw.get("input"));
        let result =
            schedule_configuration(&args, input.as_deref(), case["update"] == true, &NoFiles);
        if case.get("error").is_some() {
            assert!(result.is_err(), "{}", case["name"]);
        } else {
            assert_eq!(
                result.unwrap().unwrap_or(Value::Null),
                case["expected"],
                "{}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn start_submits_real_execution_data_and_returns_an_async_id_envelope() {
    let h = Harness::resource("Notebook", [accepted()]);
    let command = parse(&[
        "start",
        "item.Notebook",
        "-P",
        "n:int=2",
        "-C",
        "{'x':true}",
    ]);
    let plan = h.host().dispatch_json(&command, pending()).await.unwrap();
    assert_eq!(plan.exit_code, 0);
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["command"], "job");
    assert_eq!(value["result"]["data"], json!([{"id":"run"}]));
    assert_eq!(value["result"]["message"], "Job instance 'run' created");
    let requests = h.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url().as_str().ends_with("?jobType=RunNotebook"));
    match requests[0].body() {
        azure_core::http::Body::Bytes(bytes) => assert_eq!(
            serde_json::from_slice::<Value>(bytes).unwrap(),
            json!({"executionData":{"parameters":{"n":{"type":"int","value":2}},"configuration":{"x":true}}})
        ),
        _ => panic!("expected request bytes"),
    }
}

#[tokio::test]
async fn run_waits_for_execution_and_preserves_terminal_failure_and_unknown_branches() {
    for (state, exit, message) in [
        ("Completed", 0, Some("Job instance 'run' completed")),
        ("Cancelled", 0, None),
        ("Deduped", 0, None),
        ("Future", 0, None),
        ("Failed", 1, Some("Job instance 'run' Failed")),
    ] {
        let h = Harness::new([
            accepted(),
            response(
                200,
                &json!({"id":"run","status":state,"extra":null}).to_string(),
                &[],
            ),
        ]);
        let command = parse(&["run", "item.DataPipeline"]);
        let plan = h.host().dispatch_json(&command, pending()).await.unwrap();
        assert_eq!(plan.exit_code, exit, "{state}");
        if let Some(message) = message {
            let value: Value = serde_json::from_slice(&plan.payload).unwrap();
            assert_eq!(value["result"]["message"], message);
        } else {
            assert!(plan.payload.is_empty(), "{state}");
        }
        assert_eq!(*h.timer.waits.lock().unwrap(), [10]);
        if matches!(state, "Failed" | "Future") {
            assert!(
                h.ui.events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, JobEvent::Table(_)))
            );
        }
    }
}

#[tokio::test]
async fn timeout_policy_cancels_by_default_or_leaves_the_remote_job_running() {
    for cancel in [false, true] {
        let mut replies = vec![accepted()];
        if cancel {
            replies.push(response(202, "", &[]));
        }
        let h = Harness::new(replies);
        let mut host = h.host();
        host.config.cancel_on_timeout = cancel;
        let plan = host
            .dispatch_json(
                &parse(&["run", "item.DataPipeline", "--timeout", "3"]),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(plan.exit_code, 0);
        assert_eq!(*h.timer.waits.lock().unwrap(), [3]);
        let requests = h.http.requests.lock().unwrap();
        assert_eq!(requests.len(), if cancel { 2 } else { 1 });
        if cancel {
            assert!(requests[1].url().as_str().ends_with("/run/cancel"));
            let output: Value = serde_json::from_slice(&plan.payload).unwrap();
            assert_eq!(
                output["result"]["message"],
                "Job instance 'run' cancelled (async)"
            );
        } else {
            assert!(plan.payload.is_empty());
        }
    }
}

#[tokio::test]
async fn cancel_wait_is_real_status_polling_not_202_completion() {
    let h = Harness::new([
        response(202, "", &[]),
        response(200, r#"{"id":"run","status":"Cancelled"}"#, &[]),
    ]);
    let result = h
        .host()
        .execute(
            &parse(&["run-cancel", "item.DataPipeline", "--id", "run", "--wait"]),
            pending(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(h.http.requests.lock().unwrap().len(), 2);
    assert_eq!(*h.timer.waits.lock().unwrap(), [10]);
}

#[tokio::test]
async fn list_projects_first_row_keys_and_status_retains_the_ordered_full_object() {
    let h = Harness::new([response(
        200,
        r#"{"value":[{"id":"one","status":"Completed"},{"id":"two","extra":1}]}"#,
        &[],
    )]);
    let plan = h
        .host()
        .dispatch_json(&parse(&["run-list", "item.DataPipeline"]), pending())
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(
        value["result"]["data"],
        json!([{"id":"one","status":"Completed"},{"id":"two"}])
    );
    for (schedule, tail, body) in [
        (
            false,
            "/jobs/instances/run",
            r#"{"status":"InProgress","id":"run","extra":null}"#,
        ),
        (
            true,
            "/jobs/Pipeline/schedules/run",
            r#"{"enabled":false,"id":"run","configuration":{}}"#,
        ),
    ] {
        let h = Harness::new([response(200, body, &[])]);
        let mut command = parse(&["run-status", "item.DataPipeline", "--id", "run"]);
        command.schedule = schedule;
        let output = h
            .host()
            .execute(&command, pending())
            .await
            .unwrap()
            .unwrap();
        assert!(output.show_headers);
        assert_eq!(
            output.input.data.unwrap(),
            serde_json::from_str::<Value>(body).unwrap()
        );
        assert!(
            h.http.requests.lock().unwrap()[0]
                .url()
                .as_str()
                .ends_with(tail)
        );
    }
    for schedule in [false, true] {
        let h = Harness::new([response(200, r#"{"value":[]}"#, &[])]);
        let mut command = parse(&["run-list", "item.DataPipeline"]);
        command.schedule = schedule;
        let value = h
            .host()
            .execute(&command, pending())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            value.input.message.as_deref(),
            Some(if schedule {
                "No schedules found"
            } else {
                "No runs found"
            })
        );
    }
}

#[tokio::test]
async fn schedules_create_update_get_list_remove_with_real_flags_and_confirmation() {
    let h = Harness::new([response(201, r#"{"id":"sch"}"#, &[])]);
    let command = parse(&[
        "run-sch",
        "item.DataPipeline",
        "--type",
        "weekly",
        "--interval",
        "10:00",
        "--days",
        "Monday",
        "--start",
        "2024-01-01T00:00:00",
        "--end",
        "2024-12-31T23:59:59",
        "--enable",
    ]);
    let value = h
        .host()
        .execute(&command, pending())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        value.input.message.as_deref(),
        Some("Job schedule 'sch' created")
    );
    match h.http.requests.lock().unwrap()[0].body() {
        azure_core::http::Body::Bytes(bytes) => {
            let body: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(body["enabled"], true);
            assert_eq!(body["configuration"]["weekdays"], json!(["Monday"]));
        }
        _ => panic!("expected bounded request"),
    }
    for (flag, enabled) in [("--enable", true), ("--disable", false)] {
        let h = Harness::new([
            response(
                200,
                r#"{"id":"sch","enabled":true,"configuration":{"type":"Cron","interval":10}}"#,
                &[],
            ),
            response(200, r#"{"id":"sch"}"#, &[]),
        ]);
        let command = parse(&["run-update", "item.DataPipeline", "--id", "sch", flag]);
        let value = h
            .host()
            .execute(&command, pending())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            value.input.message.as_deref(),
            Some("Job schedule sch updated")
        );
        let requests = h.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        match requests[1].body() {
            azure_core::http::Body::Bytes(bytes) => assert_eq!(
                serde_json::from_slice::<Value>(bytes).unwrap(),
                json!({"configuration":{"type":"Cron","interval":10},"enabled":enabled})
            ),
            _ => panic!("expected bounded request"),
        }
    }
    for (force, answer, requests) in [(false, false, 0), (false, true, 1), (true, false, 1)] {
        let mut h = Harness::new(if requests == 1 {
            vec![response(200, "", &[])]
        } else {
            vec![]
        });
        h.ui.answer = answer;
        let mut command = parse(&["run-rm", "item.DataPipeline", "--id", "sch"]);
        command.force = force;
        let result = h.host().execute(&command, pending()).await.unwrap();
        assert_eq!(h.http.requests.lock().unwrap().len(), requests);
        assert_eq!(*h.ui.confirmations.lock().unwrap(), usize::from(!force));
        assert_eq!(result.is_some(), requests == 1);
    }
}

#[tokio::test]
async fn source_schedule_conflict_and_validation_failures_do_not_mutate() {
    let h = Harness::new([response(
        409,
        r#"{"errorCode":"EntityConflict","message":"synthetic"}"#,
        &[],
    )]);
    let command = parse(&[
        "run-sch",
        "item.DataPipeline",
        "--input",
        "{'enabled':false,'configuration':{}}",
    ]);
    let plan = h.host().dispatch_json(&command, pending()).await.unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["result"]["error_code"], "NotSupported");
    assert_eq!(
        value["result"]["message"],
        "Only one schedule per item is supported"
    );
    for args in [
        vec![
            "run",
            "item.DataPipeline",
            "--timeout",
            "10",
            "--polling_interval",
            "10",
        ],
        vec![
            "start",
            "item.DataPipeline",
            "--input",
            "{}",
            "--params",
            "n:int=1",
        ],
        vec![
            "run-update",
            "item.DataPipeline",
            "--id",
            "sch",
            "--enable",
            "--disable",
        ],
        vec!["run-sch", "item.DataPipeline", "--type", "cron"],
    ] {
        let h = Harness::new([]);
        let plan = h
            .host()
            .dispatch_json(&parse(&args), pending())
            .await
            .unwrap();
        assert_eq!(plan.exit_code, 1);
        assert!(h.http.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn source_paths_unsupported_resources_and_service_errors_are_visible() {
    let h = Harness::resource("Report", []);
    let plan = h
        .host()
        .dispatch_json(&parse(&["start", "item.Report"]), pending())
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["result"]["error_code"], "UnsupportedCommand");
    assert!(h.http.requests.lock().unwrap().is_empty());
    let h = Harness::new([accepted()]);
    let command = parse(&["start", "/demo.Workspace/item.DataPipeline"]);
    assert_eq!(
        h.host()
            .dispatch_json(&command, pending())
            .await
            .unwrap()
            .exit_code,
        0
    );
    for (status, expected_exit, code) in [
        (401, 4, "Unauthorized"),
        (403, 1, "Forbidden"),
        (404, 1, "NotFound"),
    ] {
        let h = Harness::new([response(status, "{}", &[])]);
        let plan = h
            .host()
            .dispatch_json(
                &parse(&["run-status", "item.DataPipeline", "--id", "run"]),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(plan.exit_code, expected_exit);
        let value: Value = serde_json::from_slice(&plan.payload).unwrap();
        assert_eq!(value["result"]["error_code"], code);
    }
}

#[tokio::test]
async fn local_cancellation_and_output_failures_do_not_claim_success_or_remote_cleanup() {
    let h = Harness::new([accepted()]);
    assert_eq!(
        h.host()
            .dispatch_json(&parse(&["start", "item.DataPipeline"]), async {})
            .await
            .unwrap()
            .exit_code,
        1
    );
    assert!(h.http.requests.lock().unwrap().is_empty());
    let mut h = Harness::new([accepted()]);
    h.ui.fail_output = true;
    assert!(
        h.host()
            .execute(&parse(&["start", "item.DataPipeline"]), pending())
            .await
            .is_err()
    );
    assert!(h.http.requests.lock().unwrap().is_empty());
    let h = Harness::new([]);
    h.http.pending.store(true, Ordering::SeqCst);
    let mut first = true;
    let cancel = poll_fn(move |cx| {
        if first {
            first = false;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    assert!(
        h.host()
            .execute(
                &parse(&["run-cancel", "item.DataPipeline", "--id", "run"]),
                cancel
            )
            .await
            .is_err()
    );
    assert_eq!(h.http.requests.lock().unwrap().len(), 1);
}

#[test]
fn parser_rejects_missing_values_and_preserves_source_repeat_and_signed_timeout_rules() {
    for args in [
        vec!["job", "start"],
        vec!["job", "run-cancel", "item.DataPipeline"],
        vec!["job", "run", "item.DataPipeline", "--polling_interval", "0"],
        vec!["job", "run-sch", "item.DataPipeline", "--type", "monthly"],
        vec!["job", "start", "item.DataPipeline", "--timeout", "2"],
    ] {
        assert!(job_subcommand().try_get_matches_from(args).is_err());
    }
    let command = parse(&[
        "run",
        "item.DataPipeline",
        "--timeout",
        "-1",
        "--params",
        "a:int=1",
        "--params",
        "a:int=2",
    ]);
    assert_eq!(command.timeout, Some(-1));
    assert_eq!(command.params, Some(vec!["a:int=2".into()]));
    assert_eq!(command.kind, JobKind::Run);
    assert!(
        !parse(&["run-update", "item.DataPipeline", "--id", "sch"])
            .schedule_args
            .disable
    );
    assert!(
        parse(&[
            "run-update",
            "item.DataPipeline",
            "--id",
            "sch",
            "--disable"
        ])
        .schedule_args
        .disable
    );
}

#[tokio::test]
async fn every_supported_resource_uses_its_source_job_type_and_no_definition_operation() {
    for (resource, job_type) in [
        ("Notebook", "RunNotebook"),
        ("DataPipeline", "Pipeline"),
        ("SparkJobDefinition", "sparkjob"),
        ("Lakehouse", "TableMaintenance"),
        ("GraphModel", "refreshGraph"),
    ] {
        let h = Harness::resource(resource, [accepted()]);
        let path = format!("item.{resource}");
        let command = parse(&["start", &path, "--input", "{'demo':true}"]);
        assert!(
            h.host()
                .execute(&command, pending())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            h.http.requests.lock().unwrap()[0].url().query(),
            Some(format!("jobType={job_type}").as_str())
        );
    }
}

#[tokio::test]
async fn input_file_boundary_and_result_writer_failures_are_not_hidden() {
    struct Files;
    impl fabric_cli_jobs::JsonInput for Files {
        fn read_json(&self, path: &str) -> Result<Value, CommandError> {
            assert_eq!(path, "fixture name.json");
            Ok(json!({"parameters":{"x":2}}))
        }
    }
    let h = Harness::new([accepted()]);
    let mut host = h.host();
    host.files = &Files;
    let output = host
        .execute(
            &parse(&["start", "item.DataPipeline", "-i", "'fixture", "name.json'"]),
            pending(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.after_output.len(), 1);
    let h = Harness::new([accepted()]);
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let result = h
        .host()
        .run_json(
            &parse(&["start", "item.DataPipeline"]),
            pending(),
            &mut BrokenWriter,
            &mut Vec::new(),
        )
        .await;
    assert!(matches!(result, Err(fabric_cli_jobs::JobWriteError::Io(_))));
    assert!(
        !h.ui
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e,JobEvent::Grey(text) if text.contains("To see status")))
    );
}

#[tokio::test]
async fn direct_sdk_and_cli_adapter_consume_the_same_shared_http_contract() {
    let http = ScriptedHttp::new([response(200, r#"{"id":"run","status":"InProgress"}"#, &[])]);
    let direct = sdk(http.clone(), Arc::new(TestTimer::default()));
    let target = fabric_sdk_jobs::JobTarget::new("ws", "item", JobType::Pipeline).unwrap();
    assert_eq!(
        direct
            .status(&target, "run", pending())
            .await
            .unwrap()
            .status(),
        "InProgress"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn later_page_service_failure_is_not_a_successful_partial_cli_list() {
    let h = Harness::new([
        response(
            200,
            r#"{"value":[{"id":"one"}],"continuationToken":"next"}"#,
            &[],
        ),
        response(403, r#"{"errorCode":"Denied","message":"synthetic"}"#, &[]),
    ]);
    let plan = h
        .host()
        .dispatch_json(&parse(&["run-list", "item.DataPipeline"]), pending())
        .await
        .unwrap();
    assert_eq!(plan.exit_code, 1);
    let output: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(output["status"], "Failure");
    assert_eq!(output["result"]["error_code"], "Forbidden");
    assert!(output["result"].get("data").is_none());
    assert_eq!(h.http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn generic_operation_cancellation_uses_the_injected_policy_or_fails_explicitly() {
    struct OperationResult;
    impl fabric_sdk_jobs::JobOperationReconciler for OperationResult {
        fn reconcile<'a>(
            &'a self,
            accepted: &'a fabric_sdk_jobs::JobResponse,
        ) -> fabric_sdk_jobs::JobFuture<
            'a,
            Result<fabric_sdk_jobs::JobResponse, fabric_sdk_jobs::JobError>,
        > {
            assert_eq!(
                accepted
                    .headers
                    .get("x-ms-operation-id")
                    .map(String::as_str),
                Some("operation")
            );
            Box::pin(async {
                Ok(fabric_sdk_jobs::JobResponse {
                    status: 200,
                    body: Vec::new(),
                    headers: Default::default(),
                })
            })
        }
    }
    let command = parse(&["run-cancel", "item.DataPipeline", "--id", "run", "--wait"]);
    let h = Harness::new([response(202, "", &[("x-ms-operation-id", "operation")])]);
    let plan = h.host().dispatch_json(&command, pending()).await.unwrap();
    let output: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(output["result"]["error_code"], "NotSupported");
    assert_eq!(h.http.requests.lock().unwrap().len(), 1);
    let h = Harness::new([response(202, "", &[("x-ms-operation-id", "operation")])]);
    let mut host = h.host();
    host.operations = Some(&OperationResult);
    assert!(host.execute(&command, pending()).await.unwrap().is_none());
    assert_eq!(h.http.requests.lock().unwrap().len(), 1);
    let h = Harness::new([
        accepted(),
        response(202, "", &[("x-ms-operation-id", "operation")]),
    ]);
    let mut host = h.host();
    host.operations = Some(&OperationResult);
    assert!(
        host.execute(
            &parse(&["run", "item.DataPipeline", "--timeout", "3"]),
            pending()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(h.http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unsupported_skill_header_is_not_silently_discarded_before_a_mutation() {
    let h = Harness::new([]);
    let command = parse(&["start", "item.DataPipeline", "--skill", "synthetic-skill"]);
    assert_eq!(command.skill.as_deref(), Some("synthetic-skill"));
    let plan = h.host().dispatch_json(&command, pending()).await.unwrap();
    assert_eq!(plan.exit_code, 1);
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(value["result"]["error_code"], "NotSupported");
    assert!(h.http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn multi_token_folder_relative_and_explicit_home_paths_reach_the_same_job_target() {
    for folder_current in [false, true] {
        let mut h = Harness::resource("Notebook", [accepted()]);
        let workspace = h.current.clone();
        let folder = Element::child(
            "nested",
            Some("folder".into()),
            NodeKind::Folder,
            workspace.clone(),
        )
        .unwrap();
        let item = Element::child(
            "item with space",
            Some("item".into()),
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            folder.clone(),
        )
        .unwrap();
        h.catalog = Catalog::new(vec![workspace, folder.clone(), item]);
        let command = if folder_current {
            h.current = folder;
            parse(&["start", "./item", "with", "space.Notebook"])
        } else {
            parse(&["start", "nested.Folder/item", "with", "space.Notebook"])
        };
        let before = h.current.clone();
        assert!(
            h.host()
                .execute(&command, pending())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.current, before);
        assert_eq!(
            h.http.requests.lock().unwrap()[0].url().path(),
            "/v1/workspaces/ws/items/item/jobs/instances"
        );
    }
    let h = Harness::new([accepted()]);
    let mut host = h.host();
    host.home = HomeContext::Workspace("demo.Workspace".into());
    assert!(
        host.execute(&parse(&["start", "~/item.DataPipeline"]), pending())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(h.http.requests.lock().unwrap().len(), 1);
}
