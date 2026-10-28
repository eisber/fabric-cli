// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Mounted cancellation/schedule contracts over the production job session and offline I/O.

mod job_execution_support;

use fabric_cli::parser::{Invocation, JobMutationKind, parse_args};
use job_execution_support::{accepted, metadata, observe};
use serde_json::{Value, json};

fn case(leaf: &str, extra: &[&str], responses: Vec<Value>) -> Value {
    let mut command = vec!["job", leaf, "Demo.Workspace/Book.Notebook"];
    command.extend_from_slice(extra);
    let mut replies = metadata("Notebook");
    replies.extend(responses);
    json!({"command":command,"responses":replies,"observe_confirmation":true})
}

fn envelope(row: &Value) -> Value {
    serde_json::from_str(row["stdout"].as_str().unwrap()).unwrap()
}

fn schedule() -> Value {
    json!({"type":"Cron","interval":10,"startDateTime":"2026-01-01T00:00:00",
        "endDateTime":"2026-12-01T00:00:00","localTimeZoneId":"UTC"})
}

#[test]
fn mutation_parser_preserves_source_fields_flags_and_required_ids() {
    let Invocation::JobMutation(command) = parse_args([
        "fab-rs",
        "--output_format=text",
        "job",
        "run-update",
        "Demo.Workspace/Book.Notebook",
        "--id=old",
        "--id=sch",
        "--disable",
        "--type=weekly",
        "--interval=10:00,16:00",
        "--start=2026-01-01T00:00:00",
        "--end=2026-12-01T00:00:00",
        "--days=Monday,Friday",
        "--skill=",
    ])
    .unwrap() else {
        panic!("schedule update required")
    };
    assert_eq!(command.kind, JobMutationKind::Update);
    assert_eq!(command.id.as_deref(), Some("sch"));
    assert!(command.disable);
    assert!(!command.enable);
    assert_eq!(command.schedule_type.as_deref(), Some("weekly"));
    assert_eq!(command.interval.as_deref(), Some("10:00,16:00"));
    assert_eq!(command.days.as_deref(), Some("Monday,Friday"));
    assert_eq!(command.output_format.as_deref(), Some("text"));
    assert_eq!(command.skill.as_deref(), Some(""));
    let family = command.family_command();
    assert!(family.schedule_args.disable);
    for args in [
        vec!["job", "run-cancel", "Book.Notebook"],
        vec!["job", "run-update", "Book.Notebook"],
        vec!["job", "run-rm", "Book.Notebook"],
        vec!["job", "run-sch", "Book.Notebook", "--type=monthly"],
        vec!["job", "run-rm", "Book.Notebook", "--id=sch", "--wait"],
        vec!["job", "run-cancel", "Book.Notebook", "--id=run", "--force"],
        vec![
            "job",
            "run-update",
            "Book.Notebook",
            "--id=sch",
            "--params=x:int=1",
        ],
    ] {
        assert!(parse_args(std::iter::once("fab-rs").chain(args)).is_err());
    }
}

