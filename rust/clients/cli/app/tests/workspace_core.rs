// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Workspace root invocations through the released session and shared HTTP.

#![cfg(any(feature = "native-read", feature = "headless"))]
#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod family_support;

use fabric_cli::{
    families::{FamilyEvidence, FamilyOutput},
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use family_support::{Harness, success};
use serde_json::{Value, json};
use std::{future::pending, sync::Arc, time::Duration};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const HOSTS: [&str; 2] = ["api.fabric.microsoft.com", "msitapi.fabric.microsoft.com"];

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == Duration::from_secs(240) {
                pending::<()>().await;
            }
        })
    }
}

fn configured(hostname: &str) -> Harness {
    let mut host = Harness::new();
    host.endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, hostname).unwrap();
    host
}

fn discovery(host: &Harness) {
    host.reply(
        200,
        json!({"value":[{"id":WORKSPACE,"displayName":"Demo","type":"Workspace"}]}),
    );
}

fn metadata(host: &Harness) {
    host.reply(
        200,
        json!({"id":WORKSPACE,"displayName":"Demo","description":"before"}),
    );
    host.reply(200, json!({"value":[]}));
    host.reply(200, json!({}));
    host.reply(200, json!({"value":[]}));
}

fn assert_binding(host: &Harness, hostname: &str) {
    let prefix = format!("https://{hostname}/v1/");
    assert!(
        host.requests()
            .iter()
            .all(|request| request["url"].as_str().unwrap().starts_with(&prefix))
    );
    let scopes = host.credential.0.lock().unwrap();
    assert!(!scopes.is_empty());
    assert!(
        scopes
            .iter()
            .all(|scopes| scopes == &[Audience::Fabric.scope()])
    );
    assert!(host.http.replies.lock().unwrap().is_empty());
}

async fn read(host: &Harness, args: &[&str]) -> Value {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("workspace read invocation required");
    };
    let sdk = Client::with_context(RequestContext::with_endpoint(
        host.credential.clone(),
        CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
        host.endpoint.clone(),
    ));
    let executor = HttpExecutor::new(
        host.http.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(host.endpoint.clone());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2026-11-10T21:00:00Z").unwrap(),
        config: Default::default(),
    };
    let plan = session
        .with_source_operations(&executor)
        .dispatch(&command, pending())
        .await
        .unwrap();
    assert_eq!(plan.exit_code, 0, "{plan:?}");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    serde_json::from_slice(&stdout).unwrap()
}

fn failure(output: &FamilyOutput, code: &str) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_ne!(output.write_to(&mut stdout, &mut stderr).unwrap(), 0);
    assert!(stderr.is_empty());
    assert!(String::from_utf8(stdout).unwrap().contains(code));
}

#[tokio::test]
async fn root_workspace_and_item_listing_use_selected_host_and_real_collection_routes() {
    for hostname in HOSTS {
        let host = configured(hostname);
        discovery(&host);
        let output = read(&host, &["ls", "/", "--output_format", "json"]).await;
        assert_eq!(output["result"]["data"][0]["name"], "Demo.Workspace");
        assert_eq!(host.requests().len(), 1);
        assert_binding(&host, hostname);

        let host = configured(hostname);
        discovery(&host);
        host.reply(
            200,
            json!({"value":[{"id":"item","displayName":"Book","type":"Notebook"}]}),
        );
        let output = read(&host, &["ls", "/Demo.Workspace", "--output_format", "json"]).await;
        assert_eq!(output["result"]["data"][0]["name"], "Book.Notebook");
        assert_eq!(host.requests().len(), 2);
        assert!(
            host.requests()[1]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/workspaces/{WORKSPACE}/items"))
        );
        assert_binding(&host, hostname);
    }
}

