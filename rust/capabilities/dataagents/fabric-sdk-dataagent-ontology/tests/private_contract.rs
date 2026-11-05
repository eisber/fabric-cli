// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_sdk_dataagent_ontology::{
    inputs::*,
    models::*,
    paging::{PagingErrorKind, PagingLimits},
    private::{Limits, PrivateClient},
    result::{ErrorKind, Outcome},
    scope::*,
};
use fabric_sdk_ontology_workload::workload::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn, ready},
    num::NonZeroUsize,
    sync::{Mutex, atomic::{AtomicBool, Ordering}},
    task::Poll,
};

fn id(number: u8) -> Guid {
    Guid::new(format!("11111111-2222-3333-4444-{number:012}")).unwrap()
}

fn agent() -> AgentScope { AgentScope { workspace_id: id(1), agent_id: id(2) } }
fn caller() -> CallerScope { CallerScope::new("offline-account", "offline-tenant").unwrap() }
fn source(stage: Stage) -> SourceScope {
    SourceScope { attachment: AttachmentScope { agent: agent(), stage, ontology_id: id(3) }, source_id: id(4) }
}
fn client() -> PrivateClient {
    PrivateClient::new(agent(), caller(), WorkloadIdentity::new("caller-dataagent").unwrap(), Limits::default())
}
fn json_response(status: u16, body: Value) -> WorkloadResponse {
    WorkloadResponse {
        status, headers: BTreeMap::from([
            ("Content-Type".into(), "application/json; charset=utf-8".into()),
            ("ETag".into(), "\"snapshot-1\"".into()),
        ]), body: serde_json::to_vec(&body).unwrap(),
    }
}
fn attachment() -> Value {
    json!({"id": id(3), "ontologyReference": {
        "referenceType": "ById", "itemId": id(3), "workspaceId": id(5)
    }, "displayName": "Ontology"})
}
fn element() -> Value {
    json!({"id": "returned+/=", "displayName": "Entity", "type": "OntologyEntity",
        "isSelected": true, "hasSubElements": false})
}
fn page(token: Option<&str>) -> WorkloadResponse {
    json_response(200, json!({"value": [element()], "continuationToken": token}))
}
fn few_shot() -> Value {
    json!({"id": id(6), "question": "authored question", "query": "authored query",
        "validationStatus": {"state": "Invalid"}})
}

struct Mock {
    results: Mutex<VecDeque<Result<WorkloadResponse, WorkloadError>>>,
    seen: Mutex<Vec<WorkloadRequest>>,
}
impl Mock {
    fn new(responses: Vec<WorkloadResponse>) -> Self {
        Self { results: Mutex::new(responses.into_iter().map(Ok).collect()), seen: Mutex::new(Vec::new()) }
    }
}
impl WorkloadExecutor for Mock {
    fn execute<'a>(&'a self, request: &'a WorkloadRequest) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(request.clone());
            self.results.lock().unwrap().pop_front().expect("unexpected dispatch")
        })
    }
}

