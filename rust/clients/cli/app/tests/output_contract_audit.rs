// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parsed commands -> real production sessions -> streams, with offline providers.

#![cfg(feature = "native-read")]

#[path = "family_support/metadata.rs"]
mod family_metadata;
#[allow(dead_code)]
mod family_support;
#[allow(dead_code, unused_imports)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    jobs::JobReadSession,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_cli_output::{Timestamp, text::TextPlan};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, sdk};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, ready},
    sync::Arc,
};

fn captures() -> Value {
    serde_json::from_str(include_str!(
        "../../../../reviews/20261027/python-output-captures.json"
    ))
    .unwrap()
}

fn assert_capture(plan: &TextPlan, name: &str) {
    let captures = captures();
    let expected = captures["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["name"] == name)
        .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        expected["stdout"].as_str().unwrap(),
        "{name}"
    );
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        expected["stderr"].as_str().unwrap(),
        "{name}"
    );
    assert_eq!(u64::from(plan.exit_code), expected["exit"], "{name}");
}

fn dispatch(
    args: &[&str],
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
) -> TextPlan {
    let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
    let request_count = replies.len();
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse(captures()["timestamp"].as_str().unwrap()).unwrap(),
        config: ReadConfig::default(),
    };
    let plan = match invocation {
        Invocation::Read(command) => complete(reads.dispatch(&command, cancelled)),
        Invocation::JobRead(command) => complete(
            JobReadSession {
                reads: &reads,
                jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
            }
            .dispatch(&command, cancelled),
        ),
        _ => panic!("read or job read required"),
    }
    .unwrap_or_else(|error| panic!("dispatch {args:?}: {error:?}"));
    http.assert_consumed();
    assert_eq!(http.requests.lock().unwrap().len(), request_count);
    let scopes = credential.scopes.lock().unwrap();
    assert_eq!(scopes.len(), request_count);
    for scope in scopes.iter() {
        assert_eq!(scope, &["https://api.fabric.microsoft.com/.default"]);
    }
    plan
}

fn get_replies() -> Vec<Reply> {
    vec![
        folder_support::reply(
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        ),
        folder_support::reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
        folder_support::reply(json!({
            "id":"n", "displayName":"Book", "parts":[], "settings":{},
            "description":"", "enabled":false, "nullable":null, "zero":0,
            "title":"hello", "values":["part"], "details":{"id":"part"}
        })),
    ]
}

#[test]
fn reads_and_jobs_cancel_as_errors_in_text_and_retain_json_bytes() {
    for (args, name) in [
        (vec!["get", "Demo.Workspace"], "read"),
        (
            vec!["job", "run-list", "Demo.Workspace/Book.Notebook"],
            "job",
        ),
    ] {
        for format in ["text", "json"] {
            let mut invocation = args.clone();
            invocation.extend(["--output_format", format]);
            let plan = dispatch(&invocation, Vec::new(), ready(()));
            assert_capture(&plan, &format!("{name}-{format}-cancel"));
        }
    }
}

#[test]
fn service_errors_use_the_error_marker_and_unauthorized_exit_four() {
    for (args, name) in [
        (vec!["ls", "/"], "ls-text-service"),
        (vec!["get", "Demo.Workspace"], "get-text-service"),
        (
            vec!["job", "run-list", "Demo.Workspace/Book.Notebook"],
            "job-text-service",
        ),
    ] {
        let mut invocation = args;
        invocation.extend(["--output_format", "text"]);
        let plan = dispatch(
            &invocation,
            vec![Reply {
                status: 401,
                body: json!({"errorCode":"Unauthorized","message":"private"}),
                headers: Default::default(),
            }],
            pending(),
        );
        assert_capture(&plan, name);
    }
}

#[test]
fn query_errors_use_the_error_adapter_not_the_success_renderer() {
    let plan = dispatch(
        &["ls", "/", "-q", "[", "--output_format", "text"],
        vec![folder_support::reply(
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        )],
        pending(),
    );
    assert_capture(&plan, "ls-text-query-error");
}

#[test]
fn read_success_and_empty_list_retain_source_bytes() {
    let plan = dispatch(
        &["ls", "/", "--output_format", "text"],
        vec![folder_support::reply(
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        )],
        pending(),
    );
    assert_capture(&plan, "ls-text");
    let plan = dispatch(
        &["ls", "/", "--output_format", "json"],
        vec![folder_support::reply(json!({"value":[]}))],
        pending(),
    );
    assert_capture(&plan, "ls-empty");
}

#[test]
fn empty_get_projection_is_silent_at_the_actual_command_boundary() {
    let plan = dispatch(
        &[
            "get",
            "Demo.Workspace/Book.Notebook",
            "-q",
            "parts",
            "--output_format",
            "json",
        ],
        vec![
            folder_support::reply(
                json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
            ),
            folder_support::reply(
                json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}),
            ),
            folder_support::reply(json!({"id":"n","displayName":"Book","parts":[]})),
        ],
        pending(),
    );
    assert_capture(&plan, "get-query-empty-source");
}

