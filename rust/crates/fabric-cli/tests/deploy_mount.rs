// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual root argv -> production config host -> real shared HTTP/file adapters.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

mod family_support;
use fabric_cli::{deploy::DeployFailure, families::FamilyEvidence};
use fabric_deploy::publish::PublishFailure;
use family_support::{Harness, success};
use serde_json::{Value, json};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

impl Harness {
    async fn execute_with_ui(
        &mut self,
        args: &[&str],
        ui: &dyn fabric_cli_transfer::TransferUi,
    ) -> fabric_cli::families::FamilyOutput {
        self.execute_with_capabilities(args, std::future::pending(), ui)
            .await
    }
}

fn input(host: &Harness) -> Value {
    let source: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/publish_source.json"
    ))
    .unwrap();
    let repository = host.root.join("repository");
    let notebook = repository.join("Book.Notebook");
    std::fs::create_dir(&repository).unwrap();
    std::fs::create_dir(&notebook).unwrap();
    for file in source["cases"][0]["files"].as_array().unwrap() {
        std::fs::write(
            notebook.join(file[0].as_str().unwrap()),
            file[1].as_str().unwrap(),
        )
        .unwrap();
    }
    std::fs::write(host.root.join("deploy.yml"), format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\nunpublish:\n  skip: true\n"
    )).unwrap();
    source
}

fn metadata(host: &Harness, existing: bool) {
    host.reply(
        200,
        json!({"id":WORKSPACE,"displayName":"Demo","capacityId":"capacity"}),
    );
    host.reply(
        200,
        json!({"value":if existing {
        json!([{"id":ITEM,"displayName":"Book","type":"Notebook"}])
    } else { json!([]) }}),
    );
}

#[tokio::test]
async fn root_yaml_deploy_create_and_update_match_actual_pinned_source_wire() {
    for existing in [false, true] {
        let mut host = Harness::new();
        let source = input(&host);
        metadata(&host, existing);
        host.reply(
            if existing { 200 } else { 201 },
            if existing {
                Value::Null
            } else {
                json!({"id":ITEM})
            },
        );
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-f"])
            .await;
        assert!(success(&output).contains("Deployment completed successfully"));
        let requests = host.requests();
        assert_eq!(requests.len(), 3);
        let case = &source["cases"][usize::from(existing)];
        assert_eq!(requests[2]["method"], case["requests"][0]["method"]);
        assert_eq!(requests[2]["url"], case["requests"][0]["url"]);
        assert_eq!(requests[2]["raw_body"], case["body_bytes"]);
        assert!(
            matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result))
            if result.completed.len() == 1 && result.completed[0].outcome.item_id == ITEM)
        );
        assert!(
            output
                .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
                .is_err()
        );
        assert_eq!(
            host.requests().len(),
            3,
            "writer failure must not replay publication"
        );
    }
}

#[tokio::test]
async fn root_yaml_deploy_waits_for_actual_operation_result_id() {
    let source: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/operation_result_source.json"
    ))
    .unwrap();
    let advertised = &source["cases"][1];
    assert_eq!(advertised["id"], "advertised-result");
    let result_location = advertised["terminalHeaders"]["Location"].as_str().unwrap();
    let operation_location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let mut host = Harness::new();
    input(&host);
    metadata(&host, false);
    host.headers(
        202,
        Value::Null,
        &[
            ("x-ms-operation-id", OPERATION),
            ("retry-after", "0"),
            ("Location", &operation_location),
        ],
    );
    host.headers(
        200,
        json!({"status":"Succeeded"}),
        &[("Location", result_location)],
    );
    host.reply(200, advertised["result"]["body"].clone());
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result)) if result.completed[0].outcome.item_id == ITEM)
    );
    assert_eq!(host.requests().len(), 5);
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
    assert!(
        host.requests()[4]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/operations/{OPERATION}/result"))
    );
}

#[tokio::test]
async fn terminal_success_without_advertised_result_never_invents_created_id_or_replays() {
    let source: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/operation_result_source.json"
    ))
    .unwrap();
    let missing = &source["cases"][0];
    assert_eq!(missing["id"], "terminal-without-result-location");
    assert!(missing["result"]["body"].is_null());
    assert_eq!(missing["cliRequests"].as_array().unwrap().len(), 1);
    let operation_location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let mut host = Harness::new();
    input(&host);
    metadata(&host, false);
    host.headers(
        202,
        Value::Null,
        &[
            ("x-ms-operation-id", OPERATION),
            ("retry-after", "0"),
            ("Location", &operation_location),
        ],
    );
    host.reply(200, json!({"status":"Succeeded"}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Err(error))
        if matches!(&error.failure, DeployFailure::Publish(error)
        if error.completed.is_empty() && error.current_item.as_deref() == Some("Book")
        && matches!(&error.failure, PublishFailure::Operation(error)
        if error.kind == fabric_sdk_operations::OperationErrorKind::InvalidResponse
        && error.operation_id.as_deref() == Some(OPERATION) && error.mutation_may_have_occurred)))
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
    assert!(!requests.iter().any(
        |request| request["url"].as_str().unwrap().ends_with("/result")
            || request["method"] == "DELETE"
    ));
}

