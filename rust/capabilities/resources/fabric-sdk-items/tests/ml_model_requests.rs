// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native MLModel request contracts through the public SDK and a shared executor.

#![deny(unnameable_test_items)]

use fabric_sdk_items::ml_models::{
    CreateMlModelRequest, MlModelDeleteMode, MlModelEffect, MlModelIdentity, MlModelIdentityPolicy,
    MlModelRequest, MlModelRequests, MlModelResultKind, MlModelTarget, ScoreDataRequest,
    UpdateEndpointRequest, UpdateEndpointVersionRequest, UpdateMlModelRequest,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const ITEM_PATH: &str = "/v1/workspaces/11111111-2222-3333-4444-555555555555/mlModels/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const ENDPOINT_PATH: &str = "/v1/workspaces/11111111-2222-3333-4444-555555555555/mlmodels/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/endpoint";

fn target() -> MlModelTarget {
    MlModelTarget::new(WORKSPACE, ITEM, "MLModel").unwrap()
}

fn payload<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn create_payload() -> CreateMlModelRequest {
    payload(json!({"displayName": "model", "description": "native item"}))
}

fn item_payload() -> UpdateMlModelRequest {
    payload(json!({"description": "changed"}))
}

fn endpoint_payload() -> UpdateEndpointRequest {
    payload(json!({
        "defaultVersionAssignmentBehavior": "StaticallyConfigured",
        "defaultVersionName": "01"
    }))
}

fn version_payload() -> UpdateEndpointVersionRequest {
    payload(json!({"scaleRule": "AllowScaleToZero"}))
}

fn score_payload() -> ScoreDataRequest {
    payload(json!({
        "inputs": [[false, 0, null, "01"], {"nested": [0, false, null]}],
        "formatType": "dataframe",
        "orientation": "values"
    }))
}

struct Capture {
    calls: Vec<ResourceOperation>,
    outcome: Result<ResourceOutcome, ResourceError>,
}

impl Capture {
    fn new(outcome: Result<ResourceOutcome, ResourceError>) -> Self {
        Self {
            calls: Vec::new(),
            outcome,
        }
    }
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.calls.push(operation);
        Box::pin(std::future::ready(self.outcome.clone()))
    }
}

struct Expected {
    request: MlModelRequest,
    method: ResourceMethod,
    path: String,
    body: Option<Value>,
    completion: CompletionPolicy,
    effect: MlModelEffect,
    result: MlModelResultKind,
    identity_policy: MlModelIdentityPolicy,
    statuses: &'static [u16],
}