#[test]
fn private_native_routes_methods_media_and_bodies_are_exact() {
    let sdk = client();
    let attach = AttachOntology::new(ItemReference::by_id(id(3), id(5)), "Ontology").unwrap();
    let base = format!("/v1/workspaces/{}/dataagents/{}", id(1), id(2));
    let add = sdk.add(&attach, None).unwrap();
    assert_eq!(add.request().route().as_str(), format!("{base}/staging/ontologies"));
    assert_eq!(add.request().method(), WorkloadMethod::Post);
    assert_eq!(add.request().content_type(), Some("application/json"));
    assert_eq!(serde_json::from_slice::<Value>(add.request().body()).unwrap(), json!({
        "ontologyReference": {"referenceType": "ById", "itemId": id(3), "workspaceId": id(5)},
        "displayName": "Ontology"
    }));
    assert!(add.effects().auto_selects_entities);
    let select = sdk.select(id(3), &SelectEntities::named(vec!["opaque+/=".into()], false).unwrap(), None).unwrap();
    assert_eq!(select.request().route().as_str(), format!("{base}/staging/ontologies/{}/select", id(3)));
    assert_eq!(select.request().method(), WorkloadMethod::Post);
    assert_eq!(serde_json::from_slice::<Value>(select.request().body()).unwrap(),
        json!({"elementIds": ["opaque+/="], "isSelected": false}));
    let patch = sdk.patch_instructions(source(Stage::Staging), &InstructionsPatch::new(Some("original".into()), None).unwrap(), None).unwrap();
    assert_eq!(patch.request().method(), WorkloadMethod::Patch);
    assert_eq!(patch.request().content_type(), Some("application/json"));
    assert!(patch.request().route().as_str().ends_with(&format!("/ontologydatasources/{}/instructions", id(4))));
    assert_eq!(serde_json::from_slice::<Value>(patch.request().body()).unwrap(), json!({"instructions": "original"}));
    let replace = sdk.replace_instructions(source(Stage::Staging), &InstructionsReplace::new(None, None).unwrap(), None).unwrap();
    assert_eq!(replace.request().method(), WorkloadMethod::Put);
    assert_eq!(serde_json::from_slice::<Value>(replace.request().body()).unwrap(), json!({"instructions": null, "description": null}));
    let create = sdk.create_few_shot(source(Stage::Staging), &FewShotCreate::new("".into(), Some("draft".into())).unwrap(), None).unwrap();
    assert_eq!(create.request().method(), WorkloadMethod::Post);
    assert!(create.request().route().as_str().ends_with("/fewshots"));
    assert!(create.effects().may_validate_examples);
    let publish = sdk.publish(&Publish::new(Some("published description".into())).unwrap()).unwrap();
    assert_eq!(publish.request().route().as_str(), format!("{base}/staging/publish"));
    assert_eq!(serde_json::from_slice::<Value>(publish.request().body()).unwrap(), json!({"publishedDescription":"published description"}));
    assert!(publish.effects().publishes_whole_agent);
}

#[test]
fn private_stage_and_caller_scope_are_not_inferred_or_crossed() {
    let sdk = client();
    for (stage, marker) in [(Stage::Staging, "/staging/ontologies"), (Stage::Published, "/ontologies")] {
        let list = sdk.list(stage).unwrap();
        assert!(list.request().route().as_str().ends_with(marker));
        assert!(list.effects().may_migrate_configuration);
        assert!(!list.effects().publishes_whole_agent);
        assert!(sdk.source_schema(source(stage), Some(ResponseSource::Live)).unwrap().request()
            .route().as_str().ends_with("/schema?responseSource=live"));
    }
    assert!(sdk.patch_instructions(source(Stage::Published), &InstructionsPatch::new(None, None).unwrap(), None).is_err());
    assert!(sdk.instructions(source(Stage::Published)).is_err());
    assert!(sdk.create_few_shot(source(Stage::Published), &FewShotCreate::new("q".into(), None).unwrap(), None).is_err());
    let mut other = source(Stage::Staging);
    other.attachment.agent.agent_id = id(7);
    assert!(sdk.source_schema(other, None).is_err());
}

#[test]
fn opaque_navigation_values_are_encoded_once_and_never_used_as_paths() {
    let options = ElementsOptions {
        root_id: Some("opaque+/= %2F".into()), continuation_token: Some("cursor+/=".into()),
        page_size: Some(12), response_source: Some(ResponseSource::Any),
    };
    let read = client().elements(Stage::Staging, id(3), &options).unwrap();
    let route = read.request().route().as_str();
    assert!(route.ends_with("/elements?rootId=opaque%2B%2F%3D%20%252F&continuationToken=cursor%2B%2F%3D&pageSize=12&responseSource=any"));
    assert!(read.request().body().is_empty());
    assert_eq!(read.request().content_type(), None);
}