#[tokio::test]
async fn root_deploy_retains_accepted_failed_operation_without_replay_or_delete() {
    let mut host = Harness::new();
    input(&host);
    metadata(&host, false);
    host.headers(
        202,
        Value::Null,
        &[("x-ms-operation-id", OPERATION), ("retry-after", "0")],
    );
    host.reply(
        200,
        json!({"status":"Failed","error":{"code":"SyntheticFailure","message":"synthetic"}}),
    );
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Err(error))
        if matches!(&error.failure, DeployFailure::Publish(error)
        if error.completed.is_empty() && matches!(&error.failure, PublishFailure::Operation(operation)
        if operation.operation_id.as_deref() == Some(OPERATION) && operation.mutation_may_have_occurred)))
    );
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
    assert!(
        !host
            .requests()
            .iter()
            .any(|request| request["method"] == "DELETE")
    );
}

#[tokio::test]
async fn source_default_unpublish_and_unsupported_payload_fail_before_http() {
    for body in [
        format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n"
        ),
        format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook, DataPipeline]\nunpublish:\n  skip: true\n"
        ),
        format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter: parameter.yml\nunpublish:\n  skip: true\n"
        ),
    ] {
        let mut host = Harness::new();
        input(&host);
        std::fs::write(host.root.join("deploy.yml"), body).unwrap();
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-f"])
            .await;
        assert_ne!(output.plan.exit_code, 0);
        assert!(host.requests().is_empty());
        assert!(matches!(&output.evidence, FamilyEvidence::Deploy(Err(_))));
    }
}

#[tokio::test]
async fn malformed_later_local_item_prevents_all_remote_mutations() {
    let mut host = Harness::new();
    input(&host);
    let later = host.root.join("repository").join("Later.Notebook");
    std::fs::create_dir(&later).unwrap();
    std::fs::write(later.join(".platform"), b"invalid").unwrap();
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(host.requests().is_empty());
}

#[tokio::test]
async fn cancellation_before_reads_makes_no_http_requests() {
    let mut host = Harness::new();
    let output = host
        .execute_with_cancelled(
            &["deploy", "--config", "missing.yml", "-f"],
            std::future::ready(()),
        )
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(host.requests().is_empty());
}

#[derive(Default)]
struct Confirmation {
    answer: bool,
    questions: std::sync::Mutex<Vec<String>>,
}

impl fabric_cli_transfer::TransferUi for Confirmation {
    fn emit(
        &self,
        _: fabric_cli_transfer::TransferEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        panic!("this deploy case has no progress event");
    }
    fn confirm<'a>(
        &'a self,
        question: &'a str,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>>
    {
        self.questions.lock().unwrap().push(question.into());
        Box::pin(async { Ok(self.answer) })
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>>
    {
        panic!("deploy must not invoke import/export selection");
    }
}

#[tokio::test]
async fn source_confirmation_decline_is_silent_before_config_reads_and_force_never_prompts() {
    let fixture: Value = serde_json::from_str(include_str!("deploy_wrapper_source.json")).unwrap();
    let mut host = Harness::new();
    let ui = Confirmation::default();
    let output = host
        .execute_with_ui(&["deploy", "--config", "missing.yml", "-tenv", "dev"], &ui)
        .await;
    assert_eq!(output.plan.exit_code, 0);
    assert!(output.plan.records.is_empty());
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result)) if result.declined && result.completed.is_empty())
    );
    assert_eq!(
        ui.questions.lock().unwrap()[0],
        fixture["wrappers"][0]["calls"][0][1].as_str().unwrap()
    );
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());

    input(&host);
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute_with_ui(&["deploy", "--config", "deploy.yml", "-f"], &ui)
        .await;
    success(&output);
    assert_eq!(ui.questions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn config_override_and_untrusted_host_app_use_the_real_yaml_host() {
    let mut host = Harness::new();
    input(&host);
    std::fs::write(host.root.join("deploy.yml"), format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\nunpublish:\n  skip: false\n"
    )).unwrap();
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute(&[
            "deploy",
            "--config",
            "deploy.yml",
            "-f",
            "-P",
            "config_override={\"unpublish\":{\"skip\":true}},host_app=\"untrusted\"",
        ])
        .await;
    success(&output);
    assert_eq!(host.requests().len(), 3);
}

