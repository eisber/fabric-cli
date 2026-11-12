// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Leaf contracts use the existing parser, runtime head and actual Livy SDK.

use super::{HttpFake, L, N, Resolver, Sink, V, complete, input};
use azure_core::http::Method;
use fabric_cli::runtime_v2::{
    OwnerKind, ReadErrorKind, ReadOptions, ReadStatus, ResolvedTarget, RuntimeHead,
    RuntimeResolver, TypedPath, WorkloadObservation, livy_session_get as leaf, parse_args,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    livy::LivyService, runtime::NativeError,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor, native_runtime::NativeFabricExecutor};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

#[allow(dead_code)]
#[path = "../../../../../tests/compatibility/jobs/support.rs"]
mod wire_support;

use wire_support::{ScriptedHttp, TestCredential, TestTimer, response};

#[test]
fn claims_only_get_of_exact_classic_sessions_with_typed_owners() {
    for owner in [L, V] {
        for id in ["0", "opaque-session_~.7", "0007"] {
            assert!(leaf::claims(&input(
                "get",
                &format!("{owner}/sessions/{id}")
            )));
        }
    }
    for (verb, path) in [
        ("list", format!("{L}/sessions")),
        ("list", format!("{V}/sessions")),
        ("get", format!("{N}/sessions/0")),
        ("get", format!("{N}/src/content")),
        ("get", format!("{L}/sessions/0/statements/1")),
        ("get", format!("{V}/sessions/0/statements/1/result")),
        ("get", L.into()),
    ] {
        assert!(!leaf::claims(&input(verb, &path)));
    }
    for path in [
        format!("{L}/sessions"),
        format!("{L}/sessions/active"),
        format!("{L}/sessions/livy-history"),
        format!("{L}/sessions/0/state"),
        "/Team.Workspace/Data.Report/sessions/0".into(),
    ] {
        assert!(parse_args(["fab-cli-v2", "get", &path]).is_err());
    }
}

#[test]
fn foreign_routes_reject_before_resolution_output_or_native_execution() {
    let http = HttpFake::json(json!({"id": "j1"}));
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut sink = Sink::default();
    let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
    let error = complete(leaf::run(
        &head,
        &input("get", &format!("{N}/sessions/j1")),
        &mut sink,
    ))
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::UnsupportedCapability);
    assert_eq!(resolver.calls.get(), 0);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.checks, 0);
    assert_eq!(sink.writes, 0);
}

#[test]
fn exact_native_opaque_session_and_existing_alias_keep_provider_json() {
    for (owner, kind) in [(L, OwnerKind::Lakehouse), (V, OwnerKind::LivyService)] {
        let value = json!({
            "id": "opaque-session_~.7",
            "state": "future-provider-state",
            "future": {"nested": [null, false, 3]},
            "errorInfo": [{"message": "retained but never diagnostic output"}]
        });
        let http = HttpFake::json(value.clone());
        let resolver = Resolver::new(kind);
        let mut sink = Sink::default();
        let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
        let receipt = complete(leaf::run(
            &head,
            &input("get", &format!("{owner}/sessions/opaque-session_~.7")),
            &mut sink,
        ))
        .unwrap();
        assert_eq!(receipt.data, value);
        assert_eq!(receipt.status, ReadStatus::ReadComplete);
        assert_eq!(
            receipt.meta.workload_observation,
            Some(WorkloadObservation::Unknown)
        );
        assert_eq!(
            http.request().url().as_str(),
            "https://livy.example.invalid/base/sessions/opaque-session_~.7"
        );
        assert_eq!(http.calls(), 1);
        assert_eq!(resolver.calls.get(), 1);
        assert_eq!(sink.writes, 1);
        let encoded: Value = serde_json::from_slice(&sink.bytes).unwrap();
        assert_eq!(encoded["data"], value);
    }
}

#[test]
fn explicitly_bound_fabric_get_does_not_change_profile_or_add_paging() {
    let http = HttpFake::json(json!({"id": 7, "state": "error"}));
    let resolver = Resolver {
        fabric: true,
        ..Resolver::new(OwnerKind::Lakehouse)
    };
    let mut sink = Sink::default();
    let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
    let receipt = complete(leaf::run(
        &head,
        &input("get", &format!("{L}/sessions/7")),
        &mut sink,
    ))
    .unwrap();
    assert_eq!(
        http.request().url().as_str(),
        "https://fabric.example.invalid/v1/workspaces/ws/lakehouses/lh/livyApi/versions/2024-04-01/sessions/7"
    );
    assert!(!http.request().headers().contains_key("x-requested-by"));
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Failed)
    );
    assert_eq!(http.calls(), 1);
}

#[test]
fn returned_id_is_exact_not_numeric_name_or_display_name_equivalence() {
    for value in [
        json!({"id": 7}),
        json!({"id": "7", "name": "007"}),
        json!({"id": false}),
        json!({"sessionId": "007"}),
    ] {
        let http = HttpFake::json(value);
        let resolver = Resolver::new(OwnerKind::Lakehouse);
        let mut sink = Sink::default();
        let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
        assert!(
            complete(leaf::run(
                &head,
                &input("get", &format!("{L}/sessions/007")),
                &mut sink,
            ))
            .is_err()
        );
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn native_alias_never_uses_a_fabric_binding_or_retries_another_owner() {
    let http = HttpFake::json(json!({"id": 0}));
    let resolver = Resolver {
        fabric: true,
        ..Resolver::new(OwnerKind::LivyService)
    };
    let mut sink = Sink::default();
    let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
    let error = complete(leaf::run(
        &head,
        &input("get", &format!("{V}/sessions/0")),
        &mut sink,
    ))
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::WrongOwner);
    assert_eq!(resolver.calls.get(), 1);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.checks, 0);
    assert_eq!(sink.writes, 0);
}

