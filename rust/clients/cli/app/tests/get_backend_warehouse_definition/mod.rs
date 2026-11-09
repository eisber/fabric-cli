// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;

fn warehouse_metadata(fixture: &Fixture, typed: bool) {
    let collection = if typed { "warehouses" } else { "items" };
    fixture.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/{collection}/{ITEM}"),
        200,
        &serde_json::to_vec(&json!({
            "id":ITEM,"workspaceId":WS,"type":"Warehouse","displayName":"not-a-database",
        }))
        .unwrap(),
        &[],
    );
}

fn warehouse_path(suffix: &str) -> String {
    format!("/{WS}.Workspace/{ITEM}.Warehouse{suffix}")
}

fn definition(format: &str) -> Value {
    let primary = if format == "dacpac" {
        ("definition.dacpac", vec![0, 255, 128, 1])
    } else {
        ("sqldw.sqlproj", b"<Project/>".to_vec())
    };
    json!({
        "definition":{
            "format":format,
            "parts":[
                {"path":primary.0,"payload":STANDARD.encode(primary.1),"payloadType":"InlineBase64"},
                {"path":".platform","payload":STANDARD.encode(b"{}"),"payloadType":"InlineBase64","mediaType":null},
            ],
            "futureDefinition":{"keep":true},
        },
        "futureEnvelope":{"keep":true},
    })
}

fn definition_response(fixture: &Fixture, value: &Value) {
    fixture.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
        200,
        &serde_json::to_vec(value).unwrap(),
        &[],
    );
}

#[tokio::test]
async fn warehouse_default_source_preserves_native_envelope_over_dedicated_route() {
    let f = Fixture::new();
    warehouse_metadata(&f, true);
    let native = definition("sqlproj");
    definition_response(&f, &native);
    let bytes = stdout(&f, &["get".into(), warehouse_path("/src/definition")])
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"], native);
    assert_eq!(value["meta"]["nativeFormat"], "sqlproj");
    assert_eq!(value["meta"]["owner"]["itemId"], ITEM);
    assert_eq!(value["meta"]["owner"]["provenance"], "selectedRequest");
    assert_eq!(value["meta"]["coverage"]["fullDefinition"], true);
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn warehouse_machine_id_formats_and_parts_use_actual_owner_not_generic_definition() {
    for suffix in [
        "/src/definition",
        "/src/definition/formats/sqlproj",
        "/src/definition/parts/sqldw.sqlproj",
    ] {
        let f = Fixture::new();
        warehouse_metadata(&f, false);
        definition_response(&f, &definition("sqlproj"));
        let _bytes = stdout(&f, &["get".into(), path(suffix)]).await.unwrap();
        let calls = f.executor.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            (
                "POST".into(),
                format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
            ),
        );
    }
}

#[tokio::test]
async fn warehouse_explicit_dacpac_part_preserves_binary_and_reports_part_only_coverage() {
    let f = Fixture::new();
    warehouse_metadata(&f, true);
    definition_response(&f, &definition("dacpac"));
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            warehouse_path("/src/definition/formats/dacpac/parts/definition.dacpac"),
            "--format".into(),
            "raw".into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(bytes, [0, 255, 128, 1]);
    assert_eq!(f.executor.calls.lock().unwrap().len(), 2);

    let f = Fixture::new();
    warehouse_metadata(&f, true);
    definition_response(&f, &definition("sqlproj"));
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            warehouse_path("/src/definition/parts/.platform"),
        ],
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["meta"]["coverage"]["fullDefinition"], false);
    assert_eq!(value["meta"]["mediaTypeEvidence"], "null");
    assert_eq!(value["data"]["content"], STANDARD.encode(b"{}"));
}

