// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Leaf contracts use the existing parser, runtime head and actual Livy SDK.

use super::{HttpFake, L, N, Resolver, Sink, V, complete, input};
use azure_core::{
    Bytes,
    http::{AsyncRawResponse, Method, headers::Headers},
};
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
            Some(WorkloadObservation::Failed)
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
fn accepted_response_body_is_retained_without_claiming_read_completion() {
    let value = json!({"id": 7, "state": "future", "unknown": {"keep": true}});
    let mut http = HttpFake::json(value.clone());
    http.status = 202;
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
    assert_eq!(receipt.data, value);
    assert_eq!(
        receipt.meta.coverage,
        fabric_cli::runtime_v2::ReadCoverage::Accepted
    );
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Unknown)
    );
    assert_eq!(http.calls(), 1);
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

#[tokio::test]
async fn actual_http_truncated_stream_never_becomes_a_complete_session_or_replay() {
    let mut headers = Headers::new();
    headers.insert("content-type", "application/json");
    let http = ScriptedHttp::new([AsyncRawResponse::new(
        200.into(),
        headers,
        Box::pin(futures_util::stream::iter([
            Ok(Bytes::from_static(br#"{"id":7}"#)),
            Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Io,
                "private stream failure",
            )),
        ])),
    )]);
    let credential = Arc::new(TestCredential::default());
    let executor = fabric_executor(http.clone(), credential.clone());
    let resolver = FabricResolver;
    let head = RuntimeHead::inject(&executor, &resolver, ReadOptions::default()).unwrap();
    let mut sink = Sink::default();
    let error = leaf::run(&head, &input("get", &format!("{L}/sessions/7")), &mut sink)
        .await
        .unwrap_err();
    assert!(!format!("{error} {error:?}").contains("private"));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert_eq!(sink.writes, 0);
}

#[cfg(feature = "native-host")]
mod hosted {
    use super::*;
    use fabric_cli::runtime_v2::{
        ParsedInvocation, ReadReceipt,
        host::{ExplicitResolver, acquire, prepare_destination},
    };
    use std::{
        future::pending,
        time::{Duration, Instant},
    };

