// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic command consumers; expectations are pinned to original Python.

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CacheEffect, CapacitySelection, CommandFuture, CreateOptions, MutationError, MutationHost,
    MutationMetadata, MutationUi, NoticeKind,
};
use fabric_cli_output::{Indent, OutputEnvelope, Timestamp};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

#[derive(Default)]
struct Operations {
    requests: Vec<(ResourceMethod, String, Option<Value>, CompletionPolicy)>,
    responses: VecDeque<Result<ResourceOutcome, ResourceError>>,
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.requests.push((
            operation.method(),
            operation.path().into(),
            operation.body().cloned(),
            operation.completion(),
        ));
        let response = self.responses.pop_front().expect("unexpected request");
        Box::pin(async move { response })
    }
}

#[derive(Default)]
struct Ui {
    confirmed: bool,
    prompts: Vec<String>,
    notices: Vec<(NoticeKind, String)>,
}

impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.notices.push((kind, message.into()));
    }

    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(message.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}

#[derive(Default)]
struct Metadata {
    reads: usize,
    data: Value,
}

impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        self.reads += 1;
        Box::pin(std::future::ready(Ok(self.data.clone())))
    }
}

fn workspace(id: Option<&str>) -> Arc<Element> {
    Element::child(
        "Team",
        id.map(str::to_owned),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap()
}

fn item(item_type: &str, id: Option<&str>) -> Arc<Element> {
    Element::child(
        "Thing",
        id.map(str::to_owned),
        NodeKind::Item(ItemType::parse(item_type).unwrap()),
        workspace(Some("ws")),
    )
    .unwrap()
}

fn folder(id: Option<&str>) -> Arc<Element> {
    Element::child(
        "Dir",
        id.map(str::to_owned),
        NodeKind::Folder,
        workspace(Some("ws")),
    )
    .unwrap()
}

fn completed(status: u16, data: Option<Value>) -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Completed { status, data })
}

fn options(params: Value) -> CreateOptions {
    CreateOptions {
        params: params.as_object().unwrap().clone(),
        default_capacity_id: Some("capacity".into()),
        ..CreateOptions::default()
    }
}

#[tokio::test]
async fn create_workspace_preserves_lowercase_unknown_params_and_none_capacity_omission() {
    for (capacity, expected) in [
        (
            CapacitySelection::Default,
            json!({"displayName": "Team", "description": "hello", "run": "true", "capacityId": "capacity"}),
        ),
        (
            CapacitySelection::Unassigned,
            json!({"displayName": "Team", "description": "hello", "run": "true"}),
        ),
        (
            CapacitySelection::Resolved("selected".into()),
            json!({"displayName": "Team", "description": "hello", "run": "true", "capacityId": "selected"}),
        ),
    ] {
        let mut operations = Operations {
            responses: VecDeque::from([completed(
                201,
                Some(json!({"id": "new", "displayName": "Team"})),
            )]),
            ..Operations::default()
        };
        let mut ui = Ui::default();
        let mut metadata = Metadata::default();
        let mut options =
            options(json!({"DisplayName": "ignored", "Description": "hello", "RUN": "true"}));
        options.capacity = capacity;
        let result = MutationHost {
            operations: &mut operations,
            ui: &mut ui,
            metadata: &mut metadata,
        }
        .create(&workspace(None), &options)
        .await
        .unwrap();
        assert_eq!(
            operations.requests,
            [(
                ResourceMethod::Post,
                "/v1/workspaces".into(),
                Some(expected),
                CompletionPolicy::PythonSynchronous
            )]
        );
        assert_eq!(
            result.outputs[0].message.as_deref(),
            Some("'Team.Workspace' created")
        );
        assert!(matches!(&result.effects[..], [CacheEffect::Created { .. }]));
    }
}

