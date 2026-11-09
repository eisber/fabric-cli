// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native shortcut request and shared-executor contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_onelake::link::ShortcutLinkRequest;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE_WORKSPACE: &str = "33333333-3333-4333-8333-333333333333";
const SOURCE_ITEM: &str = "44444444-4444-4444-8444-444444444444";
const CONNECTION: &str = "55555555-5555-4555-8555-555555555555";

fn one_lake() -> Value {
    json!({
        "oneLake": {
            "workspaceId": SOURCE_WORKSPACE,
            "itemId": SOURCE_ITEM,
            "path": "Files/source"
        }
    })
}

fn core(target: Value) -> Value {
    json!({"path": "Files/landing/deep folder", "name": "sales_ref", "target": target})
}

fn kql(target: Value) -> Value {
    json!({"name": "sales_ref", "enableQueryAcceleration": false, "target": target})
}

fn targets() -> Vec<(&'static str, Value)> {
    vec![
        ("oneLake", one_lake()),
        (
            "adlsGen2",
            json!({"adlsGen2": {
                "connectionId": CONNECTION,
                "location": "https://storage.dfs.core.windows.net",
                "subpath": "/container/folder"
            }}),
        ),
        (
            "amazonS3",
            json!({"amazonS3": {
                "connectionId": CONNECTION,
                "location": "https://bucket.s3.us-west-2.amazonaws.com",
                "subpath": "/folder"
            }}),
        ),
        (
            "googleCloudStorage",
            json!({"googleCloudStorage": {
                "connectionId": CONNECTION,
                "location": "https://bucket.storage.googleapis.com",
                "subpath": "/folder"
            }}),
        ),
        (
            "s3Compatible",
            json!({"s3Compatible": {
                "connectionId": CONNECTION,
                "location": "https://s3.example.invalid",
                "bucket": "bucket",
                "subpath": "/folder"
            }}),
        ),
        (
            "azureBlobStorage",
            json!({"azureBlobStorage": {
                "connectionId": CONNECTION,
                "location": "https://storage.blob.core.windows.net",
                "subpath": "container/folder"
            }}),
        ),
        (
            "dataverse",
            json!({"dataverse": {
                "connectionId": CONNECTION,
                "environmentDomain": "https://org.crm4.dynamics.com",
                "tableName": "account",
                "deltaLakeFolder": "folder/account"
            }}),
        ),
        (
            "oneDriveSharePoint",
            json!({"oneDriveSharePoint": {
                "connectionId": CONNECTION,
                "location": "https://contoso.sharepoint.com",
                "subpath": "/Shared Documents/folder"
            }}),
        ),
    ]
}

fn request(owner: &str, input: &Value) -> Result<ShortcutLinkRequest, ResourceError> {
    ShortcutLinkRequest::from_value(owner, WORKSPACE, ITEM, input)
}

fn assert_local_error(error: ResourceError, code: &str) {
    assert_eq!(error.code, code);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(error.status, None);
    assert_eq!(error.service_error, None);
    assert_eq!(error.raw_service_error, None);
    assert_eq!(error.operation_id, None);
}

#[test]
fn core_uses_exact_native_body_and_abort_only_for_all_eight_targets() {
    for (_, target) in targets() {
        let input = core(target);
        let operation = request("Lakehouse", &input)
            .unwrap()
            .operation(CompletionPolicy::ReturnAccepted)
            .unwrap();
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(
            operation.path(),
            format!(
                "/v1/workspaces/{WORKSPACE}/items/{ITEM}/shortcuts?shortcutConflictPolicy=Abort"
            )
        );
        assert_eq!(operation.body(), Some(&input));
        assert_eq!(operation.completion(), CompletionPolicy::ReturnAccepted);
    }
}

#[test]
fn kql_uses_its_native_route_and_six_target_subset_without_core_fields() {
    for (kind, target) in targets() {
        let input = kql(target);
        if matches!(kind, "dataverse" | "oneDriveSharePoint") {
            assert_local_error(
                request("KQLDatabase", &input).unwrap_err(),
                "UnsupportedCapability",
            );
            continue;
        }
        let operation = request("KQLDatabase", &input)
            .unwrap()
            .operation(CompletionPolicy::WaitForResult)
            .unwrap();
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(
            operation.path(),
            format!("/v1/workspaces/{WORKSPACE}/kqlDatabases/{ITEM}/shortcuts")
        );
        assert_eq!(operation.body(), Some(&input));
        assert_eq!(operation.completion(), CompletionPolicy::WaitForResult);
        assert!(!operation.path().contains('?'));
        assert!(operation.body().unwrap().get("path").is_none());
    }
}

