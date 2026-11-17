// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

#[path = "../../../../engine/data-plane/aihub-query/examples/support/mod.rs"]
mod support;

use super::*;
use crate::parser::{Invocation, parse_args};
use fabric_cli_query_commands::{DataAgentRoute, InputSource, Stage};
use fabric_sdk_aihub_query::{Limits, NativeValue, QueryErrorKind};
use serde_json::json;
use std::{
    future::{pending, ready},
    io,
    sync::{Arc, atomic::Ordering},
};
use support::*;

const PATH: &str = "/fixture.Workspace/agent.DataAgent/datasource";
const WORKSPACE: &str = "55555555-5555-4555-8555-555555555555";
const EXECUTION: &str = "66666666-6666-4666-8666-666666666666";

fn command(extra: &[&str]) -> NativeCommand {
    let Invocation::NativeTabular(command) = parse_args(
        ["fab-rs", "query", PATH]
            .into_iter()
            .chain(extra.iter().copied()),
    )
    .unwrap() else {
        panic!("real root query parser did not select NativeTabular")
    };
    command
}

fn bound() -> BoundTarget {
    let agent = Artifact {
        workspace: WORKSPACE.into(),
        item: AGENT.into(),
    };
    let artifact = Artifact {
        workspace: WORKSPACE.into(),
        item: EXECUTION.into(),
    };
    BoundTarget {
        path: PATH.into(),
        target: QueryTarget {
            requested_artifact: agent.clone(),
            execution_artifact: artifact.clone(),
            data_agent: Some(DataAgentRoute {
                engine: "advertised-agent".into(),
                agent,
                datasource: SOURCE.into(),
                bound_artifact: artifact,
                stage: Stage::Production,
            }),
            native_kql: Vec::new(),
        },
    }
}

fn policy() -> QueryPolicy {
    QueryPolicy {
        request_id: "query;root-fixture".into(),
        server_timeout: Duration::from_secs(5),
        kql_limits: QueryLimits::new(1024 * 1024, 1024 * 1024, 100, 8, 32, 32).unwrap(),
        aihub_max_rows: 100,
    }
}

#[derive(Default)]
struct Inputs {
    bytes: Vec<u8>,
    reads: usize,
    stdin: bool,
}

impl InputReader for Inputs {
    fn read(&mut self, source: InputSource<'_>, limit: usize) -> io::Result<Vec<u8>> {
        self.reads += 1;
        self.stdin = matches!(source, InputSource::Stdin);
        assert_eq!(limit, 1024 * 1024);
        Ok(std::mem::take(&mut self.bytes))
    }
}

#[derive(Default)]
struct Output {
    preflights: usize,
    publishes: usize,
    fail_preflight: bool,
}

impl ExportSink for Output {
    fn preflight(&mut self, _: &ExportRequest) -> io::Result<()> {
        self.preflights += 1;
        if self.fail_preflight {
            return Err(io::Error::other("private output path"));
        }
        Ok(())
    }

    fn publish(&mut self, _: &ExportRequest, _: &[u8]) -> io::Result<()> {
        self.publishes += 1;
        Ok(())
    }
}

#[tokio::test]
async fn real_root_query_without_export_executes_actual_sdk_and_retains_native_data() {
    let command = command(&["--text", "opaque inline text"]);
    assert!(command.export.is_none());
    let wire = Wire::new(setup().into_iter().chain([rpc(3, success())]));
    let credential = Arc::new(Credential::default());
    let client = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let mut inputs = Inputs::default();
    let mut output = Output::default();
    let prepared = prepare(
        &command,
        bound(),
        Providers {
            aihub: Some(&client),
            kql: None,
        },
        policy(),
        &mut inputs,
        &mut output,
    )
    .unwrap();
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(inputs.reads, 0);
    assert_eq!(output.preflights, 0);
    let observed = prepared.execute(&deadline(), pending()).await;
    assert_eq!(observed.identity.requested_artifact.item, AGENT);
    assert_eq!(observed.identity.execution_artifact.item, EXECUTION);
    assert_eq!(observed.identity.engine, "advertised-agent");
    let NativeQueryResult::AiHub(result) = observed.result.unwrap() else {
        panic!("wrong provider");
    };
    assert_eq!(
        result.resultsets[0].rows[0][0],
        NativeValue::Integer("18446744073709551615".into())
    );
    assert_eq!(result.resultsets[0].columns[0].name, "value");
    assert_eq!(result.resultsets[0].columns[1].name, "value");
    assert_eq!(
        messages(&wire)[3]["params"]["arguments"]["query"],
        "opaque inline text"
    );
}

#[tokio::test]
async fn actual_root_file_stdin_and_explicit_engine_are_preserved() {
    for path in ["-", "private-statement.kql"] {
        let command = command(&["--file", path, "--engine", "advertised-agent"]);
        let wire = Wire::new(setup().into_iter().chain([rpc(3, success())]));
        let client = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        );
        let mut inputs = Inputs {
            bytes: b"exact\r\nstatement".to_vec(),
            ..Inputs::default()
        };
        let observed = prepare(
            &command,
            bound(),
            Providers {
                aihub: Some(&client),
                kql: None,
            },
            policy(),
            &mut inputs,
            &mut Output::default(),
        )
        .unwrap()
        .execute(&deadline(), pending())
        .await;
        assert!(observed.result.is_ok());
        assert_eq!(inputs.reads, 1);
        assert_eq!(inputs.stdin, path == "-");
        assert_eq!(
            messages(&wire)[3]["params"]["arguments"]["query"],
            "exact\r\nstatement"
        );
    }
}

