// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Production job submission/run contracts with synthetic HTTP, credentials, files and UI.

mod job_execution_support;

use fabric_cli::{
    jobs::SelectedJobFiles,
    parser::{Invocation, parse_args},
};
use fabric_cli_jobs::JsonInput;
use job_execution_support::{accepted, metadata, observe};
use serde_json::{Value, json};

fn case(leaf: &str, extra: &[&str], responses: Vec<Value>) -> Value {
    let mut command = vec!["job", leaf, "Demo.Workspace/Book.Notebook"];
    command.extend_from_slice(extra);
    let mut replies = metadata("Notebook");
    replies.extend(responses);
    json!({"command":command,"responses":replies})
}

fn envelope(row: &Value) -> Value {
    serde_json::from_str(row["stdout"].as_str().unwrap()).unwrap()
}

#[test]
fn submission_parser_retains_source_options_and_signed_timeout() {
    let Invocation::JobRun(command) = parse_args([
        "fab-rs",
        "--output_format=text",
        "job",
        "run",
        "Demo.Workspace/Book.Notebook",
        "-P",
        "x:int=2,",
        "name:string=hello",
        "world",
        "-C={'driver':true}",
        "--timeout",
        "-2",
        "--polling_interval=1",
        "--skill=",
    ])
    .unwrap() else {
        panic!("job run required")
    };
    assert!(!command.asynchronous);
    assert_eq!(
        command.params,
        Some(vec!["x:int=2, name:string=hello world".into()])
    );
    assert_eq!(command.config, Some(vec!["{'driver':true}".into()]));
    assert_eq!(command.timeout, Some(-2));
    assert_eq!(command.polling_interval, Some(1));
    assert_eq!(command.skill.as_deref(), Some(""));
    assert_eq!(command.output_format.as_deref(), Some("text"));
    for args in [
        vec!["job", "run", "Book.Notebook", "--polling_interval=0"],
        vec!["job", "start", "Book.Notebook", "--timeout=2"],
        vec!["job", "run", "Book.Notebook", "--id=run"],
        vec!["job", "run", "Book.Notebook", "--params"],
        vec!["job", "run", "Book.Notebook", "--query=secret"],
    ] {
        assert!(parse_args(std::iter::once("fab-rs").chain(args)).is_err());
    }
}

