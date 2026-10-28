// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent central-host cases from Python cf67fd52ea8b47e24c725e73d792de6f4a08e2e4.
//! Sources: fs/set/fab_fs_set_folder.py, fs/get/fab_fs_get_folder.py,
//! fs/rm/fab_fs_rm_workspace.py and client/fab_api_utils.py.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

mod family_support;

use fabric_cli::{
    families::{FamilyEvidence, FamilyOutput, FamilySession},
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_mutations::{CacheEffect, CommandFuture, MutationUi, NoticeKind, RemovalUi};
use fabric_cli_output::Timestamp;
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use family_support::{Harness, success};
use serde_json::{Value, json};
use std::{future::pending, sync::Arc, time::Duration};

#[derive(Default)]
struct Ui {
    selected: Vec<String>,
    confirmed: bool,
    choices: Vec<(String, Vec<String>)>,
    confirmations: Vec<String>,
    notices: Vec<String>,
}

impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, message: &str) {
        self.notices.push(message.into());
    }

    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool> {
        self.confirmations.push(message.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}

impl RemovalUi for Ui {
    fn select<'a>(
        &'a mut self,
        prompt: &'a str,
        names: &'a [String],
    ) -> CommandFuture<'a, Vec<String>> {
        self.choices.push((prompt.into(), names.to_vec()));
        Box::pin(std::future::ready(self.selected.clone()))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

async fn execute(host: &mut Harness, args: &[&str], ui: &mut Ui) -> FamilyOutput {
    let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
    let sdk = Client::new(
        host.credential.clone(),
        CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = HttpExecutor::new(
        host.http.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )
    .unwrap();
    let tenant = Element::tenant("Tenant", "tenant");
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: tenant.clone(),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2026-10-28T21:40:00Z").unwrap(),
        config: Default::default(),
    };
    let store = ConfigStore::new(&host.root);
    let mut navigation = NavigationState::new(
        PathElement::from_fabric(tenant),
        HomeContext::Unspecified,
        host.root.to_string_lossy().into_owned(),
        37,
        true,
    )
    .unwrap();
    let mut mutation_ui = Ui {
        confirmed: ui.confirmed,
        ..Default::default()
    };
    let transfer_ui = family_support::TransferUiCapture::default();
    let removing = matches!(
        &invocation,
        fabric_cli::parser::Invocation::Resource(
            fabric_cli_mutations::frontdoor::ResourceCommand::Remove { .. }
        )
    );
    let mut session = FamilySession {
        reads: &reads,
        http: &executor,
        store: &store,
        navigation: &mut navigation,
        local: &mut host.effects,
        mutations: &mut mutation_ui,
        confirmation: None,
        transfer_ui: &transfer_ui,
        transfer_files: None,
        resource_files: None,
    };
    let output = if removing {
        session
            .dispatch_with_removal_ui(&invocation, ui, pending())
            .await
    } else {
        session.dispatch(&invocation, pending()).await
    }
    .unwrap();
    if !removing {
        *ui = mutation_ui;
    }
    assert!(host.http.replies.lock().unwrap().is_empty());
    output
}

fn workspace(host: &Harness) {
    host.reply(
        200,
        json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
    );
}

fn folders(host: &Harness) {
    host.reply(200, json!({"value":[{"id":"f","displayName":"Chosen"}]}));
}

fn failure(output: &FamilyOutput, code: &str) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
    let error: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["result"]["error_code"], code);
}

#[tokio::test]
async fn folder_set_reads_after_confirmation_patches_only_selected_property_and_keeps_cache() {
    let mut host = Harness::new();
    workspace(&host);
    folders(&host);
    host.reply(
        200,
        json!({"id":"f","displayName":"Chosen","parentFolderId":null}),
    );
    host.reply(200, json!({"id":"f","displayName":"After"}));
    let output = host
        .execute(&[
            "set", "Demo.Workspace/Chosen.Folder", "-q", "displayName", "-i", "After", "-f",
        ])
        .await;
    let stdout = success(&output);
    assert!(stdout.contains("Folder updated"));
    let requests = host.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2]["method"], "GET");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/folders/f")
    );
    assert_eq!(requests[3]["method"], "PATCH");
    assert_eq!(requests[3]["body"], json!({"displayName":"After"}));
    assert!(
        matches!(&output.evidence, FamilyEvidence::Resource(Ok(result))
        if matches!(&result.effects[..], [CacheEffect::Renamed { name, .. }] if name == "After"))
    );
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(host.requests().len(), 4);
}

