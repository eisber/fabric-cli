// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native non-shortcut link schema, routing and delegation contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_items::links::{
    NativeLinkKind, NativeLinkRequest, parse_link_json, validate_link_id,
};
use fabric_sdk_workspaces::FabricServiceError;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SOURCE_WORKSPACE: &str = "01234567-89ab-cdef-0123-456789abcdef";
const SOURCE_ITEM: &str = "fedcba98-7654-3210-fedc-ba9876543210";
const CONNECTION: &str = "01234567-ABCD-EFAB-CDEF-0123456789AB";

fn datasource(kind: &str, reference: &str) -> Value {
    let mut body = json!({"type": kind});
    body[reference] = json!({
        "referenceType": "ById",
        "workspaceId": SOURCE_WORKSPACE,
        "itemId": SOURCE_ITEM
    });
    body
}

fn connection(mode: &str) -> Value {
    json!({
        "connectionBinding": {
            "id": CONNECTION,
            "connectivityType": mode,
            "connectionDetails": {
                "type": "SQL",
                "path": "contoso.database.windows.net;sales"
            }
        }
    })
}

fn request(kind: NativeLinkKind, body: &Value) -> Result<NativeLinkRequest, ResourceError> {
    NativeLinkRequest::from_value(kind, WORKSPACE, ITEM, body)
}

fn assert_local_error(kind: NativeLinkKind, body: &Value, code: &str) {
    let error = request(kind, body).unwrap_err();
    assert_eq!(error.code, code);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(error.status, None);
    assert_eq!(error.operation_id, None);
    assert_eq!(error.service_error, None);
    assert_eq!(error.raw_service_error, None);
    assert!(!error.message.contains("private-marker"));
    assert!(!format!("{error:?}").contains("private-marker"));
    assert!(!error.to_string().contains("private-marker"));
}

#[test]
fn public_link_id_validation_accepts_hyphenated_uuid_hex_without_case_normalization() {
    for id in [
        WORKSPACE,
        ITEM,
        SOURCE_WORKSPACE,
        SOURCE_ITEM,
        CONNECTION,
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(validate_link_id(id), Ok(()));
    }
}

#[test]
fn public_link_id_validation_rejects_nonempty_malformed_response_identities() {
    for id in [
        "",
        "private-marker",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeg",
        "aaaaaaaa_bbbb_cccc_dddd_eeeeeeeeeeee",
        "aaaaaaaabbbbccccddddeeeeeeeeeeee",
        "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
        "urn:uuid:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        " aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee ",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
        "aaaaaaaa%2Dbbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?name=private-marker",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee日",
    ] {
        assert_eq!(validate_link_id(id), Err(ResourceError::input()));
    }
}

#[test]
fn strict_json_preserves_values_and_allows_equal_keys_in_distinct_objects() {
    for input in [
        r#"{"type":"FabricItem","itemReference":{"referenceType":"ById","workspaceId":"first","itemId":"second"}}"#,
        r#"{"target":{"oneLake":{"workspaceId":"first","itemId":"second","path":"Tables/销售"}}}"#,
        r#"[{"name":"first"},{"name":"second"}]"#,
        r#"{"n":-9223372036854775808,"u":18446744073709551615,"f":1.25,"zero":-0.0,"b":true,"null":null,"empty":[],"object":{}}"#,
        r#"{"first":1,"second":2,"third":3}"#,
        r#""a string with \"escaped quotes\" and \u0061""#,
        "false",
        "null",
        "0",
        " \r\n\t {\"name\":\"native\"} \r\n\t ",
    ] {
        let expected: Value = serde_json::from_str(input).unwrap();
        let actual = parse_link_json(input).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
    }
}

#[test]
fn strict_json_rejects_duplicate_members_including_nested_shortcut_targets() {
    for input in [
        r#"{"name":"private-marker","name":"second"}"#,
        r#"{"type":"FabricItem","type":"FabricItem"}"#,
        r#"{"target":{"oneLake":{"workspaceId":"private-marker","workspaceId":"second"}}}"#,
        r#"{"target":{"oneLake":{},"oneLake":{}}}"#,
        r#"{"connectionBinding":{"connectionDetails":{"path":"private-marker","path":"second"}}}"#,
        r#"{"itemReference":{"referenceType":"ById","referenceType":"ByVariable"}}"#,
        r#"[{"target":{"name":"private-marker","name":"second"}}]"#,
        r#"{"name":"private-marker","\u006eame":"second"}"#,
        r#"{"nested":{"\u0061":1,"a":2}}"#,
    ] {
        assert_eq!(parse_link_json(input).unwrap_err(), ResourceError::input());
    }
}