#[test]
fn get_empty_containers_and_string_are_silent_in_both_formats() {
    for format in ["text", "json"] {
        for expression in ["parts", "settings", "description"] {
            let plan = dispatch(
                &[
                    "get",
                    "Demo.Workspace/Book.Notebook",
                    "-q",
                    expression,
                    "--output_format",
                    format,
                ],
                get_replies(),
                pending(),
            );
            assert_capture(&plan, "get-query-empty-source");
            assert!(plan.records.is_empty());
            plan.write_to(
                &mut family_support::BrokenWriter,
                &mut family_support::BrokenWriter,
            )
            .unwrap();
        }
    }
}

#[test]
fn get_false_null_missing_zero_and_nonempty_values_remain_visible_after_query() {
    for (expression, expected) in [
        ("enabled", "False"),
        ("nullable", "None"),
        ("missing", "None"),
        ("zero", "0"),
        ("title", "hello"),
        ("values", "part"),
    ] {
        for format in ["text", "json"] {
            let plan = dispatch(
                &[
                    "get",
                    "Demo.Workspace/Book.Notebook",
                    "-q",
                    expression,
                    "--output_format",
                    format,
                ],
                get_replies(),
                pending(),
            );
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            plan.write_to(&mut stdout, &mut stderr).unwrap();
            assert_eq!(plan.exit_code, 0);
            assert!(stderr.is_empty());
            if format == "json" {
                let expected_json = format!(
                    "{{\n    \"timestamp\": \"2026-01-02T03:04:05.000006Z\",\n    \"status\": \"Success\",\n    \"command\": \"get\",\n    \"result\": {{\n        \"data\": [\n            \"{expected}\"\n        ]\n    }}\n}}\n"
                );
                assert_eq!(stdout, expected_json.as_bytes(), "{expression}/{format}");
                if expected == "False" {
                    assert_capture(&plan, "get-query-false-source");
                } else if expected == "None" {
                    assert_capture(&plan, "get-scalar-null");
                }
            } else {
                assert_eq!(
                    stdout,
                    format!("{expected}\n").as_bytes(),
                    "{expression}/{format}"
                );
            }
            assert!(
                plan.write_to(&mut family_support::BrokenWriter, &mut stderr)
                    .is_err()
            );
            assert!(stderr.is_empty());
        }
    }
}

#[test]
fn get_object_projection_uses_source_raw_json_text_not_key_value_rows() {
    for format in ["text", "json"] {
        let plan = dispatch(
            &[
                "get",
                "Demo.Workspace/Book.Notebook",
                "-q",
                "details",
                "--output_format",
                format,
            ],
            get_replies(),
            pending(),
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        plan.write_to(&mut stdout, &mut stderr).unwrap();
        assert_eq!(plan.exit_code, 0, "details/{format}");
        assert!(stderr.is_empty(), "details/{format}: stderr={stderr:?}");
        let expected = if format == "text" {
            "{\n  \"id\": \"part\"\n}\n"
        } else {
            "{\n    \"timestamp\": \"2026-01-02T03:04:05.000006Z\",\n    \"status\": \"Success\",\n    \"command\": \"get\",\n    \"result\": {\n        \"data\": [\n            {\n                \"id\": \"part\"\n            }\n        ]\n    }\n}\n"
        };
        assert_eq!(stdout, expected.as_bytes(), "details/{format}");
    }
}

#[test]
fn empty_ls_query_still_has_data_and_empty_jobs_still_report_no_runs() {
    let ls = dispatch(
        &["ls", "/", "-q", "[].missing", "--output_format", "json"],
        vec![folder_support::reply(
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        )],
        pending(),
    );
    assert_capture(&ls, "ls-empty");
    for format in ["text", "json"] {
        let job = dispatch(
            &[
                "job",
                "run-list",
                "Demo.Workspace/Book.Notebook",
                "--output_format",
                format,
            ],
            vec![
                folder_support::reply(
                    json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
                ),
                folder_support::reply(
                    json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}),
                ),
                folder_support::reply(json!({"value":[]})),
            ],
            pending(),
        );
        if format == "json" {
            assert_capture(&job, "job-empty");
        } else {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            job.write_to(&mut stdout, &mut stderr).unwrap();
            assert_eq!(job.exit_code, 0);
            assert_eq!(stdout, b"\n* No runs found\n\n");
            assert!(stderr.is_empty());
        }
    }
}