#[tokio::test]
async fn all_fourteen_native_bindings_reach_the_injected_executor_once() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let target = target();
    let create = create_payload();
    let update = item_payload();
    let endpoint = endpoint_payload();
    let version = version_payload();
    let score = score_payload();
    let cases = [
        Expected {
            request: requests.get_item(&target).unwrap(),
            method: ResourceMethod::Get,
            path: ITEM_PATH.into(),
            body: None,
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::Read,
            result: MlModelResultKind::Item,
            identity_policy: MlModelIdentityPolicy::UserOnly,
            statuses: &[200],
        },
        Expected {
            request: requests.create(WORKSPACE, &create, true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("/v1/workspaces/{WORKSPACE}/mlModels"),
            body: Some(json!({"displayName": "model", "description": "native item"})),
            completion: CompletionPolicy::WaitForResult,
            effect: MlModelEffect::CreateItem,
            result: MlModelResultKind::Item,
            identity_policy: MlModelIdentityPolicy::UserOnly,
            statuses: &[201, 202],
        },
        Expected {
            request: requests.update_item(&target, &update).unwrap(),
            method: ResourceMethod::Patch,
            path: ITEM_PATH.into(),
            body: Some(json!({"description": "changed"})),
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::UpdateItem,
            result: MlModelResultKind::Item,
            identity_policy: MlModelIdentityPolicy::UserOnly,
            statuses: &[200],
        },
        Expected {
            request: requests
                .delete_item(&target, MlModelDeleteMode::default())
                .unwrap(),
            method: ResourceMethod::Delete,
            path: ITEM_PATH.into(),
            body: None,
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::DeleteItem { permanent: false },
            result: MlModelResultKind::None,
            identity_policy: MlModelIdentityPolicy::UserOnly,
            statuses: &[200],
        },
        Expected {
            request: requests.get_endpoint(&target).unwrap(),
            method: ResourceMethod::Get,
            path: ENDPOINT_PATH.into(),
            body: None,
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::Read,
            result: MlModelResultKind::Endpoint,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200],
        },
        Expected {
            request: requests.update_endpoint(&target, &endpoint).unwrap(),
            method: ResourceMethod::Patch,
            path: ENDPOINT_PATH.into(),
            body: Some(json!({
                "defaultVersionAssignmentBehavior": "StaticallyConfigured",
                "defaultVersionName": "01"
            })),
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::UpdateEndpoint,
            result: MlModelResultKind::Endpoint,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200],
        },
        Expected {
            request: requests.list_versions_page(&target, None).unwrap(),
            method: ResourceMethod::Get,
            path: format!("{ENDPOINT_PATH}/versions"),
            body: None,
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::Read,
            result: MlModelResultKind::VersionsPage,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200],
        },
        Expected {
            request: requests.get_version(&target, "01").unwrap(),
            method: ResourceMethod::Get,
            path: format!("{ENDPOINT_PATH}/versions/01"),
            body: None,
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::Read,
            result: MlModelResultKind::Version,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200],
        },
        Expected {
            request: requests.update_version(&target, "01", &version).unwrap(),
            method: ResourceMethod::Patch,
            path: format!("{ENDPOINT_PATH}/versions/01"),
            body: Some(json!({"scaleRule": "AllowScaleToZero"})),
            completion: CompletionPolicy::ReturnAccepted,
            effect: MlModelEffect::UpdateVersion,
            result: MlModelResultKind::Version,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200],
        },
        Expected {
            request: requests.activate(&target, "01", true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("{ENDPOINT_PATH}/versions/01/activate"),
            body: None,
            completion: CompletionPolicy::WaitForCompletion,
            effect: MlModelEffect::ActivateVersion,
            result: MlModelResultKind::None,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200, 202],
        },
        Expected {
            request: requests.deactivate(&target, "01", true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("{ENDPOINT_PATH}/versions/01/deactivate"),
            body: None,
            completion: CompletionPolicy::WaitForCompletion,
            effect: MlModelEffect::DeactivateVersion,
            result: MlModelResultKind::None,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200, 202],
        },
        Expected {
            request: requests.deactivate_all(&target, true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("{ENDPOINT_PATH}/versions/deactivateAll"),
            body: None,
            completion: CompletionPolicy::WaitForCompletion,
            effect: MlModelEffect::DeactivateAll,
            result: MlModelResultKind::None,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200, 202],
        },
        Expected {
            request: requests.score_default(&target, &score, true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("{ITEM_PATH}/endpoint/score"),
            body: Some(json!({
                "inputs": [[false, 0, null, "01"], {"nested": [0, false, null]}],
                "formatType": "dataframe",
                "orientation": "values"
            })),
            completion: CompletionPolicy::WaitForResult,
            effect: MlModelEffect::ScoreDefault,
            result: MlModelResultKind::Score,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200, 202],
        },
        Expected {
            request: requests.score_version(&target, "01", &score, true).unwrap(),
            method: ResourceMethod::Post,
            path: format!("{ENDPOINT_PATH}/versions/01/score"),
            body: Some(json!({
                "inputs": [[false, 0, null, "01"], {"nested": [0, false, null]}],
                "formatType": "dataframe",
                "orientation": "values"
            })),
            completion: CompletionPolicy::WaitForResult,
            effect: MlModelEffect::ScoreVersion,
            result: MlModelResultKind::Score,
            identity_policy: MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            statuses: &[200, 202],
        },
    ];
    let outcome = ResourceOutcome::Completed {
        status: 200,
        data: Some(json!({"native": "unchanged"})),
    };
    let mut executor = Capture::new(Ok(outcome.clone()));
    for (index, case) in cases.into_iter().enumerate() {
        assert_eq!(case.request.identity(), MlModelIdentity::User);
        assert_eq!(case.request.identity_policy(), case.identity_policy);
        assert_eq!(case.request.effect(), case.effect);
        assert_eq!(case.request.result_kind(), case.result);
        assert_eq!(case.request.expected_statuses(), case.statuses);
        assert_eq!(case.request.execute(&mut executor).await.unwrap(), outcome);
        assert_eq!(executor.calls.len(), index + 1);
        let operation = executor.calls.last().unwrap();
        assert_eq!(operation.method(), case.method);
        assert_eq!(operation.path(), case.path);
        assert_eq!(operation.body(), case.body.as_ref());
        assert_eq!(operation.completion(), case.completion);
    }
}

