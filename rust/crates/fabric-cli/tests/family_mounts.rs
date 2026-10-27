// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root parser -> central session -> released production family -> shared HTTP.

#![cfg(feature = "native-read")]

mod family_support;
use fabric_cli::families::FamilyEvidence;
use family_support::{Harness, success};
use serde_json::json;

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const CREATED: &str = "33333333-3333-3333-3333-333333333333";

#[tokio::test]
async fn root_shortcut_uses_real_storage_properties_and_existing_shortcut_post() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.reply(404, serde_json::Value::Null);
    host.reply(404, serde_json::Value::Null);
    host.reply(200, json!({"name":"Chosen"}));
    let output = host
        .execute(&[
            "ln",
            "Demo.Workspace/Book.Lakehouse/Files/Chosen.Shortcut",
            "--type",
            "oneLake",
            "-i",
            "{'workspaceId':'other','itemId':'target','path':'Files'}",
            "-f",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[2]["method"], "HEAD");
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/items/n/shortcuts?shortcutConflictPolicy=GenerateUniqueName")
    );
    assert_eq!(
        requests[4]["body"]["target"]["oneLake"]["workspaceId"],
        "other"
    );
}

#[tokio::test]
async fn root_storage_copy_and_move_keep_distinct_stage_evidence() {
    for leaf in ["cp", "mv"] {
        let mut host = Harness::new();
        host.item("Lakehouse");
        host.headers(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "file")],
        );
        host.reply(404, serde_json::Value::Null);
        if leaf == "cp" {
            host.headers(
                200,
                json!("abc"),
                &[("Content-Length", "3"), ("Content-Type", "text/csv")],
            );
            host.reply(201, serde_json::Value::Null);
            host.reply(202, serde_json::Value::Null);
            host.reply(200, serde_json::Value::Null);
        } else {
            host.reply(200, serde_json::Value::Null);
        }
        let output = host
            .execute(&[
                leaf,
                "Demo.Workspace/Book.Lakehouse/Files/from.csv",
                "Demo.Workspace/Book.Lakehouse/Files/to.csv",
                "-f",
            ])
            .await;
        success(&output);
        assert!(matches!(&output.evidence, FamilyEvidence::Storage(Ok(_))));
        let requests = host.requests();
        assert_eq!(requests[2]["method"], "HEAD");
        assert!(
            requests[2]["url"]
                .as_str()
                .unwrap()
                .starts_with("https://onelake.dfs.fabric.microsoft.com/w/n/")
        );
        assert_eq!(
            requests.last().unwrap()["method"],
            if leaf == "cp" { "PATCH" } else { "PUT" }
        );
        assert!(
            host.credential
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|scopes| scopes == &[fabric_sdk::Audience::OneLake.scope()])
        );
    }
}

#[tokio::test]
async fn root_export_and_bulk_export_write_real_definition_files() {
    for bulk in [false, true] {
        let mut host = Harness::new();
        std::fs::create_dir(host.root.join("output")).unwrap();
        host.metadata_with_id(
            WORKSPACE,
            json!([{"id":ITEM,"displayName":"Book","type":"DataPipeline"}]),
        );
        if bulk {
            host.reply(200, json!({
                "definitionParts":[{"path":"/Book.DataPipeline/pipeline-content.json", "payload":"eyJuYW1lIjoic3ludGhldGljIn0=", "payloadType":"InlineBase64"}],
                "itemDefinitionsIndex":[{"id":ITEM,"rootPath":"/Book.DataPipeline"}],
            }));
        } else {
            host.reply(200, json!({"id":ITEM}));
            host.reply(200, json!({"definition":{"parts":[{
                "path":"pipeline-content.json","payload":"eyJuYW1lIjoic3ludGhldGljIn0=","payloadType":"InlineBase64",
            }]}}));
        }
        let output = if bulk {
            host.execute(&[
                "bulk-export",
                "Demo.Workspace",
                "-o",
                "output",
                "--recursive",
                "-f",
            ])
            .await
        } else {
            host.execute(&[
                "export",
                "Demo.Workspace/Book.DataPipeline",
                "-o",
                "output",
                "-f",
            ])
            .await
        };
        success(&output);
        let file = host
            .root
            .join("output")
            .join("Book.DataPipeline")
            .join("pipeline-content.json");
        let contents: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(contents, json!({"name":"synthetic"}));
        assert!(
            matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result)) if result.completed_paths == ["Book.DataPipeline/pipeline-content.json"])
        );
        assert_eq!(host.requests().last().unwrap()["method"], "POST");
    }
}

#[tokio::test]
async fn root_import_preserves_actual_create_id_and_deferred_cache_after_failed_delivery() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join("input")).unwrap();
    std::fs::write(
        host.root.join("input").join("pipeline-content.json"),
        br#"{"name":"synthetic"}"#,
    )
    .unwrap();
    std::fs::write(
        host.root.join("input").join("cache.abf"),
        b"must be excluded",
    )
    .unwrap();
    host.metadata_with_id(WORKSPACE, json!([]));
    host.reply(201, json!({"id":CREATED}));
    let output = host
        .execute(&[
            "import",
            "Demo.Workspace/Book.DataPipeline",
            "-i",
            "input",
            "-f",
        ])
        .await;
    success(&output);
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
        if result.effects.len() == 1 && result.effects[0].item_id == CREATED
        && result.excluded_paths.iter().any(|path| path.ends_with("cache.abf")))
    );
    assert_eq!(host.requests().len(), 3);
    assert_eq!(host.requests()[2]["method"], "POST");
    assert_eq!(host.requests()[2]["body"]["type"], "DataPipeline");
}