#[tokio::test]
async fn create_folder_omits_root_parent_and_keeps_nested_parent() {
    let root = folder(None);
    let parent = folder(Some("parent"));
    let nested = Element::child("Nested", None, NodeKind::Folder, parent).unwrap();
    let mut operations = Operations {
        responses: VecDeque::from([
            completed(201, Some(json!({"id": "a"}))),
            completed(200, Some(json!({"id": "b"}))),
        ]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    host.create(&root, &options(json!({}))).await.unwrap();
    host.create(&nested, &options(json!({"Description": "notes"})))
        .await
        .unwrap();
    assert_eq!(
        operations.requests[0].2,
        Some(json!({"displayName": "Dir"}))
    );
    assert_eq!(
        operations.requests[1].2,
        Some(json!({"displayName": "Nested", "description": "notes", "parentFolderId": "parent"}))
    );
}

#[tokio::test]
async fn create_items_use_typed_collection_but_delete_uses_generic_collection() {
    for (kind, collection, extra) in [
        ("Lakehouse", "lakehouses", json!({})),
        (
            "Warehouse",
            "warehouses",
            json!({"creationPayload": {"defaultCollation": "Latin1_General_100_BIN2_UTF8"}}),
        ),
        ("DataPipeline", "dataPipelines", json!({})),
        ("GraphQuerySet", "GraphQuerySets", json!({})),
    ] {
        let mut operations = Operations {
            responses: VecDeque::from([
                completed(201, Some(json!({"id": "new"}))),
                completed(200, None),
            ]),
            ..Operations::default()
        };
        let mut ui = Ui::default();
        let mut metadata = Metadata::default();
        let mut host = MutationHost {
            operations: &mut operations,
            ui: &mut ui,
            metadata: &mut metadata,
        };
        host.create(&item(kind, None), &options(json!({"run": "true"})))
            .await
            .unwrap();
        host.remove(&item(kind, Some("id")), true).await.unwrap();
        let mut expected = json!({"displayName": "Thing", "type": kind, "folderId": null});
        expected
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            operations.requests[0],
            (
                ResourceMethod::Post,
                format!("/v1/workspaces/ws/{collection}"),
                Some(expected),
                CompletionPolicy::PythonSynchronous
            )
        );
        assert_eq!(operations.requests[1].1, "/v1/workspaces/ws/items/id");
    }
}

#[tokio::test]
async fn duplicate_help_and_missing_capacity_do_not_dispatch() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    assert!(
        host.create(&workspace(Some("existing")), &CreateOptions::default())
            .await
            .unwrap()
            .outcome
            .is_none()
    );
    assert_eq!(
        host.create(
            &workspace(Some("existing")),
            &options(json!({"run": "true"}))
        )
        .await
        .unwrap_err()
        .code,
        "AlreadyExists"
    );
    let mut missing = options(json!({"run": "true"}));
    missing.default_capacity_id = None;
    assert_eq!(
        host.create(&workspace(None), &missing)
            .await
            .unwrap_err()
            .code,
        "InvalidInput"
    );
    assert!(operations.requests.is_empty());
    assert_eq!(
        ui.notices[0].1,
        "Params for '.Workspace'. Use key=value separated by commas.\n\nOptional params:\n  capacityName\n"
    );
}

