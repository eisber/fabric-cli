// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline metadata Exists source/evidence, bounds and cancellation contracts.

#![deny(unnameable_test_items)]
mod exists_support;
use exists_support::*;
use fabric_sdk::{
    Audience, Client, CredentialSelection,
    exists::{
        ExistsError, ExistsLimits, ExistsObservation, ExistsRequest, ExistsTarget,
        MetadataSelector, prepare_exists,
    },
    v2::{Effect, Precondition},
};
use fabric_sdk_api::v2_outcome::{RetentionLimits, State};
use serde_json::{Value, json};
use std::{
    future::{pending, ready},
    num::NonZeroUsize,
    sync::atomic::Ordering,
};

#[tokio::test]
async fn exact_item_success_is_one_get_with_boolean_and_native_evidence() {
    let mut response = reply(200, item_data());
    response
        .headers
        .insert("Authorization".into(), "must-not-be-retained".into());
    response
        .headers
        .insert("X-MS-Request-ID".into(), "synthetic-request".into());
    let fixture = Fixture::new(vec![response]);
    let report = fixture.run(item_target(), ExistsLimits::default()).await;
    assert_eq!(report.exists(), Some(true));
    assert_eq!(report.native().state(), State::Completed);
    assert_eq!(report.native().responses().len(), 1);
    assert_eq!(
        report.native().responses()[0].body(),
        serde_json::to_vec(&item_data()).unwrap()
    );
    assert_eq!(report.native().responses()[0].omitted_header_count(), 1);
    let data: Value = serde_json::from_slice(report.json_bytes()).unwrap();
    assert_eq!(data["exists"], true);
    assert_eq!(data["itemId"], ITEM);
    assert_eq!(data["evidence"]["atomicPrecondition"], false);
    assert_eq!(data["evidence"]["mutationMayHaveOccurred"], false);
    assert!(!String::from_utf8_lossy(report.json_bytes()).contains("synthetic-request"));
    assert!(!format!("{report:?}").contains("Note"));
    assert_eq!(
        fixture.executor.requests.lock().unwrap().as_slice(),
        [format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{ITEM}"
        )]
    );
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn only_documented_item_not_found_is_authoritative_absence() {
    for (status, code, observation, reason) in [
        (
            404,
            "ItemNotFound",
            ExistsObservation::Absent,
            "AuthoritativeItemNotFound",
        ),
        (
            404,
            "UnknownError",
            ExistsObservation::Unknown,
            "UnverifiedNotFound",
        ),
        (
            403,
            "ItemNotFound",
            ExistsObservation::Denied,
            "AccessDenied",
        ),
        (
            401,
            "Unauthorized",
            ExistsObservation::Failed,
            "AuthenticationRequired",
        ),
        (
            429,
            "TooManyRequests",
            ExistsObservation::Failed,
            "ServiceError",
        ),
        (
            500,
            "UnknownError",
            ExistsObservation::Failed,
            "ServiceError",
        ),
    ] {
        let fixture = Fixture::new(vec![reply(
            status,
            json!({"errorCode":code,"message":"untrusted provider text"}),
        )]);
        let report = fixture.run(item_target(), ExistsLimits::default()).await;
        assert_eq!(report.observation(), observation);
        assert_eq!(report.reason(), reason);
        assert_eq!(
            report.exists(),
            if observation == ExistsObservation::Absent {
                Some(false)
            } else {
                None
            }
        );
        assert_eq!(report.native().responses()[0].status(), status);
        assert!(!String::from_utf8_lossy(report.json_bytes()).contains("untrusted provider text"));
        assert!(report.json_bytes().ends_with(b"\n"));
    }
}

#[tokio::test]
async fn workspace_get_has_no_enrichment_and_bare_not_found_remains_unknown() {
    for (status, data, expected) in [
        (200, workspace_data(), Some(true)),
        (404, json!({"errorCode":"WorkspaceNotFound"}), None),
    ] {
        let fixture = Fixture::new(vec![reply(status, data)]);
        let report = fixture
            .run(
                ExistsTarget::Workspace {
                    workspace: MetadataSelector::id(WS).unwrap(),
                },
                ExistsLimits::default(),
            )
            .await;
        assert_eq!(report.exists(), expected);
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn visible_inventory_miss_is_unknown_even_after_complete_paging() {
    let fixture = Fixture::new(vec![reply(200, json!({"value":[]}))]);
    let report = fixture
        .run(
            ExistsTarget::Workspace {
                workspace: MetadataSelector::name("Hidden").unwrap(),
            },
            ExistsLimits::default(),
        )
        .await;
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "VisibleInventoryMiss");
    assert_eq!(report.native().coverage().complete(), None);
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn names_resolve_all_pages_before_presence_and_preserve_type_and_root_scope() {
    let fixture = Fixture::new(vec![
        reply(
            200,
            json!({"value":[{"id":OTHER,"displayName":"Note","type":"Report"}],"continuationToken":"next+/="}),
        ),
        reply(200, json!({"value":[item_data()]})),
        reply(200, item_data()),
    ]);
    let report = fixture.run(named_item(), ExistsLimits::default()).await;
    assert_eq!(report.exists(), Some(true));
    let requests = fixture.executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].ends_with("items?recursive=false"));
    assert!(requests[1].contains("continuationToken=next%2B%2F%3D"));
    assert!(requests[2].ends_with(ITEM));
}