#[tokio::test]
async fn root_acl_read_and_mutation_leaves_preserve_requests_and_streams() {
    let rows = json!({"value":[{"id":"assignment","role":"Admin","principal":{
        "id":"person","type":"User","displayName":"Alice","userDetails":{"userPrincipalName":"alice@example.test"},
    }}]});
    for leaf in ["ls", "get"] {
        let mut host = Harness::new();
        host.reply(
            200,
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        );
        host.reply(200, rows.clone());
        let output = if leaf == "get" {
            host.execute(&["acl", leaf, "Demo.Workspace", "-q", "."])
                .await
        } else {
            host.execute(&["acl", leaf, "Demo.Workspace"]).await
        };
        let stdout = success(&output);
        let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            envelope["result"]["data"],
            if leaf == "get" {
                rows["value"].clone()
            } else {
                json!([{"acl":"Admin","identity":"alice@example.test","type":"User"}])
            }
        );
        assert_eq!(host.requests().len(), 2);
        assert!(
            host.requests()[1]["url"]
                .as_str()
                .unwrap()
                .ends_with("/workspaces/w/roleAssignments")
        );
    }
    let mut add = Harness::new();
    add.reply(
        200,
        json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
    );
    add.reply(200, json!({"value":[]}));
    add.reply(201, json!({}));
    let output = add
        .execute(&[
            "acl",
            "set",
            "Demo.Workspace",
            "-I",
            "person",
            "-R",
            "viewer",
            "-f",
        ])
        .await;
    assert!(success(&output).contains("ACL set"));
    assert_eq!(add.requests()[2]["body"]["principal"]["id"], "person");
    let mut remove = Harness::new();
    remove.reply(
        200,
        json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
    );
    remove.reply(200, rows);
    remove.reply(200, json!({}));
    let output = remove
        .execute(&["acl", "rm", "Demo.Workspace", "-I", "person", "-f"])
        .await;
    assert!(success(&output).contains("ACL removed"));
    assert_eq!(remove.requests()[2]["method"], "DELETE");
    assert!(
        remove.requests()[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/assignment")
    );
}

#[tokio::test]
async fn root_label_local_and_both_mutations_use_configured_definitions() {
    for leaf in ["list-local", "set", "rm"] {
        let mut host = Harness::new();
        let definitions = host.root.join("labels.json");
        std::fs::write(
            &definitions,
            br#"{"labels":[{"name":"Public","id":"label"}]}"#,
        )
        .unwrap();
        let store = fabric_cli_local::ConfigStore::new(&host.root);
        let mut settings = store.read().unwrap();
        settings.insert("local_definition_labels".into(), json!(definitions));
        store.write(&settings).unwrap();
        let output = if leaf == "list-local" {
            host.execute(&["label", leaf]).await
        } else {
            host.item("Notebook");
            host.reply(200, json!({}));
            if leaf == "set" {
                host.execute(&[
                    "label",
                    leaf,
                    "Demo.Workspace/Book.Notebook",
                    "-n",
                    "Public",
                    "-f",
                ])
                .await
            } else {
                host.execute(&["label", leaf, "Demo.Workspace/Book.Notebook", "-f"])
                    .await
            }
        };
        let stdout = success(&output);
        if leaf == "list-local" {
            assert!(stdout.contains("Public"));
            assert!(host.credential.0.lock().unwrap().is_empty());
        } else {
            let requests = host.requests();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2]["body"]["items"][0]["id"], "n");
            assert!(
                requests[2]["url"]
                    .as_str()
                    .unwrap()
                    .contains(if leaf == "set" {
                        "bulkSetLabels"
                    } else {
                        "bulkRemoveLabels"
                    })
            );
        }
    }
}

#[tokio::test]
async fn root_remove_and_set_keep_cache_evidence_after_writer_failure() {
    let mut remove = Harness::new();
    remove.item("Notebook");
    remove.reply(200, json!({}));
    let output = remove
        .execute(&["rm", "Demo.Workspace/Book.Notebook", "-f"])
        .await;
    success(&output);
    assert_eq!(remove.requests()[2]["method"], "DELETE");
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Resource(Ok(result)) if !result.effects.is_empty())
    );
    assert_eq!(
        remove.requests().len(),
        3,
        "output failure must never replay"
    );
    let mut set = Harness::new();
    set.item("Notebook");
    set.reply(
        200,
        json!({"id":"n","displayName":"Book","type":"Notebook","description":"before"}),
    );
    set.reply(200, json!({}));
    let output = set
        .execute(&[
            "set",
            "Demo.Workspace/Book.Notebook",
            "-q",
            "description",
            "-i",
            "after",
            "-f",
        ])
        .await;
    success(&output);
    assert_eq!(set.requests().len(), 4);
    assert_eq!(set.requests()[3]["method"], "PATCH");
    assert_eq!(set.requests()[3]["body"]["description"], "after");
}