#[tokio::test]
async fn root_workspace_get_reads_primary_and_existing_enrichments_before_projection() {
    for hostname in HOSTS {
        let host = configured(hostname);
        discovery(&host);
        metadata(&host);
        let output = read(
            &host,
            &[
                "get",
                "/Demo.Workspace",
                "-q",
                ".",
                "--output_format",
                "json",
            ],
        )
        .await;
        assert_eq!(output["result"]["data"][0]["id"], WORKSPACE);
        assert_eq!(output["result"]["data"][0]["description"], "before");
        assert_eq!(host.requests().len(), 5);
        assert!(
            host.requests()
                .iter()
                .all(|request| request["method"] == "GET")
        );
        assert_binding(&host, hostname);
    }
}

#[tokio::test]
async fn root_workspace_exists_and_absence_do_not_send_mutations_or_primary_get() {
    for hostname in HOSTS {
        for present in [true, false] {
            let mut host = configured(hostname);
            if present {
                discovery(&host);
            } else {
                host.reply(200, json!({"value":[]}));
            }
            let output = host.execute(&["exists", "/Demo.Workspace"]).await;
            assert!(success(&output).contains(if present { "true" } else { "false" }));
            assert_eq!(host.requests().len(), 1);
            assert_eq!(host.requests()[0]["method"], "GET");
            assert_binding(&host, hostname);
        }
    }
}

#[tokio::test]
async fn root_workspace_mkdir_and_create_alias_post_without_capacity_assignment() {
    for hostname in HOSTS {
        for verb in ["mkdir", "create"] {
            let mut host = configured(hostname);
            host.reply(200, json!({"value":[]}));
            host.reply(201, json!({"id":WORKSPACE,"displayName":"Demo"}));
            let output = host
                .execute(&[
                    verb,
                    "/Demo.Workspace",
                    "-P",
                    "capacityName=none,description=studio",
                ])
                .await;
            success(&output);
            let requests = host.requests();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1]["method"], "POST");
            assert!(
                requests[1]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with("/v1/workspaces")
            );
            assert_eq!(
                requests[1]["body"],
                json!({"displayName":"Demo","description":"studio"})
            );
            assert_binding(&host, hostname);
        }
    }
}

#[tokio::test]
async fn duplicate_workspace_creation_never_posts() {
    let mut host = configured(HOSTS[1]);
    discovery(&host);
    let output = host
        .execute(&["mkdir", "/Demo.Workspace", "-P", "capacityName=none"])
        .await;
    failure(&output, "AlreadyExists");
    assert_eq!(host.requests().len(), 1);
}

#[tokio::test]
async fn workspace_creation_without_usable_identity_fails_without_replay() {
    for (status, id) in [200, 201]
        .into_iter()
        .flat_map(|status| [Value::Null, json!(""), json!(42)].map(|id| (status, id)))
    {
        let mut host = configured(HOSTS[1]);
        host.reply(200, json!({"value":[]}));
        host.reply(status, json!({"id":id,"displayName":"Demo"}));
        let output = host
            .execute(&["mkdir", "/Demo.Workspace", "-P", "capacityName=none"])
            .await;
        failure(&output, "InvalidResponse");
        assert!(
            matches!(&output.evidence, FamilyEvidence::Resource(Err(error))
                if error.operation.as_ref().is_some_and(|operation|
                    operation.status == Some(status) && operation.mutation_may_have_occurred))
        );
        assert_eq!(host.requests().len(), 2);
    }
}

#[tokio::test]
async fn accepted_workspace_creation_does_not_invent_an_id_or_publish_created_output() {
    use fabric_sdk_workspaces::mutations::ResourceOutcome;

    for hostname in HOSTS {
        let mut host = configured(hostname);
        host.reply(200, json!({"value":[]}));
        host.reply(202, Value::Null);
        let output = host
            .execute(&["mkdir", "/Demo.Workspace", "-P", "capacityName=none"])
            .await;
        assert!(
            matches!(&output.evidence, FamilyEvidence::Resource(Ok(result))
                if matches!(result.outcome, Some(ResourceOutcome::Accepted { operation_id: None }))
                    && result.effects.is_empty() && result.outputs.is_empty())
        );
        assert_eq!(host.requests().len(), 2);
        assert_binding(&host, hostname);
    }
}