#[tokio::test]
async fn complete_discovery_reads_later_page_before_update_selection() {
    let mut host = Harness::new();
    input(&host);
    host.reply(200, json!({"id":WORKSPACE,"capacityId":"capacity"}));
    host.reply(200, json!({"value":[],"continuationToken":"next+/="}));
    host.reply(
        200,
        json!({"value":[{"id":ITEM,"displayName":"Book","type":"Notebook"}]}),
    );
    host.reply(200, Value::Null);
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    let requests = host.requests();
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("continuationToken=next%2B%2F%3D")
    );
    assert!(
        requests[3]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/{ITEM}/updateDefinition?updateMetadata=True"))
    );
}

#[tokio::test]
async fn partial_failure_retains_first_actual_id_and_second_mutation_evidence() {
    let mut host = Harness::new();
    input(&host);
    let second = host.root.join("repository").join("Other.Notebook");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(
        second.join(".platform"),
        json!({
            "metadata":{"type":"Notebook","displayName":"Other"},
            "config":{"logicalId":"logical-other"},
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        second.join("notebook-content.py"),
        "# Fabric notebook source\nprint('other')\n",
    )
    .unwrap();
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    host.reply(
        500,
        json!({"error":{"code":"SyntheticFailure","message":"synthetic"}}),
    );
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    let requests = host.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Err(error))
        if matches!(&error.failure, DeployFailure::Publish(error)
        if error.completed.len() == 1 && error.completed[0].outcome.item_id == ITEM
        && Some(error.completed[0].name.as_str()) == requests[2]["body"]["displayName"].as_str()
        && error.current_item.as_deref() == requests[3]["body"]["displayName"].as_str()
        && matches!(&error.failure, PublishFailure::Operation(error) if error.mutation_may_have_occurred)))
    );
    assert!(!requests.iter().any(|request| request["method"] == "DELETE"));
}

#[tokio::test]
async fn caller_file_root_escape_and_duplicate_service_targets_fail_before_writes() {
    let mut host = Harness::new();
    let output = host
        .execute(&["deploy", "--config", "../outside.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(host.requests().is_empty());
    input(&host);
    host.reply(200, json!({"id":WORKSPACE,"capacityId":"capacity"}));
    host.reply(
        200,
        json!({"value":[
            {"id":ITEM,"displayName":"Book","type":"Notebook"},
            {"id":OPERATION,"displayName":"Book","type":"Notebook"},
        ]}),
    );
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        host.requests()
            .iter()
            .all(|request| request["method"] == "GET")
    );
}

#[tokio::test]
async fn both_source_operations_skipped_do_not_discover_or_load_item_definitions() {
    let source: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/orchestration_source.json"
    ))
    .unwrap();
    assert_eq!(source["cases"][0]["id"], "both-skipped");
    let mut host = Harness::new();
    input(&host);
    std::fs::write(
        host.root
            .join("repository")
            .join("Book.Notebook")
            .join(".platform"),
        b"invalid",
    )
    .unwrap();
    std::fs::write(host.root.join("deploy.yml"), format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\npublish:\n  skip: true\nunpublish:\n  skip: true\n"
    )).unwrap();
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    assert!(success(&output).contains(source["cases"][0]["result"]["message"].as_str().unwrap()));
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result)) if !result.declined && result.completed.is_empty())
    );
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());
}

struct BrokenNotice;