#[test]
fn get_query_and_endpoint_failures_are_never_silenced_or_presented_as_success() {
    for format in ["text", "json"] {
        for service_failure in [false, true] {
            let mut replies = get_replies();
            if service_failure {
                *replies.last_mut().unwrap() = Reply {
                    status: 401,
                    body: json!({"errorCode":"Unauthorized","message":"private"}),
                    headers: Default::default(),
                };
            }
            let plan = dispatch(
                &[
                    "get",
                    "Demo.Workspace/Book.Notebook",
                    "-q",
                    if service_failure { "parts" } else { "[" },
                    "--output_format",
                    format,
                ],
                replies,
                pending(),
            );
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            plan.write_to(&mut stdout, &mut stderr).unwrap();
            let (code, message, exit) = if service_failure {
                ("Unauthorized", "Access is unauthorized", 4)
            } else {
                (
                    "InvalidInput",
                    "Invalid jmespath query (https://jmespath.org)",
                    1,
                )
            };
            assert_eq!(plan.exit_code, exit);
            assert!(stderr.is_empty());
            if format == "json" {
                assert_eq!(
                    serde_json::from_slice::<Value>(&stdout).unwrap(),
                    json!({
                        "timestamp":"2026-01-02T03:04:05.000006Z",
                        "status":"Failure", "command":"get",
                        "result":{"error_code":code,"message":message},
                    })
                );
            } else {
                assert_eq!(stdout, format!("x get: [{code}] {message}\n").as_bytes());
            }
        }
    }
}

#[tokio::test]
async fn table_cancellation_text_is_an_error_and_json_remains_a_failure() {
    for format in ["text", "json"] {
        let mut host = family_support::Harness::new();
        let output = host
            .execute_with_cancelled(
                &[
                    "table",
                    "optimize",
                    "Demo.Workspace/Book.Lakehouse/Tables/sales",
                    "--output_format",
                    format,
                ],
                ready(()),
            )
            .await;
        assert_capture(&output.plan, &format!("table-{format}-cancel"));
        assert!(host.requests().is_empty());
    }
}

#[test]
fn job_endpoint_failure_after_metadata_retains_full_error_path() {
    let plan = dispatch(
        &[
            "job",
            "run-list",
            "Demo.Workspace/Book.Notebook",
            "--output_format",
            "text",
        ],
        vec![
            folder_support::reply(
                json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
            ),
            folder_support::reply(
                json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}),
            ),
            Reply {
                status: 401,
                body: json!({"errorCode":"Unauthorized","message":"private"}),
                headers: Default::default(),
            },
        ],
        pending(),
    );
    assert_capture(&plan, "job-text-service");
}

#[tokio::test]
async fn family_predispatch_failure_uses_error_text_without_mutation() {
    let mut host = family_support::Harness::new();
    host.reply(401, json!({"errorCode":"Unauthorized","message":"private"}));
    let output = host
        .execute(&["exists", "Demo.Workspace", "--output_format", "text"])
        .await;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 4);
    assert_eq!(stdout, b"x exists: [Unauthorized] Access is unauthorized\n");
    assert!(stderr.is_empty());
    assert_eq!(host.requests().len(), 1);
    assert_eq!(host.requests()[0]["method"], "GET");
}

#[tokio::test]
async fn resource_failure_retains_typed_evidence_and_does_not_replay_on_writer_failure() {
    let mut host = family_support::Harness::new();
    host.metadata(json!([]));
    host.reply(401, json!({"errorCode":"Unauthorized","message":"private"}));
    let output = host
        .execute(&[
            "mkdir",
            "Demo.Workspace/Book.Notebook",
            "--output_format",
            "text",
        ])
        .await;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 4);
    assert_eq!(stdout, b"x mkdir: [Unauthorized] Access is unauthorized\n");
    assert!(stderr.is_empty());
    assert!(matches!(
        &output.evidence,
        fabric_cli::families::FamilyEvidence::Resource(Err(_))
    ));
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["method"], "POST");
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut stderr)
            .is_err()
    );
    assert!(stderr.is_empty());
    assert_eq!(host.requests(), requests);
}

#[tokio::test]
async fn table_schema_failure_keeps_prior_stderr_diagnostics_before_error_text() {
    let mut host = family_support::Harness::new();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
    host.reply(200, json!({"paths":[]}));
    let output = host
        .execute(&[
            "table",
            "schema",
            "Demo.Workspace/Book.Lakehouse/Tables/sales",
            "--output_format",
            "text",
        ])
        .await;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
    let expected = captures();
    let expected = expected["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["name"] == "table-text-error")
        .unwrap();
    assert_eq!(stdout, expected["stdout"].as_str().unwrap().as_bytes());
    assert_eq!(stderr, b"Getting schema for 'sales' table...\n");
    assert_eq!(
        output.plan.records[0].stream,
        fabric_cli_output::OutputStream::Stderr,
    );
    assert_eq!(
        output.plan.records.last().unwrap().stream,
        fabric_cli_output::OutputStream::Stdout,
    );
    assert_eq!(host.requests().len(), 4);
}
