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