#[test]
fn identity_metadata_rejects_all_item_builders_for_app_only_callers() {
    let target = target();
    for identity in [
        MlModelIdentity::ServicePrincipal,
        MlModelIdentity::ManagedIdentity,
    ] {
        let requests = MlModelRequests::new(identity);
        for result in [
            requests.get_item(&target),
            requests.create(WORKSPACE, &create_payload(), false),
            requests.update_item(&target, &item_payload()),
            requests.delete_item(&target, MlModelDeleteMode::Soft),
            requests.delete_item(&target, MlModelDeleteMode::Permanent),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code, "UnsupportedIdentity");
            assert!(!error.mutation_may_have_occurred);
            assert_eq!(error.operation_id, None);
        }
    }
}

#[tokio::test]
async fn all_ten_serving_bindings_support_each_identity_without_item_resolution() {
    let target = target();
    for identity in [
        MlModelIdentity::User,
        MlModelIdentity::ServicePrincipal,
        MlModelIdentity::ManagedIdentity,
    ] {
        let requests = MlModelRequests::new(identity);
        let outcome = ResourceOutcome::Completed {
            status: 200,
            data: None,
        };
        let mut executor = Capture::new(Ok(outcome.clone()));
        for request in [
            requests.get_endpoint(&target).unwrap(),
            requests
                .update_endpoint(&target, &endpoint_payload())
                .unwrap(),
            requests.list_versions_page(&target, None).unwrap(),
            requests.get_version(&target, "01").unwrap(),
            requests
                .update_version(&target, "01", &version_payload())
                .unwrap(),
            requests.activate(&target, "01", false).unwrap(),
            requests.deactivate(&target, "01", false).unwrap(),
            requests.deactivate_all(&target, false).unwrap(),
            requests
                .score_default(&target, &score_payload(), false)
                .unwrap(),
            requests
                .score_version(&target, "01", &score_payload(), false)
                .unwrap(),
        ] {
            assert_eq!(request.identity(), identity);
            assert_eq!(
                request.identity_policy(),
                MlModelIdentityPolicy::UserServicePrincipalManagedIdentity
            );
            assert_eq!(request.execute(&mut executor).await.unwrap(), outcome);
        }
        assert_eq!(executor.calls.len(), 10);
        assert!(
            executor
                .calls
                .iter()
                .all(|operation| operation.path().contains("/endpoint"))
        );
    }
}

#[test]
fn target_requires_exact_owner_and_uuid_roots_and_retains_spelling() {
    for owner in ["", "mlmodel", "MLMODEL", "Notebook", "MLModel/endpoint"] {
        assert!(MlModelTarget::new(WORKSPACE, ITEM, owner).is_err());
    }
    for invalid in [
        "",
        "item",
        "11111111222233334444555555555555",
        "{11111111-2222-3333-4444-555555555555}",
        "11111111-2222-3333-4444-55555555555g",
        "11111111_2222-3333-4444-555555555555",
        "../11111111-2222-3333-4444-555555555555",
        "11111111-2222-3333-4444-555555555555?x=y",
        "https://example.invalid",
        "11111111-2222-3333-4444-555555555555\n",
    ] {
        assert!(MlModelTarget::new(invalid, ITEM, "MLModel").is_err());
        assert!(MlModelTarget::new(WORKSPACE, invalid, "MLModel").is_err());
        assert!(
            MlModelRequests::new(MlModelIdentity::User)
                .create(invalid, &create_payload(), false)
                .is_err()
        );
    }
    let uppercase = ITEM.to_ascii_uppercase();
    let target = MlModelTarget::new(WORKSPACE, &uppercase, "MLModel").unwrap();
    assert_eq!(target.workspace_id(), WORKSPACE);
    assert_eq!(target.item_id(), uppercase);
    assert!(
        MlModelRequests::new(MlModelIdentity::User)
            .get_endpoint(&target)
            .unwrap()
            .operation()
            .path()
            .contains(&uppercase)
    );
}

