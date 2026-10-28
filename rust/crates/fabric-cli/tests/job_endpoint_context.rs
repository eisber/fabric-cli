// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root job argv -> metadata context -> shared HTTP, without ambient credentials or services.

#[allow(dead_code)]
mod job_execution_support;

use job_execution_support::{metadata, observe};
use serde_json::{Value, json};

const HOSTS: [&str; 3] = [
    "api.fabric.microsoft.com",
    "dailyapi.fabric.microsoft.com",
    "msitapi.fabric.microsoft.com",
];
const OPERATION: &str = "11111111-1111-1111-1111-111111111111";

fn accepted(host: &str) -> Value {
    json!({"status":202,"body":null,"headers":{
        "Location":format!("https://{host}/v1/workspaces/w/items/n/jobs/instances/run"),
        "Retry-After":"1",
    }})
}

fn command(leaf: &str, resource: &str, skill: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "job".into(),
        leaf.into(),
        format!("Demo.Workspace/Book.{resource}"),
    ];
    match leaf {
        "run-cancel" | "run-status" => args.push("--id=run".into()),
        "run-update" | "run-rm" => args.push("--id=sch".into()),
        _ => {}
    }
    match leaf {
        "run-sch" | "run-update" => args.push("--input={}".into()),
        "run-cancel" => args.push("--wait".into()),
        "run-rm" => args.push("--force".into()),
        _ => {}
    }
    if let Some(skill) = skill {
        args.push(format!("--skill={skill}"));
    }
    args
}

fn assert_binding(row: &Value, host: &str, skill: Option<&str>, expected: &[(&str, String)]) {
    assert_eq!(row["exit"], 0, "{row}");
    let requests = row["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2 + expected.len(), "{row}");
    for (index, request) in requests.iter().enumerate() {
        assert!(
            request["url"]
                .as_str()
                .unwrap()
                .starts_with(&format!("https://{host}/"))
        );
        assert_eq!(
            request["skill"],
            if index < 2 {
                Value::Null
            } else {
                skill
                    .filter(|value| !value.is_empty())
                    .map_or(Value::Null, |value| json!(value))
            }
        );
    }
    for (request, (method, url)) in requests[2..].iter().zip(expected) {
        assert_eq!(request["method"], *method);
        assert_eq!(request["url"], *url);
    }
    assert_eq!(
        row["scopes"],
        json!(vec![
            vec!["https://api.fabric.microsoft.com/.default"];
            requests.len()
        ])
    );
}

#[test]
fn all_eight_leaves_five_resources_and_skill_variants_keep_one_exact_host() {
    for host in HOSTS {
        for (resource, job_type) in [
            ("SparkJobDefinition", "sparkjob"),
            ("Notebook", "RunNotebook"),
            ("DataPipeline", "Pipeline"),
            ("Lakehouse", "TableMaintenance"),
            ("GraphModel", "refreshGraph"),
        ] {
            let instances = format!("https://{host}/v1/workspaces/w/items/n/jobs/instances");
            let schedules =
                format!("https://{host}/v1/workspaces/w/items/n/jobs/{job_type}/schedules");
            for skill in [None, Some(""), Some("caller-skill")] {
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
                    let mut responses = metadata(resource);
                    let expected = match leaf {
                        "start" | "run" => {
                            responses.push(accepted(host));
                            let mut requests =
                                vec![("POST", format!("{instances}?jobType={job_type}"))];
                            if leaf == "run" {
                                responses.push(json!({"status":200,"body":{"status":"Completed"}}));
                                requests.push(("GET", format!("{instances}/run")));
                            }
                            requests
                        }
                        "run-list" => {
                            responses.extend([
                                json!({"status":200,"body":{"value":[{"id":"a"}],"continuationToken":"next"}}),
                                json!({"status":200,"body":{"value":[{"id":"b"}]}}),
                            ]);
                            vec![
                                ("GET", instances.clone()),
                                ("GET", format!("{instances}?continuationToken=next")),
                            ]
                        }
                        "run-status" => {
                            responses.push(json!({"status":200,"body":{"status":null,"id":"run"}}));
                            vec![("GET", format!("{instances}/run"))]
                        }
                        "run-cancel" => {
                            responses.extend([
                                accepted(host),
                                json!({"status":200,"body":{"status":"Cancelled"}}),
                            ]);
                            vec![
                                ("POST", format!("{instances}/run/cancel")),
                                ("GET", format!("{instances}/run")),
                            ]
                        }
                        "run-sch" => {
                            responses.push(json!({"status":201,"body":{"id":"sch"}}));
                            vec![("POST", schedules.clone())]
                        }
                        "run-update" => {
                            responses.extend([
                                json!({"status":200,"body":{"configuration":{},"enabled":"source-enabled"}}),
                                json!({"status":200,"body":{"id":"sch"}}),
                            ]);
                            vec![
                                ("GET", format!("{schedules}/sch")),
                                ("PATCH", format!("{schedules}/sch")),
                            ]
                        }
                        "run-rm" => {
                            responses.push(json!({"status":200,"body":null}));
                            vec![("DELETE", format!("{schedules}/sch"))]
                        }
                        _ => unreachable!(),
                    };
                    let row = observe(&json!({
                        "command":command(leaf, resource, skill),"endpoint":host,"responses":responses,
                    }));
                    assert_binding(&row, host, skill, &expected);
                    if leaf == "run-update" {
                        assert_eq!(
                            row["requests"][3]["body"],
                            json!({"configuration":{},"enabled":"source-enabled"})
                        );
                    }
                }
            }
        }
    }
}