    fn resolver(input: &ParsedInvocation) -> ExplicitResolver {
        ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::Lakehouse(
                LivyService::fabric(
                    super::super::endpoint("https://api.fabric.microsoft.com"),
                    "ws",
                    "lh",
                )
                .unwrap(),
            ),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn actual_parser_host_sdk_http_delivers_failed_unknown_and_accepted_reads() {
        for (status, body, observation) in [
            (
                200,
                r#"{"id":7,"state":"error","errorInfo":[{"message":"retained"}]}"#,
                Some(WorkloadObservation::Failed),
            ),
            (
                200,
                r#"{"id":7,"state":"future","unknown":{"keep":true}}"#,
                Some(WorkloadObservation::Unknown),
            ),
            (202, "", None),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let input = parse_args([
                "fab-cli-v2",
                "get",
                &format!("{L}/sessions/7"),
                "-o",
                "session.json",
            ])
            .unwrap();
            assert!(leaf::claims(&input));
            let resolver = resolver(&input);
            let http = ScriptedHttp::new([response(
                status,
                body,
                &[("content-type", "application/json")],
            )]);
            let credential = Arc::new(TestCredential::default());
            let executor = fabric_executor(http.clone(), credential.clone());
            let end = Instant::now() + Duration::from_secs(5);
            let cancelled = pending::<()>();
            tokio::pin!(cancelled);
            let destination = prepare_destination(
                input.output.clone(),
                temp.path().into(),
                end,
                cancelled.as_mut(),
            )
            .await
            .unwrap();
            let read = acquire(
                &input,
                &executor,
                &resolver,
                ReadOptions::default(),
                &destination,
                end,
                cancelled.as_mut(),
            )
            .await
            .unwrap();
            let receipt = read
                .deliver(destination, end, cancelled.as_mut())
                .await
                .unwrap();
            assert_eq!(receipt.meta.workload_observation, observation);
            assert_eq!(
                receipt.status,
                if status == 202 {
                    ReadStatus::Accepted
                } else {
                    ReadStatus::ReadComplete
                }
            );
            assert_eq!(
                receipt.data,
                if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(body).unwrap()
                }
            );
            let bytes = std::fs::read(temp.path().join("session.json")).unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            assert_eq!(
                serde_json::from_slice::<ReadReceipt>(&bytes).unwrap(),
                receipt
            );
            assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn changed_owner_and_invalid_sdk_id_fail_before_credentials_or_http() {
        for bad_id in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let input = parse_args([
                "fab-cli-v2",
                "get",
                &format!("{L}/sessions/{}", if bad_id { "bad id" } else { "7" }),
                "-o",
                "session.json",
            ])
            .unwrap();
            let resolver = if bad_id {
                resolver(&input)
            } else {
                let mut other = input.clone();
                other.path.owner.workspace = "Foreign.Workspace".into();
                resolver(&other)
            };
            let http = ScriptedHttp::new([]);
            let credential = Arc::new(TestCredential::default());
            let executor = fabric_executor(http.clone(), credential.clone());
            let end = Instant::now() + Duration::from_secs(5);
            let destination =
                prepare_destination(input.output.clone(), temp.path().into(), end, pending())
                    .await
                    .unwrap();
            let error = acquire(
                &input,
                &executor,
                &resolver,
                ReadOptions::default(),
                &destination,
                end,
                pending(),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.kind, ReadErrorKind::InvalidInput);
            assert!(!error.read_completed);
            assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
            assert!(http.requests.lock().unwrap().is_empty());
            assert!(!temp.path().join("session.json").exists());
        }
    }

    #[tokio::test]
    async fn no_clobber_preflight_and_atomic_delivery_do_not_replay_the_get() {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args([
            "fab-cli-v2",
            "get",
            &format!("{L}/sessions/7"),
            "-o",
            "session.json",
        ])
        .unwrap();
        let path = temp.path().join("session.json");
        std::fs::write(&path, "original").unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        assert!(
            prepare_destination(input.output.clone(), temp.path().into(), end, pending())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        std::fs::remove_file(&path).unwrap();
        let destination =
            prepare_destination(input.output.clone(), temp.path().into(), end, pending())
                .await
                .unwrap();
        let http = ScriptedHttp::new([response(
            200,
            r#"{"id":7}"#,
            &[("content-type", "application/json")],
        )]);
        let credential = Arc::new(TestCredential::default());
        let executor = fabric_executor(http.clone(), credential.clone());
        let read = acquire(
            &input,
            &executor,
            &resolver(&input),
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .unwrap();
        std::fs::write(&path, "racing owner").unwrap();
        let error = read.deliver(destination, end, pending()).await.unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::Delivery);
        assert!(error.read_completed);
        assert!(!error.file_created);
        assert_eq!(error.bytes_written, 0);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "racing owner");
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_after_acquisition_retains_read_evidence_without_remote_replay() {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args([
            "fab-cli-v2",
            "get",
            &format!("{L}/sessions/7"),
            "-o",
            "session.json",
        ])
        .unwrap();
        let http = ScriptedHttp::new([response(
            200,
            r#"{"id":7}"#,
            &[("content-type", "application/json")],
        )]);
        let credential = Arc::new(TestCredential::default());
        let executor = fabric_executor(http.clone(), credential.clone());
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let cancelled = async {
            let _ = cancelled.await;
        };
        tokio::pin!(cancelled);
        let end = Instant::now() + Duration::from_secs(5);
        let destination = prepare_destination(
            input.output.clone(),
            temp.path().into(),
            end,
            cancelled.as_mut(),
        )
        .await
        .unwrap();
        let read = acquire(
            &input,
            &executor,
            &resolver(&input),
            ReadOptions::default(),
            &destination,
            end,
            cancelled.as_mut(),
        )
        .await
        .unwrap();
        cancel.send(()).unwrap();
        let error = read
            .deliver(destination, end, cancelled.as_mut())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::Cancelled);
        assert!(error.read_completed);
        assert_eq!(error.http_status, Some(200));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_http_is_dropped_at_the_original_deadline_without_cancel_or_replay() {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args([
            "fab-cli-v2",
            "get",
            &format!("{L}/sessions/7"),
            "-o",
            "session.json",
        ])
        .unwrap();
        let http = ScriptedHttp::new([]);
        http.pending.store(true, Ordering::SeqCst);
        let credential = Arc::new(TestCredential::default());
        let executor = fabric_executor(http.clone(), credential.clone());
        let end = Instant::now() + Duration::from_secs(1);
        let destination =
            prepare_destination(input.output.clone(), temp.path().into(), end, pending())
                .await
                .unwrap();
        let error = acquire(
            &input,
            &executor,
            &resolver(&input),
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ReadErrorKind::Timeout);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(!temp.path().join("session.json").exists());
    }
}