#[test]
fn unsafe_versions_fail_every_exact_version_builder_before_dispatch() {
    let requests = MlModelRequests::new(MlModelIdentity::ManagedIdentity);
    let target = target();
    for version in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "../x",
        "https://example.invalid",
        "\n",
        "a\0b",
        "a\u{7f}b",
    ] {
        for result in [
            requests.get_version(&target, version),
            requests.update_version(&target, version, &version_payload()),
            requests.activate(&target, version, false),
            requests.deactivate(&target, version, false),
            requests.score_version(&target, version, &score_payload(), false),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code, "InvalidInput");
            assert!(!error.mutation_may_have_occurred);
            assert_eq!(error.operation_id, None);
        }
    }
}

#[test]
fn version_names_are_one_encoded_opaque_segment_not_numeric_or_all_sentinels() {
    let requests = MlModelRequests::new(MlModelIdentity::ServicePrincipal);
    let target = target();
    let version = "v é?#%";
    let encoded = "v%20%C3%A9%3F%23%25";
    for (request, suffix) in [
        (requests.get_version(&target, version).unwrap(), ""),
        (
            requests
                .update_version(&target, version, &version_payload())
                .unwrap(),
            "",
        ),
        (
            requests.activate(&target, version, false).unwrap(),
            "/activate",
        ),
        (
            requests.deactivate(&target, version, false).unwrap(),
            "/deactivate",
        ),
        (
            requests
                .score_version(&target, version, &score_payload(), false)
                .unwrap(),
            "/score",
        ),
    ] {
        assert_eq!(
            request.operation().path(),
            format!("{ENDPOINT_PATH}/versions/{encoded}{suffix}")
        );
    }
    for version in [
        "1",
        "01",
        "0001",
        "all",
        "default",
        "deactivateAll",
        "-1",
        "1.5",
    ] {
        assert_eq!(
            requests
                .get_version(&target, version)
                .unwrap()
                .operation()
                .path(),
            format!("{ENDPOINT_PATH}/versions/{version}")
        );
        assert_eq!(
            requests
                .deactivate(&target, version, false)
                .unwrap()
                .operation()
                .path(),
            format!("{ENDPOINT_PATH}/versions/{version}/deactivate")
        );
    }
    assert_ne!(
        requests
            .get_version(&target, "1")
            .unwrap()
            .operation()
            .path(),
        requests
            .get_version(&target, "01")
            .unwrap()
            .operation()
            .path()
    );
    assert_eq!(
        requests
            .get_version(&target, "%2F%2e%2E")
            .unwrap()
            .operation()
            .path(),
        format!("{ENDPOINT_PATH}/versions/%252F%252e%252E")
    );
}

#[tokio::test]
async fn one_page_retains_opaque_token_and_continuation_without_collecting() {
    let requests = MlModelRequests::new(MlModelIdentity::ServicePrincipal);
    let target = target();
    let request = requests
        .list_versions_page(&target, Some("next é?#%&=+"))
        .unwrap();
    assert_eq!(
        request.operation().path(),
        format!("{ENDPOINT_PATH}/versions?continuationToken=next+%C3%A9%3F%23%25%26%3D%2B")
    );
    assert_eq!(
        requests
            .list_versions_page(&target, Some(""))
            .unwrap()
            .operation()
            .path(),
        format!("{ENDPOINT_PATH}/versions?continuationToken=")
    );
    for token in ["next\n", "\0", "a\u{7f}b"] {
        assert!(requests.list_versions_page(&target, Some(token)).is_err());
    }
    let outcome = ResourceOutcome::Completed {
        status: 200,
        data: Some(json!({
            "value": [{"versionName": "01"}],
            "continuationToken": "more",
            "continuationUri": "opaque provider data"
        })),
    };
    let mut executor = Capture::new(Ok(outcome.clone()));
    assert_eq!(request.execute(&mut executor).await.unwrap(), outcome);
    assert_eq!(executor.calls.len(), 1);
}