#[tokio::test]
async fn returned_attachment_and_generated_example_identity_are_checked() {
    let sdk = client();
    let mock = Mock::new(vec![json_response(200, attachment()), json_response(201, few_shot())]);
    let add = sdk.add(&AttachOntology::new(ItemReference::by_id(id(3), id(5)), "Ontology").unwrap(), None).unwrap();
    let Outcome::Completed(added) = sdk.execute(&mock, add, pending()).await.unwrap() else { panic!("not completed") };
    assert_eq!(added.value.id, id(3));
    let operation = sdk.create_few_shot(source(Stage::Staging), &FewShotCreate::new("authored question".into(), Some("authored query".into())).unwrap(), None).unwrap();
    let Outcome::Completed(created) = sdk.execute(&mock, operation, pending()).await.unwrap() else { panic!("not completed") };
    assert_eq!(created.value.id, id(6));
    assert_eq!(created.value.validation_status["state"], "Invalid");
    assert_eq!(mock.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn exact_optional_conditions_and_stale_etag_are_never_dropped_or_retried() {
    let sdk = client();
    let scope = source(Stage::Staging);
    let tag = Validator::new(caller(), Resource::Instructions(scope.clone()), "\"stale-source\"").unwrap();
    let input = InstructionsPatch::new(Some("new instructions".into()), None).unwrap();
    let operation = sdk.patch_instructions(scope.clone(), &input, Some(&tag)).unwrap();
    assert_eq!(operation.request().if_match(), Some("\"stale-source\""));
    let mock = Mock::new(vec![json_response(412, json!({"error": {"code": "PreconditionFailed"}}))]);
    let error = sdk.execute(&mock, operation, pending()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::PreconditionFailed);
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].if_match(), Some("\"stale-source\""));
    assert_eq!(sdk.patch_instructions(scope.clone(), &input, None).unwrap().request().if_match(), None);
    let other_source = SourceScope { source_id: id(8), ..scope };
    assert!(sdk.patch_instructions(other_source, &input, Some(&tag)).is_err());
    let attachment_tag = Validator::new(caller(), Resource::Attachment(source(Stage::Staging).attachment), "\"existing\"").unwrap();
    let attach = AttachOntology::new(ItemReference::by_id(id(3), id(5)), "Other name").unwrap();
    assert_eq!(sdk.add(&attach, Some(&attachment_tag)).unwrap().request().if_match(), Some("\"existing\""));
}

#[tokio::test]
async fn selected_source_authority_survives_partial_instruction_and_example_updates() {
    let sdk = client();
    let scope = source(Stage::Staging);
    let mock = Mock::new(vec![
        json_response(200, json!({
            "artifactId": id(4), "dataSourceInstructions": "preserved authored instructions",
            "userDescription": "updated authored description"
        })),
        json_response(200, few_shot()),
        WorkloadResponse { status: 200, headers: BTreeMap::new(), body: Vec::new() },
    ]);
    let input = InstructionsPatch::new(None, Some("updated authored description".into())).unwrap();
    let Outcome::Completed(result) = sdk.execute(&mock, sdk.patch_instructions(scope.clone(), &input, None).unwrap(), pending()).await.unwrap() else { panic!("not completed") };
    assert_eq!(result.value.data_source_instructions.as_deref(), Some("preserved authored instructions"));
    let patch = FewShotPatch::new(None, Some("updated query".into())).unwrap();
    sdk.execute(&mock, sdk.patch_few_shot(scope.clone(), id(6), &patch, None).unwrap(), pending()).await.unwrap();
    sdk.execute(&mock, sdk.delete_instructions(scope, None).unwrap(), pending()).await.unwrap();
    let seen = mock.seen.lock().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(seen[0].body()).unwrap(), json!({"description": "updated authored description"}));
    assert_eq!(serde_json::from_slice::<Value>(seen[1].body()).unwrap(), json!({"query": "updated query"}));
    assert!(seen.iter().all(|request| request.route().as_str().contains(&format!("/ontologies/{}/ontologydatasources/{}/", id(3), id(4)))));
    assert!(seen.iter().all(|request| !request.route().as_str().contains("/publish") && !request.route().as_str().contains("/responses")));
    assert!(seen[2].body().is_empty());
}