#[test]
fn strict_json_rejects_trailing_documents_malformed_json_and_parser_errors_without_echo() {
    for input in [
        "",
        " \r\n\t ",
        "{} {}",
        "{} []",
        "null true",
        "true private-marker",
        r#"{"name":"private-marker"} null"#,
        r#"{"name":"private-marker",}"#,
        r#"{"name":"private-marker""#,
        r#"{'name':'private-marker'}"#,
        r#"{"name":NaN}"#,
        r#"{"name":1e9999}"#,
        r#"{"name":"\uD800"}"#,
        r#"{"name":"private-marker"} // comment"#,
    ] {
        assert_eq!(parse_link_json(input).unwrap_err(), ResourceError::input());
    }
}

#[test]
fn strict_json_retains_the_default_json_recursion_bound() {
    let ordinary = format!("{}null{}", "[".repeat(32), "]".repeat(32));
    assert!(parse_link_json(&ordinary).is_ok());
    let excessive_arrays = format!("{}null{}", "[".repeat(256), "]".repeat(256));
    assert_eq!(
        parse_link_json(&excessive_arrays).unwrap_err(),
        ResourceError::input()
    );
    let excessive_objects = format!("{}null{}", "{\"next\":".repeat(256), "}".repeat(256));
    assert_eq!(
        parse_link_json(&excessive_objects).unwrap_err(),
        ResourceError::input()
    );
}

#[test]
fn strict_json_can_feed_the_native_constructor_without_a_second_input_convention() {
    for (kind, body) in [
        (
            NativeLinkKind::DataAgentDatasource,
            datasource("FabricItem", "itemReference"),
        ),
        (
            NativeLinkKind::SemanticModelConnection,
            connection("PersonalCloud"),
        ),
    ] {
        let decoded = parse_link_json(&serde_json::to_string(&body).unwrap()).unwrap();
        let operation = request(kind, &decoded)
            .unwrap()
            .operation(CompletionPolicy::ReturnAccepted)
            .unwrap();
        assert_eq!(operation.body(), Some(&body));
    }
}

#[test]
fn both_dataagent_variants_use_only_the_exact_staging_create_schema() {
    for (kind, reference) in [
        ("LakehouseTables", "lakehouseReference"),
        ("FabricItem", "itemReference"),
    ] {
        let body = datasource(kind, reference);
        let request = request(NativeLinkKind::DataAgentDatasource, &body).unwrap();
        for completion in [
            CompletionPolicy::PythonSynchronous,
            CompletionPolicy::ReturnAccepted,
            CompletionPolicy::WaitForResult,
            CompletionPolicy::WaitForCompletion,
        ] {
            let operation = request.operation(completion).unwrap();
            assert_eq!(operation.method(), ResourceMethod::Post);
            assert_eq!(
                operation.path(),
                format!("/v1/workspaces/{WORKSPACE}/dataAgents/{ITEM}/staging/datasources")
            );
            assert_eq!(operation.body(), Some(&body));
            assert_eq!(operation.completion(), completion);
        }
    }
}

#[test]
fn all_explicit_supported_connection_modes_retain_exact_native_metadata() {
    for mode in [
        "ShareableCloud",
        "PersonalCloud",
        "OnPremisesGateway",
        "OnPremisesGatewayPersonal",
        "VirtualNetworkGateway",
        "StreamingVirtualNetworkGateway",
    ] {
        let body = connection(mode);
        let request = request(NativeLinkKind::SemanticModelConnection, &body).unwrap();
        let operation = request.operation(CompletionPolicy::ReturnAccepted).unwrap();
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(
            operation.path(),
            format!("/v1/workspaces/{WORKSPACE}/semanticModels/{ITEM}/bindConnection")
        );
        assert_eq!(operation.body(), Some(&body));
        assert_eq!(operation.completion(), CompletionPolicy::ReturnAccepted);
    }
}

#[test]
fn destination_ids_are_strict_uuids_not_names_paths_or_encoded_selectors() {
    let body = connection("ShareableCloud");
    for invalid in [
        "",
        "private-marker",
        "../private-marker",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?x=private-marker",
        "aaaaaaaabbbbccccddddeeeeeeeeeeee",
        "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeg",
        "aaaaaaaa_bbbb_cccc_dddd_eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
    ] {
        for (workspace, item) in [(invalid, ITEM), (WORKSPACE, invalid)] {
            let error = NativeLinkRequest::from_value(
                NativeLinkKind::SemanticModelConnection,
                workspace,
                item,
                &body,
            )
            .unwrap_err();
            assert_eq!(error, ResourceError::input());
        }
    }
}

