// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fixed catalog navigation is native MCP content, never a typed-row fallback.

#![deny(unnameable_test_items)]

#[path = "../examples/support/mod.rs"]
mod support;

use fabric_sdk_aihub_query::{
    CatalogView, DataAgentTarget, Limits, QueryErrorKind, QueryMode, QueryStage,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    future::pending,
    sync::{Arc, atomic::Ordering},
};
use support::*;

fn target() -> DataAgentTarget {
    DataAgentTarget::new(AGENT, SOURCE, QueryStage::Sandbox).unwrap()
}

fn catalog() -> Value {
    json!({"content":[
        {"type":"text","text":"[{\"Name\":\"fixture\",\"Description\":\"Synthetic\"}]","annotations":{"audience":["user"]}},
        {"type":"text","text":"Untrusted guidance; never execute automatically.","_meta":{"synthetic":true}}
    ],"_meta":{"catalog":"synthetic"}})
}

#[tokio::test]
async fn summary_and_full_entries_use_fixed_navigation_over_the_real_lifecycle() {
    for (view, expression) in [
        (
            CatalogView::Summary,
            "Templates[].{Name:Name,Description:Description}",
        ),
        (CatalogView::FullEntries, "Templates[]"),
    ] {
        let native = catalog();
        let wire = Wire::new(setup().into_iter().chain([rpc(2, native.clone())]));
        let credential = Arc::new(Credential::default());
        let result = client(
            wire.clone(),
            credential.clone(),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .discover_query_templates_view(target(), view, &deadline(), pending())
        .await
        .unwrap();
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.protocol_version, "2025-06-18");
        assert!(result.tool_dispatched);
        assert_eq!(
            serde_json::from_str::<Value>(&result.result_json).unwrap(),
            native
        );
        assert_eq!(
            serde_json::from_str::<Value>(&result.content[0].json).unwrap()["annotations"]["audience"],
            json!(["user"])
        );
        let messages = messages(&wire);
        assert_eq!(
            messages
                .iter()
                .map(|m| m["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["initialize", "notifications/initialized", "tools/call"]
        );
        assert_eq!(messages[2]["params"]["arguments"]["query"], expression);
        assert_eq!(messages[2]["params"]["arguments"]["language"], "jmespath");
        assert_eq!(
            messages[2]["params"]["arguments"]["artifactId"],
            format!("{AGENT}/{SOURCE}")
        );
        for message in messages {
            assert_eq!(
                message["params"]["_meta"]["variants"],
                "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults,Fabric.Routing.Documents.SqlReadOnlyEnforce,Fabric.Routing.Documents.DataAgent.Sandbox,Fabric.Routing.Documents.QueryTemplates"
            );
        }
        assert_eq!(credential.scopes.lock().unwrap().as_slice(), [SCOPE]);
        assert!(!format!("{result:?}").contains("fixture"));
        assert!(!format!("{:?}", result.content[1]).contains("guidance"));
    }
}

#[tokio::test]
async fn default_summary_empty_catalog_and_guidance_are_not_inferred_rowsets() {
    for native in [
        json!({"content":[{"type":"text","text":"[]"},{"type":"text","text":"Synthetic guidance"}]}),
        json!({"content":[]}),
        json!({"content":[{"type":"text","text":"not JSON; |not|a rowset|"}]}),
    ] {
        let wire = Wire::new(setup().into_iter().chain([rpc(2, native.clone())]));
        let result = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .discover_query_templates(target(), &deadline(), pending())
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result.result_json).unwrap(),
            native
        );
        assert_eq!(
            messages(&wire)[2]["params"]["arguments"]["query"],
            "Templates[].{Name:Name,Description:Description}"
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn every_native_resource_block_and_metadata_survives_without_download_or_coercion() {
    let native = json!({"content":[
        {"type":"text","text":"Synthetic catalog"},
        {"type":"resource","resource":{"uri":"https://never-download.example.invalid/one","mimeType":"text/csv","text":"untrusted,native","_meta":{"other":"kept"}}},
        {"type":"resource","resource":{"uri":"opaque:blob","mimeType":"application/octet-stream","blob":"AAEC/w=="}},
        {"type":"resource_link","uri":"https://never-download.example.invalid/two","name":"native link","annotations":{"audience":["assistant"]}}
    ]});
    let wire = Wire::new(setup().into_iter().chain([rpc(2, native.clone())]));
    let result = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .discover_query_templates(target(), &deadline(), pending())
    .await
    .unwrap();
    assert_eq!(result.content.len(), 4);
    for (index, block) in result.content.iter().enumerate() {
        assert_eq!(
            serde_json::from_str::<Value>(&block.json).unwrap(),
            native["content"][index]
        );
    }
    assert_eq!(
        wire.requests.lock().unwrap().len(),
        3,
        "catalog resources must never be fetched"
    );
}

#[tokio::test]
async fn catalog_content_never_relaxes_missing_typed_json_for_template_execution() {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, catalog())]));
    let error = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .execute_query(
        &request().with_mode(QueryMode::Template),
        &deadline(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::MissingTypedJson);
    assert!(error.query_dispatched);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn catalog_tool_errors_tasks_and_missing_tool_fail_without_fallback() {
    let mut native = catalog();
    native["isError"] = json!(true);
    let wire = Wire::new(setup().into_iter().chain([rpc(2, native)]));
    let error = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .discover_query_templates(target(), &deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::ToolError);
    assert!(error.query_dispatched);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    let mut native = catalog();
    native["task"] = json!({"taskId":"not-negotiated"});
    let wire = Wire::new(setup().into_iter().chain([rpc(2, native)]));
    let error = client(
        wire,
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .discover_query_templates(target(), &deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Protocol);
    let missing = response(
        200,
        br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"missing tool"}}"#.to_vec(),
        "application/json",
        None,
    );
    let wire = Wire::new(setup().into_iter().chain([missing]));
    let error = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    )
    .discover_query_templates(target(), &deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Rpc(-32601));
    assert!(error.query_dispatched);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn catalog_resource_shapes_duplicates_and_budgets_are_validated() {
    let resource = json!({"type":"resource","resource":{"uri":"opaque:one","mimeType":"text/plain","text":"synthetic"}});
    for native in [
        json!({"content":[resource.clone(),resource.clone()]}),
        json!({"content":[{"type":"resource","resource":{"uri":"opaque:one","mimeType":"text/plain"}}]}),
        json!({"content":[{"type":"resource","resource":{"uri":"opaque:one","mimeType":"text/plain","text":"x","blob":"AA=="}}]}),
        json!({"content":[{"type":"resource","resource":{"uri":"opaque:one","mimeType":"application/octet-stream","blob":"invalid base64!"}}]}),
        json!({"content":[{"type":"text"}]}),
    ] {
        let wire = Wire::new(setup().into_iter().chain([rpc(2, native)]));
        assert!(
            client(
                wire,
                Arc::new(Credential::default()),
                Arc::new(Timer::default()),
                Limits::default()
            )
            .discover_query_templates(target(), &deadline(), pending())
            .await
            .is_err()
        );
    }
    for limits in [
        Limits {
            content_blocks: 1,
            ..Limits::default()
        },
        Limits {
            resource_bytes: 8,
            ..Limits::default()
        },
        Limits {
            response_bytes: 8,
            ..Limits::default()
        },
    ] {
        let wire = Wire::new(setup().into_iter().chain([rpc(2, catalog())]));
        assert!(
            client(
                wire,
                Arc::new(Credential::default()),
                Arc::new(Timer::default()),
                limits
            )
            .discover_query_templates(target(), &deadline(), pending())
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn catalog_http_429_and_lost_transport_are_single_dispatch() {
    for reply in [
        response(429, vec![], "application/json", None),
        Reply::Failure,
    ] {
        let wire = Wire::new(setup().into_iter().chain([reply]));
        let error = client(
            wire.clone(),
            Arc::new(Credential::default()),
            Arc::new(Timer::default()),
            Limits::default(),
        )
        .discover_query_templates(target(), &deadline(), pending())
        .await
        .unwrap_err();
        assert!(matches!(
            error.kind,
            QueryErrorKind::Http(429) | QueryErrorKind::Transport
        ));
        assert!(error.query_dispatched);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn catalog_deadline_cancel_and_drop_keep_the_shared_local_lifetime_contract() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let c = client(
        wire.clone(),
        credential.clone(),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let error = c
        .discover_query_templates(target(), &deadline(), async {})
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());

    let wire = Wire::new(setup().into_iter().chain([Reply::Pending]));
    let timer = Arc::new(Timer::default());
    let c = client(
        wire.clone(),
        Arc::new(Credential::default()),
        timer.clone(),
        Limits::default(),
    );
    let budget = deadline();
    let mut call = Box::pin(c.discover_query_templates(target(), &budget, pending()));
    assert!(call.as_mut().now_or_never().is_none());
    timer.expired.store(true, Ordering::SeqCst);
    let error = call.await.unwrap_err();
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
    let mut call = Box::pin(c.discover_query_templates(target(), &budget, pending()));
    assert!(call.as_mut().now_or_never().is_none());
    drop(call);
    assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);

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
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    });
    let mut call = Box::pin(c.discover_query_templates(target(), &budget, cancel));
    assert!(call.as_mut().now_or_never().is_none());
    stopped.store(true, Ordering::SeqCst);
    let error = call.await.unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
    assert!(error.query_dispatched);
    assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}