#[tokio::test]
async fn duplicate_headers_invalid_media_and_empty_publish_shape_are_not_success() {
    let sdk = client();
    let mut duplicate = json_response(200, attachment());
    duplicate.headers.insert("etag".into(), "\"ambiguous\"".into());
    let mock = Mock::new(vec![duplicate]);
    assert_eq!(sdk.execute(&mock, sdk.read(Stage::Staging, id(3)).unwrap(), pending()).await.unwrap_err().kind, ErrorKind::InvalidResponse);
    let mut wrong_media = json_response(200, attachment());
    wrong_media.headers.insert("Content-Type".into(), "text/markdown".into());
    let mock = Mock::new(vec![wrong_media]);
    assert_eq!(sdk.execute(&mock, sdk.read(Stage::Staging, id(3)).unwrap(), pending()).await.unwrap_err().kind, ErrorKind::InvalidResponse);
    let mock = Mock::new(vec![json_response(200, json!({}))]);
    let error = sdk.execute(&mock, sdk.publish(&Publish::new(None).unwrap()).unwrap(), pending()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidResponse);
    assert_eq!(error.dispatch, DispatchState::Unknown);
    assert!(error.effects.publishes_whole_agent);
}

#[tokio::test]
async fn malformed_or_wrong_identity_responses_are_errors_with_original_evidence() {
    let sdk = client();
    for body in [json!({}), json!({"id": id(8)}), json!({"id": id(3), "ontologyReference": {
        "referenceType": "ById", "itemId": id(8), "workspaceId": id(5)
    }})] {
        let response = json_response(200, body);
        let mock = Mock::new(vec![response.clone()]);
        let operation = sdk.read(Stage::Staging, id(3)).unwrap();
        let error = sdk.execute(&mock, operation, pending()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidResponse);
        assert_eq!(error.response.unwrap(), response);
    }
    let mock = Mock::new(vec![json_response(200, json!({
        "dataSourceId": id(9), "cacheLastUpdatedTime": "2026-11-05T00:00:00Z", "elements": []
    }))]);
    let error = sdk.execute(&mock, sdk.source_schema(source(Stage::Staging), None).unwrap(), pending()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidResponse);
}

#[tokio::test]
async fn every_source_http_failure_stays_distinct_and_no_request_is_replayed() {
    let sdk = client();
    for (status, kind) in [
        (400, ErrorKind::InvalidInput), (401, ErrorKind::Unauthorized),
        (403, ErrorKind::Forbidden), (404, ErrorKind::NotFound),
        (409, ErrorKind::Conflict), (412, ErrorKind::PreconditionFailed),
        (429, ErrorKind::Throttled), (503, ErrorKind::Provider),
    ] {
        let response = json_response(status, json!({"error": {"code": "native-code", "message": "provider data"}}));
        let mock = Mock::new(vec![response.clone()]);
        let error = sdk.execute(&mock, sdk.cleanup_unavailable_source(source(Stage::Staging), None).unwrap(), pending()).await.unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.response.unwrap(), response);
        assert_eq!(mock.seen.lock().unwrap().len(), 1);
        assert!(!error.effects.publishes_whole_agent);
    }
}