#[test]
fn actual_root_unsupported_parameters_reject_before_input_staging_or_credentials() {
    let command = command(&["--file", "private.query", "--parameters", "private.json"]);
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let client = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let mut inputs = Inputs::default();
    let mut output = Output::default();
    let result = prepare(
        &command,
        bound(),
        Providers {
            aihub: Some(&client),
            kql: None,
        },
        policy(),
        &mut inputs,
        &mut output,
    );
    assert!(matches!(
        result,
        Err(QueryHostError::Preparation(
            PreparationError::UnsupportedParameters
        ))
    ));
    assert_eq!(inputs.reads, 0);
    assert_eq!(output.preflights, 0);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn actual_root_no_clobber_preflight_precedes_query_dispatch_and_stdin() {
    let command = command(&[
        "--file",
        "-",
        "--format",
        "parquet",
        "--output",
        "existing.csv",
    ]);
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let client = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let mut inputs = Inputs::default();
    let mut output = Output {
        fail_preflight: true,
        ..Output::default()
    };
    assert!(matches!(
        prepare(
            &command,
            bound(),
            Providers {
                aihub: Some(&client),
                kql: None,
            },
            policy(),
            &mut inputs,
            &mut output,
        ),
        Err(QueryHostError::OutputPreflight)
    ));
    assert_eq!(output.preflights, 1);
    assert_eq!(output.publishes, 0);
    assert_eq!(inputs.reads, 0);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn wrong_path_snapshot_and_unadvertised_engine_fail_before_input() {
    for wrong_path in [true, false] {
        let command = command(&["--text", "query", "--engine", "advertised-agent"]);
        let mut target = bound();
        if wrong_path {
            target.path = "/another/path".into();
        } else {
            target.target.data_agent.as_mut().unwrap().engine = "another-engine".into();
        }
        let mut inputs = Inputs::default();
        let mut output = Output::default();
        assert!(
            prepare(
                &command,
                target,
                Providers {
                    aihub: None,
                    kql: None
                },
                policy(),
                &mut inputs,
                &mut output,
            )
            .is_err()
        );
        assert_eq!(inputs.reads, 0);
        assert_eq!(output.preflights, 0);
    }
}

#[test]
fn real_parser_rejects_competing_stdin_and_unapproved_selector_flags() {
    for options in [
        vec!["--file", "-", "--parameters", "-"],
        vec!["--text", "query", "--provider", "aihub"],
        vec!["--text", "query", "--resultset", "0"],
        vec!["--text", "query", "--language", "jmespath"],
    ] {
        assert!(parse_args(["fab-rs", "query", PATH].into_iter().chain(options)).is_err());
    }
}

#[tokio::test]
async fn truncated_or_missing_typed_results_retain_identity_without_success() {
    for truncated in [true, false] {
        let command = command(&["--text", "query"]);
        let result = if truncated {
            let mut rows: serde_json::Value = serde_json::from_str(&rowset()).unwrap();
            rows["truncated"] = json!(true);
            json!({"content":[typed(&rows.to_string(), "file:///truncated.json")]})
        } else {
            json!({"content":[{"type":"resource","resource":{
                "uri":"file:///legacy.csv","mimeType":"text/csv","text":"n\n1"}}]})
        };
        let wire = Wire::new(setup().into_iter().chain([rpc(3, result)]));
        let client = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        );
        let observed = prepare(
            &command,
            bound(),
            Providers {
                aihub: Some(&client),
                kql: None,
            },
            policy(),
            &mut Inputs::default(),
            &mut Output::default(),
        )
        .unwrap()
        .execute(&deadline(), pending())
        .await;
        assert_eq!(observed.identity.requested_artifact.item, AGENT);
        if truncated {
            assert!(matches!(
                observed.result,
                Err(QueryHostError::IncompleteAiHub(_))
            ));
        } else {
            assert!(matches!(observed.result, Err(QueryHostError::AiHub(error))
                if error.kind == QueryErrorKind::MissingTypedJson && error.query_dispatched));
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn local_cancel_retains_exact_target_and_does_not_dispatch() {
    let command = command(&["--text", "query"]);
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let client = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let observed = prepare(
        &command,
        bound(),
        Providers {
            aihub: Some(&client),
            kql: None,
        },
        policy(),
        &mut Inputs::default(),
        &mut Output::default(),
    )
    .unwrap()
    .execute(&deadline(), ready(()))
    .await;
    assert_eq!(observed.identity.execution_artifact.item, EXECUTION);
    assert!(matches!(observed.result, Err(QueryHostError::AiHub(error))
        if error.kind == QueryErrorKind::Canceled && !error.query_dispatched));
    assert!(wire.requests.lock().unwrap().is_empty());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}
