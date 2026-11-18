// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_aihub_query::{NativeWorkbookRequest, NativeWorkbookResponse, WORKBOOK_MIME};
use std::future::{poll_fn, ready};

fn request_native(stage: QueryStage, mode: QueryMode) -> NativeWorkbookRequest {
    NativeWorkbookRequest::new(
        DataAgentTarget::new(AGENT, SOURCE, stage).unwrap(),
        "opaque native query".into(),
    )
    .unwrap()
    .with_mode(mode)
    .with_max_rows(10)
    .unwrap()
    .with_filename("synthetic_query")
    .unwrap()
}

fn attachment(bytes: &[u8]) -> Value {
    json!({"type":"resource","resource":{
        "uri":"https://never-fetch.example.invalid/private.xlsx",
        "mimeType":WORKBOOK_MIME,"blob":STANDARD.encode(bytes),
        "_meta":{"syntheticSensitivity":"retain-verbatim"}
    },"annotations":{"audience":["user"],"priority":1},
      "_meta":{"syntheticBlock":"retain-verbatim"}})
}

#[tokio::test]
async fn immutable_query_carrying_workbook_fixture_survives_native_decode_byte_for_byte() {
    const BYTES: &[u8] = include_bytes!("../fixtures/inert-native-workbook.xlsx");
    assert_eq!(BYTES.len(), 7524);
    for marker in [
        b"Microsoft.Mashup.OleDb.1".as_slice(),
        b"connectionId=\"1\"".as_slice(),
        b"customXml/passthrough-sentinel.xml".as_slice(),
    ] {
        assert!(BYTES.windows(marker.len()).any(|bytes| bytes == marker));
    }
    let result = json!({"content":[attachment(BYTES)]});
    let (observed, wire) = observe_native(result, Limits::default()).await;
    assert_eq!(observed.unwrap().workbook.unwrap().bytes, BYTES);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

async fn observe_native(
    result: Value,
    limits: Limits,
) -> (
    Result<NativeWorkbookResponse, fabric_sdk_aihub_query::QueryError>,
    Arc<Wire>,
) {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, result)]));
    let result = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        limits,
    )
    .execute_native_workbook(
        &request_native(QueryStage::Production, QueryMode::Native),
        &deadline(),
        pending(),
    )
    .await;
    (result, wire)
}

#[tokio::test]
async fn native_profile_omits_only_typed_token_on_all_exchanges_for_both_stages_and_modes() {
    for stage in [QueryStage::Production, QueryStage::Sandbox] {
        for mode in [QueryMode::Native, QueryMode::Template] {
            // Protected native workbooks need not be ZIP. Never sniff or rewrite.
            let bytes = b"\xd0\xcf\x11\xe0synthetic-protected-payload";
            let result = json!({"content":[attachment(bytes)],"_meta":{"native":"metadata"}});
            let wire = Wire::new(setup().into_iter().chain([rpc(2, result.clone())]));
            let credential = Arc::new(Credential::default());
            let timer = Arc::new(Timer::default());
            let observed = client(
                wire.clone(),
                credential.clone(),
                timer.clone(),
                Limits::default(),
            )
            .execute_native_workbook(&request_native(stage, mode), &deadline(), pending())
            .await
            .unwrap();
            assert_eq!(observed.protocol_version, "2025-06-18");
            assert_eq!(observed.requested_max_rows, 10);
            assert_eq!(
                serde_json::from_str::<Value>(&observed.result_json).unwrap(),
                result
            );
            assert!(!format!("{observed:?}").contains("synthetic-protected"));
            assert!(!format!("{observed:?}").contains("private.xlsx"));
            let workbook = observed.workbook.unwrap();
            assert_eq!(workbook.bytes, bytes);
            assert_eq!(workbook.mime_type, WORKBOOK_MIME);
            assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
            assert_eq!(timer.waits.lock().unwrap().len(), 1);
            let messages = messages(&wire);
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0]["id"], 1);
            assert_eq!(messages[2]["id"], 2);
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message["method"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["initialize", "notifications/initialized", "tools/call"]
            );
            let mut expected =
                "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.SqlReadOnlyEnforce"
                    .to_owned();
            if stage == QueryStage::Sandbox {
                expected.push_str(",Fabric.Routing.Documents.DataAgent.Sandbox");
            }
            if mode == QueryMode::Template {
                expected.push_str(",Fabric.Routing.Documents.QueryTemplates");
            }
            for message in &messages {
                assert_eq!(message["params"]["_meta"]["variants"], expected);
            }
            let arguments = &messages[2]["params"]["arguments"];
            assert_eq!(arguments["artifactId"], format!("{AGENT}/{SOURCE}"));
            assert_eq!(arguments["query"], "opaque native query");
            assert_eq!(arguments["max_rows"], 10);
            assert_eq!(arguments["filename"], "synthetic_query");
            assert_eq!(
                arguments.as_object().unwrap().len(),
                if mode == QueryMode::Template { 5 } else { 4 }
            );
            if mode == QueryMode::Template {
                assert_eq!(arguments["language"], "jmespath");
            } else {
                assert!(arguments.get("language").is_none());
            }
            for request in wire.requests.lock().unwrap().iter() {
                assert!(
                    request
                        .headers()
                        .get_str(&"fabaihub-x-variants".into())
                        .is_err()
                );
                assert!(request.headers().get_str(&"x-variants".into()).is_err());
            }
        }
    }
}