#[tokio::test]
async fn folder_set_decline_and_blocked_query_do_not_fetch_metadata_or_patch() {
    for (query, expected) in [("displayName", None), ("id", Some("InvalidQuery"))] {
        let mut host = Harness::new();
        workspace(&host);
        folders(&host);
        let mut ui = Ui::default();
        let output = execute(
            &mut host,
            &[
                "set", "Demo.Workspace/Chosen.Folder", "-q", query, "-i", "After",
            ],
            &mut ui,
        )
        .await;
        if let Some(code) = expected {
            failure(&output, code);
            assert!(ui.confirmations.is_empty());
        } else {
            success(&output);
            assert_eq!(ui.confirmations, ["Are you sure?"]);
            assert!(
                matches!(&output.evidence, FamilyEvidence::Resource(Ok(result)) if result.declined)
            );
        }
        assert_eq!(host.requests().len(), 2);
    }
}

#[tokio::test]
async fn folder_set_only_status_200_emits_success_and_rename() {
    for status in [201, 202, 204] {
        let mut host = Harness::new();
        workspace(&host);
        folders(&host);
        host.reply(200, json!({"id":"f","displayName":"Chosen"}));
        host.reply(status, Value::Null);
        let output = host
            .execute(&[
                "set", "Demo.Workspace/Chosen.Folder", "-q", "displayName", "-i", "After", "-f",
            ])
            .await;
        assert!(success(&output).is_empty());
        assert!(
            matches!(&output.evidence, FamilyEvidence::Resource(Ok(result))
            if result.effects.is_empty() && result.outcome.is_some())
        );
    }
}

#[tokio::test]
async fn folder_set_metadata_failure_never_dispatches_a_patch() {
    let mut host = Harness::new();
    workspace(&host);
    folders(&host);
    host.reply(403, json!({"errorCode":"Forbidden","message":"denied"}));
    let output = host
        .execute(&[
            "set", "Demo.Workspace/Chosen.Folder", "-q", "displayName", "-i", "After", "-f",
        ])
        .await;
    failure(&output, "Forbidden");
    assert_eq!(host.requests().len(), 3);
    assert!(
        host.requests()
            .iter()
            .all(|request| request["method"] == "GET")
    );
}

#[tokio::test]
async fn forced_workspace_reads_all_known_items_before_one_workspace_delete() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(
        200,
        json!({"value":[
            {"id":"d","displayName":"NotRemovable","type":"Dashboard"},
            {"id":"n","displayName":"Book","type":"Notebook"},
            {"id":"future","displayName":"Ignored","type":"FutureItem"}
        ]}),
    );
    host.reply(202, Value::Null);
    let output = host
        .execute(&["rm", "Demo.Workspace", "-f", "--hard"])
        .await;
    assert!(success(&output).contains("'Demo.Workspace' is being deleted"));
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["method"], "GET");
    assert_eq!(requests[2]["method"], "DELETE");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w")
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Removal(Ok(result))
        if result.source_deleted_count == 1 && result.effects.len() == 1)
    );
}