#[test]
fn dataagent_requires_complete_by_id_references_and_rejects_response_fields() {
    for (kind, reference) in [
        ("LakehouseTables", "lakehouseReference"),
        ("FabricItem", "itemReference"),
    ] {
        let valid = datasource(kind, reference);
        for field in ["type", reference] {
            let mut body = valid.clone();
            body.as_object_mut().unwrap().remove(field);
            assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
        }
        for field in ["referenceType", "workspaceId", "itemId"] {
            let mut body = valid.clone();
            body[reference].as_object_mut().unwrap().remove(field);
            assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
            for invalid in [
                Value::Null,
                json!(true),
                json!("private-marker"),
                json!({"ById": null}),
            ] {
                let mut body = valid.clone();
                body[reference][field] = invalid;
                assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
            }
        }
        for field in [
            "fabricItemType",
            "instructions",
            "displayName",
            "id",
            "workspaceId",
            "itemId",
            "name",
            "published",
            "select",
        ] {
            let mut body = valid.clone();
            body[field] = json!("private-marker");
            assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
        }
        for field in ["variableReference", "name", "extra"] {
            let mut body = valid.clone();
            body[reference][field] = json!("private-marker");
            assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
        }
        let mut body = valid.clone();
        body[reference] = json!({
            "referenceType": "ByVariable",
            "variableReference": "$(private-marker/library)"
        });
        assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
        let mut body = valid;
        let alternative = if reference == "itemReference" {
            "lakehouseReference"
        } else {
            "itemReference"
        };
        body[alternative] = body[reference].clone();
        assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
    }
}

#[test]
fn datasource_discriminators_and_native_link_schemas_are_not_interchangeable() {
    for body in [
        json!({}),
        json!(null),
        json!([]),
        json!("private-marker"),
        datasource("Warehouse", "itemReference"),
        datasource("fabricItem", "itemReference"),
        datasource("FabricItem", "lakehouseReference"),
        datasource("LakehouseTables", "itemReference"),
        connection("ShareableCloud"),
    ] {
        assert_local_error(NativeLinkKind::DataAgentDatasource, &body, "InvalidInput");
    }
    assert_local_error(
        NativeLinkKind::SemanticModelConnection,
        &datasource("FabricItem", "itemReference"),
        "InvalidInput",
    );
}