#[test]
fn permanent_delete_is_explicit_and_never_uses_generic_items_route() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let target = target();
    let soft = requests
        .delete_item(&target, MlModelDeleteMode::default())
        .unwrap();
    let permanent = requests
        .delete_item(&target, MlModelDeleteMode::Permanent)
        .unwrap();
    assert_eq!(soft.operation().path(), ITEM_PATH);
    assert_eq!(
        permanent.operation().path(),
        format!("{ITEM_PATH}?hardDelete=true")
    );
    assert_eq!(
        permanent.effect(),
        MlModelEffect::DeleteItem { permanent: true }
    );
    assert_eq!(permanent.operation().body(), None);
    assert_eq!(permanent.expected_statuses(), &[200]);
    assert_eq!(permanent.result_kind(), MlModelResultKind::None);
}

#[test]
fn wait_choices_distinguish_acceptance_result_and_no_result_completion() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let target = target();
    for wait in [false, true] {
        let result_policy = if wait {
            CompletionPolicy::WaitForResult
        } else {
            CompletionPolicy::ReturnAccepted
        };
        let no_result_policy = if wait {
            CompletionPolicy::WaitForCompletion
        } else {
            CompletionPolicy::ReturnAccepted
        };
        for request in [
            requests.create(WORKSPACE, &create_payload(), wait).unwrap(),
            requests
                .score_default(&target, &score_payload(), wait)
                .unwrap(),
            requests
                .score_version(&target, "01", &score_payload(), wait)
                .unwrap(),
        ] {
            assert_eq!(request.operation().completion(), result_policy);
            assert_ne!(request.result_kind(), MlModelResultKind::None);
        }
        for request in [
            requests.activate(&target, "01", wait).unwrap(),
            requests.deactivate(&target, "01", wait).unwrap(),
            requests.deactivate_all(&target, wait).unwrap(),
        ] {
            assert_eq!(request.operation().completion(), no_result_policy);
            assert_eq!(request.result_kind(), MlModelResultKind::None);
            assert_eq!(request.operation().body(), None);
        }
    }
}

