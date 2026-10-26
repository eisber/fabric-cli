// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public focused SDK consumers; no facade, CLI or production identity backend.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk_workspaces::{
    CredentialSelection, TokenCredential, WorkspaceClient, WorkspaceExecutionError,
    WorkspaceExecutor, WorkspaceListErrorKind as ReadError, WorkspaceListLimits, WorkspaceMetadata,
    WorkspaceMetadataErrorKind as MetadataError, WorkspaceMetadataStep as Step,
    WorkspaceOmission as Omission, WorkspaceRequest, WorkspaceResponse,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    fail_at: Option<usize>,
    hold: bool,
    dropped: AtomicUsize,
}

struct Dropped<'a>(&'a AtomicUsize);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at == Some(call) {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "provider-secret",
            ));
        }
        if self.hold {
            let _dropped = Dropped(&self.dropped);
            pending::<()>().await;
        }
        Ok(AccessToken::new(
            "offline-metadata-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<Result<WorkspaceResponse, WorkspaceExecutionError>>>,
    requests: Mutex<Vec<String>>,
    hold_at: Option<usize>,
    cancel_at: Option<usize>,
    cancel: AtomicBool,
    dropped: AtomicUsize,
}

impl Executor {
    fn new(responses: impl IntoIterator<Item = WorkspaceResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
}

impl WorkspaceExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(request.token().token.secret(), "offline-metadata-token");
            let debug = format!("{request:?}");
            assert!(!debug.contains("resource-secret"));
            assert!(!debug.contains("offline-metadata-token"));
            assert!(!debug.contains("token-secret"));
            let call = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.url().to_string());
                requests.len()
            };
            if self.cancel_at == Some(call) {
                self.cancel.store(true, Ordering::SeqCst);
            }
            if self.hold_at == Some(call) {
                let _dropped = Dropped(&self.dropped);
                pending::<()>().await;
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }
}

