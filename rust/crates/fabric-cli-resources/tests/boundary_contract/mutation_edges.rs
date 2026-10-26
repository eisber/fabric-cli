// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_resources::{
    AclChange, Confirmation, LabelChange, MutationOptions, mutate_acl, mutate_label,
};
use fabric_sdk_access::LabelItem;

struct NoPrompts;
impl Confirmation for NoPrompts {
    fn confirm<'a>(
        &'a mut self,
        _: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, CommandError>> + Send + 'a>> {
        panic!("force must not ask for confirmation")
    }
}

#[tokio::test]
async fn source_duplicate_first_match_empty_role_label_defaults_and_malformed_exceptions_stay_distinct()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/mutation_edges.json"
    ))
    .unwrap();
    for (index, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let replies = case["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                reply(
                    r["status"].as_u64().unwrap() as u16,
                    &r["body"].to_string(),
                    &[],
                )
            })
            .collect();
        let (api, wire, _) = setup(replies);
        let client = AccessClient::new(api);
        let options = MutationOptions {
            force: true,
            ..MutationOptions::default()
        };
        let mut prompts = NoPrompts;
        let actual = if case["resource"] == "label" {
            let path = std::env::temp_dir().join(format!(
                "fabric-label-edge-7196-{}-{index}.json",
                std::process::id()
            ));
            std::fs::write(&path, case["definitions"].to_string()).unwrap();
            let result = mutate_label(
                &client,
                &LabelItem {
                    id: "resource".into(),
                    item_type: "Notebook".into(),
                },
                &LabelChange::Set("Public".into()),
                Some(&path),
                options,
                &mut prompts,
                pending(),
            )
            .await;
            std::fs::remove_file(path).unwrap();
            result
        } else {
            let change = if case["command"] == "set" {
                AclChange::Set {
                    identity: case["identity"].as_str().unwrap().into(),
                    role: case["role"].as_str().unwrap().into(),
                }
            } else {
                AclChange::Remove {
                    identity: case["identity"].as_str().unwrap().into(),
                }
            };
            mutate_acl(
                &client,
                &NamedResource {
                    resource: resource("workspace"),
                    name: "Resource".into(),
                },
                &change,
                options,
                &mut prompts,
                pending(),
            )
            .await
        };
        let expected = &case["expected"];
        if expected.get("python_exception").is_some() {
            assert_eq!(actual.unwrap_err().code, "InvalidEntriesFormat");
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            continue;
        }
        if expected["error"].is_null() {
            assert_eq!(
                json!(actual.unwrap().message),
                expected["message"],
                "{}",
                case["name"]
            );
        } else {
            let e = actual.unwrap_err();
            assert_eq!(
                e.code,
                expected["error"]["code"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                e.message,
                expected["error"]["message"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
        let actual:Vec<_>=wire.requests.lock().unwrap().iter().map(|r|{
            let bytes=azure_core::Bytes::from(r.body());
            json!({"method":r.method().to_string(),"url":r.url().as_str(),"body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}})
        }).collect();
        assert_eq!(json!(actual), expected["requests"], "{}", case["name"]);
    }
}

#[test]
fn local_label_buffer_limit_succeeds_at_four_mib_and_rejects_one_extra_byte() {
    let limit = fabric_sdk_api::RequestBounds::default()
        .max_body_bytes
        .get();
    let mut bytes = br#"{"labels":[]}"#.to_vec();
    bytes.resize(limit, b' ');
    assert_eq!(
        fabric_cli_resources::labels_from_bytes(&bytes)
            .unwrap()
            .data,
        Some(json!([]))
    );
    bytes.push(b' ');
    assert_eq!(
        fabric_cli_resources::labels_from_bytes(&bytes)
            .unwrap_err()
            .code,
        "InvalidInput"
    );
}

#[test]
fn malformed_acl_principal_details_and_member_containers_are_not_successful_null_projections() {
    for (target, rows) in [
        (
            resource("workspace"),
            json!([{"id":"assignment","role":"Admin","principal":null}]),
        ),
        (
            resource("workspace"),
            json!([{"id":"assignment","role":"Admin","principal":{"userDetails":["invalid"]}}]),
        ),
        (
            resource("item"),
            json!([{"principal":{"id":"id","displayName":"Name","type":"Group","groupDetails":null},"itemAccessDetails":{"permissions":["Read"]}}]),
        ),
        (
            resource("onelake"),
            json!([{"name":"Role","decisionRules":[],"members":null}]),
        ),
    ] {
        assert_eq!(
            fabric_cli_resources::project_acls(
                &target,
                rows.as_array().unwrap().clone(),
                false,
                None
            )
            .unwrap_err()
            .code,
            "InvalidEntriesFormat"
        );
    }
}

#[test]
fn resolved_context_acl_support_restrictions_are_enforced_before_any_service_call() {
    use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
    let tenant = Element::tenant("tenant", "tenant-id");
    let workspace = Element::child(
        "Workspace",
        Some("workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant,
    )
    .unwrap();
    assert_eq!(
        fabric_cli_resources::acl_resource(&workspace).unwrap(),
        Resource::Workspace("workspace".into())
    );
    for kind in ["Notebook", "SemanticModel", "Environment", "Dashboard"] {
        let item = Element::child(
            "Item",
            Some("item".into()),
            NodeKind::Item(ItemType::parse(kind).unwrap()),
            workspace.clone(),
        )
        .unwrap();
        let result = fabric_cli_resources::acl_resource(&item);
        if kind == "Notebook" {
            assert_eq!(
                result.unwrap(),
                Resource::Item {
                    workspace: "workspace".into(),
                    item: "item".into()
                }
            );
        } else {
            assert_eq!(result.unwrap_err().code, "UnsupportedCommand");
        }
    }
}