#[test]
fn cancellation_is_one_post_with_async_message_and_no_fabricated_id_data() {
    let row = observe(&case(
        "run-cancel",
        &["--id=run", "--skill=selected"],
        vec![accepted()],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(envelope(&row)["command"], "job");
    assert_eq!(
        envelope(&row)["result"]["message"],
        "Job instance 'run' cancelled (async)"
    );
    assert!(envelope(&row)["result"].get("data").is_none());
    assert_eq!(row["requests"][2]["method"], "POST");
    assert!(
        row["requests"][2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/run/cancel")
    );
    assert_eq!(row["requests"][2]["skill"], "selected");
    assert_eq!(row["requests"].as_array().unwrap().len(), 3);
    assert_eq!(row["waits"], json!([]));
    assert_eq!(row["confirmations"], 0);
}

#[test]
fn cancellation_wait_polls_service_status_instead_of_treating_acceptance_as_completion() {
    let row = observe(&case(
        "run-cancel",
        &["--id=run", "-w"],
        vec![
            json!({"status":202,"body":null,"headers":{"Retry-After":"2"}}),
            json!({"status":200,"body":{"id":"run","status":"InProgress"}}),
            json!({"status":200,"body":{"id":"run","status":"Cancelled"}}),
        ],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(row["stdout"], "");
    assert_eq!(row["requests"].as_array().unwrap().len(), 5);
    assert_eq!(row["requests"][3]["method"], "GET");
    assert_eq!(row["requests"][4]["method"], "GET");
    assert_eq!(row["waits"], json!([2, 10]));
    assert!(
        row["stderr"]
            .as_str()
            .unwrap()
            .contains("finished with status: Cancelled")
    );
}

#[test]
fn cancellation_reuses_shared_generic_operation_and_result_without_replaying_post() {
    let operation = "11111111-1111-1111-1111-111111111111";
    let url = format!("https://api.fabric.microsoft.com/v1/operations/{operation}");
    let result_url = format!("{url}/result");
    let row = observe(&case(
        "run-cancel",
        &["--id=run", "--wait", "--skill=selected"],
        vec![
            json!({"status":202,"body":null,"headers":{"x-ms-operation-id":operation,"Location":url}}),
            json!({"status":200,"body":{"status":"Succeeded"},"headers":{"Location":result_url}}),
            json!({"status":200,"body":{"id":"result"}}),
        ],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(row["stdout"], "");
    assert_eq!(row["requests"].as_array().unwrap().len(), 5);
    assert_eq!(row["requests"][2]["method"], "POST");
    assert_eq!(row["requests"][3]["url"], url);
    assert_eq!(row["requests"][4]["url"], result_url);
    for request in &row["requests"].as_array().unwrap()[2..] {
        assert_eq!(request["skill"], "selected");
    }
}

#[test]
fn creating_schedule_preserves_fields_enabled_state_and_real_post() {
    let row = observe(&case(
        "run-sch",
        &[
            "--type=cron",
            "--interval=10",
            "--start=2026-01-01T00:00:00",
            "--end=2026-12-01T00:00:00",
        ],
        vec![json!({"status":201,"body":{"id":"sch"}})],
    ));
    assert_eq!(row["exit"], 0);
    assert_eq!(
        envelope(&row)["result"]["message"],
        "Job schedule 'sch' created"
    );
    assert_eq!(row["requests"][2]["method"], "POST");
    assert!(
        row["requests"][2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/jobs/RunNotebook/schedules")
    );
    assert_eq!(
        row["requests"][2]["body"],
        json!({"configuration":schedule(),"enabled":false})
    );
}

#[test]
fn schedule_file_input_retains_full_envelope_and_selected_nonboolean_enabled_value() {
    let payload = json!({"configuration":schedule(),"enabled":"source value"});
    let mut input = case(
        "run-sch",
        &["--input=schedule.json"],
        vec![json!({"status":201,"body":{"id":"sch"}})],
    );
    input["files"] = json!({"schedule.json":payload});
    let row = observe(&input);
    assert_eq!(row["exit"], 0);
    assert_eq!(row["requests"][2]["body"], payload);
    assert_eq!(row["file_reads"], json!(["schedule.json"]));
}

#[test]
fn update_gets_existing_schedule_before_patch_and_preserves_or_changes_enabled() {
    for explicit_disable in [false, true] {
        let mut options = vec!["--id=sch"];
        if explicit_disable {
            options.push("--disable");
        } else {
            options.push("--input={'type':'Daily','times':['10:00']}");
        }
        let row = observe(&case(
            "run-update",
            &options,
            vec![
                json!({"status":200,"body":{"id":"sch","configuration":schedule(),"enabled":true}}),
                json!({"status":200,"body":{"id":"sch"}}),
            ],
        ));
        assert_eq!(row["exit"], 0);
        assert_eq!(row["requests"][2]["method"], "GET");
        assert_eq!(row["requests"][3]["method"], "PATCH");
        assert_eq!(row["requests"][2]["url"], row["requests"][3]["url"]);
        assert_eq!(
            row["requests"][3]["body"]["enabled"],
            json!(!explicit_disable)
        );
        if explicit_disable {
            assert_eq!(row["requests"][3]["body"]["configuration"], schedule());
        } else {
            assert_eq!(
                row["requests"][3]["body"]["configuration"],
                json!({"type":"Daily","times":["10:00"]})
            );
        }
        assert_eq!(
            envelope(&row)["result"]["message"],
            "Job schedule sch updated"
        );
    }
}

#[test]
fn conflicting_switches_or_invalid_schedule_do_not_send_mutations() {
    for (leaf, options, code) in [
        (
            "run-update",
            vec!["--id=sch", "--enable", "--disable"],
            "InvalidInput",
        ),
        (
            "run-sch",
            vec![
                "--type=weekly",
                "--interval=10:00",
                "--start=2026-01-01T00:00:00",
                "--end=2026-12-01T00:00:00",
                "--days=monday",
            ],
            "NotRunnable",
        ),
        ("run-sch", vec!["--input={}", "--enable"], "NotRunnable"),
    ] {
        let row = observe(&case(leaf, &options, vec![]));
        assert_eq!(row["exit"], 1);
        assert_eq!(envelope(&row)["result"]["error_code"], code);
        assert_eq!(row["requests"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn removal_confirms_declines_or_forces_without_implicit_yes() {
    for (force, confirmed) in [(false, false), (false, true), (true, false)] {
        let options = if force {
            vec!["--id=sch", "-f"]
        } else {
            vec!["--id=sch"]
        };
        let mut input = case(
            "run-rm",
            &options,
            if force || confirmed {
                vec![json!({"status":200,"body":null})]
            } else {
                vec![]
            },
        );
        input["confirmed"] = json!(confirmed);
        let row = observe(&input);
        assert_eq!(row["exit"], 0);
        assert_eq!(row["confirmations"], usize::from(!force));
        assert_eq!(
            row["requests"].as_array().unwrap().len(),
            if force || confirmed { 3 } else { 2 }
        );
        if force || confirmed {
            assert_eq!(row["requests"][2]["method"], "DELETE");
            assert_eq!(
                envelope(&row)["result"]["message"],
                "Job schedule 'sch' removed"
            );
        } else {
            assert_eq!(row["stdout"], "");
        }
        assert_eq!(
            row["stderr"].as_str().unwrap().contains("cannot be undone"),
            !force
        );
    }
}

#[test]
fn cancellation_and_errors_while_confirming_never_delete() {
    for key in ["cancel_confirmation", "fail_confirmation"] {
        let mut input = case("run-rm", &["--id=sch"], vec![]);
        input[key] = json!(true);
        let row = observe(&input);
        assert_eq!(row["exit"], 1);
        assert_eq!(row["requests"].as_array().unwrap().len(), 2);
        assert_eq!(row["confirmations"], 1);
        assert_eq!(
            envelope(&row)["result"]["error_code"],
            if key == "cancel_confirmation" {
                "LongRunningOperationCancelled"
            } else {
                "UnexpectedError"
            }
        );
    }
}

#[test]
fn failed_read_or_mutation_never_yields_update_or_replay() {
    let row = observe(&case(
        "run-update",
        &["--id=sch", "--disable"],
        vec![json!({"status":401,"body":{"errorCode":"Unauthorized","message":"synthetic"}})],
    ));
    assert_eq!(row["exit"], 4);
    assert_eq!(envelope(&row)["command"], "job run-update");
    assert_eq!(row["requests"].as_array().unwrap().len(), 3);
    let row = observe(&case(
        "run-rm",
        &["--id=sch", "--force"],
        vec![json!({"status":503,"body":{"errorCode":"ServiceUnavailable","message":"synthetic"}})],
    ));
    assert_eq!(row["exit"], 1);
    assert_eq!(row["requests"].as_array().unwrap().len(), 3);
    assert_eq!(row["requests"][2]["method"], "DELETE");
}