#[test]
fn all_completion_policies_are_preserved_without_installing_a_polling_policy() {
    for owner in ["Lakehouse", "KQLDatabase"] {
        let input = if owner == "Lakehouse" {
            core(one_lake())
        } else {
            kql(one_lake())
        };
        for completion in [
            CompletionPolicy::PythonSynchronous,
            CompletionPolicy::ReturnAccepted,
            CompletionPolicy::WaitForResult,
            CompletionPolicy::WaitForCompletion,
        ] {
            assert_eq!(
                request(owner, &input)
                    .unwrap()
                    .operation(completion)
                    .unwrap()
                    .completion(),
                completion
            );
        }
    }
}

#[test]
fn unsupported_owners_and_non_creatable_or_unknown_targets_fail_locally() {
    for owner in [
        "Workspace",
        "Warehouse",
        "DataAgent",
        "lakehouse",
        "",
        "KqlDatabase",
    ] {
        assert_local_error(
            request(owner, &core(one_lake())).unwrap_err(),
            "UnsupportedCapability",
        );
    }
    for owner in ["Lakehouse", "KQLDatabase"] {
        for target in [
            json!({"externalDataShare": {"connectionId": CONNECTION}}),
            json!({"futureTarget": {}}),
        ] {
            let input = if owner == "Lakehouse" {
                core(target)
            } else {
                kql(target)
            };
            assert_local_error(request(owner, &input).unwrap_err(), "UnsupportedCapability");
        }
    }
}

#[test]
fn request_fields_are_exact_required_and_typed_with_no_implicit_defaults() {
    for (owner, input) in [
        ("Lakehouse", core(one_lake())),
        ("KQLDatabase", kql(one_lake())),
    ] {
        for missing in input.as_object().unwrap().keys() {
            let mut invalid = input.clone();
            invalid.as_object_mut().unwrap().remove(missing);
            assert_local_error(request(owner, &invalid).unwrap_err(), "InvalidInput");
        }
        for (field, value) in [
            ("unknown", json!("synthetic-sensitive-input")),
            ("shortcutConflictPolicy", json!("OverwriteOnly")),
            ("transform", json!({"type": "csvToDelta"})),
            ("name", json!(null)),
            ("name", json!(42)),
            ("name", json!("")),
            ("target", json!(null)),
        ] {
            let mut invalid = input.clone();
            invalid[field] = value;
            let error = request(owner, &invalid).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic-sensitive-input"));
            assert_local_error(error, "InvalidInput");
        }
        for non_object in [json!(null), json!([]), json!("body"), json!(true)] {
            assert_local_error(request(owner, &non_object).unwrap_err(), "InvalidInput");
        }
    }
    for value in [json!(null), json!("false"), json!(0), json!({})] {
        let mut invalid = kql(one_lake());
        invalid["enableQueryAcceleration"] = value;
        assert_local_error(
            request("KQLDatabase", &invalid).unwrap_err(),
            "InvalidInput",
        );
    }
    let mut wrong_core = core(one_lake());
    wrong_core["enableQueryAcceleration"] = json!(false);
    assert_local_error(
        request("Lakehouse", &wrong_core).unwrap_err(),
        "InvalidInput",
    );
    let mut wrong_kql = kql(one_lake());
    wrong_kql["path"] = json!("Tables");
    assert_local_error(
        request("KQLDatabase", &wrong_kql).unwrap_err(),
        "InvalidInput",
    );
}

#[test]
fn exactly_one_target_is_required_and_response_discriminators_are_not_inputs() {
    for target in [
        json!({}),
        json!(null),
        json!([]),
        json!({"type": "OneLake"}),
        json!({"oneLake": {}, "type": "OneLake"}),
        json!({"oneLake": {}, "adlsGen2": {}}),
        json!({"oneLake": null}),
        json!({"oneLake": []}),
    ] {
        assert_local_error(
            request("Lakehouse", &core(target)).unwrap_err(),
            "InvalidInput",
        );
    }
}