#[tokio::test]
async fn later_page_ambiguity_does_not_select_first_and_duplicate_id_is_one_identity() {
    for (later_id, expected, requests) in [
        (OTHER, ExistsObservation::Ambiguous, 2),
        (ITEM, ExistsObservation::Present, 3),
    ] {
        let mut later = item_data();
        later["id"] = later_id.into();
        let fixture = Fixture::new(vec![
            reply(
                200,
                json!({"value":[item_data()],"continuationToken":"next"}),
            ),
            reply(200, json!({"value":[later]})),
            reply(200, item_data()),
        ]);
        let report = fixture.run(named_item(), ExistsLimits::default()).await;
        assert_eq!(report.observation(), expected);
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), requests);
    }
}

#[tokio::test]
async fn nested_folder_item_and_other_types_do_not_match_a_root_typed_name() {
    let mut nested = item_data();
    nested["folderId"] = OTHER.into();
    let fixture = Fixture::new(vec![reply(200, json!({"value":[nested]}))]);
    let report = fixture.run(named_item(), ExistsLimits::default()).await;
    assert_eq!(report.reason(), "VisibleInventoryMiss");
    assert_eq!(report.exists(), None);
}

#[tokio::test]
async fn partial_discovery_and_later_404_never_become_absent() {
    let cases = [
        (
            vec![reply(200, json!({"value":[],"continuationToken":"next"}))],
            1,
            "BudgetExceeded",
        ),
        (
            vec![
                reply(
                    200,
                    json!({"value":[item_data()],"continuationToken":"next"}),
                ),
                reply(404, json!({"errorCode":"ItemNotFound"})),
            ],
            2,
            "UnverifiedNotFound",
        ),
    ];
    for (replies, pages, reason) in cases {
        let fixture = Fixture::new(replies);
        let mut limits = ExistsLimits::default();
        limits.items.max_pages = NonZeroUsize::new(pages).unwrap();
        let report = fixture.run(named_item(), limits).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.reason(), reason);
        assert!(!report.native().responses().is_empty());
        assert!(report.cause().is_some());
    }
}

#[tokio::test]
async fn malformed_wrong_identity_type_and_changed_name_are_not_presence() {
    for data in [
        json!({}),
        json!({"id":OTHER,"displayName":"Note","type":"Notebook"}),
        json!({"id":ITEM,"workspaceId":OTHER,"displayName":"Note","type":"Notebook"}),
        json!({"id":ITEM,"displayName":"Note","type":null}),
        json!({"id":ITEM,"displayName":"Note","type":"Report"}),
    ] {
        let fixture = Fixture::new(vec![reply(200, data)]);
        let report = fixture.run(item_target(), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.observation(), ExistsObservation::Failed);
    }
    let fixture = Fixture::new(vec![
        reply(200, json!({"value":[item_data()]})),
        reply(404, json!({"errorCode":"ItemNotFound"})),
    ]);
    let report = fixture.run(named_item(), ExistsLimits::default()).await;
    assert_eq!(report.reason(), "ChangedDuringProbe");
    assert_eq!(report.exists(), None);
}

#[tokio::test]
async fn unexpected_success_status_and_invalid_error_bytes_preserve_unknown() {
    for status in [201, 202, 204] {
        let fixture = Fixture::new(vec![reply(status, item_data())]);
        let report = fixture.run(item_target(), ExistsLimits::default()).await;
        assert_eq!(report.exists(), None);
        assert_eq!(report.native().responses()[0].status(), status);
    }
    let fixture = Fixture::new(vec![Reply {
        status: 404,
        body: b"not JSON".to_vec(),
        headers: Default::default(),
    }]);
    let report = fixture.run(item_target(), ExistsLimits::default()).await;
    assert_eq!(report.exists(), None);
}

#[tokio::test]
async fn total_dispatch_and_native_retention_limits_preserve_earlier_evidence() {
    let fixture = Fixture::new(vec![
        reply(200, json!({"value":[item_data()]})),
        reply(200, item_data()),
    ]);
    let limits = ExistsLimits {
        retention: RetentionLimits::new(1, 4096, 4096, 1024, 8).unwrap(),
        ..ExistsLimits::default()
    };
    let report = fixture.run(named_item(), limits).await;
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "BudgetExceeded");
    assert_eq!(report.native().responses().len(), 1);
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);

    let mut response = reply(200, item_data());
    response
        .headers
        .insert("X-MS-Request-ID".into(), "\r\ninvalid".into());
    let fixture = Fixture::new(vec![response]);
    let report = fixture.run(item_target(), ExistsLimits::default()).await;
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "EvidenceRetentionFailed");
    assert!(report.retention_failure().is_some());
}

