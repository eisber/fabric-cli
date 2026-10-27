// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual root argv -> production config host -> real shared HTTP/file adapters.

#![cfg(feature = "native-read")]

mod family_support;
use fabric_cli::{deploy::DeployFailure, families::FamilyEvidence};
use fabric_deploy::publish::PublishFailure;
use family_support::{Harness, success};
use serde_json::{Value, json};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

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
    let mut host = Harness::new();
    input(&host);
    metadata(&host, false);
    host.headers(
        202,
        Value::Null,
        &[("x-ms-operation-id", OPERATION), ("retry-after", "0")],
    );
    host.reply(200, json!({"status":"Succeeded"}));
    host.reply(200, json!({"id":ITEM}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result)) if result.completed[0].outcome.item_id == ITEM)
    );
    assert!(
        host.requests()[4]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/operations/{OPERATION}/result"))
    );
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