#[test]
fn every_target_rejects_missing_mistyped_and_unknown_native_fields() {
    for (kind, target) in targets() {
        let details = target[kind].as_object().unwrap();
        for field in details.keys() {
            let mut missing = target.clone();
            missing[kind].as_object_mut().unwrap().remove(field);
            assert_local_error(
                request("Lakehouse", &core(missing)).unwrap_err(),
                "InvalidInput",
            );
            for value in [
                json!(null),
                json!(false),
                json!(7),
                json!([]),
                json!({}),
                json!(""),
            ] {
                let mut invalid = target.clone();
                invalid[kind][field] = value;
                assert_local_error(
                    request("Lakehouse", &core(invalid)).unwrap_err(),
                    "InvalidInput",
                );
            }
        }
        for field in ["type", "password", "sasToken", "credential", "unknown"] {
            let mut invalid = target.clone();
            invalid[kind][field] = json!("synthetic-secret-not-echoed");
            let error = request("Lakehouse", &core(invalid)).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic-secret-not-echoed"));
            assert_local_error(error, "InvalidInput");
        }
    }
}

#[test]
fn identifiers_require_uuid_shape_and_cannot_inject_origins_or_routes() {
    for invalid in [
        "",
        "workspace",
        "11111111111141118111111111111111",
        "11111111-1111-4111-8111-11111111111z",
        "11111111_1111-4111-8111-111111111111",
        "https://example.invalid",
        "../items",
        "id?shortcutConflictPolicy=OverwriteOnly",
    ] {
        for (workspace, item) in [(invalid, ITEM), (WORKSPACE, invalid)] {
            assert_local_error(
                ShortcutLinkRequest::from_value("Lakehouse", workspace, item, &core(one_lake()))
                    .unwrap_err(),
                "InvalidInput",
            );
        }
        for field in ["workspaceId", "itemId", "connectionId"] {
            let mut target = one_lake();
            target["oneLake"][field] = json!(invalid);
            assert_local_error(
                request("Lakehouse", &core(target)).unwrap_err(),
                "InvalidInput",
            );
        }
    }
    let mut target = one_lake();
    target["oneLake"]["connectionId"] = json!(CONNECTION);
    let input = core(target);
    assert_eq!(
        request("Lakehouse", &input)
            .unwrap()
            .operation(CompletionPolicy::ReturnAccepted)
            .unwrap()
            .body(),
        Some(&input)
    );
    let mut null_connection = one_lake();
    null_connection["oneLake"]["connectionId"] = Value::Null;
    assert_local_error(
        request("Lakehouse", &core(null_connection)).unwrap_err(),
        "InvalidInput",
    );
}

#[test]
fn paths_are_safe_relative_ascii_without_traversal_encoding_or_empty_segments() {
    for path in [
        "",
        "/Files",
        "Files/",
        "Files//folder",
        "Files/.",
        "Files/..",
        "Files/../Tables",
        "Files\\folder",
        "Files/%2e%2e",
        "Files/a+b",
        "Files/a?b",
        "Files/a#b",
        "Files/a\u{0}b",
        "Files/a\u{7f}b",
        "Files/日本語",
        "Files/ space",
        "https://example.invalid",
    ] {
        let mut invalid = core(one_lake());
        invalid["path"] = json!(path);
        assert_local_error(request("Lakehouse", &invalid).unwrap_err(), "InvalidInput");
        let mut target = one_lake();
        target["oneLake"]["path"] = json!(path);
        assert_local_error(
            request("Lakehouse", &core(target)).unwrap_err(),
            "InvalidInput",
        );
    }
    let mut input = core(one_lake());
    input["path"] = json!("Files/a/b/c/d/e/f/g/h/i/j/k/l/m/n");
    assert!(request("Lakehouse", &input).is_ok());
}

#[test]
fn native_external_subpaths_preserve_one_leading_slash_and_explicit_bucket_roots() {
    for (kind, mut target) in targets() {
        if matches!(kind, "oneLake" | "dataverse") {
            continue;
        }
        for invalid in [
            "//folder",
            "/folder/",
            "/../folder",
            "/a//b",
            "/a%2fb",
            "/a+b",
            "/a?sig=secret",
            "https://example.invalid",
        ] {
            target[kind]["subpath"] = json!(invalid);
            assert_local_error(
                request("Lakehouse", &core(target.clone())).unwrap_err(),
                "InvalidInput",
            );
        }
        target[kind]["subpath"] = json!("/");
        let result = request("Lakehouse", &core(target));
        if matches!(kind, "amazonS3" | "googleCloudStorage" | "s3Compatible") {
            assert!(result.is_ok());
        } else {
            assert_local_error(result.unwrap_err(), "InvalidInput");
        }
    }
}

