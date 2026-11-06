// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller observation policy is additive; existing item mutation routes stay intact.

#![deny(unnameable_test_items)]

use fabric_sdk_items::{ItemCollection, mutations::ItemMutations};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::json;
use std::{future::Future, pin::Pin};

#[derive(Default)]
struct Recorder(Vec<ResourceOperation>);

impl ResourceOperations for Recorder {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.0.push(operation);
        Box::pin(async {
            Ok(ResourceOutcome::Accepted {
                operation_id: Some("original-operation".into()),
            })
        })
    }
}

#[tokio::test]
async fn explicit_acceptance_does_not_change_create_patch_or_hard_delete_requests() {
    let mut recorder = Recorder::default();
    let result = ItemMutations::create_with_completion(
        &mut recorder,
        "workspace",
        ItemCollection::Ontology,
        json!({"displayName":"new"}),
        CompletionPolicy::ReturnAccepted,
    )
    .await
    .unwrap();
    assert!(matches!(result, ResourceOutcome::Accepted { .. }));
    ItemMutations::update_with_completion(
        &mut recorder,
        "workspace",
        ItemCollection::Ontology,
        "item",
        json!({"description":"changed"}),
        CompletionPolicy::ReturnAccepted,
    )
    .await
    .unwrap();
    ItemMutations::delete_with_completion(
        &mut recorder,
        "workspace",
        "item",
        true,
        CompletionPolicy::ReturnAccepted,
    )
    .await
    .unwrap();
    assert_eq!(recorder.0.len(), 3);
    assert_eq!(recorder.0[0].method(), ResourceMethod::Post);
    assert_eq!(recorder.0[0].path(), "/v1/workspaces/workspace/ontologies");
    assert_eq!(recorder.0[1].method(), ResourceMethod::Patch);
    assert_eq!(
        recorder.0[1].path(),
        "/v1/workspaces/workspace/ontologies/item"
    );
    assert_eq!(recorder.0[2].method(), ResourceMethod::Delete);
    assert_eq!(
        recorder.0[2].path(),
        "/v1/workspaces/workspace/items/item?hardDelete=true"
    );
    assert!(
        recorder
            .0
            .iter()
            .all(|op| op.completion() == CompletionPolicy::ReturnAccepted)
    );
}

#[tokio::test]
async fn legacy_entry_points_keep_source_synchronous_policy_and_same_bodies() {
    let mut recorder = Recorder::default();
    ItemMutations::create(
        &mut recorder,
        "workspace",
        ItemCollection::Ontology,
        json!({"displayName":"new"}),
    )
    .await
    .unwrap();
    ItemMutations::update(
        &mut recorder,
        "workspace",
        ItemCollection::Ontology,
        "item",
        json!({"description":"changed"}),
    )
    .await
    .unwrap();
    ItemMutations::delete(&mut recorder, "workspace", "item", false)
        .await
        .unwrap();
    assert_eq!(recorder.0[0].body(), Some(&json!({"displayName":"new"})));
    assert_eq!(
        recorder.0[1].body(),
        Some(&json!({"description":"changed"}))
    );
    assert_eq!(recorder.0[2].path(), "/v1/workspaces/workspace/items/item");
    assert!(
        recorder
            .0
            .iter()
            .all(|op| op.completion() == CompletionPolicy::PythonSynchronous)
    );
}