#[tokio::test]
async fn request_approval_and_tenant_mismatch_are_zero_credential_zero_http() {
    let fixture = Fixture::new(vec![]);
    let request = ExistsRequest::new(TENANT, item_target()).unwrap();
    let prepared = prepare_exists(&request)
        .unwrap()
        .with_preconditions(vec![Precondition::ExistingOnly])
        .unwrap();
    let approved = prepared.authorize(&mut Permissive).unwrap();
    assert!(matches!(
        fixture
            .host()
            .execute(&approved, &request, ExistsLimits::default(), pending())
            .await,
        Err(ExistsError::ContextMismatch)
    ));
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut ReadPolicy).unwrap();
    let client = Client::new(
        fixture.credential.clone(),
        CredentialSelection::new("offline", WS, Audience::Fabric).unwrap(),
    );
    let host = fabric_sdk::exists::ExistsHost {
        client: &client,
        items: &fixture.executor,
        workspaces: &fixture.executor,
    };
    assert!(matches!(
        host.execute(&approved, &request, ExistsLimits::default(), pending())
            .await,
        Err(ExistsError::ContextMismatch)
    ));
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
    assert!(fixture.executor.requests.lock().unwrap().is_empty());
}
struct Permissive;
impl fabric_sdk::v2::ApprovalPolicy for Permissive {
    type Error = ();
    fn authorize(&mut self, _: &fabric_sdk::v2::PreparedOperation) -> Result<(), ()> {
        Ok(())
    }
}

#[test]
fn invalid_local_or_untyped_targets_cannot_construct_a_request() {
    for name in [
        "",
        "/tmp/file",
        "C:\\private",
        "bad*name",
        "..",
        "name?query",
        " name ",
    ] {
        assert!(MetadataSelector::name(name).is_err());
    }
    assert!(
        ExistsRequest::new(
            TENANT,
            ExistsTarget::Item {
                workspace: MetadataSelector::Id("invalid".into()),
                item: MetadataSelector::Id(ITEM.into()),
                item_type: None,
            }
        )
        .is_err()
    );
    assert!(
        ExistsRequest::new(
            TENANT,
            ExistsTarget::Item {
                workspace: MetadataSelector::Id(WS.into()),
                item: MetadataSelector::Name("Note".into()),
                item_type: None,
            }
        )
        .is_err()
    );
    assert_eq!(
        fabric_sdk::exists::exists_descriptor().unwrap().effects(),
        [Effect::RemoteRead]
    );
}

#[tokio::test]
async fn pre_dispatch_cancellation_and_inflight_drop_do_not_infer_absence() {
    let fixture = Fixture::new(vec![]);
    let request = ExistsRequest::new(TENANT, item_target()).unwrap();
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut ReadPolicy).unwrap();
    let report = fixture
        .host()
        .execute(&approved, &request, ExistsLimits::default(), ready(()))
        .await
        .unwrap();
    assert_eq!(report.exists(), None);
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
    assert!(fixture.executor.requests.lock().unwrap().is_empty());

    let mut fixture = Fixture::new(vec![]);
    fixture.executor.stall = true;
    let host = fixture.host();
    let cancelled = async { fixture.executor.started.notified().await };
    let report = host
        .execute(&approved, &request, ExistsLimits::default(), cancelled)
        .await
        .unwrap();
    assert_eq!(report.reason(), "Cancelled");
    assert_eq!(report.exists(), None);
    assert_eq!(fixture.executor.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn typed_deadline_is_latched_and_distinct_from_cancellation() {
    let fixture = Fixture::new(vec![]);
    let request = ExistsRequest::new(TENANT, item_target()).unwrap();
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut ReadPolicy).unwrap();
    let report = fixture
        .host()
        .execute_until(&approved, &request, ExistsLimits::default(), async {
            fabric_sdk::exists::ExistsInterruption::DeadlineExceeded
        })
        .await
        .unwrap();
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "TimedOut");
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn report_byte_limit_returns_original_evidence_and_new_invocations_are_fresh() {
    let fixture = Fixture::new(vec![reply(200, item_data())]);
    let request = ExistsRequest::new(TENANT, item_target()).unwrap();
    let prepared = prepare_exists(&request).unwrap();
    let approved = prepared.authorize(&mut ReadPolicy).unwrap();
    let limits = ExistsLimits {
        max_output_bytes: NonZeroUsize::new(1).unwrap(),
        ..ExistsLimits::default()
    };
    let error = fixture
        .host()
        .execute(&approved, &request, limits, pending())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ExistsError::OutputLimit(report) if report.native().responses().len() == 1)
    );
    let fixture = Fixture::new(vec![
        reply(200, item_data()),
        reply(404, json!({"errorCode":"ItemNotFound"})),
    ]);
    assert_eq!(
        fixture
            .run(item_target(), ExistsLimits::default())
            .await
            .exists(),
        Some(true)
    );
    assert_eq!(
        fixture
            .run(item_target(), ExistsLimits::default())
            .await
            .exists(),
        Some(false)
    );
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 2);
}
