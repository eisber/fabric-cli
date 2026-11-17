// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic public-API tests; no credentials or Fabric services are discovered.

#![deny(unnameable_test_items)]

#[path = "../examples/support/mod.rs"]
mod support;

use azure_core::http::Url;
use fabric_sdk_aihub_query::{
    AuthBinding, Completeness, DataAgentTarget, Limits, NativeValue, QueryErrorKind, QueryMode,
    QueryOutcome, QueryRequest, QueryStage, TypedJson,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
};
use support::*;

async fn observe(result: Value) -> Result<QueryOutcome, fabric_sdk_aihub_query::QueryError> {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, result)]));
    client(
        wire,
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), pending())
    .await
}

fn complete(outcome: QueryOutcome) -> fabric_sdk_aihub_query::TypedQueryResult {
    match outcome {
        QueryOutcome::Complete(result) => result,
        _ => panic!("unexpected incomplete data"),
    }
}

fn mutate_text(block: &mut Value, change: impl FnOnce(&mut Value)) {
    let mut rowset: Value =
        serde_json::from_str(block["resource"]["text"].as_str().unwrap()).unwrap();
    change(&mut rowset);
    *block = typed(&rowset.to_string(), "file:///changed.json");
}

#[test]
fn shared_json_parser_preserves_private_sentinel_keys_as_ordinary_objects() {
    let text = r#"{"$serde_json::private::RawValue":"123","other":true}"#;
    let value: Value = serde_json::from_str(text).unwrap();
    assert!(value.is_object());
    assert_eq!(value["$serde_json::private::RawValue"], "123");
    assert_eq!(value["other"], true);
    let text = r#"{"$serde_json::private::Number":"123"}"#;
    let value: Value = serde_json::from_str(text).unwrap();
    assert!(value.is_object());
    assert_eq!(value["$serde_json::private::Number"], "123");
}

