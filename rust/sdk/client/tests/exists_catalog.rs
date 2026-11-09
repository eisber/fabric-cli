// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline OneLake catalog contracts; no live endpoint or ambient credentials.

#![deny(unnameable_test_items)]
mod exists_catalog_support;
mod exists_support;
use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenRequestOptions};
use azure_core::http::Method;
use exists_catalog_support::*;
use exists_support::{ITEM, OTHER, TENANT, WS, reply};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, ServiceOrigin, TokenCredential,
    api::{ApiClient, ExecutionError},
    exists::{
        CatalogBackend, ExistsError, ExistsInterruption, ExistsLimits, ExistsObservation,
        ExistsRequest, ExistsTarget, MetadataSelector, prepare_exists,
    },
};
use fabric_sdk_api::v2_outcome::{RetentionLimits, State};
use serde_json::{Value, json};
use std::{
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex, atomic::Ordering},
};

#[derive(Debug, Default)]
struct PendingCredential {
    started: tokio::sync::Notify,
}
#[async_trait]
impl TokenCredential for PendingCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        self.started.notify_one();
        pending().await
    }
}

#[tokio::test]
async fn cancellation_during_config_credentials_drops_without_dispatch_or_fallback() {
    let credential = Arc::new(PendingCredential::default());
    let executor = Arc::new(CatalogExecutor::default());
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline", TENANT, Audience::OneLake).unwrap(),
        executor.clone(),
    );
    let backend = CatalogBackend::new(
        api,
        EndpointBinding::with_host(ServiceOrigin::OneLake, TABLE_HOST).unwrap(),
    )
    .unwrap();
    let fixture = exists_support::Fixture::new(vec![parent()]);
    let request = ExistsRequest::new(TENANT, target(true)).unwrap();
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut exists_support::ReadPolicy).unwrap();
    let report = fixture
        .host()
        .with_catalog(&backend)
        .execute_until(&approved, &request, ExistsLimits::default(), async {
            credential.started.notified().await;
            ExistsInterruption::Cancelled
        })
        .await
        .unwrap();
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "Cancelled");
    assert!(executor.requests.lock().unwrap().is_empty());
    assert_eq!(report.native().responses().len(), 1);
}

#[tokio::test]
async fn exact_catalog_spelling_is_encoded_once_and_config_cannot_select_another_host() {
    let mut value = config(PREFIX);
    value["overrides"]["uri"] = "https://foreign.invalid/changed".into();
    value["overrides"]["warehouse"] = format!("{OTHER}/{OTHER}").into();
    let fixture = CatalogFixture::new(vec![configuration(value), response(204, vec![])]);
    let mut selected = target(true);
    if let ExistsTarget::CatalogTable {
        namespace, table, ..
    } = &mut selected
    {
        *namespace = "Sales Schema".into();
        *table = "MiXeD Orders".into();
    }
    let report = fixture.run(selected, ExistsLimits::default()).await;
    assert_eq!(report.exists(), Some(true));
    let requests = fixture.executor.requests.lock().unwrap();
    assert_eq!(
        requests[1].1,
        format!(
            "https://{TABLE_HOST}/iceberg/v1/{PREFIX}/namespaces/Sales%20Schema/tables/MiXeD%20Orders"
        )
    );
    assert_eq!(
        requests[0].1,
        format!("https://{TABLE_HOST}/iceberg/v1/config?warehouse={WS}%2F{ITEM}")
    );
}

#[tokio::test]
async fn additive_catalog_targets_leave_metadata_fixture_behavior_unchanged() {
    let fixture = exists_support::Fixture::new(vec![reply(200, exists_support::workspace_data())]);
    assert_eq!(
        fixture
            .run(
                ExistsTarget::Workspace {
                    workspace: MetadataSelector::id(WS).unwrap(),
                },
                ExistsLimits::default()
            )
            .await
            .exists(),
        Some(true)
    );
    let fixture = exists_support::Fixture::new(vec![reply(200, exists_support::item_data())]);
    assert_eq!(
        fixture
            .run(exists_support::item_target(), ExistsLimits::default())
            .await
            .exists(),
        Some(true)
    );
    let fixture = exists_support::Fixture::new(vec![
        reply(200, json!({"value":[exists_support::item_data()]})),
        reply(200, exists_support::item_data()),
    ]);
    assert_eq!(
        fixture
            .run(exists_support::named_item(), ExistsLimits::default())
            .await
            .exists(),
        Some(true)
    );
}