fn generic_responses(host: &str, status: u16, body: Value) -> Vec<Value> {
    let operation = format!("https://{host}/v1/operations/{OPERATION}");
    vec![
        json!({"status":202,"body":null,"headers":{"Location":operation,"x-ms-operation-id":OPERATION}}),
        json!({"status":200,"body":{"status":"Succeeded"},"headers":{"Location":format!("{operation}/result")}}),
        json!({"status":status,"body":body}),
    ]
}

#[test]
fn every_synchronous_command_keeps_context_through_actual_generic_poll_and_result() {
    for host in HOSTS {
        for leaf in [
            "run",
            "run-list",
            "run-status",
            "run-cancel",
            "run-sch",
            "run-update",
            "run-rm",
        ] {
            let mut responses = metadata("Notebook");
            let (status, body) = match leaf {
                "run-sch" => (201, json!({"id":"sch"})),
                "run-list" => (200, json!({"value":[]})),
                "run-update" => (200, json!({"configuration":{},"enabled":false})),
                _ => (200, json!({})),
            };
            responses.extend(generic_responses(host, status, body));
            let route = match leaf {
                "run" => "instances?jobType=RunNotebook",
                "run-list" => "instances",
                "run-status" => "instances/run",
                "run-cancel" => "instances/run/cancel",
                "run-sch" => "RunNotebook/schedules",
                "run-update" | "run-rm" => "RunNotebook/schedules/sch",
                _ => unreachable!(),
            };
            let method = match leaf {
                "run-list" | "run-status" | "run-update" => "GET",
                "run-rm" => "DELETE",
                _ => "POST",
            };
            let mut expected = vec![
                (
                    method,
                    format!("https://{host}/v1/workspaces/w/items/n/jobs/{route}"),
                ),
                ("GET", format!("https://{host}/v1/operations/{OPERATION}")),
                (
                    "GET",
                    format!("https://{host}/v1/operations/{OPERATION}/result"),
                ),
            ];
            if leaf == "run-update" {
                responses.push(json!({"status":200,"body":{"id":"sch"}}));
                expected.push((
                    "PATCH",
                    format!("https://{host}/v1/workspaces/w/items/n/jobs/{route}"),
                ));
            }
            let row = observe(&json!({
                "command":command(leaf, "Notebook", Some("caller-skill")),
                "endpoint":host,"responses":responses,
            }));
            assert_binding(&row, host, Some("caller-skill"), &expected);
        }
    }
}

#[test]
fn foreign_submission_continuation_and_operation_hosts_fail_without_foreign_requests() {
    let host = "dailyapi.fabric.microsoft.com";
    for (leaf, reply) in [
        ("start", accepted("api.fabric.microsoft.com")),
        (
            "run-list",
            json!({"status":200,"body":{"value":[{"id":"one"}],"continuationToken":"next",
            "continuationUri":"https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances?continuationToken=next"}}),
        ),
        (
            "run",
            generic_responses("api.fabric.microsoft.com", 200, json!({})).remove(0),
        ),
    ] {
        let mut responses = metadata("Notebook");
        responses.push(reply);
        let row = observe(&json!({
            "command":command(leaf, "Notebook", Some("caller-skill")),"endpoint":host,"responses":responses,
        }));
        assert_eq!(row["exit"], 1, "{leaf}: {row}");
        assert_eq!(row["requests"].as_array().unwrap().len(), 3);
        assert!(!row["stdout"].as_str().unwrap().contains("completed"));
        for request in row["requests"].as_array().unwrap() {
            assert!(
                request["url"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("https://{host}/"))
            );
        }
    }
}