#[tokio::test]
async fn real_lifecycle_opt_in_binding_and_no_export_preserve_exact_values() {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
    let credential = Arc::new(Credential::default());
    let timer = Arc::new(Timer::default());
    let result = complete(
        client(
            wire.clone(),
            credential.clone(),
            timer.clone(),
            Limits::default(),
        )
        .execute_query(&request(), &deadline(), pending())
        .await
        .unwrap(),
    );
    assert_eq!(result.completeness, Completeness::Complete);
    let table = &result.resultsets[0];
    assert_eq!(
        table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["value", "value", ""]
    );
    assert_eq!(table.columns[2].engine_type, "vendor-extension");
    assert_eq!(
        table.rows[0],
        [
            NativeValue::Integer("18446744073709551615".into()),
            NativeValue::Decimal("0.1000000000000000000000000001".into()),
            NativeValue::Null
        ]
    );
    assert_eq!(table.total_row_count, None);
    assert_eq!(credential.scopes.lock().unwrap().as_slice(), [SCOPE]);
    assert_eq!(timer.waits.lock().unwrap().len(), 1);
    let messages = messages(&wire);
    assert_eq!(
        messages
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/call"]
    );
    assert_eq!(
        messages[2]["params"]["arguments"]["artifactId"],
        format!("{AGENT}/{SOURCE}")
    );
    assert!(messages[2]["params"]["arguments"].get("language").is_none());
    for (index, r) in wire.requests.lock().unwrap().iter().enumerate() {
        assert!(r.headers().get_str(&"fabaihub-x-variants".into()).is_err());
        if index > 0 {
            assert_eq!(
                r.headers().get_str(&"mcp-session-id".into()).unwrap(),
                "synthetic-session"
            );
            assert_eq!(
                r.headers().get_str(&"mcp-protocol-version".into()).unwrap(),
                "2025-06-18"
            );
        }
    }
    for message in messages {
        assert_eq!(
            message["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults,Fabric.Routing.Documents.SqlReadOnlyEnforce"
        );
    }
    assert!(!format!("{result:?}").contains("184467"));
}

#[tokio::test]
async fn sandbox_template_multiple_outputs_and_auxiliary_resources_are_retained() {
    let mut second: Value = serde_json::from_str(&rowset()).unwrap();
    second["resultId"] = json!("44444444-4444-4444-8444-444444444444");
    let result = json!({"content":[typed(&rowset(),"file:///one.json"),
        {"type":"resource","resource":{"uri":"https://never-download.example.invalid/template","mimeType":"text/plain","text":"synthetic template"}},
        typed(&second.to_string(),"file:///two.json")]});
    let wire = Wire::new(setup().into_iter().chain([rpc(2, result)]));
    let req = QueryRequest::new(
        DataAgentTarget::new(AGENT, SOURCE, QueryStage::Sandbox).unwrap(),
        "synthetic_template(`null`)".into(),
        TypedJson::Required,
    )
    .unwrap()
    .with_mode(QueryMode::Template)
    .with_filename("result")
    .unwrap();
    let result = complete(
        client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_query(&req, &deadline(), pending())
        .await
        .unwrap(),
    );
    assert_eq!(result.resultsets.len(), 2);
    assert_eq!(result.resources.len(), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    for message in messages(&wire) {
        assert_eq!(
            message["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults,Fabric.Routing.Documents.SqlReadOnlyEnforce,Fabric.Routing.Documents.DataAgent.Sandbox,Fabric.Routing.Documents.QueryTemplates"
        );
    }
    assert_eq!(
        messages(&wire)[2]["params"]["arguments"]["language"],
        "jmespath"
    );
}

#[tokio::test]
async fn null_empty_and_zero_rows_keep_the_authoritative_schema() {
    for rows in [json!([]), json!([[null, null, null]])] {
        let mut rowset: Value = serde_json::from_str(&rowset()).unwrap();
        rowset["rowCount"] = json!(rows.as_array().unwrap().len());
        rowset["rows"] = rows;
        let result = complete(
            observe(json!({"content":[typed(&rowset.to_string(),"opaque:no-extension")]}))
                .await
                .unwrap(),
        );
        assert_eq!(result.resultsets[0].columns.len(), 3);
    }
    let text = format!(
        r#"{{"schemaVersion":1,"resultId":"{RESULT}","columns":[],"rows":[],
        "rowCount":0,"truncated":false,"maxRows":null,"totalRowCount":0}}"#
    );
    assert!(
        complete(
            observe(json!({"content":[typed(&text,"opaque:empty")]}))
                .await
                .unwrap()
        )
        .resultsets[0]
            .columns
            .is_empty()
    );
}

#[tokio::test]
async fn native_temporal_binary_guid_boolean_signed_and_unknown_values_survive() {
    let cases = [
        (
            "int64",
            "integer",
            "json-number",
            json!(-9007199254740993_i64),
            NativeValue::Integer("-9007199254740993".into()),
        ),
        (
            "datetimeoffset",
            "datetimeoffset",
            "iso8601",
            json!("2026-01-02T03:04:05.1234567+05:30"),
            NativeValue::DateTimeOffset("2026-01-02T03:04:05.1234567+05:30".into()),
        ),
        (
            "datetime",
            "datetime",
            "iso8601",
            json!("2026-01-02T03:04:05.0000000"),
            NativeValue::DateTime("2026-01-02T03:04:05.0000000".into()),
        ),
        (
            "date",
            "date",
            "iso8601",
            json!("2026-01-02"),
            NativeValue::Date("2026-01-02".into()),
        ),
        (
            "time",
            "time",
            "iso8601",
            json!("03:04:05.1234567"),
            NativeValue::Time("03:04:05.1234567".into()),
        ),
        (
            "timespan",
            "duration",
            "text",
            json!("-2.03:04:05.1234567"),
            NativeValue::Duration("-2.03:04:05.1234567".into()),
        ),
        (
            "binary",
            "binary",
            "base64",
            json!("AAEC/w=="),
            NativeValue::Binary("AAEC/w==".into()),
        ),
        (
            "guid",
            "guid",
            "text",
            json!(AGENT),
            NativeValue::Guid(AGENT.into()),
        ),
        (
            "boolean",
            "boolean",
            "json-boolean",
            json!(false),
            NativeValue::Boolean(false),
        ),
        (
            "native-unknown",
            "string",
            "text",
            json!("Unicode \u{1f642}"),
            NativeValue::Text("Unicode \u{1f642}".into()),
        ),
    ];
    for (engine, logical, encoding, value, expected) in cases {
        let mut rowset: Value = serde_json::from_str(&rowset()).unwrap();
        rowset["columns"] = json!([{"name":"column","engineType":engine,"logicalType":logical,"encoding":encoding}]);
        rowset["rows"] = json!([[value]]);
        let result = complete(
            observe(json!({"content":[typed(&rowset.to_string(),"opaque:typed")]}))
                .await
                .unwrap(),
        );
        assert_eq!(result.resultsets[0].rows[0][0], expected);
    }
}

#[tokio::test]
async fn truncation_is_an_explicit_incomplete_outcome() {
    let mut block = typed(&rowset(), "opaque:truncated");
    mutate_text(&mut block, |r| {
        r["truncated"] = json!(true);
        r["totalRowCount"] = json!(2000);
    });
    let QueryOutcome::Incomplete(result) = observe(json!({"content":[block]})).await.unwrap()
    else {
        panic!("truncated data reported complete")
    };
    assert_eq!(result.completeness, Completeness::Truncated);
}

#[tokio::test]
async fn legacy_csv_markdown_direct_strings_and_structured_content_are_not_typed_data() {
    let cases = [
        json!({"content":[{"type":"resource","resource":{"uri":"file:///looks-like.json","mimeType":"text/csv","text":"a\n1"}}]}),
        json!({"content":[{"type":"text","text":"|a|\n|1|"}]}),
        json!({"content":[{"type":"text","text":"legacy direct artifact string"}],"structuredContent":{"rows":[[1]]}}),
        json!({"content":[{"type":"resource","resource":{"uri":"file:///fake.json","mimeType":"application/json","text":"{}"}}]}),
    ];
    for case in cases {
        assert_eq!(
            observe(case).await.unwrap_err().kind,
            QueryErrorKind::MissingTypedJson
        );
    }
}

#[tokio::test]
async fn invalid_version_count_schema_metadata_ids_nulls_and_duplicate_resources_fail() {
    for mutation in 0..16 {
        let mut block = typed(&rowset(), "opaque:result");
        match mutation {
            0 => mutate_text(&mut block, |r| r["schemaVersion"] = json!(2)),
            1 => mutate_text(&mut block, |r| r["rowCount"] = json!(2)),
            2 => mutate_text(&mut block, |r| r["rows"] = json!([[1]])),
            3 => mutate_text(&mut block, |r| r["totalRowCount"] = json!(0)),
            4 => mutate_text(&mut block, |r| r["columns"][0]["encoding"] = json!("text")),
            5 => mutate_text(&mut block, |r| {
                r["columns"][0]["logicalType"] = json!("dynamic")
            }),
            6 => block["resource"]["_meta"]["fabric.queryResult"]["resultId"] = json!(AGENT),
            7 => block["resource"]["_meta"]["fabric.queryResult"]["rowCount"] = json!(0),
            8 => block["resource"]["_meta"]["fabric.queryResult"]["nulls"] = json!([]),
            9 => {
                block["resource"]["_meta"]["fabric.queryResult"]["columns"][0]["name"] =
                    json!("mismatch")
            }
            10 => block["resource"]["mimeType"] = json!("text/plain"),
            11 => mutate_text(&mut block, |r| r["resultId"] = json!("invalid")),
            12 => mutate_text(&mut block, |r| r["rows"][0][0] = json!(-1)),
            13 => mutate_text(&mut block, |r| r["rows"][0][1] = json!("0.1")),
            14 => mutate_text(&mut block, |r| r["maxRows"] = json!(0)),
            _ => block["resource"]["_meta"]["fabric.queryResult"]["nulls"] = json!([[0, 2, 999]]),
        }
        assert!(
            observe(json!({"content":[block]})).await.is_err(),
            "mutation {mutation}"
        );
    }
    let block = typed(&rowset(), "opaque:duplicate");
    assert_eq!(
        observe(json!({"content":[block.clone(),block]}))
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::DuplicateResource
    );
    assert_eq!(
        observe(json!({"content":[typed(&rowset(),"one"),typed(&rowset(),"two")]}))
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::DuplicateResource
    );
}

#[tokio::test]
async fn typed_json_duplicate_fields_and_malformed_number_representations_fail() {
    let duplicated = rowset().replacen(
        "\"schemaVersion\":1",
        "\"schemaVersion\":1,\"schemaVersion\":1",
        1,
    );
    let mut block = typed(&rowset(), "opaque:duplicate-fields");
    block["resource"]["text"] = json!(duplicated);
    assert_eq!(
        observe(json!({"content":[block]})).await.unwrap_err().kind,
        QueryErrorKind::InvalidTypedResource
    );
    let mut block = typed(&rowset(), "opaque:integer");
    mutate_text(&mut block, |r| r["rows"][0][0] = json!(1.5));
    assert!(observe(json!({"content":[block]})).await.is_err());
}

#[tokio::test]
async fn tool_rpc_unknown_and_task_errors_never_return_completed_data() {
    let mut result = success();
    result["isError"] = json!(true);
    assert_eq!(
        observe(result).await.unwrap_err().kind,
        QueryErrorKind::ToolError
    );
    let mut result = success();
    result["task"] = json!({"taskId":"not-negotiated","status":"working"});
    assert_eq!(
        observe(result).await.unwrap_err().kind,
        QueryErrorKind::Protocol
    );
    for bytes in [
        br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"synthetic-sensitive-provider-text"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":99,"result":{"content":[]}}"#,
        br#"{"jsonrpc":"2.0","id":2,"result":{"content":[]},"result":{"content":[]}}"#,
        br#"{"jsonrpc":"2.0","id":2,"result":null}"#,
    ] {
        let wire = Wire::new(setup().into_iter().chain([response(200,bytes.to_vec(),"application/json",None)]));
        let error = client(wire,Arc::new(Credential::default()),Arc::new(Timer::default()),Limits::default())
            .execute_query(&request(),&deadline(),pending()).await.unwrap_err();
        assert!(error.query_dispatched);
        assert!(!format!("{error} {error:?}").contains("synthetic-sensitive"));
    }
}

#[tokio::test]
async fn fixed_tool_call_never_lists_tools_in_native_or_template_mode() {
    for mode in [QueryMode::Native, QueryMode::Template] {
        let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
        let result = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits {
                discovery_pages: 0,
                ..Limits::default()
            },
        )
        .execute_query(&request().with_mode(mode), &deadline(), pending())
        .await
        .unwrap();
        assert_eq!(complete(result).resultsets.len(), 1);
        let sent = messages(&wire);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2]["id"], 2);
        assert_eq!(sent[2]["method"], "tools/call");
        assert_eq!(sent[2]["params"]["name"], "execute_query");
        assert!(sent.iter().all(|message| message["method"] != "tools/list"));
        assert_eq!(
            sent[2]["params"]["arguments"]
                .get("language")
                .and_then(Value::as_str),
            (mode == QueryMode::Template).then_some("jmespath")
        );
    }
}