#[tokio::test]
async fn configured_prefix_not_warehouse_drives_bodyless_namespace_and_table_heads() {
    for table in [false, true] {
        for (status, exists) in [(204, true), (404, false)] {
            let fixture = CatalogFixture::new(vec![
                configuration(config(PREFIX)),
                response(status, vec![]),
            ]);
            let report = fixture.run(target(table), ExistsLimits::default()).await;
            assert_eq!(report.exists(), Some(exists), "{}", report.reason());
            assert_eq!(report.native().state(), State::Completed);
            assert_eq!(report.native().coverage().complete(), Some(true));
            let native = report.native().responses();
            assert_eq!(
                native.iter().map(|r| r.status()).collect::<Vec<_>>(),
                [200, 200, status]
            );
            assert!(native[2].body().is_empty());
            let requests = fixture.executor.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].0, Method::Get);
            assert_eq!(
                requests[0].1,
                format!("https://{TABLE_HOST}/iceberg/v1/config?warehouse={WS}%2F{ITEM}")
            );
            assert_eq!(requests[1].0, Method::Head);
            let suffix = if table { "/tables/Orders" } else { "" };
            assert_eq!(
                requests[1].1,
                format!("https://{TABLE_HOST}/iceberg/v1/{PREFIX}/namespaces/dbo{suffix}")
            );
            assert_eq!(fixture.metadata.credential.0.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.storage.0.load(Ordering::SeqCst), 2);
            assert_eq!(fixture.metadata.executor.requests.lock().unwrap().len(), 1);
            let projection: Value = serde_json::from_slice(report.json_bytes()).unwrap();
            assert_eq!(projection["exists"], exists);
            assert_eq!(projection["evidence"]["atomicPrecondition"], false);
            assert_eq!(projection["evidence"]["mutationMayHaveOccurred"], false);
        }
    }
}

#[tokio::test]
async fn synthetic_dbo_is_a_catalog_namespace_not_a_physical_directory_requirement() {
    let fixture = CatalogFixture::new(vec![configuration(config(PREFIX)), response(204, vec![])]);
    let report = fixture.run(target(false), ExistsLimits::default()).await;
    assert_eq!(report.exists(), Some(true));
    let all = fixture.metadata.executor.requests.lock().unwrap();
    assert!(
        !all.iter()
            .any(|url| url.contains("Tables") || url.contains("dfs"))
    );
    assert!(
        fixture
            .executor
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, url, _)| !url.contains("/Tables/"))
    );
}