#[tokio::test]
async fn root_workspace_set_description_and_name_patch_only_the_selected_property() {
    for hostname in HOSTS {
        for (query, value) in [("description", "after"), ("displayName", "Renamed")] {
            let mut host = configured(hostname);
            discovery(&host);
            metadata(&host);
            host.reply(200, json!({}));
            let output = host
                .execute(&["set", "/Demo.Workspace", "-q", query, "-i", value, "-f"])
                .await;
            success(&output);
            let requests = host.requests();
            assert_eq!(requests.len(), 6);
            assert_eq!(requests[5]["method"], "PATCH");
            assert_eq!(requests[5]["body"], json!({query:value}));
            assert!(
                requests[5]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("/workspaces/{WORKSPACE}"))
            );
            assert_binding(&host, hostname);
        }
    }
}

#[tokio::test]
async fn root_workspace_rm_and_del_alias_delete_once_after_item_catalog() {
    for hostname in HOSTS {
        for verb in ["rm", "del"] {
            let mut host = configured(hostname);
            discovery(&host);
            host.reply(200, json!({"value":[]}));
            host.reply(200, json!({}));
            let output = host.execute(&[verb, "/Demo.Workspace", "-f"]).await;
            success(&output);
            let requests = host.requests();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2]["method"], "DELETE");
            assert!(
                requests[2]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("/workspaces/{WORKSPACE}"))
            );
            assert!(
                output
                    .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
                    .is_err()
            );
            assert_eq!(
                host.requests().len(),
                3,
                "output failure must not replay deletion"
            );
            assert_binding(&host, hostname);
        }
    }
}

#[tokio::test]
async fn workspace_creation_poll_and_result_retain_the_selected_endpoint() {
    let operation = "22222222-2222-2222-2222-222222222222";
    for hostname in HOSTS {
        let mut host = configured(hostname);
        host.reply(200, json!({"value":[]}));
        host.headers(202, Value::Null, &[("x-ms-operation-id", operation)]);
        let result = format!("https://{hostname}/v1/operations/{operation}/result");
        host.headers(200, json!({"status":"Succeeded"}), &[("Location", &result)]);
        host.reply(200, json!({"id":WORKSPACE,"displayName":"Demo"}));
        let output = host
            .execute(&["mkdir", "/Demo.Workspace", "-P", "capacityName=none"])
            .await;
        success(&output);
        let requests = host.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1]["method"], "POST");
        assert_eq!(requests[2]["method"], "GET");
        assert_eq!(requests[3]["url"], result);
        assert_binding(&host, hostname);
    }
}

#[tokio::test]
async fn workspace_creation_never_follows_a_public_result_link_from_msit() {
    let operation = "22222222-2222-2222-2222-222222222222";
    let mut host = configured(HOSTS[1]);
    host.reply(200, json!({"value":[]}));
    host.headers(202, Value::Null, &[("x-ms-operation-id", operation)]);
    let foreign = format!("https://api.fabric.microsoft.com/v1/operations/{operation}/result");
    host.headers(
        200,
        json!({"status":"Succeeded"}),
        &[("Location", &foreign)],
    );
    let output = host
        .execute(&["mkdir", "/Demo.Workspace", "-P", "capacityName=none"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert_eq!(host.requests().len(), 3);
    assert_binding(&host, HOSTS[1]);
}

#[tokio::test]
async fn workspace_mutation_provider_denial_is_not_success_or_retried() {
    for (verb, status) in [("create", 403), ("rm", 403)] {
        let mut host = configured(HOSTS[1]);
        if verb == "create" {
            host.reply(200, json!({"value":[]}));
        } else {
            discovery(&host);
            host.reply(200, json!({"value":[]}));
        }
        host.reply(status, json!({"errorCode":"Forbidden","message":"denied"}));
        let args = if verb == "create" {
            vec![verb, "/Demo.Workspace", "-P", "capacityName=none"]
        } else {
            vec![verb, "/Demo.Workspace", "-f"]
        };
        failure(&host.execute(&args).await, "Forbidden");
        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request["method"] != "GET")
                .count(),
            1
        );
    }
}