#[tokio::test]
async fn forced_workspace_catalog_failure_cannot_authorize_deletion() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(403, json!({"errorCode":"Forbidden","message":"denied"}));
    let output = host.execute(&["rm", "Demo.Workspace", "-f"]).await;
    failure(&output, "Forbidden");
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn forced_workspace_must_finish_catalog_paging_before_deleting() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(200, json!({
        "value":[{"id":"a","displayName":"A","type":"Notebook"}],
        "continuationToken":"next",
        "continuationUri":"https://api.fabric.microsoft.com/v1/workspaces/w/items?continuationToken=next"
    }));
    host.reply(403, json!({"errorCode":"Forbidden","message":"denied"}));
    let output = host.execute(&["rm", "Demo.Workspace", "-f"]).await;
    failure(&output, "Forbidden");
    assert_eq!(host.requests().len(), 3);
    assert!(host.requests().iter().all(|request| request["method"] == "GET"));
}

#[tokio::test]
async fn forced_workspace_invalid_item_parent_is_not_silently_counted_or_deleted() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(200, json!({"value":[
        {"id":"n","displayName":"Book","type":"Notebook","folderId":"missing"}
    ]}));
    host.reply(200, json!({"value":[]}));
    let output = host.execute(&["rm", "Demo.Workspace", "-f"]).await;
    failure(&output, "InvalidResponse");
    assert_eq!(host.requests().len(), 3);
    assert!(host.requests().iter().all(|request| request["method"] == "GET"));
}

#[tokio::test]
async fn missing_selection_capability_rejects_nonforce_workspace_and_forced_tenant() {
    for tenant in [false, true] {
        let mut host = Harness::new();
        let args = if tenant {
            vec!["rm", "/", "-f"]
        } else {
            workspace(&host);
            vec!["rm", "Demo.Workspace"]
        };
        let output = host.execute(&args).await;
        failure(&output, "NotSupported");
        assert!(host.requests().iter().all(|request| request["method"] == "GET"));
    }
}

#[tokio::test]
async fn workspace_selection_sorts_supported_nested_items_and_confirms_before_hard_delete() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(200, json!({"value":[
        {"id":"z","displayName":"zeta","type":"Notebook","folderId":"f"},
        {"id":"a","displayName":"Alpha","type":"Report"},
        {"id":"d","displayName":"blocked","type":"Dashboard"}
    ]}));
    folders(&host);
    host.reply(200, Value::Null);
    let mut ui = Ui {
        selected: vec!["zeta.Notebook".into()],
        confirmed: true,
        ..Default::default()
    };
    let output = execute(&mut host, &["rm", "Demo.Workspace", "--hard"], &mut ui).await;
    assert!(success(&output).contains("1 items deleted successfully"));
    assert_eq!(ui.choices.len(), 1);
    assert_eq!(ui.choices[0].0, "Select items:");
    assert_eq!(ui.choices[0].1, ["Alpha.Report", "zeta.Notebook"]);
    assert_eq!(ui.confirmations, ["Are you sure?"]);
    assert!(host.requests().last().unwrap()["url"].as_str().unwrap().ends_with("/items/z?hardDelete=true"));
    assert!(matches!(&output.evidence, FamilyEvidence::Removal(Ok(result))
        if matches!(&result.effects[..], [CacheEffect::Deleted { target }]
            if target.parent().unwrap().kind() == fabric_cli_context::NodeKind::Folder)));
}