#[tokio::test]
async fn unsupported_protocol_or_missing_tool_capability_reject_before_query() {
    for mutation in 0..5 {
        let mut result = json!({
            "protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
            "serverInfo":{"name":"offline-fixture","version":"1"},
        });
        match mutation {
            0 => result["protocolVersion"] = json!("unsupported-version"),
            1 => result["capabilities"] = json!({}),
            2 => result["capabilities"]["tools"] = json!([]),
            3 => result["serverInfo"]["name"] = json!(""),
            _ => result["serverInfo"]["version"] = json!(""),
        }
        let wire = Wire::new([rpc(1, result)]);
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_query(&request(), &deadline(), pending())
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Protocol);
        assert!(!error.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unavailable_fixed_tool_returns_the_remote_error_without_discovery_or_replay() {
    let reply = response(
        200,
        br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"synthetic-sensitive-missing-tool"}}"#.to_vec(),
        "application/json",
        None,
    );
    let wire = Wire::new(setup().into_iter().chain([reply]));
    let error = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Rpc(-32601));
    assert!(error.query_dispatched);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    assert!(!format!("{error} {error:?}").contains("synthetic-sensitive"));
}

#[tokio::test]
async fn direct_call_sse_uses_the_fixed_request_id_without_discovery_or_replay() {
    let data = json!({"jsonrpc":"2.0","id":2,"result":success()});
    let sse = format!(
        ": keepalive\n\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{}}}}\n\nevent: message\ndata: {data}\n\n"
    );
    let wire = Wire::new([
        initialize(),
        response(202, vec![], "application/json", None),
        response(
            200,
            sse.into_bytes(),
            "text/event-stream; charset=utf-8",
            None,
        ),
    ]);
    let result = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), pending())
    .await
    .unwrap();
    assert_eq!(complete(result).resultsets.len(), 1);
    let messages = messages(&wire);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["id"], 2);
    assert_eq!(messages[2]["method"], "tools/call");
}

