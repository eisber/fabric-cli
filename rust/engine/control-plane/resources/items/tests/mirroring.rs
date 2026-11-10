// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused mirroring actions and their retained generic-item compatibility APIs.

#![deny(unnameable_test_items)]

use fabric_sdk_items::{mirroring, mutations::ItemMutations};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use std::{future::Future, pin::Pin};

struct Capture {
    calls: Vec<ResourceOperation>,
    result: Result<ResourceOutcome, ResourceError>,
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.calls.push(operation);
        Box::pin(std::future::ready(self.result.clone()))
    }
}

async fn invoke(
    index: usize,
    legacy: bool,
    executor: &mut Capture,
    workspace: &str,
    item: &str,
) -> Result<ResourceOutcome, ResourceError> {
    match (index, legacy) {
        (0, false) => mirroring::status(executor, workspace, item).await,
        (1, false) => mirroring::start(executor, workspace, item).await,
        (2, false) => mirroring::stop(executor, workspace, item).await,
        (0, true) => ItemMutations::mirroring_status(executor, workspace, item).await,
        (1, true) => ItemMutations::start_mirroring(executor, workspace, item).await,
        (2, true) => ItemMutations::stop_mirroring(executor, workspace, item).await,
        _ => unreachable!("three finite mirroring actions"),
    }
}

#[tokio::test]
async fn focused_and_legacy_actions_preserve_routes_policy_and_acceptance() {
    for (index, action) in ["getMirroringStatus", "startMirroring", "stopMirroring"]
        .into_iter()
        .enumerate()
    {
        for legacy in [false, true] {
            let expected = ResourceOutcome::Accepted {
                operation_id: Some("offline-operation".into()),
            };
            let mut executor = Capture {
                calls: Vec::new(),
                result: Ok(expected.clone()),
            };
            assert_eq!(
                invoke(index, legacy, &mut executor, "workspace", "item")
                    .await
                    .unwrap(),
                expected
            );
            assert_eq!(executor.calls.len(), 1);
            let request = &executor.calls[0];
            assert_eq!(request.method(), ResourceMethod::Post);
            assert_eq!(
                request.path(),
                format!("/v1/workspaces/workspace/mirroredDatabases/item/{action}")
            );
            assert_eq!(request.body(), None);
            assert_eq!(request.completion(), CompletionPolicy::PythonSynchronous);
        }
    }
}

#[tokio::test]
async fn unsafe_ids_fail_before_dispatch_on_both_public_paths() {
    for index in 0..3 {
        for legacy in [false, true] {
            for (workspace, item) in [("../workspace", "item"), ("workspace", "..")] {
                let mut executor = Capture {
                    calls: Vec::new(),
                    result: Ok(ResourceOutcome::Completed {
                        status: 200,
                        data: None,
                    }),
                };
                assert_eq!(
                    invoke(index, legacy, &mut executor, workspace, item).await,
                    Err(ResourceError::input())
                );
                assert!(executor.calls.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn execution_failure_retains_uncertain_mutation_evidence_without_replay() {
    for index in 0..3 {
        for legacy in [false, true] {
            let mut expected = ResourceError::input();
            expected.code = "OfflineFailure".into();
            expected.operation_id = Some("offline-operation".into());
            expected.mutation_may_have_occurred = true;
            let mut executor = Capture {
                calls: Vec::new(),
                result: Err(expected.clone()),
            };
            assert_eq!(
                invoke(index, legacy, &mut executor, "workspace", "item").await,
                Err(expected)
            );
            assert_eq!(executor.calls.len(), 1);
        }
    }
}