#[tokio::test]
async fn decline_is_not_service_cancellation_and_acceptance_is_not_completion() {
    let mut operations = Operations {
        responses: VecDeque::from([Ok(ResourceOutcome::Accepted {
            operation_id: Some("operation".into()),
        })]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    assert!(
        host.remove(&folder(Some("dir")), false)
            .await
            .unwrap()
            .declined
    );
    let accepted = host.remove(&folder(Some("dir")), true).await.unwrap();
    assert_eq!(
        accepted.outputs[0].message.as_deref(),
        Some("'Dir.Folder' is being deleted")
    );
    assert!(matches!(
        accepted.outcome,
        Some(ResourceOutcome::Accepted { .. })
    ));
    assert_eq!(operations.requests.len(), 1);
    assert_eq!(ui.prompts, ["Are you sure?"]);
}

#[tokio::test]
async fn hard_delete_is_explicit_in_request_prompt_and_warning() {
    let mut operations = Operations {
        responses: VecDeque::from([completed(200, None)]),
        ..Operations::default()
    };
    let mut ui = Ui {
        confirmed: true,
        ..Ui::default()
    };
    let mut metadata = Metadata::default();
    MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .remove_with_options(&item("Notebook", Some("id")), false, true)
    .await
    .unwrap();
    assert_eq!(
        operations.requests[0].1,
        "/v1/workspaces/ws/items/id?hardDelete=true"
    );
    assert_eq!(
        ui.prompts,
        ["'Thing.Notebook' will be deleted forever. Are you sure you want to proceed?"]
    );
    assert!(
        ui.notices
            .iter()
            .any(|(_, message)| message == "! Executing hard delete")
    );
}

#[tokio::test]
async fn set_preserves_workspace_read_before_prompt_and_folder_read_after_prompt() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata {
        data: json!({"displayName": "old"}),
        ..Metadata::default()
    };
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    assert!(
        host.set(&workspace(Some("ws")), "displayName", "new", false)
            .await
            .unwrap()
            .declined
    );
    assert!(
        host.set(&folder(Some("dir")), "displayName", "new", false)
            .await
            .unwrap()
            .declined
    );
    assert_eq!(metadata.reads, 1);
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn set_updates_only_top_property_and_routes_spark_separately() {
    let mut operations = Operations {
        responses: VecDeque::from([
            completed(200, None),
            completed(200, None),
            completed(200, None),
        ]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata {
        data: json!({"displayName": "old", "description": "old", "sparkSettings": {"pool": {"n": 1, "keep": true}}, "properties": {"values": [1, 2]}}),
        ..Metadata::default()
    };
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    let rename = host
        .set(&folder(Some("dir")), "displayName", "\"new\"", true)
        .await
        .unwrap();
    host.set(&workspace(Some("ws")), "sparkSettings.pool.n", "2", true)
        .await
        .unwrap();
    host.set(
        &item("Notebook", Some("id")),
        "properties.values[-1]",
        "null",
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        operations.requests[0].2,
        Some(json!({"displayName": "new"}))
    );
    assert_eq!(operations.requests[1].1, "/v1/workspaces/ws/spark/settings");
    assert_eq!(
        operations.requests[1].2,
        Some(json!({"pool": {"n": 2, "keep": true}}))
    );
    assert_eq!(
        operations.requests[2].2,
        Some(json!({"properties": {"values": [1, null]}}))
    );
    assert!(matches!(&rename.effects[..], [CacheEffect::Renamed { name, .. }] if name == "new"));
}

#[tokio::test]
async fn set_rejects_blocked_filtered_and_unported_definition_paths_without_mutation() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    for query in [
        "id",
        "capacityId",
        "workspaceIdentity.id",
        "items[*].id",
        "length(items)",
    ] {
        assert_eq!(
            host.set(&workspace(Some("ws")), query, "value", true)
                .await
                .unwrap_err()
                .code,
            "InvalidQuery"
        );
    }
    assert_eq!(
        host.set(
            &item("Notebook", Some("id")),
            "definition.parts",
            "[]",
            true
        )
        .await
        .unwrap_err()
        .code,
        "NotSupported"
    );
    assert!(operations.requests.is_empty());
    assert_eq!(metadata.reads, 0);
}

#[tokio::test]
async fn failure_retains_uncertain_mutation_and_renders_existing_error_envelope() {
    let error = ResourceError {
        code: "Cancelled".into(),
        message: "Resource operation cancelled".into(),
        status: None,
        service_error: None,
        raw_service_error: None,
        operation_id: Some("pending".into()),
        mutation_may_have_occurred: true,
    };
    let mut operations = Operations {
        responses: VecDeque::from([Err(error.clone())]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let failure = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .remove(&folder(Some("dir")), true)
    .await
    .unwrap_err();
    assert_eq!(failure.operation.as_deref(), Some(&error));
    let plan = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        failure.output(),
    )
    .render(Indent::Four)
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert!(stderr.is_empty());
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains("\"error_code\": \"Cancelled\"")
    );
}

#[test]
fn exists_uses_truthy_context_id_and_renders_string_message_not_boolean_data() {
    for (element, expected) in [
        (workspace(Some("ws")), "true"),
        (folder(None), "false"),
        (item("Notebook", Some("")), "false"),
    ] {
        let result = MutationHost::exists(&element).unwrap();
        let value = OutputEnvelope::new(
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            result.outputs[0].clone(),
        )
        .to_value()
        .unwrap();
        assert_eq!(value["result"]["message"], expected);
        assert!(value["result"].get("data").is_none());
    }
}

#[tokio::test]
async fn malformed_created_resource_is_an_explicit_uncertain_failure() {
    for response in [
        ResourceOutcome::Completed {
            status: 201,
            data: None,
        },
        ResourceOutcome::Completed {
            status: 200,
            data: Some(json!([])),
        },
    ] {
        let mut operations = Operations {
            responses: VecDeque::from([Ok(response)]),
            ..Operations::default()
        };
        let mut ui = Ui::default();
        let mut metadata = Metadata::default();
        let error = MutationHost {
            operations: &mut operations,
            ui: &mut ui,
            metadata: &mut metadata,
        }
        .create(&item("Lakehouse", None), &options(json!({"run": "true"})))
        .await
        .unwrap_err();
        assert!(error.operation.unwrap().mutation_may_have_occurred);
    }
}

#[tokio::test]
async fn headerless_accepted_creation_is_not_invented_completion_or_a_synthetic_error() {
    let mut operations = Operations {
        responses: VecDeque::from([Ok(ResourceOutcome::Accepted { operation_id: None })]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let result = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .create(&item("Lakehouse", None), &options(json!({"run": "true"})))
    .await
    .unwrap();
    assert!(result.outputs.is_empty());
    assert!(result.effects.is_empty());
    assert_eq!(
        result.outcome,
        Some(ResourceOutcome::Accepted { operation_id: None })
    );
}

#[tokio::test]
async fn set_rejects_outside_json_domain_without_rounding_or_dispatching_a_mutation() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata {
        data: json!({"description":"old"}),
        ..Metadata::default()
    };
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    for input in [
        "18446744073709551616",
        "-9223372036854775809",
        "1e309",
        "NaN",
        "Infinity",
        "-Infinity",
        r#"{"nested":[NaN]}"#,
    ] {
        let error = host
            .set(&folder(Some("dir")), "description", input, true)
            .await
            .unwrap_err();
        assert_eq!(error.code, "InvalidJson");
    }
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn set_preserves_uint64_boundary_and_intentional_non_json_string_fallback() {
    let mut operations = Operations {
        responses: VecDeque::from([completed(200, None), completed(200, None)]),
        ..Operations::default()
    };
    let mut ui = Ui::default();
    let mut metadata = Metadata {
        data: json!({"description":"old"}),
        ..Metadata::default()
    };
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    host.set(
        &folder(Some("dir")),
        "description",
        "18446744073709551615",
        true,
    )
    .await
    .unwrap();
    host.set(
        &folder(Some("dir")),
        "description",
        "word NaN is not JSON",
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        operations.requests[0].2,
        Some(json!({"description":u64::MAX}))
    );
    assert_eq!(
        operations.requests[1].2,
        Some(json!({"description":"word NaN is not JSON"}))
    );
}

#[tokio::test]
async fn payload_errors_keep_source_progress_order_and_required_params_precede_progress() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .create(
        &item("SQLDatabase", None),
        &options(json!({"creationMode":"invalid"})),
    )
    .await
    .unwrap_err();
    assert_eq!(
        ui.notices,
        [(NoticeKind::Progress, "Creating a new SQLDatabase...".into())]
    );
    ui.notices.clear();
    MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .create(
        &item("MountedDataFactory", None),
        &options(json!({"run":"true"})),
    )
    .await
    .unwrap_err();
    assert!(ui.notices.is_empty());
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn unported_definition_encoding_values_fail_explicitly_without_a_payload_request() {
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let error = MutationHost { operations: &mut operations, ui: &mut ui, metadata: &mut metadata }
        .create(&item("MountedDataFactory", None), &options(json!({
            "subscriptionId":"subscription", "resourceGroup":"non-ascii-\u{65e5}", "factoryName":"factory",
        }))).await.unwrap_err();
    assert_eq!(error.code, "NotSupported");
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn within_workspace_folder_move_keeps_source_count_and_only_completed_200_cache_effect() {
    let parent = folder(Some("parent"));
    let source = Element::child("Child", Some("child".into()), NodeKind::Folder, parent).unwrap();
    for (response, effect) in [
        (
            ResourceOutcome::Completed {
                status: 200,
                data: None,
            },
            true,
        ),
        (
            ResourceOutcome::Accepted {
                operation_id: Some("operation".into()),
            },
            false,
        ),
    ] {
        let mut operations = Operations {
            responses: VecDeque::from([Ok(response.clone())]),
            ..Operations::default()
        };
        let mut ui = Ui::default();
        let mut metadata = Metadata::default();
        let result = MutationHost {
            operations: &mut operations,
            ui: &mut ui,
            metadata: &mut metadata,
        }
        .move_folder(&source, &workspace(Some("ws")))
        .await
        .unwrap();
        assert_eq!(result.moved_count, 1);
        assert_eq!(result.result.outcome, Some(response));
        assert_eq!(result.result.effects.len(), usize::from(effect));
        if effect {
            assert!(
                matches!(&result.result.effects[0],CacheEffect::MovedFolder { target,.. } if target.id()==Some("child"))
            );
        }
        assert_eq!(operations.requests[0].2, Some(json!({})));
        assert!(ui.prompts.is_empty());
    }
}

#[tokio::test]
async fn same_parent_folder_move_is_noop_and_cross_workspace_stays_definition_interface_gap() {
    let source = folder(Some("folder"));
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let mut metadata = Metadata::default();
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    let result = host
        .move_folder(&source, source.parent().unwrap())
        .await
        .unwrap();
    assert_eq!(result.moved_count, 0);
    assert!(result.result.outcome.is_none());
    let different = Element::child(
        "Other",
        Some("other".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    assert_eq!(
        host.move_folder(&source, &different)
            .await
            .unwrap_err()
            .code,
        "NotSupported"
    );
    assert!(operations.requests.is_empty());
}