#[tokio::test]
async fn http_auth_permission_redirect_accepted_and_429_are_not_replayed() {
    for status in [202, 301, 307, 401, 403, 429, 500] {
        let wire = Wire::new(setup().into_iter().chain([response(
            status,
            vec![],
            "application/json",
            None,
        )]));
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_query(&request(), &deadline(), pending())
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Http(status));
        assert!(error.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
    let credential = Arc::new(Credential {
        fail: true,
        ..Credential::default()
    });
    let wire = Wire::new([]);
    let error = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Authentication);
    assert!(!error.query_dispatched);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transport_and_stream_failure_after_call_retain_unknown_dispatch_evidence() {
    for reply in [
        Reply::Failure,
        Reply::Response {
            status: 200,
            media: "application/json".into(),
            session: None,
            chunks: vec![Chunk::Failure],
        },
    ] {
        let wire = Wire::new(setup().into_iter().chain([reply]));
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_query(&request(), &deadline(), pending())
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Transport);
        assert!(error.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn cancellation_deadline_and_drop_dispose_local_work_without_remote_cancellation() {
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let c = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    assert_eq!(
        c.execute_query(&request(), &deadline(), async {})
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::Canceled
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);

    let wire = Wire::new(setup().into_iter().chain([Reply::Pending]));
    let timer = Arc::new(Timer::default());
    let c = client(wire.clone(), credential, timer.clone(), Limits::default());
    let request = request();
    let deadline = deadline();
    let mut future = Box::pin(c.execute_query(&request, &deadline, pending()));
    assert!(future.as_mut().now_or_never().is_none());
    timer.expired.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Deadline);
    assert!(error.query_dispatched);
    assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);

    let wire = Wire::new(setup().into_iter().chain([Reply::Pending]));
    let c = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let mut future = Box::pin(c.execute_query(&request, &deadline, pending()));
    assert!(future.as_mut().now_or_never().is_none());
    drop(future);
    assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn pending_credentials_and_pending_body_are_dropped_on_cancellation() {
    let wire = Wire::new([]);
    let credential = Arc::new(Credential {
        suspend: true,
        ..Credential::default()
    });
    let c = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let req = request();
    let budget = deadline();
    let mut future = Box::pin(c.execute_query(&req, &budget, pending()));
    assert!(future.as_mut().now_or_never().is_none());
    drop(future);
    assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
    assert!(wire.requests.lock().unwrap().is_empty());

    let reply = Reply::Response {
        status: 200,
        media: "application/json".into(),
        session: None,
        chunks: vec![Chunk::Pending],
    };
    let wire = Wire::new(setup().into_iter().chain([reply]));
    let timer = Arc::new(Timer::default());
    let c = client(
        wire.clone(),
        Arc::new(Credential::default()),
        timer.clone(),
        Limits::default(),
    );
    let mut future = Box::pin(c.execute_query(&req, &budget, pending()));
    assert!(future.as_mut().now_or_never().is_none());
    timer.expired.store(true, Ordering::SeqCst);
    assert_eq!(future.await.unwrap_err().kind, QueryErrorKind::Deadline);
    assert_eq!(wire.body_dropped.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn every_resource_shape_and_body_budget_is_enforced() {
    for limits in [
        Limits {
            response_bytes: 8,
            ..Limits::default()
        },
        Limits {
            resource_bytes: 8,
            ..Limits::default()
        },
        Limits {
            columns: 1,
            ..Limits::default()
        },
        Limits {
            cells: 1,
            ..Limits::default()
        },
        Limits {
            content_blocks: 1,
            ..Limits::default()
        },
    ] {
        let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
        assert!(
            client(
                wire,
                Arc::new(Credential::default()),
                Arc::new(Timer::default()),
                limits
            )
            .execute_query(&request(), &deadline(), pending())
            .await
            .is_err()
        );
    }
}

#[test]
fn invalid_bindings_targets_and_arguments_fail_without_credentials_or_network() {
    for endpoint in [
        "http://aihub.example.invalid/mcp",
        "https://user@aihub.example.invalid/mcp",
        "https://aihub.example.invalid/mcp?token=bad",
    ] {
        assert!(
            AuthBinding::new(
                Url::parse(endpoint).unwrap(),
                SCOPE.into(),
                "tenant".into(),
                "account".into()
            )
            .is_err()
        );
    }
    for id in [
        "workspace/item",
        "",
        "00000000-0000-0000-0000-000000000000",
        "not-a-guid",
    ] {
        assert!(DataAgentTarget::new(id, SOURCE, QueryStage::Production).is_err());
    }
    assert!(request().with_max_rows(0).is_err());
    assert!(request().with_max_rows(10001).is_err());
    assert!(request().with_filename("../bad.csv").is_err());
    assert!(!format!("{:?}", request()).contains("synthetic query"));
}

struct ReadyOnSecondPoll(usize);
impl Future for ReadyOnSecondPoll {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        self.0 += 1;
        if self.0 > 1 {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[tokio::test]
async fn simultaneous_ready_cancellation_suppresses_success() {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
    let error = client(
        wire,
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), ReadyOnSecondPoll(0))
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
}

#[tokio::test]
async fn cancellation_of_dispatched_call_drops_transport_and_is_latched() {
    let wire = Wire::new(setup().into_iter().chain([Reply::Pending]));
    let c = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let cancel = std::future::poll_fn(|_| {
        if stopped.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let req = request();
    let budget = deadline();
    let mut future = Box::pin(c.execute_query(&req, &budget, cancel));
    assert!(future.as_mut().now_or_never().is_none());
    stopped.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
    assert!(error.query_dispatched);
    assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn exact_numeric_lexemes_and_required_nullable_metadata_are_validated() {
    for number in [
        "1.2300",
        "-0.000",
        "1234567890123456789012345678.9",
        "1.25e-20",
    ] {
        let text = rowset().replace("0.1000000000000000000000000001", number);
        let result = complete(
            observe(json!({"content":[typed(&text,"opaque:exact")]}))
                .await
                .unwrap(),
        );
        assert_eq!(
            result.resultsets[0].rows[0][1],
            NativeValue::Decimal(number.into())
        );
    }
    let mut block = typed(&rowset(), "opaque:required");
    let text = rowset().replace("\"totalRowCount\":null,", "");
    block["resource"]["text"] = json!(text);
    assert_eq!(
        observe(json!({"content":[block]})).await.unwrap_err().kind,
        QueryErrorKind::InvalidTypedResource
    );
}

#[tokio::test]
async fn terminal_sse_frame_completes_without_transport_eof_at_every_byte_boundary() {
    let mut result = success();
    result["content"][0]["text"] = json!("Synthetic \u{1f642} summary");
    let event = format!(
        "event: message\r\ndata: {}\r\n\r\n",
        json!({"jsonrpc":"2.0","id":2,"result":result})
    );
    let chunks = event
        .bytes()
        .map(|b| Chunk::Bytes(vec![b]))
        .chain([Chunk::Pending])
        .collect();
    let reply = Reply::Response {
        status: 200,
        media: "text/event-stream".into(),
        session: None,
        chunks,
    };
    let wire = Wire::new(setup().into_iter().chain([reply]));
    let result = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(&request(), &deadline(), pending())
    .await
    .unwrap();
    assert_eq!(complete(result).resultsets.len(), 1);
    assert_eq!(wire.body_dropped.load(Ordering::SeqCst), 3);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn duplicate_and_wrong_id_sse_terminals_fail_without_waiting_for_eof() {
    let right = format!(
        "data: {}\n\n",
        json!({"jsonrpc":"2.0","id":2,"result":success()})
    );
    let wrong = format!(
        "data: {}\n\n",
        json!({"jsonrpc":"2.0","id":99,"result":success()})
    );
    for event in [right.repeat(2), wrong] {
        let reply = Reply::Response {
            status: 200,
            media: "text/event-stream".into(),
            session: None,
            chunks: vec![Chunk::Bytes(event.into_bytes()), Chunk::Pending],
        };
        let wire = Wire::new(setup().into_iter().chain([reply]));
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .execute_query(&request(), &deadline(), pending())
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Protocol);
        assert!(error.query_dispatched);
        assert_eq!(wire.body_dropped.load(Ordering::SeqCst), 3);
    }
}