#[test]
fn tables_placement_is_only_root_to_single_onelake_table_not_schema_inference() {
    let mut input = core(one_lake());
    input["path"] = json!("Tables");
    input["target"]["oneLake"]["path"] = json!("Tables/sales");
    assert!(request("Lakehouse", &input).is_ok());
    for path in ["Tables/dbo", "Tables/dbo/sales", "tables", "Other"] {
        let mut invalid = input.clone();
        invalid["path"] = json!(path);
        assert_local_error(
            request("Lakehouse", &invalid).unwrap_err(),
            "UnsupportedCapability",
        );
    }
    for source in [
        "Tables/dbo/sales",
        "Tables/sales/folder",
        "Files/sales",
        "Tables/sales data",
    ] {
        let mut invalid = input.clone();
        invalid["target"]["oneLake"]["path"] = json!(source);
        assert_local_error(
            request("Lakehouse", &invalid).unwrap_err(),
            "UnsupportedCapability",
        );
    }
    input["name"] = json!("sales data");
    assert_local_error(
        request("Lakehouse", &input).unwrap_err(),
        "UnsupportedCapability",
    );
    for (kind, target) in targets() {
        if kind != "oneLake" {
            let mut invalid = core(target);
            invalid["path"] = json!("Tables");
            assert_local_error(
                request("Lakehouse", &invalid).unwrap_err(),
                "UnsupportedCapability",
            );
        }
    }
}

#[test]
fn kql_names_reject_documented_characters_and_safe_segment_failures() {
    for character in ['?', ':', '/', '\\', '"', ']', '[', '+', '#', '%', '\0'] {
        let mut invalid = kql(one_lake());
        invalid["name"] = json!(format!("sales{character}ref"));
        assert_local_error(
            request("KQLDatabase", &invalid).unwrap_err(),
            "InvalidInput",
        );
    }
    for name in ["", ".", "..", " name", "name ", "日本語"] {
        let mut invalid = kql(one_lake());
        invalid["name"] = json!(name);
        assert_local_error(
            request("KQLDatabase", &invalid).unwrap_err(),
            "InvalidInput",
        );
    }
    let mut input = kql(one_lake());
    input["name"] = json!("sales reference");
    assert!(request("KQLDatabase", &input).is_ok());
}

#[test]
fn locations_reject_credentials_queries_fragments_non_https_and_normalization_tricks() {
    for (kind, mut target) in targets() {
        if kind == "oneLake" {
            continue;
        }
        let field = if kind == "dataverse" {
            "environmentDomain"
        } else {
            "location"
        };
        let good = target[kind][field].as_str().unwrap().to_owned();
        let host = good.strip_prefix("https://").unwrap();
        for invalid in [
            good.replace("https://", "http://"),
            good.replace("https://", "s3://"),
            good.replace("https://", "gs://"),
            good.replace("https://", "https:"),
            format!("https://user:synthetic-secret@{host}"),
            format!("https://@{host}"),
            format!("{good}?sig=synthetic-secret"),
            format!("{good}?"),
            format!("{good}#fragment"),
            format!("{good}#"),
            format!("{good}/bucket"),
            format!("{good}/a/.."),
            format!("{good}/%2e%2e"),
            format!("{good}\\folder"),
            format!("{good}\n"),
            format!(" {good}"),
        ] {
            target[kind][field] = json!(invalid);
            let error = request("Lakehouse", &core(target.clone())).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic-secret"));
            assert_local_error(error, "InvalidInput");
        }
    }
}

#[test]
fn known_external_endpoint_types_do_not_accept_lookalike_hosts_or_other_target_locations() {
    for (kind, mut target) in targets() {
        if matches!(kind, "oneLake" | "s3Compatible") {
            continue;
        }
        let field = if kind == "dataverse" {
            "environmentDomain"
        } else {
            "location"
        };
        for location in [
            "https://example.invalid",
            "https://storage.dfs.core.windows.net.example.invalid",
            "https://bucket.s3.us-west-2.amazonaws.com.example.invalid",
        ] {
            target[kind][field] = json!(location);
            assert_local_error(
                request("Lakehouse", &core(target.clone())).unwrap_err(),
                "InvalidInput",
            );
        }
    }
}