#[tokio::test]
async fn workspace_type_sort_and_declined_confirmation_preserve_no_mutation() {
    let mut host = Harness::new();
    let store = ConfigStore::new(&host.root);
    let mut settings = store.read().unwrap();
    settings.insert("output_item_sort_criteria".into(), json!("bytype"));
    store.write(&settings).unwrap();
    workspace(&host);
    host.reply(200, json!({"value":[
        {"id":"n","displayName":"Z","type":"Notebook"},
        {"id":"r","displayName":"A","type":"Report"}
    ]}));
    let mut ui = Ui {
        selected: vec!["A.Report".into(), "Z.Notebook".into()],
        ..Default::default()
    };
    let output = execute(&mut host, &["rm", "Demo.Workspace"], &mut ui).await;
    success(&output);
    assert_eq!(ui.choices[0].1, ["Z.Notebook", "A.Report"]);
    assert_eq!(ui.confirmations, ["Are you sure?"]);
    assert!(matches!(&output.evidence, FamilyEvidence::Removal(Ok(result)) if result.declined));
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn empty_or_rejected_workspace_and_declined_selection_never_delete_workspace() {
    for items in [
        json!([]),
        json!([{"id":"d","displayName":"Blocked","type":"Dashboard"}]),
        json!([{"id":"n","displayName":"Book","type":"Notebook"}]),
    ] {
        let mut host = Harness::new();
        workspace(&host);
        host.reply(200, json!({"value":items}));
        let mut ui = Ui::default();
        let output = execute(&mut host, &["rm", "Demo.Workspace"], &mut ui).await;
        if items.as_array().unwrap().first().is_some_and(|item| item["type"] == "Notebook") {
            success(&output);
            assert!(matches!(&output.evidence, FamilyEvidence::Removal(Ok(result)) if result.declined));
        } else {
            failure(&output, "NotSupported");
        }
        assert!(ui.confirmations.is_empty());
        assert_eq!(host.requests().len(), 2);
    }
}

#[tokio::test]
async fn tenant_force_preserves_selection_confirmation_and_catalog_order_not_ui_order() {
    let mut host = Harness::new();
    host.reply(200, json!({"value":[
        {"id":"a","displayName":"a","type":"Workspace"},
        {"id":"b","displayName":"B","type":"Workspace"},
        {"id":"A","displayName":"A","type":"Workspace"}
    ]}));
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let mut ui = Ui {
        selected: vec!["a.Workspace".into(), "B.Workspace".into()],
        confirmed: true,
        ..Default::default()
    };
    let output = execute(&mut host, &["rm", "/", "-f"], &mut ui).await;
    assert!(success(&output).contains("2 workspaces deleted successfully"));
    assert_eq!(ui.choices[0].1, ["A.Workspace", "B.Workspace", "a.Workspace"]);
    assert_eq!(ui.confirmations, ["Are you sure?"]);
    let requests = host.requests();
    assert!(requests[1]["url"].as_str().unwrap().ends_with("/workspaces/b"));
    assert!(requests[2]["url"].as_str().unwrap().ends_with("/workspaces/a"));
}

#[tokio::test]
async fn tenant_force_decline_does_not_skip_confirmation() {
    let mut host = Harness::new();
    workspace(&host);
    let mut ui = Ui {
        selected: vec!["Demo.Workspace".into()],
        ..Default::default()
    };
    let output = execute(&mut host, &["rm", "/", "-f"], &mut ui).await;
    success(&output);
    assert_eq!(ui.confirmations, ["Are you sure?"]);
    assert!(matches!(&output.evidence, FamilyEvidence::Removal(Ok(result)) if result.declined));
    assert_eq!(host.requests().len(), 1);
}

#[tokio::test]
async fn ordered_batch_failure_keeps_prior_accepted_output_effects_and_original_error() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(200, json!({"value":[
        {"id":"a","displayName":"A","type":"Notebook"},
        {"id":"b","displayName":"B","type":"Notebook"}
    ]}));
    host.reply(202, Value::Null);
    host.reply(403, json!({"errorCode":"Forbidden","message":"denied"}));
    let mut ui = Ui {
        selected: vec!["B.Notebook".into(), "A.Notebook".into()],
        confirmed: true,
        ..Default::default()
    };
    let output = execute(&mut host, &["rm", "Demo.Workspace"], &mut ui).await;
    failure(&output, "Forbidden");
    let mut stdout = Vec::new();
    output.write_to(&mut stdout, &mut Vec::new()).unwrap();
    let stdout = String::from_utf8(stdout).unwrap();
    assert!(stdout.contains("'A.Notebook' deleted"));
    assert!(!stdout.contains("items deleted successfully"));
    assert!(matches!(&output.evidence, FamilyEvidence::Removal(Err(error))
        if error.partial.responses.len() == 1 && error.partial.effects.len() == 1
            && error.error.operation.is_some()));
    assert_eq!(host.requests().len(), 4);
}