#[tokio::test]
async fn warehouse_definition_never_degrades_strict_content_failure_to_generic_parts() {
    for corrupt in [
        {
            let mut value = definition("sqlproj");
            value["definition"]["parts"][0]["payload"] = json!("bad base64");
            serde_json::to_vec(&value).unwrap()
        },
        {
            let mut value = definition("sqlproj");
            value["definition"]["parts"][0]["path"] = json!("../escape.sqlproj");
            serde_json::to_vec(&value).unwrap()
        },
        br#"{"definition":{"format":"sqlproj","parts":[]},"definition":{"parts":[]}}"#.to_vec(),
    ] {
        let f = Fixture::new();
        warehouse_metadata(&f, true);
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
            200,
            &corrupt,
            &[],
        );
        assert!(
            stdout(&f, &["get".into(), warehouse_path("/src/definition")])
                .await
                .is_err()
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn warehouse_definition_acceptance_without_wait_keeps_real_or_missing_handle() {
    for has_handle in [false, true] {
        let f = Fixture::new();
        warehouse_metadata(&f, true);
        let headers = if has_handle {
            vec![("x-ms-operation-id", OP.into())]
        } else {
            Vec::new()
        };
        f.executor.response(
            "POST",
            format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
            202,
            b"",
            &headers,
        );
        let bytes = stdout(&f, &["get".into(), warehouse_path("/src/definition")])
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["meta"]["outcome"], "accepted");
        assert_eq!(value["data"]["completed"], false);
        assert_eq!(
            value["data"]["handle"],
            if has_handle {
                json!(format!("/operations/{OP}"))
            } else {
                Value::Null
            },
        );
        assert_eq!(f.executor.calls.lock().unwrap().len(), 2);
        assert!(f.executor.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn warehouse_definition_wait_observes_exact_native_handle_then_strictly_admits_result() {
    let f = Fixture::new();
    warehouse_metadata(&f, true);
    f.executor.response(
        "POST",
        format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
        202,
        b"",
        &[("x-ms-operation-id", OP.into())],
    );
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}"),
        200,
        br#"{"status":"Succeeded"}"#,
        &[(
            "Location",
            format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
        )],
    );
    let native = definition("sqlproj");
    f.executor.response(
        "GET",
        format!("/v1/operations/{OP}/result"),
        200,
        &serde_json::to_vec(&native).unwrap(),
        &[],
    );
    let bytes = stdout(
        &f,
        &[
            "get".into(),
            warehouse_path("/src/definition"),
            "--wait".into(),
        ],
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"], native);
    assert_eq!(f.executor.calls.lock().unwrap().len(), 4);
    assert_eq!(
        f.executor
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn warehouse_definition_rejects_unknown_native_format_before_network() {
    let f = Fixture::new();
    assert!(
        stdout(
            &f,
            &[
                "get".into(),
                warehouse_path("/src/definition/formats/unknown")
            ],
        )
        .await
        .is_err()
    );
    assert!(f.executor.calls.lock().unwrap().is_empty());
    assert!(f.credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn warehouse_management_unexpected_acceptance_keeps_native_handle_and_never_polls() {
    let f = Fixture::new();
    f.executor.response(
        "GET",
        format!("/v1/workspaces/{WS}/warehouses/{ITEM}"),
        202,
        b"",
        &[("x-ms-operation-id", OP.into())],
    );
    let selection =
        WarehouseItemRead::new(WS, ITEM, "Warehouse", WarehouseRepresentation::Metadata).unwrap();
    match warehouse_host(&f)
        .get(
            &selection,
            OperationLimits::default(),
            std::num::NonZeroUsize::new(16_384).unwrap(),
            pending(),
        )
        .await
    {
        Err(WarehouseReadError::Operation(error)) => {
            assert!(matches!(
                error.kind,
                OperationErrorKind::Http { status: 202, .. }
            ));
            assert_eq!(error.operation_id.as_deref(), Some(OP));
        }
        _ => panic!("unexpected management acceptance lost native handle"),
    }
    assert_eq!(f.executor.calls.lock().unwrap().len(), 1);
    assert!(f.executor.waits.lock().unwrap().is_empty());
}