#[test]
fn accepted_get_is_not_session_read_completion_or_workload_readiness() {
    let mut http = HttpFake::json(Value::Null);
    http.status = 202;
    http.body.clear();
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let mut sink = Sink::default();
    let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
    let receipt = complete(leaf::run(
        &head,
        &input("get", &format!("{L}/sessions/7")),
        &mut sink,
    ))
    .unwrap();
    assert_eq!(receipt.status, ReadStatus::Accepted);
    assert_eq!(receipt.meta.http_status, 202);
    assert_eq!(receipt.meta.workload_observation, None);
    assert_eq!(receipt.data, Value::Null);
    assert_eq!(http.calls(), 1);
    assert_eq!(sink.writes, 1);
}

#[test]
fn partial_empty_denied_and_malformed_replies_never_deliver_or_replay() {
    for (status, body) in [
        (206, r#"{"id":7,"state":"idle"}"#),
        (204, ""),
        (403, r#"{"private":"not a diagnostic"}"#),
        (200, r#"{"id":7,"state":"idle""#),
        (200, ""),
        (200, r#"{"id":7,"state":false}"#),
    ] {
        let mut http = HttpFake::json(Value::Null);
        http.status = status;
        http.body = body.as_bytes().to_vec();
        let resolver = Resolver::new(OwnerKind::Lakehouse);
        let mut sink = Sink::default();
        let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
        let error = complete(leaf::run(
            &head,
            &input("get", &format!("{L}/sessions/7")),
            &mut sink,
        ))
        .unwrap_err();
        assert_eq!(error.http_status, Some(status));
        assert!(!format!("{error} {error:?}").contains("private"));
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn failed_output_retains_observed_session_without_replaying_the_read() {
    let value = json!({"id": 7, "state": "error", "future": {"keep": true}});
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let mut sink = Sink {
        fail_write: true,
        ..Sink::default()
    };
    let head = RuntimeHead::inject(&http, &resolver, ReadOptions::default()).unwrap();
    let error = complete(leaf::run(
        &head,
        &input("get", &format!("{L}/sessions/7")),
        &mut sink,
    ))
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    let receipt = error.receipt.unwrap();
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Failed)
    );
    assert_eq!(http.calls(), 1);
    assert_eq!(sink.writes, 1);
    assert!(!sink.bytes.is_empty());
}

struct FabricResolver;

impl RuntimeResolver for FabricResolver {
    fn resolve(&self, path: &TypedPath) -> Result<ResolvedTarget, NativeError> {
        assert_eq!(path.owner.workspace, "Team.Workspace");
        assert_eq!(path.owner.artifact, "Data.Lakehouse");
        Ok(ResolvedTarget::Lakehouse(LivyService::fabric(
            super::endpoint("https://api.fabric.microsoft.com"),
            "ws",
            "lh",
        )?))
    }
}

fn fabric_executor(
    http: Arc<ScriptedHttp>,
    credential: Arc<TestCredential>,
) -> NativeFabricExecutor {
    let binding = EndpointBinding::public(ServiceOrigin::Fabric);
    NativeFabricExecutor::new(
        RequestContext::with_endpoint(
            credential,
            CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric)
                .unwrap(),
            binding.clone(),
        ),
        HttpExecutor::new(
            http,
            Arc::new(TestTimer::default()),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(binding),
    )
    .unwrap()
}

#[tokio::test]
async fn parser_to_leaf_to_sdk_uses_actual_fabric_http_and_credential_seam() {
    let value = json!({
        "id": "opaque-session",
        "state": "error",
        "errorInfo": [{"message": "provider details retained in data only"}],
        "future": {"keep": [null, 1]}
    });
    let http = ScriptedHttp::new([response(
        200,
        &value.to_string(),
        &[("content-type", "application/json")],
    )]);
    let credential = Arc::new(TestCredential::default());
    let executor = fabric_executor(http.clone(), credential.clone());
    let resolver = FabricResolver;
    let head = RuntimeHead::inject(&executor, &resolver, ReadOptions::default()).unwrap();
    let mut sink = Sink::default();
    let receipt = leaf::run(
        &head,
        &input("get", &format!("{L}/sessions/opaque-session")),
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(receipt.data, value);
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Failed)
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(
        requests[0].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/lh/livyApi/versions/2024-04-01/sessions/opaque-session"
    );
    assert_eq!(sink.writes, 1);
}

#[tokio::test]
async fn actual_http_partial_denied_malformed_and_wrong_ids_fail_without_delivery() {
    for (status, body, options) in [
        (206, r#"{"id":7}"#, ReadOptions::default()),
        (
            403,
            r#"{"private":"not a diagnostic"}"#,
            ReadOptions::default(),
        ),
        (200, r#"{"id":7"#, ReadOptions::default()),
        (200, r#"{"id":8}"#, ReadOptions::default()),
        (
            200,
            r#"{"id":7,"state":"idle"}"#,
            ReadOptions {
                max_response_bytes: 8,
                ..ReadOptions::default()
            },
        ),
    ] {
        let http = ScriptedHttp::new([response(
            status,
            body,
            &[("content-type", "application/json")],
        )]);
        let credential = Arc::new(TestCredential::default());
        let executor = fabric_executor(http.clone(), credential.clone());
        let resolver = FabricResolver;
        let head = RuntimeHead::inject(&executor, &resolver, options).unwrap();
        let mut sink = Sink::default();
        let error = leaf::run(&head, &input("get", &format!("{L}/sessions/7")), &mut sink)
            .await
            .unwrap_err();
        assert!(!format!("{error} {error:?}").contains("private"));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(sink.writes, 0);
    }
}