#[test]
fn start_submits_once_with_real_headers_body_and_deferred_hint() {
    let row = observe(&case(
        "start",
        &["-P", "n:int=2", "-C", "{'x':true}", "--skill=selected"],
        vec![accepted()],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(envelope(&row)["command"], "job");
    assert_eq!(envelope(&row)["result"]["data"], json!([{"id":"run"}]));
    assert_eq!(row["requests"][2]["method"], "POST");
    assert_eq!(row["requests"][2]["skill"], "selected");
    assert_eq!(row["requests"][0]["skill"], Value::Null);
    assert_eq!(
        row["requests"][2]["body"],
        json!({"executionData":{
            "parameters":{"n":{"type":"int","value":2}},"configuration":{"x":true}
        }})
    );
    assert_eq!(row["requests"].as_array().unwrap().len(), 3);
    assert!(row["waits"].as_array().unwrap().is_empty());
    assert!(
        row["stderr"]
            .as_str()
            .unwrap()
            .ends_with("Book.Notebook --id run'\n")
    );
}

#[test]
fn synchronous_run_polls_real_status_and_keeps_source_output() {
    let row = observe(&case(
        "run",
        &["--polling_interval=2"],
        vec![
            accepted(),
            json!({"status":200,"body":{"id":"run","status":"InProgress"}}),
            json!({"status":200,"body":{"id":"run","status":"Completed"}}),
        ],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(envelope(&row)["command"], "job run");
    assert_eq!(
        envelope(&row)["result"]["message"],
        "Job instance 'run' completed"
    );
    assert_eq!(row["waits"], json!([2, 2]));
    assert_eq!(row["requests"][3]["method"], "GET");
    assert!(
        row["stderr"]
            .as_str()
            .unwrap()
            .contains("Job instance status: InProgress")
    );
}

#[test]
fn timeout_uses_injected_config_and_never_claims_completed() {
    for cancel in [false, true] {
        let mut replies = vec![accepted()];
        if cancel {
            replies.push(json!({"status":202,"body":null}));
        }
        let mut input = case("run", &["--timeout=1"], replies);
        input["cancel_on_timeout"] = json!(cancel);
        let row = observe(&input);
        assert_eq!(row["exit"], 0);
        assert_eq!(row["waits"], json!([1]));
        if cancel {
            assert_eq!(row["requests"][3]["method"], "POST");
            assert!(
                row["requests"][3]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with("/run/cancel")
            );
            assert_eq!(
                envelope(&row)["result"]["message"],
                "Job instance 'run' cancelled (async)"
            );
        } else {
            assert_eq!(row["stdout"], "");
            assert_eq!(row["requests"].as_array().unwrap().len(), 3);
        }
        assert!(!row["stdout"].as_str().unwrap().contains("completed"));
    }
}

#[test]
fn submission_errors_keep_leaf_exit_and_never_replay_a_mutation() {
    for (status, code, exit) in [(401, "Unauthorized", 4), (503, "ServiceUnavailable", 1)] {
        let row = observe(&case(
            "start",
            &[],
            vec![json!({"status":status,"body":{"errorCode":code,"message":"synthetic detail"}})],
        ));
        assert_eq!(row["exit"], exit);
        assert_eq!(envelope(&row)["command"], "job start");
        assert_eq!(envelope(&row)["result"]["error_code"], code);
        assert_eq!(row["requests"].as_array().unwrap().len(), 3);
        assert_eq!(row["requests"][2]["method"], "POST");
    }
}

#[test]
fn terminal_failure_unknown_and_cancelled_preserve_actual_ui_branches() {
    for (status, exit, raw) in [
        ("Failed", 1, true),
        ("Future", 0, true),
        ("Cancelled", 0, false),
        ("Deduped", 0, false),
    ] {
        let row = observe(&case(
            "run",
            &[],
            vec![
                accepted(),
                json!({"status":200,"body":{"id":"run","status":status}}),
            ],
        ));
        assert_eq!(row["exit"], exit);
        if raw {
            assert!(row["stdout"].as_str().unwrap().contains(status));
        } else {
            assert_eq!(row["stdout"], "");
        }
        assert!(!row["stdout"].as_str().unwrap().contains("completed"));
    }
}

#[test]
fn file_capability_is_explicit_and_input_conflicts_do_not_submit() {
    let mut input = case("start", &["--input", "execution.json"], vec![accepted()]);
    input["files"] = json!({"execution.json":{"key":false,"values":[1,null]}});
    let row = observe(&input);
    assert_eq!(row["file_reads"], json!(["execution.json"]));
    assert_eq!(
        row["requests"][2]["body"],
        json!({"executionData":{"key":false,"values":[1,null]}})
    );
    let row = observe(&case("start", &["--input={}", "--params=x:int=1"], vec![]));
    assert_eq!(row["exit"], 1);
    assert_eq!(envelope(&row)["result"]["error_code"], "NotRunnable");
    assert_eq!(row["requests"].as_array().unwrap().len(), 2);
}

#[test]
fn cancellation_invalid_skill_and_ui_failures_cannot_submit() {
    for (key, value, extra, replies, expected_requests) in [
        ("cancelled", json!(true), vec![], vec![], 0),
        (
            "cancelled",
            json!(false),
            vec!["--skill=bad\r\nheader"],
            vec![],
            0,
        ),
        ("fail_ui", json!(true), vec![], metadata("Notebook"), 2),
    ] {
        let command = ["job", "start", "Demo.Workspace/Book.Notebook"]
            .into_iter()
            .chain(extra)
            .collect::<Vec<_>>();
        let mut input = json!({
            "command": command,
            "responses":replies,
        });
        input[key] = value;
        let row = observe(&input);
        assert_eq!(row["exit"], 1);
        assert_eq!(row["requests"].as_array().unwrap().len(), expected_requests);
    }
}

#[test]
fn cancelling_after_submission_drops_local_poll_without_replay_or_rollback() {
    let mut input = case("run", &[], vec![accepted()]);
    input["hold_after"] = json!(4);
    let row = observe(&input);
    assert_eq!(row["exit"], 1);
    assert_eq!(
        envelope(&row)["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    assert_eq!(row["requests"].as_array().unwrap().len(), 4);
    assert_eq!(row["requests"][2]["method"], "POST");
    assert_eq!(row["requests"][3]["method"], "GET");
}

#[test]
fn native_file_reader_preserves_json_and_rejects_lossy_numbers_and_invalid_files() {
    let path = std::env::temp_dir().join(format!("fabric-job-input-{}.json", std::process::id()));
    assert!(!path.exists());
    std::fs::write(
        &path,
        br#"{"false":false,"null":null,"integer":18446744073709551615}"#,
    )
    .unwrap();
    let result = SelectedJobFiles.read_json(path.to_str().unwrap()).unwrap();
    assert_eq!(result["integer"].as_u64(), Some(u64::MAX));
    std::fs::write(&path, b"{\"integer\":18446744073709551616}").unwrap();
    assert!(SelectedJobFiles.read_json(path.to_str().unwrap()).is_err());
    std::fs::write(&path, b"not-json").unwrap();
    assert!(SelectedJobFiles.read_json(path.to_str().unwrap()).is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(SelectedJobFiles.read_json(path.to_str().unwrap()).is_err());
}