#[test]
fn connection_requires_every_reference_field_without_connectivity_fallback() {
    let valid = connection("ShareableCloud");
    let mut body = valid.clone();
    body.as_object_mut().unwrap().remove("connectionBinding");
    assert_local_error(
        NativeLinkKind::SemanticModelConnection,
        &body,
        "InvalidInput",
    );
    for field in ["id", "connectivityType", "connectionDetails"] {
        let mut body = valid.clone();
        body["connectionBinding"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_local_error(
            NativeLinkKind::SemanticModelConnection,
            &body,
            "InvalidInput",
        );
        for invalid in [Value::Null, json!(false), json!([])] {
            let mut body = valid.clone();
            body["connectionBinding"][field] = invalid;
            assert_local_error(
                NativeLinkKind::SemanticModelConnection,
                &body,
                "InvalidInput",
            );
        }
    }
    for field in ["type", "path"] {
        let mut body = valid.clone();
        body["connectionBinding"]["connectionDetails"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_local_error(
            NativeLinkKind::SemanticModelConnection,
            &body,
            "InvalidInput",
        );
        for invalid in [Value::Null, json!(0), json!(""), json!(" \t")] {
            let mut body = valid.clone();
            body["connectionBinding"]["connectionDetails"][field] = invalid;
            assert_local_error(
                NativeLinkKind::SemanticModelConnection,
                &body,
                "InvalidInput",
            );
        }
    }
    let mut body = valid;
    body["connectionBinding"]["id"] = json!("private-marker");
    assert_local_error(
        NativeLinkKind::SemanticModelConnection,
        &body,
        "InvalidInput",
    );
}

#[test]
fn automatic_unbind_unknown_and_example_gateway_alias_are_explicitly_unsupported() {
    for mode in [
        "Automatic",
        "None",
        "OnPremisesDataGateway",
        "private-marker",
        "shareableCloud",
        "",
    ] {
        assert_local_error(
            NativeLinkKind::SemanticModelConnection,
            &connection(mode),
            "UnsupportedCapability",
        );
    }
}

#[test]
fn extra_selectors_and_credentials_are_rejected_at_every_connection_level() {
    let valid = connection("ShareableCloud");
    for object in [
        "",
        "/connectionBinding",
        "/connectionBinding/connectionDetails",
    ] {
        for field in [
            "name",
            "workspaceId",
            "itemId",
            "connectionId",
            "gatewayObjectId",
            "credentialDetails",
            "credentials",
            "password",
            "accessToken",
            "refresh",
            "takeover",
            "reportId",
        ] {
            let mut body = valid.clone();
            body.pointer_mut(object).unwrap()[field] = json!("private-marker");
            assert_local_error(
                NativeLinkKind::SemanticModelConnection,
                &body,
                "InvalidInput",
            );
        }
    }
}

#[test]
fn connection_detail_controls_and_obvious_embedded_credentials_are_not_forwarded() {
    for value in [
        "server\nprivate-marker",
        "server\u{0}private-marker",
        "Bearer private-marker",
        "  Bearer private-marker",
        "-----BEGIN PRIVATE KEY-----private-marker",
        "server;Password=private-marker",
        "server;PWD = private-marker",
        "https://user:private-marker@example.invalid/source",
        "https://private-marker@example.invalid/source",
        "https://example.invalid/source?sig=private-marker",
        "https://example.invalid/source?%73ig=private-marker",
        "https://example.invalid/source?api_key=private-marker",
        "https://example.invalid/source?access_token=private-marker",
        "server;Client Secret=private-marker",
        "server;AccountKey=private-marker",
    ] {
        for field in ["type", "path"] {
            let mut body = connection("ShareableCloud");
            body["connectionBinding"]["connectionDetails"][field] = json!(value);
            assert_local_error(
                NativeLinkKind::SemanticModelConnection,
                &body,
                "InvalidInput",
            );
        }
    }
}

#[test]
fn native_sql_unicode_and_nonsecret_url_paths_are_preserved_without_uri_conversion() {
    for path in [
        "contoso.database.windows.net;sales",
        "server\\instance;database with spaces",
        "服务器;销售",
        "https://example.invalid/source?database=sales",
        " private-marker;database ",
    ] {
        let mut body = connection("PersonalCloud");
        body["connectionBinding"]["connectionDetails"]["path"] = json!(path);
        let request = request(NativeLinkKind::SemanticModelConnection, &body).unwrap();
        let operation = request
            .operation(CompletionPolicy::WaitForCompletion)
            .unwrap();
        assert_eq!(operation.body(), Some(&body));
        assert!(!format!("{request:?}").contains("private-marker"));
        assert!(!format!("{request:?}").contains(CONNECTION));
        assert!(!format!("{request:?}").contains(WORKSPACE));
    }
}

struct Capture {
    calls: Vec<ResourceOperation>,
    outcome: Result<ResourceOutcome, ResourceError>,
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

#[tokio::test]
async fn shared_executor_outcomes_and_uncertainty_are_returned_unchanged_once() {
    let error = ResourceError {
        code: "ConnectionUseDenied".into(),
        message: "Sanitized executor failure".into(),
        status: Some(403),
        service_error: Some(Box::new(FabricServiceError {
            error_code: "ConnectionUseDenied".into(),
            message: "Untrusted service evidence".into(),
            additional_fields: Default::default(),
        })),
        raw_service_error: Some(Box::new(json!({"errorCode": "ConnectionUseDenied"}))),
        operation_id: Some("existing-operation".into()),
        mutation_may_have_occurred: true,
    };
    for (kind, body) in [
        (
            NativeLinkKind::DataAgentDatasource,
            datasource("FabricItem", "itemReference"),
        ),
        (
            NativeLinkKind::SemanticModelConnection,
            connection("OnPremisesGateway"),
        ),
    ] {
        let request = request(kind, &body).unwrap();
        for outcome in [
            Ok(ResourceOutcome::Completed {
                status: 200,
                data: None,
            }),
            Ok(ResourceOutcome::Completed {
                status: 201,
                data: Some(json!({"id": SOURCE_ITEM, "instructions": "service result"})),
            }),
            Ok(ResourceOutcome::Accepted {
                operation_id: Some("existing-operation".into()),
            }),
            Ok(ResourceOutcome::Accepted { operation_id: None }),
            Err(error.clone()),
            Err(ResourceError {
                code: "Cancelled".into(),
                mutation_may_have_occurred: true,
                ..ResourceError::input()
            }),
        ] {
            let mut executor = Capture {
                calls: Vec::new(),
                outcome: outcome.clone(),
            };
            let actual = request
                .execute(&mut executor, CompletionPolicy::ReturnAccepted)
                .await;
            assert_eq!(actual, outcome);
            assert_eq!(executor.calls.len(), 1);
            assert_eq!(executor.calls[0].body(), Some(&body));
            assert_eq!(
                executor.calls[0].completion(),
                CompletionPolicy::ReturnAccepted
            );
        }
    }
}
