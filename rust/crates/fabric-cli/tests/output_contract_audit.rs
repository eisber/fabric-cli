// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parsed commands -> real production sessions -> streams, with offline providers.

#![cfg(feature = "native-read")]

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
        "../../../reviews/20261027/python-output-captures.json"
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
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential);
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
        Invocation::Read(command) => complete(reads.dispatch(&command, cancelled)).unwrap(),
        Invocation::JobRead(command) => complete(
            JobReadSession {
                reads: &reads,
                jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
            }
            .dispatch(&command, cancelled),
        )
        .unwrap(),
        _ => panic!("read or job read required"),
    };
    http.assert_consumed();
    plan
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