#[tokio::test]
async fn root_start_stop_use_mirroring_state_then_the_existing_action() {
    for (leaf, state, action) in [
        ("start", "Stopped", "startMirroring"),
        ("stop", "Running", "stopMirroring"),
    ] {
        let mut host = Harness::new();
        host.item("MirroredDatabase");
        host.reply(200, json!({"status":state}));
        host.reply(200, json!({}));
        let output = host
            .execute(&[leaf, "Demo.Workspace/Book.MirroredDatabase", "-f"])
            .await;
        success(&output);
        assert_eq!(host.requests().len(), 4);
        assert!(
            host.requests()[3]["url"]
                .as_str()
                .unwrap()
                .ends_with(action)
        );
        assert_eq!(host.requests()[3]["method"], "POST");
    }
}

#[tokio::test]
async fn root_assign_unassign_resolve_real_capacity_and_workspace_ids() {
    for leaf in ["assign", "unassign"] {
        let mut host = Harness::new();
        host.reply(200, json!({"value":[{"id":"cap","displayName":"Chosen"}]}));
        host.reply(
            200,
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        );
        if leaf == "unassign" {
            host.reply(200, json!({"capacityId":"cap"}));
        }
        host.reply(200, json!({}));
        let output = host
            .execute(&[
                leaf,
                "/.capacities/Chosen.Capacity",
                "-W",
                "Demo.Workspace",
                "-f",
            ])
            .await;
        success(&output);
        let requests = host.requests();
        assert!(
            requests.last().unwrap()["url"]
                .as_str()
                .unwrap()
                .contains(if leaf == "assign" {
                    "assignToCapacity"
                } else {
                    "unassignFromCapacity"
                })
        );
        assert_eq!(requests.last().unwrap()["method"], "POST");
    }
}

#[tokio::test]
async fn root_access_headerless_accepted_response_retains_uncertainty_without_replay() {
    let mut host = Harness::new();
    let definitions = host.root.join("labels.json");
    std::fs::write(&definitions, br#"{"labels":[]}"#).unwrap();
    let store = fabric_cli_local::ConfigStore::new(&host.root);
    let mut settings = store.read().unwrap();
    settings.insert("local_definition_labels".into(), json!(definitions));
    store.write(&settings).unwrap();
    host.item("Notebook");
    host.reply(202, serde_json::Value::Null);
    let output = host
        .execute(&["label", "rm", "Demo.Workspace/Book.Notebook", "-f"])
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Access(Err(error)) if error.mutation_may_have_occurred)
    );
    assert_eq!(host.requests().len(), 3);
}

#[tokio::test]
async fn root_open_launches_the_injected_browser_only_after_real_metadata() {
    let mut host = Harness::new();
    host.item("Notebook");
    let output = host
        .execute(&["open", "Demo.Workspace/Book.Notebook"])
        .await;
    success(&output);
    assert_eq!(host.effects.browser.len(), 1);
    assert!(host.effects.browser[0].starts_with("https://app.fabric.microsoft.com/groups/w/"));
    assert!(host.effects.browser[0].contains("n"));
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn root_exists_and_mkdir_use_resolved_ids_and_real_post() {
    let mut exists = Harness::new();
    exists.item("Notebook");
    let output = exists
        .execute(&["exists", "Demo.Workspace/Book.Notebook"])
        .await;
    assert!(success(&output).contains("true"));
    assert_eq!(exists.requests().len(), 2);
    let mut mkdir = Harness::new();
    mkdir.metadata(json!([]));
    mkdir.reply(
        201,
        json!({"id":"created","displayName":"Book","type":"Notebook"}),
    );
    let output = mkdir
        .execute(&["mkdir", "Demo.Workspace/Book.Notebook"])
        .await;
    success(&output);
    assert!(matches!(output.evidence, FamilyEvidence::Resource(Ok(_))));
    let requests = mkdir.requests();
    assert_eq!(requests.last().unwrap()["method"], "POST");
    assert!(
        requests.last().unwrap()["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/notebooks"),
        "{requests:?}"
    );
    assert_eq!(requests.last().unwrap()["body"]["type"], "Notebook");
}

#[tokio::test]
async fn root_api_and_find_reach_the_real_family_without_metadata_prefetch() {
    let mut api = Harness::new();
    api.reply(200, json!({"value":[{"id":"w"}]}));
    let output = api.execute(&["api", "workspaces"]).await;
    assert!(success(&output).contains("w"));
    assert_eq!(api.requests().len(), 1);
    let mut find = Harness::new();
    find.reply(200, json!({"value":[]}));
    let output = find.execute(&["find", "Book"]).await;
    success(&output);
    assert_eq!(find.requests().len(), 1);
    assert_eq!(find.requests()[0]["method"], "POST");
    assert!(
        find.requests()[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/catalog/search")
    );
}