#[test]
fn sensitivity_and_acceleration_effects_are_explicit_typed_and_never_added() {
    let (_, mut sharepoint) = targets().pop().unwrap();
    for enabled in [false, true] {
        sharepoint["oneDriveSharePoint"]["updateFabricItemSensitivity"] = json!(enabled);
        let input = core(sharepoint.clone());
        let request = request("Lakehouse", &input).unwrap();
        assert_eq!(request.updates_fabric_item_sensitivity(), enabled);
        assert!(!request.enables_query_acceleration());
        assert_eq!(
            request
                .operation(CompletionPolicy::ReturnAccepted)
                .unwrap()
                .body(),
            Some(&input)
        );
    }
    for value in [json!(null), json!("true"), json!(1), json!({})] {
        sharepoint["oneDriveSharePoint"]["updateFabricItemSensitivity"] = value;
        assert_local_error(
            request("Lakehouse", &core(sharepoint.clone())).unwrap_err(),
            "InvalidInput",
        );
    }
    for enabled in [false, true] {
        let mut input = kql(one_lake());
        input["enableQueryAcceleration"] = json!(enabled);
        let request = request("KQLDatabase", &input).unwrap();
        assert_eq!(request.enables_query_acceleration(), enabled);
        assert!(!request.updates_fabric_item_sensitivity());
    }
}

#[test]
fn debug_and_local_errors_never_include_request_metadata() {
    let mut input = core(one_lake());
    input["name"] = json!("synthetic-private-name");
    input["target"]["oneLake"]["path"] = json!("Files/synthetic-private-path");
    let request = request("Lakehouse", &input).unwrap();
    let debug = format!("{request:?}");
    for hidden in [
        WORKSPACE,
        ITEM,
        SOURCE_WORKSPACE,
        SOURCE_ITEM,
        "synthetic-private-name",
        "synthetic-private-path",
    ] {
        assert!(!debug.contains(hidden));
    }
    let operation = request.operation(CompletionPolicy::ReturnAccepted).unwrap();
    assert!(!format!("{operation:?}").contains("synthetic-private-name"));
}

struct FakeOperations {
    operations: Vec<ResourceOperation>,
    result: Result<ResourceOutcome, ResourceError>,
}

impl ResourceOperations for FakeOperations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.operations.push(operation);
        Box::pin(std::future::ready(self.result.clone()))
    }
}

#[tokio::test]
async fn execution_preserves_real_core_and_kql_completion_or_acceptance_without_replay() {
    for (owner, input, outcomes) in [
        (
            "Lakehouse",
            core(one_lake()),
            vec![
                ResourceOutcome::Completed {
                    status: 200,
                    data: Some(json!({"name": "actual-service-name"})),
                },
                ResourceOutcome::Completed {
                    status: 201,
                    data: None,
                },
            ],
        ),
        (
            "KQLDatabase",
            kql(one_lake()),
            vec![
                ResourceOutcome::Completed {
                    status: 201,
                    data: Some(json!({"name": "actual-service-name"})),
                },
                ResourceOutcome::Accepted {
                    operation_id: Some(CONNECTION.into()),
                },
            ],
        ),
    ] {
        for outcome in outcomes {
            let mut executor = FakeOperations {
                operations: Vec::new(),
                result: Ok(outcome.clone()),
            };
            let result = request(owner, &input)
                .unwrap()
                .execute(&mut executor, CompletionPolicy::ReturnAccepted)
                .await
                .unwrap();
            assert_eq!(result, outcome);
            assert_eq!(executor.operations.len(), 1);
            assert_eq!(executor.operations[0].body(), Some(&input));
            assert_eq!(
                executor.operations[0].completion(),
                CompletionPolicy::ReturnAccepted
            );
        }
    }
}

#[tokio::test]
async fn execution_errors_preserve_service_and_uncertain_mutation_evidence_verbatim() {
    for owner in ["Lakehouse", "KQLDatabase"] {
        let input = if owner == "Lakehouse" {
            core(one_lake())
        } else {
            kql(one_lake())
        };
        let mut error = ResourceError::input();
        error.code = "SyntheticConflict".into();
        error.message = "Synthetic sanitized failure".into();
        error.status = Some(409);
        error.operation_id = Some(CONNECTION.into());
        error.service_error = Some(Box::new(
            serde_json::from_value(json!({
                "errorCode": "SyntheticConflict",
                "message": "Synthetic provider detail",
                "requestId": SOURCE_ITEM,
                "moreDetails": [{"errorCode": "SyntheticDetail", "message": "preserved"}]
            }))
            .unwrap(),
        ));
        error.raw_service_error = Some(Box::new(json!({
            "errorCode": "SyntheticConflict",
            "requestId": SOURCE_ITEM,
            "futureField": {"preserved": true}
        })));
        error.mutation_may_have_occurred = true;
        let mut executor = FakeOperations {
            operations: Vec::new(),
            result: Err(error.clone()),
        };
        assert_eq!(
            request(owner, &input)
                .unwrap()
                .execute(&mut executor, CompletionPolicy::WaitForResult)
                .await
                .unwrap_err(),
            error
        );
        assert_eq!(executor.operations.len(), 1);
    }
}
