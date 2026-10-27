// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual root argv -> production TABLE session -> shared HTTP/OneLake/jobs.

#![cfg(feature = "native-read")]

mod family_support;
use fabric_cli::{
    families::FamilyEvidence,
    parser::{Invocation, parse_args},
};
use family_support::Harness;
use serde_json::{Value, json};

const TABLE: &str = "Demo.Workspace/Book.Lakehouse/Tables/sales";
const FILE: &str = "Demo.Workspace/Book.Lakehouse/Files/input.csv";
const RUN: &str = "https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances/run";
const OP: &str = "22222222-2222-2222-2222-222222222222";

fn target(host: &Harness) {
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
}

fn streams(output: &fabric_cli::families::FamilyOutput, exit: u8) -> (String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut out, &mut err).unwrap(), exit);
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn all_four_root_leaves_select_real_table_invocations_and_inherited_output() {
    for leaf in ["schema", "load", "vacuum", "optimize"] {
        let mut args = vec!["fab-rs", "--output_format", "json", "table", leaf, TABLE];
        if leaf == "load" {
            args.extend(["--file", FILE]);
        }
        assert!(
            matches!(parse_args(args).unwrap(), Invocation::Table(command)
            if command.leaf_name() == format!("table {leaf}")
            && command.output_format.as_deref() == Some("json"))
        );
    }
}

#[tokio::test]
async fn root_table_schema_reads_actual_metadata_head_delta_log_and_source_output() {
    let mut host = Harness::new();
    target(&host);
    host.reply(
        200,
        json!({"paths":[{"name":"n/Tables/sales/_delta_log/00001.json"}]}),
    );
    host.reply(200, json!({"metaData":{"schemaString":"{\"fields\":[{\"name\":\"id\",\"type\":\"long\",\"nullable\":false,\"metadata\":{}}]}"}}));
    let output = host.execute(&["table", "schema", TABLE]).await;
    let (out, err) = streams(&output, 0);
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["command"], "table");
    assert_eq!(
        value["result"]["data"],
        json!([{"name":"id","type":"long","nullable":false,"metadata":{}}])
    );
    assert_eq!(
        err,
        "Getting schema for 'sales' table...\nSchema extracted successfully\n"
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[2]["method"], "HEAD");
    assert!(
        requests[3]["url"]
            .as_str()
            .unwrap()
            .contains("directory=n%2FTables%2Fsales%2F_delta_log")
    );
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with("/w/n/Tables/sales/_delta_log/00001.json")
    );
    assert!(matches!(&output.evidence, FamilyEvidence::Table(evidence) if evidence.result.is_ok()));
}

#[tokio::test]
async fn root_table_load_uses_real_source_head_post_shared_operation_and_result() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.headers(202, Value::Null, &[("x-ms-operation-id", OP)]);
    host.headers(
        200,
        json!({"status":"Succeeded"}),
        &[(
            "Location",
            &format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
        )],
    );
    host.reply(200, json!({}));
    let output = host
        .execute(&["table", "load", TABLE, "--file", FILE])
        .await;
    let (out, err) = streams(&output, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["result"]["message"],
        "Table 'sales' loaded successfully"
    );
    assert_eq!(err, "Loading 'sales' table. It may take some time...\n");
    let requests = host.requests();
    assert_eq!(requests.len(), 8);
    assert_eq!(requests[2]["method"], "HEAD");
    assert_eq!(requests[3]["method"], "HEAD");
    assert_eq!(requests[4]["method"], "HEAD");
    assert_eq!(requests[5]["method"], "POST");
    assert_eq!(
        requests[5]["body"],
        json!({
            "pathType":"File","relativePath":"Files/input.csv","mode":"Overwrite",
            "formatOptions":{"format":"Csv"},
        })
    );
    assert!(
        requests[5]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/lakehouses/n/tables/sales/load")
    );
    assert!(
        requests[6]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/operations/{OP}"))
    );
    assert!(
        requests[7]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/operations/{OP}/result"))
    );
    let scopes = host.credential.0.lock().unwrap();
    assert!(
        scopes
            .iter()
            .any(|scope| scope == &[fabric_sdk::Audience::OneLake.scope()])
    );
    assert!(
        scopes
            .iter()
            .any(|scope| scope == &[fabric_sdk::Audience::Fabric.scope()])
    );
}

async fn maintenance(leaf: &str, extra: &[&str], expected: Value) {
    let mut host = Harness::new();
    target(&host);
    host.headers(202, Value::Null, &[("Location", RUN)]);
    host.reply(200, json!({"id":"run","status":"Completed"}));
    let mut args = vec!["table", leaf, TABLE];
    args.extend(extra);
    let output = host.execute(&args).await;
    let (out, err) = streams(&output, 0);
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["command"], format!("table {leaf}"));
    assert_eq!(value["result"]["data"], json!([{"id":"run"}]));
    assert_eq!(value["result"]["message"], "Job instance 'run' completed");
    assert!(err.starts_with(if leaf == "vacuum" {
        "Vacuuming"
    } else {
        "Optimizing"
    }));
    assert!(err.contains("Job instance 'run' created"));
    assert!(err.contains("Job instance status: Completed"));
    assert!(err.ends_with('\n'));
    let requests = host.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests[3]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/items/n/jobs/instances?jobType=TableMaintenance")
    );
    assert_eq!(requests[3]["body"], json!({"executionData": expected}));
    assert_eq!(requests[4]["url"], RUN);
    assert_eq!(requests[4]["method"], "GET");
}

