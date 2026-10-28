// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root argv -> real session -> shared HTTP, against actual unchanged Python output.

#[allow(dead_code)]
mod job_execution_support;

use job_execution_support::{metadata, observe};
use serde_json::{Value, json};

#[test]
fn all_eight_leaves_match_the_additive_frozen_source_command_observations() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/command_source.json"
    ))
    .unwrap();
    assert_eq!(
        corpus["source_commit"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    let rows = corpus["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 210);
    for row in rows {
        let case = &row["input"];
        let resource = case["resource"].as_str().unwrap();
        let leaf = case["leaf"].as_str().unwrap();
        let mut command = vec![
            "job".into(),
            leaf.into(),
            format!("Demo.Workspace/Book.{resource}"),
        ];
        if matches!(leaf, "run-status" | "run-list") {
            if leaf == "run-status" {
                command.push("--id=run".into());
            }
            if case["schedule"] == true {
                command.push("--schedule".into());
            }
        } else if let Some(options) = case["options"].as_array() {
            command.extend(options.iter().map(|value| value.as_str().unwrap().into()));
        }
        command.extend([
            "--output_format".into(),
            row["format"].as_str().unwrap().into(),
        ]);
        let mut responses = metadata(resource);
        if let Some(replies) = case["responses"].as_array() {
            responses.extend(replies.clone());
        } else {
            responses.push(case["response"].clone());
        }
        let input = json!({
            "command": command, "responses": responses,
            "observe_confirmation": row["observed"].get("confirmations").is_some(),
        });
        assert_eq!(observe(&input), row["observed"], "{}", case["name"]);
    }
}

fn invocation(leaf: &str) -> Vec<String> {
    let mut args = vec![
        "job".into(),
        leaf.into(),
        "Demo.Workspace/Book.Notebook".into(),
    ];
    match leaf {
        "run-cancel" | "run-status" | "run-update" | "run-rm" => args.push("--id=run".into()),
        _ => {}
    }
    match leaf {
        "run-update" => args.push("--enable".into()),
        "run-sch" => args.push("--input={}".into()),
        "run-rm" => args.push("--force".into()),
        _ => {}
    }
    args
}

#[test]
fn all_leaves_preserve_unauthorized_failures_and_pre_io_cancellation() {
    for leaf in [
        "start",
        "run",
        "run-list",
        "run-status",
        "run-cancel",
        "run-sch",
        "run-update",
        "run-rm",
    ] {
        let command = invocation(leaf);
        let cancelled = observe(&json!({"command":command,"responses":[],"cancelled":true}));
        assert_eq!(cancelled["exit"], 1, "{leaf}");
        assert_eq!(cancelled["requests"], json!([]), "{leaf}");
        let mut responses = metadata("Notebook");
        responses.push(json!({"status":401,"body":{}}));
        let failed = observe(&json!({"command":command,"responses":responses}));
        assert_eq!(failed["exit"], 4, "{leaf}");
        assert_eq!(failed["requests"].as_array().unwrap().len(), 3, "{leaf}");
    }
}

#[test]
fn missing_fields_fail_only_where_source_consumes_them_and_never_claim_completion() {
    for (leaf, response, exit) in [
        ("start", json!({"status":202,"body":null}), 1),
        ("run", json!({"status":202,"body":null}), 1),
        ("run-status", json!({"status":200,"body":null}), 0),
        ("run-list", json!({"status":200,"body":{}}), 1),
        ("run-sch", json!({"status":201,"body":{}}), 1),
        ("run-update", json!({"status":200,"body":{}}), 1),
        (
            "run-cancel",
            json!({"status":200,"body":"ignored non-JSON"}),
            0,
        ),
        ("run-rm", json!({"status":200,"body":"ignored non-JSON"}), 0),
    ] {
        let mut responses = metadata("Notebook");
        responses.push(response);
        let row = observe(&json!({"command":invocation(leaf),"responses":responses}));
        assert_eq!(row["exit"], exit, "{leaf}");
        assert!(
            !row["stdout"].as_str().unwrap().contains("completed"),
            "{leaf}"
        );
    }
}

#[test]
fn permissive_poll_status_and_non200_poll_preserve_source_control_flow() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/poll_source.json"
    ))
    .unwrap();
    assert_eq!(corpus["rows"].as_array().unwrap().len(), 10);
    for row in corpus["rows"].as_array().unwrap() {
        let mut command = invocation("run");
        command.extend([
            "--output_format".into(),
            row["format"].as_str().unwrap().into(),
        ]);
        let mut responses = metadata("Notebook");
        responses.extend(row["input"]["responses"].as_array().unwrap().clone());
        let observed = observe(&json!({"command":command,"responses":responses}));
        assert_eq!(observed, row["observed"], "{}", row["input"]["name"]);
    }
}