#[tokio::test]
async fn native_mime_essence_and_auxiliary_resources_preserve_all_annotations_and_metadata() {
    let mut workbook = attachment(b"unchanged-service-bytes");
    workbook["resource"]["mimeType"] = json!(format!(
        " {} ; name=\"native;workbook\" ",
        WORKBOOK_MIME.to_uppercase()
    ));
    let result = json!({"content":[
        {"type":"text","text":"untrusted service prose"},
        {"type":"resource","resource":{"uri":"opaque:csv","mimeType":"text/csv","text":"n\r\n1\r\n"}},
        {"type":"resource","resource":{"uri":"opaque:optional-mime","text":"auxiliary text"}},
        workbook
    ],"_meta":{"serviceIdentity":"opaque-native-id"}});
    let (observed, wire) = observe_native(result.clone(), Limits::default()).await;
    let observed = observed.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&observed.result_json).unwrap(),
        result
    );
    assert_eq!(observed.workbook.unwrap().bytes, b"unchanged-service-bytes");
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn native_missing_zero_empty_corrupt_duplicate_and_multiple_outputs_are_explicit_without_requery()
 {
    let valid = attachment(b"native");
    let mut duplicate = valid.clone();
    duplicate["resource"]["uri"] = json!("opaque:other-workbook");
    let mut cases = vec![
        (json!({"content":[]}), QueryErrorKind::MissingNativeWorkbook),
        (
            json!({"content":[{"type":"text","text":"0 rows"}]}),
            QueryErrorKind::MissingNativeWorkbook,
        ),
        (
            json!({"content":[valid.clone(), valid.clone()]}),
            QueryErrorKind::DuplicateResource,
        ),
        (
            json!({"content":[valid.clone(), duplicate]}),
            QueryErrorKind::MultipleNativeWorkbooks,
        ),
        (
            json!({"content":[attachment(b"")]}),
            QueryErrorKind::InvalidNativeWorkbook,
        ),
    ];
    for blob in ["%%%", "YQ", "YQ==\n", "AR==", "_w=="] {
        let mut block = valid.clone();
        block["resource"]["blob"] = json!(blob);
        cases.push((
            json!({"content":[block]}),
            QueryErrorKind::InvalidNativeWorkbook,
        ));
    }
    for (field, value, kind) in [
        (
            "text",
            json!("not a binary workbook"),
            QueryErrorKind::InvalidNativeWorkbook,
        ),
        (
            "mimeType",
            json!(format!("{WORKBOOK_MIME};")),
            QueryErrorKind::Protocol,
        ),
        ("uri", json!(""), QueryErrorKind::InvalidNativeWorkbook),
        (
            "uri",
            json!("opaque:\nunsafe"),
            QueryErrorKind::InvalidNativeWorkbook,
        ),
    ] {
        let mut block = valid.clone();
        block["resource"][field] = value;
        cases.push((json!({"content":[block]}), kind));
    }
    let mut no_blob = valid.clone();
    no_blob["resource"].as_object_mut().unwrap().remove("blob");
    cases.push((
        json!({"content":[no_blob]}),
        QueryErrorKind::InvalidNativeWorkbook,
    ));
    let mut text_only = valid;
    text_only["resource"]
        .as_object_mut()
        .unwrap()
        .remove("blob");
    text_only["resource"]["text"] = json!("not a blob");
    cases.push((
        json!({"content":[text_only]}),
        QueryErrorKind::InvalidNativeWorkbook,
    ));
    for (result, expected) in cases {
        let (observed, wire) = observe_native(result.clone(), Limits::default()).await;
        let observed = observed.unwrap();
        assert_eq!(observed.workbook.unwrap_err(), expected);
        assert_eq!(
            serde_json::from_str::<Value>(&observed.result_json).unwrap(),
            result
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn typed_json_and_native_excel_remain_strictly_separate_representations() {
    let typed_result = success();
    let (native, wire) = observe_native(typed_result, Limits::default()).await;
    assert_eq!(
        native.unwrap().workbook.unwrap_err(),
        QueryErrorKind::MissingNativeWorkbook
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    let error = observe(json!({"content":[attachment(b"native")]}))
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::MissingTypedJson);
    assert!(error.query_dispatched);
}

#[tokio::test]
async fn native_resource_and_block_budgets_fail_with_completed_response_evidence() {
    for limits in [
        Limits {
            resource_bytes: 64,
            ..Limits::default()
        },
        Limits {
            content_blocks: 1,
            ..Limits::default()
        },
    ] {
        let (observed, wire) = observe_native(
            json!({"content":[
                attachment(&[0_u8; 128]), {"type":"text","text":"auxiliary"}
            ]}),
            limits,
        )
        .await;
        let observed = observed.unwrap();
        assert_eq!(observed.workbook.unwrap_err(), QueryErrorKind::Limit);
        assert!(!observed.result_json.is_empty());
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn native_tool_and_task_errors_never_accept_a_workbook_as_completed_output() {
    for (result, expected) in [
        (
            json!({"isError":true,"content":[attachment(b"native")]}),
            QueryErrorKind::ToolError,
        ),
        (
            json!({"task":{"taskId":"no-poll"},"content":[attachment(b"native")]}),
            QueryErrorKind::Protocol,
        ),
    ] {
        let (observed, wire) = observe_native(result, Limits::default()).await;
        let error = observed.unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(error.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn native_cancellation_and_transport_failure_preserve_dispatch_uncertainty_without_replay() {
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let error = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_native_workbook(
        &request_native(QueryStage::Production, QueryMode::Native),
        &deadline(),
        ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
    assert!(!error.query_dispatched);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    for reply in [Reply::Failure, Reply::Pending] {
        let wire = Wire::new(setup().into_iter().chain([reply]));
        let cancel_wire = wire.clone();
        let result = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_native_workbook(
            &request_native(QueryStage::Production, QueryMode::Native),
            &deadline(),
            poll_fn(move |cx| {
                if cancel_wire.requests.lock().unwrap().len() == 3 {
                    Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            result.kind,
            QueryErrorKind::Transport | QueryErrorKind::Canceled
        ));
        assert!(result.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[test]
fn native_request_uses_existing_argument_validation_and_redacted_debug() {
    let target = DataAgentTarget::new(AGENT, SOURCE, QueryStage::Production).unwrap();
    assert!(NativeWorkbookRequest::new(target.clone(), "".into()).is_err());
    assert!(
        NativeWorkbookRequest::new(target, "native".into())
            .unwrap()
            .with_max_rows(0)
            .is_err()
    );
    assert!(
        request_native(QueryStage::Production, QueryMode::Native)
            .with_filename("../bad")
            .is_err()
    );
    assert!(
        !format!(
            "{:?}",
            request_native(QueryStage::Production, QueryMode::Native)
        )
        .contains("opaque native")
    );
}

#[tokio::test]
async fn native_http_bounds_incomplete_envelopes_and_rpc_errors_never_accept_partial_bytes() {
    let bytes = include_bytes!("../fixtures/inert-native-workbook.xlsx");
    let cases = [
        (
            rpc(2, json!({"content":[attachment(bytes)]})),
            Limits {
                response_bytes: 512,
                ..Limits::default()
            },
            QueryErrorKind::Limit,
        ),
        (
            rpc(3, json!({"content":[attachment(b"native")]})),
            Limits::default(),
            QueryErrorKind::Protocol,
        ),
        (
            response(
                200,
                br#"{"jsonrpc":"2.0","id":2,"result":{"content":["#.to_vec(),
                "application/json",
                None,
            ),
            Limits::default(),
            QueryErrorKind::Protocol,
        ),
        (
            response(
                200,
                br#"{"jsonrpc":"2.0","id":2,"result":{"content":[],"content":[]}}"#.to_vec(),
                "application/json",
                None,
            ),
            Limits::default(),
            QueryErrorKind::Protocol,
        ),
        (
            response(
                200,
                br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"private-query"}}"#
                    .to_vec(),
                "application/json",
                None,
            ),
            Limits::default(),
            QueryErrorKind::Rpc(-32602),
        ),
        (
            response(403, b"private-service-detail".to_vec(), "text/plain", None),
            Limits::default(),
            QueryErrorKind::Http(403),
        ),
    ];
    for (reply, limits, expected) in cases {
        let wire = Wire::new(setup().into_iter().chain([reply]));
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            limits,
        )
        .execute_native_workbook(
            &request_native(QueryStage::Production, QueryMode::Native),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(error.query_dispatched);
        assert!(!error.to_string().contains("private-"));
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn native_absolute_deadline_after_dispatch_is_not_remote_cancellation_or_replay() {
    let wire = Wire::new(setup().into_iter().chain([Reply::Pending]));
    let timer = Arc::new(Timer::default());
    let expire_timer = timer.clone();
    let expire_wire = wire.clone();
    let error = client(
        wire.clone(),
        Arc::new(Credential::default()),
        timer.clone(),
        Limits::default(),
    )
    .execute_native_workbook(
        &request_native(QueryStage::Production, QueryMode::Native),
        &deadline(),
        poll_fn(move |cx| {
            if expire_wire.requests.lock().unwrap().len() == 3 {
                expire_timer.expired.store(true, Ordering::SeqCst);
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Deadline);
    assert!(error.query_dispatched);
    assert_eq!(timer.waits.lock().unwrap().len(), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}