#[tokio::test]
async fn selection_keeps_not_found_and_rejects_reordered_or_missing_results() {
    let sdk = client();
    let input = SelectEntities::named(vec!["first".into(), "second".into()], false).unwrap();
    let mock = Mock::new(vec![json_response(200, json!({"elementsStatus": [
        {"elementId": "first", "status": "Succeeded"}, {"elementId": "second", "status": "NotFound"}
    ]}))]);
    let Outcome::Completed(result) = sdk.execute(&mock, sdk.select(id(3), &input, None).unwrap(), pending()).await.unwrap() else { panic!("not completed") };
    assert_eq!(result.value.elements_status[1].status, "NotFound");
    let mock = Mock::new(vec![json_response(200, json!({"elementsStatus": [
        {"elementId": "second", "status": "Succeeded"}, {"elementId": "first", "status": "Succeeded"}
    ]}))]);
    assert_eq!(sdk.execute(&mock, sdk.select(id(3), &input, None).unwrap(), pending()).await.unwrap_err().kind, ErrorKind::InvalidResponse);
}

#[tokio::test]
async fn served_markdown_is_not_authored_instructions_or_an_answer_fallback() {
    let sdk = client();
    let projection = WorkloadResponse {
        status: 200, headers: BTreeMap::from([("content-type".into(), "text/markdown".into())]),
        body: b"# generated selected-entity context".to_vec(),
    };
    let mock = Mock::new(vec![projection]);
    let Outcome::Completed(context) = sdk.execute(&mock, sdk.context_markdown(id(3)).unwrap(), pending()).await.unwrap() else { panic!("not completed") };
    assert!(context.value.markdown.starts_with("# generated"));
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].route().as_str().ends_with("/contextmarkdown"));
    assert!(!seen[0].route().as_str().contains("/responses"));
}

#[tokio::test]
async fn publish_acceptance_and_unknown_effects_never_become_completed_or_replayed() {
    let sdk = client();
    let mut accepted = json_response(202, json!({"state": "accepted"}));
    accepted.headers.insert("Location".into(), "https://caller-bound.invalid/operation/receipt".into());
    let mock = Mock::new(vec![accepted.clone()]);
    let outcome = sdk.execute(&mock, sdk.publish(&Publish::new(None).unwrap()).unwrap(), pending()).await.unwrap();
    let Outcome::Accepted { response, effects } = outcome else { panic!("accepted is not completed") };
    assert_eq!(response, accepted);
    assert!(effects.publishes_whole_agent);
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
    let mock = Mock { results: Mutex::new(VecDeque::from([Err(WorkloadError::execution(DispatchState::Unknown, "diagnostic".into()))])), seen: Mutex::new(Vec::new()) };
    let error = sdk.execute(&mock, sdk.publish(&Publish::new(None).unwrap()).unwrap(), pending()).await.unwrap_err();
    assert_eq!(error.dispatch, DispatchState::Unknown);
    assert!(error.workload_error.is_some());
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
}

struct PendingExecutor(AtomicBool);
impl WorkloadExecutor for PendingExecutor {
    fn execute<'a>(&'a self, _: &'a WorkloadRequest) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move { self.0.store(true, Ordering::SeqCst); pending().await })
    }
}

#[tokio::test]
async fn cancellation_distinguishes_not_sent_from_post_dispatch_unknown() {
    let sdk = client();
    let executor = PendingExecutor(AtomicBool::new(false));
    let error = sdk.execute(&executor, sdk.publish(&Publish::new(None).unwrap()).unwrap(), ready(())).await.unwrap_err();
    assert_eq!(error.dispatch, DispatchState::NotSent);
    assert!(!executor.0.load(Ordering::SeqCst));
    let cancelled = poll_fn(|context| {
        if executor.0.load(Ordering::SeqCst) { Poll::Ready(()) }
        else { context.waker().wake_by_ref(); Poll::Pending }
    });
    let error = sdk.execute(&executor, sdk.publish(&Publish::new(None).unwrap()).unwrap(), cancelled).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(error.dispatch, DispatchState::Unknown);
}