fn response(status: u16, body: Value) -> WorkspaceResponse {
    WorkspaceResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn primary() -> WorkspaceResponse {
    response(200, json!({"id":"resource-secret", "displayName":"Space"}))
}

fn empty() -> WorkspaceResponse {
    response(200, json!({"value":[]}))
}

fn client(credential: Arc<Credential>, audience: Audience) -> WorkspaceClient {
    WorkspaceClient::new(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
    )
}

#[tokio::test]
async fn completed_primary_enrichment_reuses_optional_steps_without_rereading_primary() {
    let executor = Executor::new([empty(), response(200, json!({"enabled":true})), empty()]);
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let metadata: WorkspaceMetadata = serde_json::from_slice(&primary().body).unwrap();
    let result = client
        .enrich_workspace_metadata(
            &executor,
            "w",
            metadata,
            WorkspaceListLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.enrichments.len(), 3);
    assert!(
        result
            .enrichments
            .iter()
            .all(|step| step.omission.is_none())
    );
    assert_eq!(
        result.metadata.as_value()["sparkSettings"],
        json!({"enabled":true})
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        *executor.requests.lock().unwrap(),
        [
            "https://api.fabric.microsoft.com/v1/workspaces/w/managedPrivateEndpoints",
            "https://api.fabric.microsoft.com/v1/workspaces/w/spark/settings",
            "https://api.fabric.microsoft.com/v1/workspaces/w/roleAssignments",
        ]
    );
}

#[tokio::test]
async fn completed_primary_enrichment_keeps_selection_identifier_and_cancellation_guards() {
    let executor = Executor::default();
    let credential = Arc::new(Credential::default());
    for (audience, id, cancelled, expected) in [
        (
            Audience::OneLake,
            "w",
            false,
            MetadataError::Read(ReadError::InvalidAudience),
        ),
        (
            Audience::Fabric,
            "../escape",
            false,
            MetadataError::InvalidIdentifier,
        ),
        (
            Audience::Fabric,
            "w",
            true,
            MetadataError::Read(ReadError::Cancelled),
        ),
    ] {
        let signal: Pin<Box<dyn Future<Output = ()> + Send>> = if cancelled {
            Box::pin(ready(()))
        } else {
            Box::pin(pending())
        };
        let metadata = serde_json::from_slice(&primary().body).unwrap();
        let error = client(credential.clone(), audience)
            .enrich_workspace_metadata(
                &executor,
                id,
                metadata,
                WorkspaceListLimits::default(),
                signal,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordered_metadata_preserves_unknowns_numbers_nulls_and_replacement_positions() {
    let original = r#"{"before":{"z":null,"a":[1,1.0]},"sparkSettings":"old","id":"opaque","roleAssignments":false,"displayName":"\u65e5","managedPrivateEndpoints":null,"after":9007199254740993}"#;
    let executor = Executor::new([
        WorkspaceResponse {
            body: original.as_bytes().to_vec(),
            ..primary()
        },
        response(200, json!({"value":[{"unknown":null,"number":1.0}]})),
        response(
            200,
            serde_json::from_str(r#"{"z":null,"a":false}"#).unwrap(),
        ),
        response(200, json!({"value":[]})),
    ]);
    let credential = Arc::new(Credential::default());
    let result = client(credential.clone(), Audience::Fabric)
        .get_workspace_metadata(
            &executor,
            "opaque",
            WorkspaceListLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.metadata.id(), "opaque");
    assert_eq!(result.metadata.display_name(), "\u{65e5}");
    assert_eq!(
        result
            .metadata
            .as_value()
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "before",
            "sparkSettings",
            "id",
            "roleAssignments",
            "displayName",
            "managedPrivateEndpoints",
            "after"
        ]
    );
    assert!(result.metadata.as_value()["before"]["a"][0].is_u64());
    assert!(result.metadata.as_value()["before"]["a"][1].is_f64());
    assert_eq!(
        result.metadata.as_value()["after"],
        json!(9_007_199_254_740_993_u64)
    );
    assert_eq!(result.enrichments.len(), 3);
    assert!(
        result
            .enrichments
            .iter()
            .all(|outcome| outcome.omission.is_none())
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        *executor.requests.lock().unwrap(),
        [
            "https://api.fabric.microsoft.com/v1/workspaces/opaque",
            "https://api.fabric.microsoft.com/v1/workspaces/opaque/managedPrivateEndpoints",
            "https://api.fabric.microsoft.com/v1/workspaces/opaque/spark/settings",
            "https://api.fabric.microsoft.com/v1/workspaces/opaque/roleAssignments",
        ]
    );
    assert_eq!(
        serde_json::to_value(result.metadata.clone()).unwrap(),
        result.metadata.into_value()
    );
}

#[tokio::test]
async fn collections_complete_all_pages_in_order_and_tokens_are_scoped_per_step() {
    let executor = Executor::new([
        primary(),
        response(
            200,
            json!({"value":[1,null], "continuationToken":"token-secret+/= next"}),
        ),
        response(200, json!({"value":[{"x":false}]})),
        response(200, json!({"spark": {"future":1}})),
        response(200, json!({"value":["r"], "continuationToken":"roles"})),
        empty(),
    ]);
    let result = client(Arc::default(), Audience::Fabric)
        .get_workspace_metadata(
            &executor,
            "opaque +%\u{e9}",
            WorkspaceListLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.metadata.as_value()["managedPrivateEndpoints"],
        json!([1,null,{"x":false}])
    );
    assert_eq!(result.metadata.as_value()["roleAssignments"], json!(["r"]));
    let requests = executor.requests.lock().unwrap();
    assert!(
        requests[0].ends_with("/opaque%20+%25%C3%A9"),
        "{}",
        requests[0]
    );
    assert!(requests[2].ends_with("?continuationToken=token-secret%2B%2F%3D+next"));
    assert!(!requests[3].contains('?'));
    assert!(requests[5].ends_with("/roleAssignments?continuationToken=roles"));
}

#[tokio::test]
async fn optional_service_and_shape_failures_are_typed_omissions_and_later_steps_run() {
    for index in 0..3 {
        for status in [400, 401, 403, 404, 201, 204] {
            let mut responses = [primary(), empty(), response(200, json!({})), empty()];
            responses[index + 1] = response(
                status,
                json!({"errorCode":"secret-code","message":"provider-secret"}),
            );
            let executor = Executor::new(responses);
            let result = client(Arc::default(), Audience::Fabric)
                .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
                .await
                .unwrap();
            let omission = if [201, 204].contains(&status) {
                Omission::IgnoredStatus(status)
            } else {
                Omission::ServiceStatus(status)
            };
            assert_eq!(result.enrichments[index].omission, Some(omission));
            assert_eq!(executor.requests.lock().unwrap().len(), 4);
            assert!(!format!("{result:?}").contains("provider-secret"));
            assert!(!format!("{result:?}").contains("secret-code"));
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({"value":null}),
            json!({"value":{}}),
        ] {
            if index == 1 && invalid.is_object() {
                continue;
            }
            let mut responses = [primary(), empty(), response(200, json!({})), empty()];
            responses[index + 1] = response(200, invalid);
            let executor = Executor::new(responses);
            let result = client(Arc::default(), Audience::Fabric)
                .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
                .await
                .unwrap();
            assert_eq!(
                result.enrichments[index].omission,
                Some(Omission::InvalidResponse)
            );
            assert_eq!(executor.requests.lock().unwrap().len(), 4);
        }
    }
}

#[tokio::test]
async fn failed_later_page_discards_whole_augmentation_but_retains_preexisting_key() {
    let executor = Executor::new([
        response(
            200,
            json!({"id":"w","displayName":"W","managedPrivateEndpoints":"original"}),
        ),
        response(200, json!({"value":[1],"continuationToken":"next"})),
        response(
            403,
            json!({"errorCode":"Denied","message":"provider-secret"}),
        ),
        response(200, json!({})),
        empty(),
    ]);
    let result = client(Arc::default(), Audience::Fabric)
        .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        result.metadata.as_value()["managedPrivateEndpoints"],
        json!("original")
    );
    assert_eq!(
        result.enrichments[0].omission,
        Some(Omission::ServiceStatus(403))
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn primary_errors_keep_status_precedence_and_never_attempt_optional_steps() {
    for status in [400, 401, 403, 404, 201, 202, 204] {
        let mut reply = response(
            status,
            json!({"errorCode":"body-secret","message":"provider-secret"}),
        );
        reply
            .headers
            .insert("x-ms-public-api-error-code".into(), "header-secret".into());
        let executor = Executor::new([reply]);
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.step, Step::Primary);
        if let MetadataError::Read(ReadError::Http { code, .. }) = &error.kind {
            assert_eq!(
                code,
                match status {
                    401 | 403 => "header-secret",
                    404 => "NotFound",
                    _ => "body-secret",
                }
            );
        } else {
            assert_eq!(
                error.kind,
                MetadataError::Read(ReadError::UnsupportedStatus(status))
            );
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert!(error.enrichments.is_empty());
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    for value in [
        json!({}),
        json!({"id":null,"displayName":"W"}),
        json!({"id":"w","displayName":1}),
        json!([]),
    ] {
        let executor = Executor::new([response(200, value)]);
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, MetadataError::Read(ReadError::InvalidResponse));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn origin_cycles_page_item_and_byte_bounds_are_fatal_not_optional_omissions() {
    let limits = WorkspaceListLimits::default();
    let page = || response(200, json!({"value":[1], "continuationToken":"next"}));
    let mut foreign = page();
    foreign.body = serde_json::to_vec(&json!({"value":[1],"continuationToken":"next","continuationUri":"https://other.invalid/secret"})).unwrap();
    let cases = [
        (vec![foreign], limits, ReadError::InvalidContinuation),
        (
            vec![page(), page()],
            limits,
            ReadError::RepeatedContinuation,
        ),
        (
            vec![page()],
            WorkspaceListLimits {
                max_pages: NonZeroUsize::new(1).unwrap(),
                ..limits
            },
            ReadError::PageLimit,
        ),
        (
            vec![page(), response(200, json!({"value":[2]}))],
            WorkspaceListLimits {
                max_items: NonZeroUsize::new(1).unwrap(),
                ..limits
            },
            ReadError::ItemLimit,
        ),
        (
            vec![response(200, json!({"value":["x".repeat(256)]}))],
            WorkspaceListLimits {
                max_response_bytes: NonZeroUsize::new(128).unwrap(),
                ..limits
            },
            ReadError::ResponseLimit,
        ),
        (
            vec![page(), response(201, json!({"value":[2]}))],
            limits,
            ReadError::InvalidContinuation,
        ),
    ];
    for (responses, limits, expected) in cases {
        let executor = Executor::new(std::iter::once(primary()).chain(responses));
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", limits, pending())
            .await
            .unwrap_err();
        assert_eq!(error.step, Step::ManagedPrivateEndpoints);
        assert_eq!(error.kind, MetadataError::Read(expected));
        assert!(error.enrichments.is_empty());
    }
    let bytes = primary().body.len();
    let executor = Executor::new([primary(), empty(), response(200, json!({})), empty()]);
    let exact = WorkspaceListLimits {
        max_response_bytes: NonZeroUsize::new(bytes).unwrap(),
        ..limits
    };
    assert!(
        client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", exact, pending())
            .await
            .is_ok()
    );
    let executor = Executor::new([primary()]);
    let too_small = WorkspaceListLimits {
        max_response_bytes: NonZeroUsize::new(bytes - 1).unwrap(),
        ..limits
    };
    assert_eq!(
        client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", too_small, pending())
            .await
            .unwrap_err()
            .kind,
        MetadataError::Read(ReadError::ResponseLimit)
    );
}

#[tokio::test]
async fn finite_default_page_and_item_thresholds_are_measured() {
    let pages = (0..100).map(|index| {
        response(
            200,
            if index == 99 {
                json!({"value":[]})
            } else {
                json!({"value":[],"continuationToken":index.to_string()})
            },
        )
    });
    let executor = Executor::new(
        std::iter::once(primary())
            .chain(pages)
            .chain([response(200, json!({})), empty()]),
    );
    assert!(
        client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .is_ok()
    );
    let pages = (0..100).map(|index| {
        response(
            200,
            json!({"value":[],"continuationToken":index.to_string()}),
        )
    });
    let executor = Executor::new(std::iter::once(primary()).chain(pages));
    assert_eq!(
        client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err()
            .kind,
        MetadataError::Read(ReadError::PageLimit)
    );
    for count in [100_000, 100_001] {
        let executor = Executor::new([
            primary(),
            response(200, json!({"value":vec![Value::Null;count]})),
            response(200, json!({})),
            empty(),
        ]);
        let result = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await;
        if count == 100_000 {
            assert_eq!(
                result.unwrap().metadata.as_value()["managedPrivateEndpoints"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                MetadataError::Read(ReadError::ItemLimit)
            );
        }
    }
}

#[tokio::test]
async fn identity_identifier_and_execution_failures_never_turn_into_optional_success() {
    for audience in [Audience::Arm, Audience::OneLake] {
        let credential = Arc::new(Credential::default());
        let executor = Executor::default();
        assert_eq!(
            client(credential.clone(), audience)
                .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
                .await
                .unwrap_err()
                .kind,
            MetadataError::Read(ReadError::InvalidAudience)
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    }
    for id in ["", ".", "..", "a/b", "a\\b", "a?b", "a#b", "a\nb"] {
        let credential = Arc::new(Credential::default());
        assert_eq!(
            client(credential.clone(), Audience::Fabric)
                .get_workspace_metadata(
                    &Executor::default(),
                    id,
                    WorkspaceListLimits::default(),
                    pending()
                )
                .await
                .unwrap_err()
                .kind,
            MetadataError::InvalidIdentifier
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    }
    for call in 1..=4 {
        let credential = Arc::new(Credential {
            fail_at: Some(call),
            ..Credential::default()
        });
        let executor = Executor::new([primary(), empty(), response(200, json!({})), empty()]);
        let error = client(credential, Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, MetadataError::Read(ReadError::Credential));
        assert_eq!(executor.requests.lock().unwrap().len(), call - 1);
    }
    for failure in [
        WorkspaceExecutionError::Failed,
        WorkspaceExecutionError::TimedOut,
        WorkspaceExecutionError::Cancelled,
        WorkspaceExecutionError::RetriesExhausted,
        WorkspaceExecutionError::ResponseLimit,
    ] {
        let executor = Executor::new([primary()]);
        executor.responses.lock().unwrap().push_back(Err(failure));
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.step, Step::ManagedPrivateEndpoints);
        assert_eq!(
            error.kind,
            MetadataError::Read(if failure == WorkspaceExecutionError::ResponseLimit {
                ReadError::ResponseLimit
            } else {
                ReadError::Execution(failure)
            })
        );
    }
}

#[tokio::test]
async fn cancellation_before_during_and_between_steps_drops_work_and_never_commits_partial_success()
{
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    assert_eq!(
        client(credential.clone(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), ready(()))
            .await
            .unwrap_err()
            .kind,
        MetadataError::Read(ReadError::Cancelled)
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    for call in 1..=4 {
        let executor = Executor {
            cancel_at: Some(call),
            ..Executor::new([primary(), empty(), response(200, json!({})), empty()])
        };
        let cancelled = poll_fn(|_| {
            if executor.cancel.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), cancelled)
            .await
            .unwrap_err();
        assert_eq!(error.kind, MetadataError::Read(ReadError::Cancelled));
        assert_eq!(executor.requests.lock().unwrap().len(), call);
    }
    for call in 1..=4 {
        let executor = Executor {
            hold_at: Some(call),
            ..Executor::new([primary(), empty(), response(200, json!({})), empty()])
        };
        let client = client(Arc::default(), Audience::Fabric);
        let cancelled = poll_fn(|_| {
            if executor.requests.lock().unwrap().len() == call {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut operation = Box::pin(client.get_workspace_metadata(
            &executor,
            "w",
            WorkspaceListLimits::default(),
            cancelled,
        ));
        poll_fn(|cx| {
            assert!(operation.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let error = operation.await.unwrap_err();
        assert_eq!(error.kind, MetadataError::Read(ReadError::Cancelled));
        assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
    }
    let credential = Arc::new(Credential {
        hold: true,
        ..Credential::default()
    });
    let client = client(credential.clone(), Audience::Fabric);
    let mut operation = Box::pin(client.get_workspace_metadata(
        &executor,
        "w",
        WorkspaceListLimits::default(),
        pending(),
    ));
    poll_fn(|cx| {
        assert!(operation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(operation);
    assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn metadata_shape_debug_and_send_are_independent_of_legacy_workspace_model() {
    for value in [json!({}), json!(null), json!({"id":1,"displayName":"W"})] {
        assert!(serde_json::from_value::<WorkspaceMetadata>(value).is_err());
    }
    let metadata: WorkspaceMetadata = serde_json::from_value(
        json!({"id":"resource-secret","displayName":"private","future":null}),
    )
    .unwrap();
    assert!(!format!("{metadata:?}").contains("secret"));
    fn assert_send(_: impl Send) {}
    let client = client(Arc::default(), Audience::Fabric);
    let executor = Executor::default();
    assert_send(client.get_workspace_metadata(
        &executor,
        "w",
        WorkspaceListLimits::default(),
        pending(),
    ));
}

#[tokio::test]
async fn generic_201_paging_is_merged_only_when_the_subtree_finishes_at_200() {
    let executor = Executor::new([
        primary(),
        response(201, json!({"value":[1],"continuationToken":"next"})),
        response(200, json!({"value":[2]})),
        response(200, json!({})),
        empty(),
    ]);
    let result = client(Arc::default(), Audience::Fabric)
        .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        result.metadata.as_value()["managedPrivateEndpoints"],
        json!([1, 2])
    );
    for last in [201, 204] {
        let executor = Executor::new([
            primary(),
            response(200, json!({"value":[1],"continuationToken":"next"})),
            response(last, json!({"value":[2]})),
        ]);
        let error = client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            MetadataError::Read(ReadError::InvalidContinuation)
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn continuation_header_precedence_and_malformed_raw_bodies_keep_explicit_outcomes() {
    let mut endpoints = response(
        200,
        json!({"value":[1],"continuationToken":"ignored","continuationUri":"https://unused.invalid"}),
    );
    endpoints
        .headers
        .insert("X-MS-Continuation".into(), String::new());
    let mut spark = response(200, json!({}));
    spark.body = b"invalid-json".to_vec();
    let executor = Executor::new([primary(), endpoints, spark, empty()]);
    let result = client(Arc::default(), Audience::Fabric)
        .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        result.metadata.as_value()["managedPrivateEndpoints"],
        json!([1])
    );
    assert_eq!(
        result.enrichments[1].omission,
        Some(Omission::InvalidResponse)
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 4);
    let mut malformed = primary();
    malformed.body = vec![0xff];
    let executor = Executor::new([malformed]);
    assert_eq!(
        client(Arc::default(), Audience::Fabric)
            .get_workspace_metadata(&executor, "w", WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err()
            .kind,
        MetadataError::Read(ReadError::InvalidResponse)
    );
}