#[tokio::test]
async fn malformed_or_unsafe_prefix_never_retargets_or_dispatches_head() {
    for prefix in [
        "",
        "/other",
        "//evil.invalid",
        "https://evil.invalid/x",
        "a//b",
        "a/",
        ".",
        "..",
        "a/../b",
        "a/%2e%2e/b",
        "a%2fb",
        "a\\b",
        "a?x=1",
        "a#frag",
        "a:b",
        "a b",
        "a\nb",
    ] {
        let fixture = CatalogFixture::new(vec![configuration(config(prefix))]);
        let report = fixture.run(target(true), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None, "{prefix}");
        assert_eq!(report.reason(), "InvalidCatalogResponse", "{prefix}");
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn only_advertised_check_is_used_without_get_list_or_credentials_fallback() {
    for table in [false, true] {
        for endpoints in [
            None,
            Some(json!([])),
            Some(json!([
                "GET /v1/{prefix}/namespaces/{namespace}/tables/{table}"
            ])),
        ] {
            let mut value = config(PREFIX);
            if let Some(endpoints) = endpoints {
                value["endpoints"] = endpoints;
            } else {
                value.as_object_mut().unwrap().remove("endpoints");
            }
            let fixture = CatalogFixture::new(vec![configuration(value)]);
            let report = fixture.run(target(table), ExistsLimits::default()).await;
            assert_eq!(report.exists(), None);
            assert_eq!(report.reason(), "CatalogHeadUnavailable");
            assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn config_failures_denials_and_disabled_conversion_never_prove_absence() {
    for status in [204, 301, 400, 401, 403, 404, 405, 429, 500, 501] {
        let fixture = CatalogFixture::new(vec![response(
            status,
            br#"{"error":{"type":"ConversionDisabled"}}"#.to_vec(),
        )]);
        let report = fixture.run(target(true), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None, "{status}");
        if status == 403 {
            assert_eq!(report.observation(), ExistsObservation::Denied);
        }
        assert_eq!(report.native().responses().last().unwrap().status(), status);
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn head_denials_unsupported_and_unexpected_success_never_become_absence() {
    for table in [false, true] {
        for status in [
            200, 201, 202, 205, 206, 301, 400, 401, 403, 405, 429, 500, 501,
        ] {
            let fixture = CatalogFixture::new(vec![
                configuration(config(PREFIX)),
                response(status, vec![]),
            ]);
            let report = fixture.run(target(table), ExistsLimits::default()).await;
            assert_eq!(report.exists(), None, "{status}");
            if status == 403 {
                assert_eq!(report.observation(), ExistsObservation::Denied);
            }
            if status == 401 {
                assert_eq!(report.reason(), "AuthenticationRequired");
            }
            assert_eq!(report.native().responses().last().unwrap().status(), status);
            assert_eq!(fixture.executor.requests.lock().unwrap().len(), 2);
        }
    }
    for status in [204, 404] {
        let fixture = CatalogFixture::new(vec![
            configuration(config(PREFIX)),
            response(
                status,
                br#"{"error":{"type":"ConversionDisabled"}}"#.to_vec(),
            ),
        ]);
        let report = fixture.run(target(true), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.reason(), "UnexpectedCatalogHeadBody");
    }
    let fixture = CatalogFixture::new(vec![
        configuration(config(PREFIX)),
        response(403, br#"{"error":{"type":"ConversionDisabled"}}"#.to_vec()),
    ]);
    let report = fixture.run(target(true), ExistsLimits::default()).await;
    assert_eq!(report.observation(), ExistsObservation::Denied);
    assert_eq!(report.exists(), None);
}

#[tokio::test]
async fn malformed_configuration_and_prefix_precedence_fail_closed() {
    for value in [
        json!(null),
        json!({}),
        json!({"defaults":[],"overrides":{}}),
        json!({"defaults":{},"overrides":{"prefix":42},"endpoints":[]}),
        json!({"defaults":{},"overrides":{"prefix":PREFIX},"endpoints":[42]}),
    ] {
        let fixture = CatalogFixture::new(vec![configuration(value)]);
        assert_eq!(
            fixture
                .run(target(false), ExistsLimits::default())
                .await
                .exists(),
            None
        );
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
    }
    let fixture = CatalogFixture::new(vec![response(200, b"not-json".to_vec())]);
    assert_eq!(
        fixture
            .run(target(false), ExistsLimits::default())
            .await
            .reason(),
        "InvalidCatalogResponse"
    );
    for override_prefix in [true, false] {
        let mut value = config(PREFIX);
        value["defaults"]["prefix"] = "default-prefix".into();
        if !override_prefix {
            value["overrides"].as_object_mut().unwrap().remove("prefix");
        }
        let fixture = CatalogFixture::new(vec![configuration(value), response(204, vec![])]);
        assert_eq!(
            fixture
                .run(target(false), ExistsLimits::default())
                .await
                .exists(),
            Some(true)
        );
        let expected = if override_prefix {
            PREFIX
        } else {
            "default-prefix"
        };
        assert!(
            fixture.executor.requests.lock().unwrap()[1]
                .1
                .contains(&format!("/v1/{expected}/"))
        );
    }
}

#[tokio::test]
async fn parent_names_reuse_complete_scoped_metadata_resolution_and_never_treat_list_miss_as_absence()
 {
    let named = ExistsTarget::CatalogTable {
        workspace: MetadataSelector::id(WS).unwrap(),
        lakehouse: MetadataSelector::name("Lake").unwrap(),
        namespace: "dbo".into(),
        table: "Orders".into(),
    };
    for (rows, reason) in [
        (json!([]), "VisibleInventoryMiss"),
        (
            json!([{"id":ITEM,"displayName":"Lake","type":"Lakehouse"},{"id":OTHER,"displayName":"Lake","type":"Lakehouse"}]),
            "AmbiguousTarget",
        ),
        (
            json!([{"id":ITEM,"displayName":"Lake","type":"Lakehouse","folderId":OTHER}]),
            "VisibleInventoryMiss",
        ),
    ] {
        let fixture = CatalogFixture::with_executor(
            vec![reply(200, json!({"value":rows}))],
            CatalogExecutor::default(),
        );
        let report = fixture.run(named.clone(), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.reason(), reason);
        assert_eq!(fixture.storage.0.load(Ordering::SeqCst), 0);
    }
    let fixture = CatalogFixture::with_executor(
        vec![
            reply(
                200,
                json!({"value":[{"id":ITEM,"displayName":"lAkE","type":"Lakehouse"}]}),
            ),
            parent(),
        ],
        CatalogExecutor {
            replies: Mutex::new(
                vec![Ok(configuration(config(PREFIX))), Ok(response(204, vec![]))].into(),
            ),
            ..CatalogExecutor::default()
        },
    );
    assert_eq!(
        fixture.run(named, ExistsLimits::default()).await.exists(),
        Some(true)
    );
    assert!(
        fixture.metadata.executor.requests.lock().unwrap()[0].ends_with("items?recursive=false")
    );
}

#[tokio::test]
async fn parent_not_found_wrong_type_and_missing_backend_do_not_probe_the_catalog() {
    for (parent, reason) in [
        (
            reply(404, json!({"errorCode":"ItemNotFound"})),
            "CatalogParentNotEstablished",
        ),
        (reply(200, exists_support::item_data()), "TypeMismatch"),
    ] {
        let fixture = CatalogFixture::with_executor(vec![parent], CatalogExecutor::default());
        let report = fixture.run(target(false), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.reason(), reason);
        assert_eq!(fixture.storage.0.load(Ordering::SeqCst), 0);
    }
    let fixture = exists_support::Fixture::new(vec![]);
    let report = fixture.run(target(false), ExistsLimits::default()).await;
    assert_eq!(report.reason(), "CatalogBackendUnavailable");
    assert_eq!(report.exists(), None);
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_before_credentials_and_during_config_or_head_is_latched_and_not_replayed() {
    for (stall_at, interruption) in [
        (0, ExistsInterruption::Cancelled),
        (1, ExistsInterruption::Cancelled),
        (2, ExistsInterruption::Cancelled),
        (2, ExistsInterruption::DeadlineExceeded),
    ] {
        let fixture = CatalogFixture::with_executor(
            vec![parent()],
            CatalogExecutor {
                stall_at: Some(stall_at),
                replies: Mutex::new(vec![Ok(configuration(config(PREFIX)))].into()),
                ..CatalogExecutor::default()
            },
        );
        let request = ExistsRequest::new(TENANT, target(true)).unwrap();
        let prepared = prepare_exists(&request).unwrap();
        let approved = prepared.authorize(&mut exists_support::ReadPolicy).unwrap();
        let interrupted = async {
            if stall_at != 0 {
                fixture.executor.started.notified().await;
            }
            interruption
        };
        let report = fixture
            .metadata
            .host()
            .with_catalog(&fixture.backend)
            .execute_until(&approved, &request, ExistsLimits::default(), interrupted)
            .await
            .unwrap();
        assert_eq!(report.exists(), None);
        assert_eq!(
            report.reason(),
            if interruption == ExistsInterruption::Cancelled {
                "Cancelled"
            } else {
                "TimedOut"
            }
        );
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), stall_at);
        assert_eq!(
            fixture.executor.drops.load(Ordering::SeqCst),
            usize::from(stall_at != 0)
        );
        if stall_at == 0 {
            assert_eq!(fixture.metadata.credential.0.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn shared_request_response_aggregate_and_output_bounds_prevent_extra_work() {
    for requests in [1, 2] {
        let fixture =
            CatalogFixture::new(vec![configuration(config(PREFIX)), response(204, vec![])]);
        let limits = ExistsLimits {
            retention: RetentionLimits::new(requests, 65536, 32768, 4096, 32).unwrap(),
            ..ExistsLimits::default()
        };
        let report = fixture.run(target(true), limits).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.observation(), ExistsObservation::Incomplete);
        assert_eq!(
            fixture.executor.requests.lock().unwrap().len(),
            requests - 1
        );
    }
    let config_response = configuration(config(PREFIX));
    let config_bytes = config_response.body.len();
    let fixture = CatalogFixture::new(vec![config_response]);
    let limits = ExistsLimits {
        retention: RetentionLimits::new(4, 65536, config_bytes - 1, 64, 32).unwrap(),
        ..ExistsLimits::default()
    };
    let report = fixture.run(target(false), limits).await;
    assert_eq!(report.observation(), ExistsObservation::Incomplete);
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
    let native_parent_bytes = parent().body.len();
    let config_response = configuration(config(PREFIX));
    let total = native_parent_bytes + config_response.body.len() - 1;
    let fixture = CatalogFixture::new(vec![config_response]);
    let limits = ExistsLimits {
        retention: RetentionLimits::new(4, total, total, 64, 32).unwrap(),
        ..ExistsLimits::default()
    };
    let report = fixture.run(target(false), limits).await;
    assert_eq!(report.observation(), ExistsObservation::Incomplete);
    assert!(report.retention_failure().is_some());
    assert_eq!(report.native().responses().len(), 1);
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);

    let fixture = CatalogFixture::new(vec![configuration(config(PREFIX)), response(204, vec![])]);
    let report = fixture.run(target(true), ExistsLimits::default()).await;
    let length = report.json_bytes().len();
    for (limit, success) in [(length, true), (length - 1, false)] {
        let fixture =
            CatalogFixture::new(vec![configuration(config(PREFIX)), response(204, vec![])]);
        let request = ExistsRequest::new(TENANT, target(true)).unwrap();
        let prepared = prepare_exists(&request).unwrap();
        let approved = prepared.authorize(&mut exists_support::ReadPolicy).unwrap();
        let result = fixture
            .metadata
            .host()
            .with_catalog(&fixture.backend)
            .execute(
                &approved,
                &request,
                ExistsLimits {
                    max_output_bytes: NonZeroUsize::new(limit).unwrap(),
                    ..ExistsLimits::default()
                },
                pending(),
            )
            .await;
        assert_eq!(result.is_ok(), success);
        if !success {
            assert!(matches!(result, Err(ExistsError::OutputLimit(_))));
        }
    }
}

#[tokio::test]
async fn transport_failure_retains_prior_evidence_without_fallback() {
    let fixture = CatalogFixture::with_executor(
        vec![parent()],
        CatalogExecutor {
            replies: Mutex::new(
                vec![
                    Ok(configuration(config(PREFIX))),
                    Err(ExecutionError::Failed),
                ]
                .into(),
            ),
            ..CatalogExecutor::default()
        },
    );
    let report = fixture.run(target(true), ExistsLimits::default()).await;
    assert_eq!(report.reason(), "ExecutionFailed");
    assert_eq!(report.exists(), None);
    assert_eq!(report.native().responses().len(), 2);
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_input_approval_audience_and_tenant_are_rejected_before_credentials() {
    for namespace in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a%2fb",
        "a?b",
        "a#b",
        "a\u{001f}b",
    ] {
        assert!(
            ExistsRequest::new(
                TENANT,
                ExistsTarget::CatalogNamespace {
                    workspace: MetadataSelector::id(WS).unwrap(),
                    lakehouse: MetadataSelector::id(ITEM).unwrap(),
                    namespace: namespace.into()
                }
            )
            .is_err()
        );
    }
    let credential = Arc::new(StorageCredential::default());
    let executor = Arc::new(CatalogExecutor::default());
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
        executor.clone(),
    );
    assert!(
        CatalogBackend::new(
            api,
            EndpointBinding::with_host(ServiceOrigin::OneLake, TABLE_HOST).unwrap()
        )
        .is_err()
    );
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline", OTHER, Audience::OneLake).unwrap(),
        executor,
    );
    let backend = CatalogBackend::new(
        api,
        EndpointBinding::with_host(ServiceOrigin::OneLake, TABLE_HOST).unwrap(),
    )
    .unwrap();
    let fixture = exists_support::Fixture::new(vec![]);
    let request = ExistsRequest::new(TENANT, target(true)).unwrap();
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut exists_support::ReadPolicy).unwrap();
    assert!(matches!(
        fixture
            .host()
            .with_catalog(&backend)
            .execute(&approved, &request, ExistsLimits::default(), ready(()))
            .await,
        Err(ExistsError::ContextMismatch)
    ));
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
}