#[tokio::test]
async fn root_table_vacuum_uses_real_typed_job_submission_status_and_progress() {
    maintenance(
        "vacuum",
        &["--retain_n_hours", "24"],
        json!({
            "tableName":"sales","vacuumSettings":{"retentionPeriod":"1:00:00:00"},
        }),
    )
    .await;
}

#[tokio::test]
async fn root_table_optimize_uses_real_typed_job_submission_status_and_progress() {
    maintenance(
        "optimize",
        &["--vorder", "--zorder", "id, region"],
        json!({
            "tableName":"sales","optimizeSettings":{"vOrder":true,"zOrderBy":["id","region"]},
        }),
    )
    .await;
}

#[tokio::test]
async fn root_table_bare_acceptance_is_started_not_completed_and_writer_failure_never_replays() {
    let mut host = Harness::new();
    target(&host);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.reply(202, Value::Null);
    let output = host
        .execute(&["table", "load", TABLE, "--file", FILE])
        .await;
    let out = family_support::success(&output);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["result"]["message"],
        "Load table operation started"
    );
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn root_table_failed_shared_operation_retains_original_id_uncertainty_and_nonzero_exit() {
    let mut host = Harness::new();
    target(&host);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.headers(202, Value::Null, &[("x-ms-operation-id", OP)]);
    host.reply(
        200,
        json!({"status":"Failed","error":{"errorCode":"Denied","message":"Rejected"}}),
    );
    let output = host
        .execute(&["table", "load", TABLE, "--file", FILE])
        .await;
    let (out, _) = streams(&output, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["status"],
        "Failure"
    );
    match &output.evidence {
        FamilyEvidence::Table(evidence) => match &evidence.result {
            Err(fabric_cli_tables::TableFailure::Sdk(error)) => {
                assert_eq!(error.operation_id.as_deref(), Some(OP));
                assert!(error.mutation_may_have_occurred);
            }
            other => panic!("missing original failure: {other:?}"),
        },
        other => panic!("wrong family evidence: {other:?}"),
    }
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn root_table_load_literal_wire_cases_match_independent_pinned_source_fixtures() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/tables/wire.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let mut host = Harness::new();
        target(&host);
        let kind = case["source_kind"].as_str().unwrap();
        host.headers(200, Value::Null, &[("x-ms-resource-type", kind)]);
        host.headers(200, Value::Null, &[("x-ms-resource-type", kind)]);
        host.reply(200, Value::Null);
        let source = format!(
            "Demo.Workspace/Book.Lakehouse/{}",
            case["source_path"].as_str().unwrap_or("Files/input.csv"),
        );
        let mut args = vec![
            "table".to_owned(),
            "load".to_owned(),
            TABLE.to_owned(),
            "--file".to_owned(),
            source,
        ];
        for key in ["mode", "format", "extension"] {
            if let Some(value) = case[key].as_str() {
                args.push(format!("--{key}={value}"));
            }
        }
        let output = host
            .execute(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .await;
        streams(&output, 0);
        let requests = host.requests();
        assert_eq!(requests.len(), 6, "{}", case["id"]);
        assert_eq!(
            requests[5]["raw_body"], case["expected_body"],
            "{}",
            case["id"]
        );
        assert_eq!(requests[5]["method"], "POST");
    }
}

#[tokio::test]
async fn root_table_schema_text_uses_shared_headers_and_keeps_progress_on_stderr() {
    let mut host = Harness::new();
    target(&host);
    host.reply(
        200,
        json!({"paths":[{"name":"n/Tables/sales/_delta_log/00001.json"}]}),
    );
    host.reply(200, json!({"metaData":{"schemaString":"{\"fields\":[{\"name\":\"id\",\"type\":\"long\",\"nullable\":false}]}"}}));
    let output = host
        .execute(&["table", "schema", TABLE, "--output_format", "text"])
        .await;
    let (out, err) = streams(&output, 0);
    assert_eq!(
        out.lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["name", "type", "nullable"],
    );
    assert!(out.contains("id") && out.contains("long"));
    assert!(!out.contains("Getting schema"));
    assert_eq!(
        err,
        "Getting schema for 'sales' table...\nSchema extracted successfully\n"
    );
}

#[tokio::test]
async fn root_table_schema_rejects_wide_integers_after_source_extraction_without_coercion() {
    let mut host = Harness::new();
    target(&host);
    host.reply(
        200,
        json!({"paths":[{"name":"n/Tables/sales/_delta_log/00001.json"}]}),
    );
    host.reply(200, json!({"metaData":{"schemaString":"{\"fields\":[{\"metadata\":{\"wide\":18446744073709551616}}]}"}}));
    let output = host.execute(&["table", "schema", TABLE]).await;
    let (out, err) = streams(&output, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["result"]["error_code"],
        "InvalidJson"
    );
    assert!(err.ends_with("Schema extracted successfully\n"));
}

#[tokio::test]
async fn root_table_vacuum_malformed_integer_is_explicit_and_never_submits() {
    let mut host = Harness::new();
    target(&host);
    let output = host
        .execute(&["table", "vacuum", TABLE, "--retain_n_hours", "1.5"])
        .await;
    let (out, err) = streams(&output, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["result"]["error_code"],
        "UnexpectedError"
    );
    assert!(err.is_empty());
    assert_eq!(host.requests().len(), 3);
}

#[tokio::test]
async fn root_table_local_cancellation_has_no_http_mutation_or_success() {
    let mut host = Harness::new();
    let output = host
        .execute_with_cancelled(&["table", "optimize", TABLE], std::future::ready(()))
        .await;
    let (out, err) = streams(&output, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    assert!(err.is_empty());
    assert!(host.requests().is_empty());
}