#[tokio::test]
async fn shared_pager_preserves_rows_and_encodes_native_cursor_once() {
    let sdk = client();
    let mock = Mock::new(vec![page(Some("cursor+/=")), page(None)]);
    let operation = sdk.elements(Stage::Staging, id(3), &ElementsOptions::default()).unwrap();
    let result = sdk.collect_elements(&mock, operation, "https://caller-bound.invalid/".parse().unwrap(), PagingLimits::default(), pending()).await.unwrap();
    assert_eq!(result.value.len(), 2);
    assert_eq!(result.pages_received, 2);
    assert!(mock.seen.lock().unwrap()[1].route().as_str().ends_with("?continuationToken=cursor%2B%2F%3D"));
}

#[tokio::test]
async fn repeated_cursor_page_row_byte_and_snapshot_bounds_report_partial_coverage() {
    let sdk = client();
    let origin = || "https://caller-bound.invalid/".parse().unwrap();
    let operation = || sdk.elements(Stage::Staging, id(3), &ElementsOptions::default()).unwrap();
    let mock = Mock::new(vec![page(Some("repeated")), page(Some("repeated"))]);
    let error = sdk.collect_elements(&mock, operation(), origin(), PagingLimits::default(), pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::RepeatedContinuation));
    assert_eq!(error.error.partial.pages_received, 2);
    let limits = PagingLimits { max_pages: NonZeroUsize::new(1).unwrap(), ..PagingLimits::default() };
    let error = sdk.collect_elements(&Mock::new(vec![page(Some("next"))]), operation(), origin(), limits, pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::PageLimit));
    assert_eq!(error.error.partial.value.len(), 1);
    let limits = PagingLimits { max_items: NonZeroUsize::new(1).unwrap(), ..PagingLimits::default() };
    let error = sdk.collect_elements(&Mock::new(vec![page(Some("next")), page(None)]), operation(), origin(), limits, pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::ItemLimit));
    assert_eq!(error.error.partial.value.len(), 1);
    let limits = PagingLimits { max_response_bytes: NonZeroUsize::new(8).unwrap(), ..PagingLimits::default() };
    let error = sdk.collect_elements(&Mock::new(vec![page(None)]), operation(), origin(), limits, pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::Fetch(ref error) if error.kind == ErrorKind::Limit && error.response_truncated));
    let mut changed = page(None);
    changed.headers.insert("ETag".into(), "\"snapshot-2\"".into());
    let error = sdk.collect_elements(&Mock::new(vec![page(Some("next")), changed]), operation(), origin(), PagingLimits::default(), pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::Fetch(ref error) if error.kind == ErrorKind::SnapshotChanged));
    assert_eq!(error.error.partial.pages_received, 1);
}

#[tokio::test]
async fn unsafe_continuation_does_not_change_executor_or_route_and_malformed_second_page_is_not_success() {
    let sdk = client();
    let mut first = page(Some("next"));
    let mut body: Value = serde_json::from_slice(&first.body).unwrap();
    body["continuationUri"] = json!("https://untrusted.invalid/other/stage");
    first.body = serde_json::to_vec(&body).unwrap();
    let mock = Mock::new(vec![first]);
    let operation = || sdk.elements(Stage::Staging, id(3), &ElementsOptions::default()).unwrap();
    let error = sdk.collect_elements(&mock, operation(), "https://caller-bound.invalid/".parse().unwrap(), PagingLimits::default(), pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::InvalidContinuation));
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
    let mock = Mock::new(vec![page(Some("next")), json_response(200, json!({"value": [{}]}))]);
    let error = sdk.collect_elements(&mock, operation(), "https://caller-bound.invalid/".parse().unwrap(), PagingLimits::default(), pending()).await.unwrap_err();
    assert!(matches!(error.error.kind, PagingErrorKind::Fetch(ref error) if error.kind == ErrorKind::InvalidResponse));
    assert_eq!(error.error.partial.pages_received, 1);
}