impl fabric_cli_transfer::TransferUi for BrokenNotice {
    fn emit(
        &self,
        _: fabric_cli_transfer::TransferEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        Err(fabric_cli_output::CommandError::new(
            Some("Injected notice writer failure".into()),
            Some("OutputFailed".into()),
        ))
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>>
    {
        panic!("forced deploy must not prompt");
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>>
    {
        panic!("deploy must not select import/export items");
    }
}

#[tokio::test]
async fn warning_writer_failure_is_explicit_before_credentials_or_mutations() {
    let mut host = Harness::new();
    input(&host);
    std::fs::write(host.root.join("deploy.yml"), format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter:\n    other: parameter.yml\nunpublish:\n  skip: true\n"
    )).unwrap();
    let output = host
        .execute_with_ui(
            &["deploy", "--config", "deploy.yml", "-tenv", "dev", "-f"],
            &BrokenNotice,
        )
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Err(error)) if matches!(&error.failure, DeployFailure::Command(_)))
    );
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn configured_yaml_literal_parameter_is_read_and_changes_the_actual_post_part() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/literal_parameter_payload_source.json"
    ))
    .unwrap();
    let case = &fixture["cases"][0];
    assert_eq!(case["id"], "literal");
    let mut host = Harness::new();
    input(&host);
    std::fs::write(
        host.root
            .join("repository")
            .join("Book.Notebook")
            .join("notebook-content.py"),
        case["input"].as_str().unwrap(),
    )
    .unwrap();
    std::fs::write(
        host.root.join("parameter.yml"),
        "find_replace:\n- find_value: source\n  replace_value:\n    dev: target\n",
    )
    .unwrap();
    std::fs::write(host.root.join("deploy.yml"), format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter: parameter.yml\nunpublish:\n  skip: true\n"
    )).unwrap();
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-tenv", "dev", "-f"])
        .await;
    success(&output);
    assert_eq!(
        host.requests()[2]["body"]["definition"]["parts"][1],
        case["expectedPart"]
    );
}

#[tokio::test]
async fn configured_parameter_duplicates_and_unsupported_regex_fail_before_http() {
    for contents in [
        "find_replace:\n- find_value: source\n  replace_value:\n    dev: target\n    DEV: duplicate\n",
        "find_replace:\n- find_value: '(?i)(source)'\n  replace_value:\n    dev: target\n  is_regex: 'true'\n",
    ] {
        let mut host = Harness::new();
        input(&host);
        std::fs::write(host.root.join("parameter.yml"), contents).unwrap();
        std::fs::write(host.root.join("deploy.yml"), format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter: parameter.yml\nunpublish:\n  skip: true\n"
        )).unwrap();
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-tenv", "dev", "-f"])
            .await;
        assert_ne!(output.plan.exit_code, 0);
        assert!(host.requests().is_empty());
        assert!(host.credential.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn configured_regex_jsonpath_programs_reach_actual_source_derived_post_parts() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/transform_program_source.json"
    ))
    .unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 4);
    for case in fixture["cases"].as_array().unwrap() {
        let mut host = Harness::new();
        input(&host);
        std::fs::write(
            host.root
                .join("repository")
                .join("Book.Notebook")
                .join("notebook-content.py"),
            case["input"].as_str().unwrap(),
        )
        .unwrap();
        std::fs::write(
            host.root.join("parameter.yml"),
            serde_json::to_string(&case["parameters"]).unwrap(),
        )
        .unwrap();
        std::fs::write(host.root.join("deploy.yml"), format!(
                "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter: parameter.yml\nunpublish:\n  skip: true\n"
            )).unwrap();
        metadata(&host, false);
        host.reply(201, json!({"id":ITEM}));
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-tenv", "dev", "-f"])
            .await;
        success(&output);
        assert_eq!(
            host.requests()[2]["body"]["definition"]["parts"][1],
            case["expectedPart"],
            "{}",
            case["id"]
        );
        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request["method"] == "POST")
                .count(),
            1
        );
        assert!(
            matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result)) if result.completed[0].outcome.item_id == ITEM)
        );
    }
}

#[tokio::test]
async fn malformed_capture_and_empty_first_match_never_dispatch_mutation() {
    for (pattern, content, needs_discovery) in [("(a)(b)", "ab", false), ("id=(x*);", "id=;", true)]
    {
        let mut host = Harness::new();
        input(&host);
        std::fs::write(
            host.root
                .join("repository")
                .join("Book.Notebook")
                .join("notebook-content.py"),
            content,
        )
        .unwrap();
        std::fs::write(host.root.join("parameter.yml"), json!({
                "find_replace":[{"find_value":pattern,"is_regex":"true","replace_value":{"dev":"new"}}],
            }).to_string()).unwrap();
        std::fs::write(host.root.join("deploy.yml"), format!(
                "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\n  parameter: parameter.yml\nunpublish:\n  skip: true\n"
            )).unwrap();
        if needs_discovery {
            metadata(&host, false);
        }
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-tenv", "dev", "-f"])
            .await;
        assert_ne!(output.plan.exit_code, 0);
        assert!(
            host.requests()
                .iter()
                .all(|request| request["method"] == "GET")
        );
        assert!(matches!(&output.evidence, FamilyEvidence::Deploy(Err(_))));
    }
}