#[test]
fn builders_validate_payloads_and_never_add_wrappers_or_omitted_defaults() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let target = target();
    let score: ScoreDataRequest = payload(json!({"inputs": []}));
    assert_eq!(
        requests
            .score_default(&target, &score, false)
            .unwrap()
            .operation()
            .body(),
        Some(&json!({"inputs": []}))
    );
    assert_eq!(
        requests
            .create(WORKSPACE, &payload(json!({"displayName": "model"})), false)
            .unwrap()
            .operation()
            .body(),
        Some(&json!({"displayName": "model"}))
    );
    let invalid: UpdateMlModelRequest = payload(json!({"description": "x".repeat(257)}));
    assert!(requests.update_item(&target, &invalid).is_err());
    let invalid: CreateMlModelRequest = payload(json!({"displayName": ""}));
    assert!(requests.create(WORKSPACE, &invalid, false).is_err());
    let invalid: CreateMlModelRequest =
        payload(json!({"displayName": "model", "folderId": "../another-folder"}));
    assert!(requests.create(WORKSPACE, &invalid, false).is_err());
    let invalid: UpdateEndpointRequest = payload(json!({}));
    assert!(requests.update_endpoint(&target, &invalid).is_err());
    for json in [
        json!({"description": "x", "displayName": "renamed"}),
        json!({"description": "x", "definition": {}}),
        json!({"description": "x", "unknown": false}),
    ] {
        assert!(serde_json::from_value::<UpdateMlModelRequest>(json).is_err());
    }
    assert!(
        serde_json::from_value::<CreateMlModelRequest>(
            json!({"displayName": "model", "definition": {}})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<UpdateEndpointRequest>(
            json!({"defaultVersionName": "01", "url": "https://example.invalid"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<UpdateEndpointVersionRequest>(
            json!({"scaleRule": "AlwaysOn", "versionName": "02"})
        )
        .is_err()
    );
    for json in [
        json!({"inputs": [], "parameters": {}}),
        json!({"inputs": [], "orientation": "records"}),
        json!({"inputs": [], "formatType": "json"}),
        json!({"inputs": [], "orientation": null}),
        json!({"inputs": [], "formatType": null}),
    ] {
        assert!(serde_json::from_value::<ScoreDataRequest>(json).is_err());
    }
}

#[test]
fn serving_versions_remain_scoped_to_the_supplied_workspace_and_model() {
    let requests = MlModelRequests::new(MlModelIdentity::ManagedIdentity);
    let workspace = "99999999-8888-7777-6666-555555555555";
    let model = "eeeeeeee-dddd-cccc-bbbb-aaaaaaaaaaaa";
    let target = MlModelTarget::new(workspace, model, "MLModel").unwrap();
    assert_eq!(
        requests
            .score_version(&target, "01", &score_payload(), true)
            .unwrap()
            .operation()
            .path(),
        format!("/v1/workspaces/{workspace}/mlmodels/{model}/endpoint/versions/01/score")
    );
    assert_eq!(
        requests
            .deactivate_all(&target, false)
            .unwrap()
            .operation()
            .path(),
        format!("/v1/workspaces/{workspace}/mlmodels/{model}/endpoint/versions/deactivateAll")
    );
    assert_eq!(
        requests
            .score_default(&target, &score_payload(), false)
            .unwrap()
            .operation()
            .path(),
        format!("/v1/workspaces/{workspace}/mlModels/{model}/endpoint/score")
    );
}

#[test]
fn request_debug_redacts_native_values_and_scoped_identifiers() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let score: ScoreDataRequest = payload(json!({"inputs": [["private-input-marker"]]}));
    let request = requests
        .score_version(&target(), "private-version-marker", &score, true)
        .unwrap();
    let debug = format!("{request:?}");
    for sensitive in [
        WORKSPACE,
        ITEM,
        "private-input-marker",
        "private-version-marker",
    ] {
        assert!(!debug.contains(sensitive));
    }
    assert!(debug.contains("ScoreVersion"));
    assert!(debug.contains("WaitForResult"));
}

#[tokio::test]
async fn native_acceptance_completion_and_errors_are_returned_without_rewriting() {
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let target = target();
    let uncertain = ResourceError {
        code: "Cancelled".into(),
        message: "Observation cancelled after dispatch".into(),
        status: Some(202),
        raw_service_error: Some(Box::new(json!({"native": false, "details": [0, null]}))),
        operation_id: Some("bbbbbbbb-cccc-dddd-eeee-ffffffffffff".into()),
        mutation_may_have_occurred: true,
        ..ResourceError::input()
    };
    for outcome in [
        Ok(ResourceOutcome::Accepted {
            operation_id: Some("bbbbbbbb-cccc-dddd-eeee-ffffffffffff".into()),
        }),
        Ok(ResourceOutcome::Accepted { operation_id: None }),
        Ok(ResourceOutcome::Completed {
            status: 200,
            data: Some(json!({
                "predictions": [false, 0, null, "", [], {}],
                "orientation": "values",
                "future": {"native": true}
            })),
        }),
        Ok(ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({"id": ITEM, "displayName": "model"})),
        }),
        Ok(ResourceOutcome::Completed {
            status: 200,
            data: None,
        }),
        Err(uncertain),
        Err(ResourceError {
            code: "Forbidden".into(),
            status: Some(403),
            raw_service_error: Some(Box::new(json!({"error": {"code": "NotAllowed"}}))),
            ..ResourceError::input()
        }),
    ] {
        for wait in [false, true] {
            let request = requests
                .score_version(&target, "01", &score_payload(), wait)
                .unwrap();
            let mut executor = Capture::new(outcome.clone());
            assert_eq!(request.execute(&mut executor).await, outcome);
            assert_eq!(executor.calls.len(), 1);
        }
    }
}
