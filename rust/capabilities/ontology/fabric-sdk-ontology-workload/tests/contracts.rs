// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_sdk_ontology_workload::{
    capabilities::{GAPS, RESOURCES, ResourceAccess},
    client::*,
    models::*,
    rdf::*,
    scripts::*,
    workload::*,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Default)]
struct Fixture {
    requests: Mutex<Vec<WorkloadRequest>>,
    responses: Mutex<VecDeque<Result<WorkloadResponse, WorkloadError>>>,
}

impl Fixture {
    fn new(responses: Vec<WorkloadResponse>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
        }
    }
    fn calls(&self) -> Vec<WorkloadRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl WorkloadExecutor for Fixture {
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Err(WorkloadError::execution(
                        DispatchState::NotSent,
                        "missing fixture response".into(),
                    ))
                })
        })
    }
}

fn features() -> Features {
    Features {
        tmsl: true,
        inheritance: true,
        reusable_properties: true,
        auto_projection: true,
    }
}
fn target(features: Features) -> OntologyTarget {
    OntologyTarget::new(
        WorkloadIdentity::new("DigitalOperations").unwrap(),
        WORKSPACE,
        ITEM,
        "Ontology",
        2,
        features,
    )
    .unwrap()
}
fn client(executor: &dyn WorkloadExecutor) -> OntologyClient<'_> {
    OntologyClient::new(
        target(features()),
        executor,
        ByteLimits::new(16 * 1024 * 1024, 16 * 1024 * 1024).unwrap(),
    )
    .unwrap()
}
fn response(status: u16, body: Value) -> WorkloadResponse {
    WorkloadResponse {
        status,
        headers: BTreeMap::from([(
            "Content-Type".into(),
            "application/json; charset=utf-8".into(),
        )]),
        body: serde_json::to_vec(&body).unwrap(),
    }
}
fn empty() -> WorkloadResponse {
    WorkloadResponse {
        status: 204,
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}
fn lineage<T: LineageResource>(value: &str) -> MemberId<T> {
    MemberId::lineage(LineageTag::new(value).unwrap())
}
fn name(value: &str) -> ObjectName {
    ObjectName::new(value).unwrap()
}
fn entity(value: Value) -> EntityType {
    serde_json::from_value(value).unwrap()
}
fn wire(request: &WorkloadRequest) -> Value {
    serde_json::from_slice(request.body()).unwrap()
}

#[test]
fn target_rejects_wrong_type_generation_ids_and_flight_before_executor() {
    let executor = Fixture::default();
    for (workspace, item, kind, generation) in [
        ("invalid", ITEM, "Ontology", 2),
        (WORKSPACE, "invalid", "Ontology", 2),
        (WORKSPACE, ITEM, "DigitalTwinBuilder", 2),
        (WORKSPACE, ITEM, "Ontology", 1),
        (WORKSPACE, ITEM, "Ontology", 3),
        (WORKSPACE, ITEM, "SemanticModel", 2),
    ] {
        assert!(
            OntologyTarget::new(
                WorkloadIdentity::new("DigitalOperations").unwrap(),
                workspace,
                item,
                kind,
                generation,
                features()
            )
            .is_err()
        );
    }
    assert!(
        OntologyTarget::new(
            WorkloadIdentity::new("DigitalOperations").unwrap(),
            WORKSPACE,
            ITEM,
            "Ontology",
            2,
            Features::default()
        )
        .is_err()
    );
    assert!(executor.calls().is_empty());
    assert!(
        OntologyTarget::new(
            WorkloadIdentity::new("ML").unwrap(),
            WORKSPACE,
            ITEM,
            "Ontology",
            2,
            features()
        )
        .is_err()
    );
}

#[test]
fn injected_route_method_content_type_if_match_and_bounds_are_real() {
    let logical = WorkloadIdentity::new("ML").unwrap();
    let route = WorkloadRoute::new(
        &["v1", "names", "plant#A%23 B/?"],
        &[("continuationToken", "a+/= &b")],
    )
    .unwrap();
    assert_eq!(
        route.as_str(),
        "/v1/names/plant%23A%2523%20B%2F%3F?continuationToken=a%2B%2F%3D%20%26b"
    );
    let request = WorkloadRequest::new(
        logical,
        route,
        WorkloadMethod::Put,
        Some("application/json"),
        b"{}".to_vec(),
        2,
        10,
    )
    .unwrap()
    .with_if_match(" W/\"original-version\" ")
    .unwrap();
    assert_eq!(request.method().as_str(), "PUT");
    assert_eq!(request.if_match(), Some(" W/\"original-version\" "));
    assert_eq!(request.workload().as_str(), "ML");
    assert_eq!(request.body(), b"{}");
    assert_eq!(request.content_type(), Some("application/json"));
    assert_eq!(request.max_response_bytes(), 10);
    for token in ["", " ", "bad\r\nHost: bad", "\0", "é"] {
        assert!(request.clone().with_if_match(token).is_err());
    }
    assert!(request.clone().with_if_match("x".repeat(8193)).is_err());
    assert!(request.with_if_match("x".repeat(8192)).is_ok());
    assert!(WorkloadIdentity::new("https://host.invalid").is_err());
    for segments in [&[".."][..], &["."][..], &["\r\n"][..], &[][..]] {
        assert!(WorkloadRoute::new(segments, &[]).is_err());
    }
    for media in [
        None,
        Some("text"),
        Some("text/"),
        Some("application/json\r\nX: y"),
        Some(" /json"),
    ] {
        assert!(
            WorkloadRequest::new(
                WorkloadIdentity::new("DO").unwrap(),
                WorkloadRoute::new(&["v4"], &[]).unwrap(),
                WorkloadMethod::Post,
                media,
                vec![1],
                1,
                1
            )
            .is_err()
        );
    }
}

async fn crud_contract<T: MutableResource>(body: Value, id: MemberId<T>, identity: &str) {
    let typed: T = serde_json::from_value(body.clone()).unwrap();
    let mut read = body;
    read["lineageTag"] = json!("server-lineage");
    read["futureField"] = json!({"unknown":null});
    let executor = Fixture::new(vec![
        response(201, read.clone()),
        response(200, read.clone()),
        response(200, read.clone()),
        response(200, json!({"value":[read.clone()]})),
        empty(),
    ]);
    let client = client(&executor);
    let created = client.create(&typed).await.unwrap();
    let Outcome::Completed { value, .. } = created else {
        panic!("strict create must complete");
    };
    assert_eq!(value.raw, read);
    client.get(&id, false).await.unwrap();
    let patch = MergePatch::<T>::new(json!({"name":"Renamed"})).unwrap();
    client.patch(&id, &patch).await.unwrap();
    let listed = client
        .list::<T>(&ListOptions::new(1, 10, 65536).unwrap())
        .await
        .unwrap();
    assert!(matches!(listed.stop, ListStop::Complete));
    assert_eq!(listed.items.len(), 1);
    client.delete(&id).await.unwrap();
    let calls = executor.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].method(), WorkloadMethod::Post);
    assert_eq!(
        calls[0].route().as_str(),
        format!(
            "/v4/workspaces/{WORKSPACE}/ontologies/{ITEM}/{}",
            T::COLLECTION
        )
    );
    assert_eq!(
        calls[1].route().as_str(),
        format!(
            "/v4/workspaces/{WORKSPACE}/ontologies/{ITEM}/{}/{identity}",
            T::COLLECTION
        )
    );
    assert_eq!(
        calls[2].content_type(),
        Some("application/merge-patch+json")
    );
    assert_eq!(wire(&calls[2]), patch.value().clone());
    assert_eq!(calls[4].method(), WorkloadMethod::Delete);
    assert!(
        calls
            .iter()
            .all(|r| r.workload().as_str() == "DigitalOperations"
                && !r.route().as_str().contains("api.fabric"))
    );
}

#[tokio::test]
async fn all_seven_native_collections_execute_real_crud_list_get_requests() {
    crud_contract::<EntityType>(
        json!({"name":"plant#Machine"}),
        lineage("entity-tag"),
        "entity-tag",
    )
    .await;
    crud_contract::<EntityRelationship>(
        json!({"name":"Owns","fromEntity":"A","toEntity":"B"}),
        lineage("rel-tag"),
        "rel-tag",
    )
    .await;
    crud_contract::<DataSource>(json!({"name":"Machines","columns":[{"name":"Id","lineageTag":"keep-me","dataType":"int64"}]}), lineage("table-tag"), "table-tag").await;
    crud_contract::<Expression>(
        json!({"name":"Source","expression":"let source = 1 in source","kind":"m"}),
        lineage("expr-tag"),
        "expr-tag",
    )
    .await;
    crud_contract::<Namespace>(json!({"name":"urn:plant"}), lineage("default"), "default").await;
    crud_contract::<Relationship>(json!({"type":"singleColumn","name":"Join#1","fromTable":"A","toTable":"B","fromColumn":"Id","toColumn":"Id"}),
        MemberId::named(name("Join#1")).unwrap(), "Join%231").await;
    crud_contract::<Rule>(json!({"name":"Safety","statement":"Temperature must be bounded","referencedEntities":[{"entity":"plant#Machine","propertyScope":"specific","properties":["Temperature"]}]}),
        lineage("rule-tag"), "rule-tag").await;
}

#[test]
fn direct_capability_advertisement_is_read_only_for_metrics_and_reusable_properties() {
    assert_eq!(RESOURCES.len(), 9);
    let readonly: Vec<_> = RESOURCES
        .iter()
        .filter(|c| c.access == ResourceAccess::ReadOnly)
        .map(|c| c.collection)
        .collect();
    assert_eq!(readonly, ["metrics", "reusableProperties"]);
    assert!(
        GAPS.iter()
            .any(|g| g.operation == "rdf.import" && g.reason.contains("atomic create-only"))
    );
    assert!(
        GAPS.iter()
            .any(|g| g.operation == "definition.tmdl" && g.reason.contains("501"))
    );
}

#[tokio::test]
async fn readonly_metrics_and_reusable_properties_have_typed_get_and_list_with_gates() {
    let executor = Fixture::new(vec![
        response(
            200,
            json!({"type":"explicit","name":"M","lineageTag":"metric-tag","newMetadata":7}),
        ),
        response(200, json!({"value":[]})),
        response(
            200,
            json!({"name":"Shared","lineageTag":"property-tag","newMetadata":true}),
        ),
        response(200, json!({"value":[]})),
    ]);
    let client = client(&executor);
    client
        .get::<Metric>(&lineage("metric-tag"), false)
        .await
        .unwrap();
    client
        .list::<Metric>(&ListOptions::new(1, 1, 1000).unwrap())
        .await
        .unwrap();
    client
        .get::<ReusableProperty>(&MemberId::named(name("Shared")).unwrap(), false)
        .await
        .unwrap();
    client
        .list::<ReusableProperty>(&ListOptions::new(1, 1, 1000).unwrap())
        .await
        .unwrap();
    assert_eq!(executor.calls().len(), 4);
    let gated = OntologyClient::new(
        target(Features {
            reusable_properties: false,
            ..features()
        }),
        &executor,
        ByteLimits::new(1000, 1000).unwrap(),
    )
    .unwrap();
    assert!(
        gated
            .get_request::<ReusableProperty>(&MemberId::named(name("Shared")).unwrap(), false)
            .is_err()
    );
    assert!(
        gated
            .page_request::<ReusableProperty>(None, false, &[])
            .is_err()
    );
    assert_eq!(executor.calls().len(), 4);
}

#[test]
fn patch_keeps_omission_null_and_full_array_replacement_without_projection_writes() {
    let fixture = Fixture::default();
    let client = client(&fixture);
    let patch = MergePatch::<EntityType>::new(json!({"description":null,"properties":[{"name":"Id","dataType":"int64","lineageTag":"stable-property"}],"keyProperties":[]})).unwrap();
    let request = client
        .patch_request(&lineage("stable-entity"), &patch)
        .unwrap();
    let body = wire(&request);
    assert!(body.get("description").unwrap().is_null());
    assert!(body.get("backingTable").is_none());
    assert_eq!(body["keyProperties"], json!([]));
    assert_eq!(body["properties"][0]["lineageTag"], "stable-property");
    for invalid in [
        json!({"lineageTag":"illegal-root"}),
        json!({"namespace":"plant"}),
        json!({"displayName":"Machine"}),
        json!({"arbitraryGraph":{"nodes":[]}}),
        json!({"properties":[{"name":"Id"}]}),
        json!({"properties":[{"name":"Id","dataType":"int64","unknown":true}]}),
        json!({"baseEntityType":"Other"}),
        json!({"properties":"not-an-array"}),
    ] {
        assert!(MergePatch::<EntityType>::new(invalid).is_err());
    }
    assert!(MergePatch::<EntityRelationship>::new(json!({"label":null})).is_err());
    assert!(fixture.calls().is_empty());
}

#[test]
fn payload_and_identity_types_do_not_conflate_public_projection_names_or_lineage() {
    let fixture = Fixture::default();
    let client = client(&fixture);
    assert!(
        serde_json::from_value::<EntityType>(
            json!({"name":"Machine","namespace":"plant","displayName":"Machine"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<EntityType>(json!({"name":"Machine","lineageTag":"server-only"}))
            .is_err()
    );
    for invalid in ["", "#A", "A#", "A#B#C", "A%notEncoded"] {
        assert!(validate_composite_name(invalid).is_err());
    }
    assert!(validate_composite_name("plant%23west#A%25").is_ok());
    let read = client
        .get_request::<EntityType>(&lineage("engine-derived:machine/1"), false)
        .unwrap();
    assert!(
        read.route()
            .as_str()
            .ends_with("/entityTypes/engine-derived%3Amachine%2F1")
    );
    let tom = client
        .get_request::<Relationship>(&MemberId::named(name("Join#A%25")).unwrap(), false)
        .unwrap();
    assert!(
        tom.route()
            .as_str()
            .ends_with("/relationships/Join%23A%2525")
    );
}

#[tokio::test]
async fn read_projection_keeps_unknown_fields_nulls_nested_tags_and_original_body() {
    let raw = json!({"name":"plant#Machine","lineageTag":"engine-identity","namespace":"plant","displayName":"Machine",
        "properties":[{"name":"Id","dataType":"int64","lineageTag":"nested-tag","future":{"a":null}}],
        "resourceLinks":[{"type":"FabricItem","item":{"type":"Variable","variableReference":"$(/**/Library/Target:ItemReference)",
            "resolvedItem":{"workspaceId":WORKSPACE,"itemId":ITEM}}}],
        "unknown":null,"description":null});
    let executor = Fixture::new(vec![response(200, raw.clone())]);
    let Outcome::Completed { value, response } = client(&executor)
        .get::<EntityType>(&lineage("engine-identity"), false)
        .await
        .unwrap()
    else {
        panic!("read must complete");
    };
    assert_eq!(value.raw, raw);
    assert_eq!(value.lineage_tag(), Some("engine-identity"));
    assert_eq!(
        value.fields.properties.unwrap()[0].lineage_tag.as_deref(),
        Some("nested-tag")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap(),
        raw
    );
}

#[test]
fn inheritance_feature_gate_covers_null_and_empty_overrides_but_not_unrelated_edits() {
    let fixture = Fixture::default();
    let client = OntologyClient::new(
        target(Features {
            inheritance: false,
            ..features()
        }),
        &fixture,
        ByteLimits::new(65536, 65536).unwrap(),
    )
    .unwrap();
    for body in [
        json!({"name":"Derived","baseEntityType":"Base"}),
        json!({"name":"Derived","overriddenMetadataFields":[]}),
        json!({"name":"Derived","properties":[{"name":"Id","dataType":"int64","redefines":{"entity":"Base","property":"Id"}}]}),
    ] {
        assert!(client.create_request(&entity(body)).is_err());
    }
    let patch = MergePatch::<EntityType>::new(json!({"overriddenMetadataFields":null})).unwrap();
    assert!(
        client
            .patch_request(&lineage("derived-tag"), &patch)
            .is_err()
    );
    assert!(
        client
            .patch_request(
                &lineage("derived-tag"),
                &MergePatch::<EntityType>::new(json!({"description":"unrelated"})).unwrap()
            )
            .is_ok()
    );
    assert!(
        client
            .delete_request::<EntityType>(&lineage("derived-tag"))
            .is_ok()
    );
    assert!(
        client
            .get_request::<EntityType>(&lineage("derived-tag"), false)
            .is_ok()
    );
    assert!(fixture.calls().is_empty());
}

#[test]
fn recursive_struct_backing_requires_exact_coverage_and_time_series_pairing() {
    let fixture = Fixture::default();
    let client = client(&fixture);
    let logical = json!({"kind":"struct","fields":[{"name":"Id","type":{"kind":"primitive","dataType":"int64"}}]});
    let backing = json!({"type":"struct","fields":[{"name":"id","backingConfiguration":{"type":"default","valueColumn":{"table":"T","column":"Id"}}}]});
    let valid = json!({"name":"Entity","properties":[{"name":"P","dataType":"complex","complexDataType":logical,"backingConfiguration":backing}]});
    assert!(client.create_request(&entity(valid.clone())).is_ok());
    for replacement in [
        Value::Null,
        json!({"type":"struct","fields":[]}),
        json!({"type":"default","valueColumn":{"table":"T","column":"Json"}}),
    ] {
        let mut invalid = valid.clone();
        if replacement.is_null() {
            invalid["properties"][0]
                .as_object_mut()
                .unwrap()
                .remove("backingConfiguration");
        } else {
            invalid["properties"][0]["backingConfiguration"] = replacement;
        }
        assert!(client.create_request(&entity(invalid)).is_err());
    }
    let invalid = json!({"name":"Entity","properties":[{"name":"P","dataType":"complex",
        "complexDataType":{"kind":"timeSeries","elementType":logical},
        "backingConfiguration":{"type":"timeSeries","orderingColumn":{"table":"T","column":"Time"},"valueColumn":{"table":"T","column":"Json"}}}]});
    assert!(client.create_request(&entity(invalid)).is_err());
}

#[tokio::test]
async fn partial_paging_keeps_evidence_and_never_follows_continuation_urls() {
    let executor = Fixture::new(vec![
        response(
            200,
            json!({"value":[{"name":"A"}],"continuationToken":"a+/=&","continuationUri":"https://untrusted.invalid/never-follow"}),
        ),
        response(
            503,
            json!({"code":"Unavailable","message":"provider-secret"}),
        ),
    ]);
    let result = client(&executor)
        .list::<EntityType>(&ListOptions::new(5, 100, 65536).unwrap())
        .await
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.responses.len(), 2);
    let ListStop::Failed(error) = result.stop else {
        panic!("must be explicitly partial");
    };
    assert_eq!(error.response.unwrap().status, 503);
    let calls = executor.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[1]
            .route()
            .as_str()
            .ends_with("/entityTypes?continuationToken=a%2B%2F%3D%26")
    );
    assert!(!calls[1].route().as_str().contains("untrusted"));
}

#[tokio::test]
async fn repeated_token_stops_before_retaining_duplicate_page_items() {
    let page = response(
        200,
        json!({"value":[{"name":"A"}],"continuationToken":"repeat"}),
    );
    let executor = Fixture::new(vec![page.clone(), page]);
    let result = client(&executor)
        .list::<EntityType>(&ListOptions::new(5, 100, 65536).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        result.stop,
        ListStop::Failed(WorkloadError {
            kind: WorkloadErrorKind::Protocol("repeated continuation token"),
            ..
        })
    ));
    assert_eq!(result.items.len(), 1);
    assert_eq!(executor.calls().len(), 2);
}

#[tokio::test]
async fn page_item_and_aggregate_byte_bounds_produce_explicit_partial_results() {
    let page = response(
        200,
        json!({"value":[{"name":"A"}],"continuationToken":"next"}),
    );
    for options in [
        ListOptions::new(1, 100, 65536).unwrap(),
        ListOptions::new(5, 1, 65536).unwrap(),
        ListOptions::new(5, 100, page.body.len()).unwrap(),
    ] {
        let executor = Fixture::new(vec![page.clone()]);
        let result = client(&executor)
            .list::<EntityType>(&options)
            .await
            .unwrap();
        assert!(matches!(result.stop, ListStop::Limit));
        assert_eq!(result.continuation_token.as_deref(), Some("next"));
        assert_eq!(executor.calls().len(), 1);
    }
    let executor = Fixture::new(vec![response(
        200,
        json!({"value":[{"name":"A"},{"name":"B"}]}),
    )]);
    let result = client(&executor)
        .list::<EntityType>(&ListOptions::new(3, 1, 65536).unwrap())
        .await
        .unwrap();
    assert!(matches!(result.stop, ListStop::Limit));
    assert!(result.items.is_empty());
    assert_eq!(result.responses.len(), 1);
}

#[tokio::test]
async fn valid_empty_is_complete_but_missing_value_or_token_only_uri_is_not() {
    let executor = Fixture::new(vec![response(200, json!({"value":[]}))]);
    let result = client(&executor)
        .list::<EntityType>(&ListOptions::new(1, 1, 1000).unwrap())
        .await
        .unwrap();
    assert!(matches!(result.stop, ListStop::Complete));
    assert!(result.items.is_empty());
    for body in [
        json!({}),
        json!({"value":null}),
        json!({"value":[],"continuationUri":"https://foreign.invalid"}),
        json!({"value":[],"continuationToken":7}),
        json!({"value":[],"continuationToken":""}),
    ] {
        let executor = Fixture::new(vec![response(200, body)]);
        assert!(matches!(
            client(&executor)
                .list::<EntityType>(&ListOptions::new(2, 1, 1000).unwrap())
                .await
                .unwrap()
                .stop,
            ListStop::Failed(_)
        ));
        assert_eq!(executor.calls().len(), 1);
    }
}

#[test]
fn rule_filters_are_repeated_exact_names_and_refresh_is_endpoint_specific() {
    let fixture = Fixture::default();
    let client = client(&fixture);
    let request = client
        .page_request::<Rule>(None, false, &[name("A,B"), name("plant#C")])
        .unwrap();
    assert!(
        request
            .route()
            .as_str()
            .ends_with("/rules?entity=A%2CB&entity=plant%23C")
    );
    assert!(
        client
            .page_request::<EntityType>(None, false, &[name("A")])
            .is_err()
    );
    assert!(client.page_request::<EntityType>(None, true, &[]).is_err());
    assert!(
        client
            .page_request::<DataSource>(None, true, &[])
            .unwrap()
            .route()
            .as_str()
            .ends_with("refreshBindingResolution=true")
    );
}

#[tokio::test]
async fn bindings_read_unbind_clear_and_definition_reset_are_distinct_from_deletion() {
    let executor = Fixture::new(vec![
        response(200, json!({"value":[]})),
        response(200, json!({"name":"T","lineageTag":"table-tag"})),
        response(200, json!({"value":[]})),
        empty(),
        empty(),
        empty(),
    ]);
    let client = client(&executor);
    let entity = LineageTag::new("entity-lineage").unwrap();
    let table = name("T");
    let Outcome::Completed { value: page, .. } = client
        .backing_data_sources(&entity, None, false)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(page.value.is_empty());
    client
        .backing_data_source(&entity, &table, true)
        .await
        .unwrap();
    client
        .backing_expressions(&entity, &table, None, false)
        .await
        .unwrap();
    client.unbind_data_source(&entity, &table).await.unwrap();
    client.clear_data_source_bindings(&entity).await.unwrap();
    client.reset_definition().await.unwrap();
    let calls = executor.calls();
    assert!(
        calls[3]
            .route()
            .as_str()
            .ends_with("/entityTypes/entity-lineage/dataSources/T")
    );
    assert!(
        calls[4]
            .route()
            .as_str()
            .ends_with("/entityTypes/entity-lineage/dataSources")
    );
    assert!(calls[5].route().as_str().ends_with("/definition"));
    assert!(
        calls[3..]
            .iter()
            .all(|r| r.method() == WorkloadMethod::Delete)
    );
    assert!(calls.iter().all(|r| !r.route().as_str().ends_with(ITEM)));
}

#[tokio::test]
async fn definition_read_is_internal_and_expansion_is_fail_closed() {
    let executor = Fixture::new(vec![response(
        200,
        json!({"name":"Server name","description":null,"model":{"entities":[{"name":"E","lineageTag":"server"}],"future":[]}}),
    )]);
    let client = client(&executor);
    let Outcome::Completed { value, .. } = client.definition(false, false).await.unwrap() else {
        panic!()
    };
    assert_eq!(value.raw["model"]["entities"][0]["lineageTag"], "server");
    assert_eq!(value.fields.model.entities.unwrap()[0].name, "E");
    let gated = OntologyClient::new(
        target(Features {
            auto_projection: false,
            ..features()
        }),
        &executor,
        ByteLimits::new(1000, 1000).unwrap(),
    )
    .unwrap();
    assert!(gated.definition_request(true, false).is_err());
    assert!(
        gated
            .definition_request(false, true)
            .unwrap()
            .route()
            .as_str()
            .ends_with("definition?refreshBindingResolution=true")
    );
}

#[tokio::test]
async fn service_status_body_content_type_acceptance_and_unknown_outcomes_are_preserved() {
    for status in [400, 401, 403, 404, 409, 429, 500, 503, 302] {
        let original = response(
            status,
            json!({"code":"ServiceCode","details":[{"message":"preserve-me"}]}),
        );
        let executor = Fixture::new(vec![original.clone()]);
        let error = client(&executor)
            .create(&entity(json!({"name":"E"})))
            .await
            .unwrap_err();
        assert_eq!(error.kind, WorkloadErrorKind::Http);
        assert_eq!(error.response, Some(original));
        assert_eq!(executor.calls().len(), 1);
    }
    let mut accepted = response(202, json!({"state":"queued"}));
    accepted.headers.insert(
        "Location".into(),
        "https://do-not-follow.invalid/operation".into(),
    );
    let executor = Fixture::new(vec![accepted.clone()]);
    let Outcome::Accepted { response: actual } = client(&executor)
        .create(&entity(json!({"name":"E"})))
        .await
        .unwrap()
    else {
        panic!("202 is not completion");
    };
    assert_eq!(actual, accepted);
    assert_eq!(executor.calls().len(), 1);
    let executor = Fixture::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(Err(WorkloadError::execution(
            DispatchState::Unknown,
            "secret transport diagnostic".into(),
        )));
    let error = client(&executor)
        .create(&entity(json!({"name":"E"})))
        .await
        .unwrap_err();
    assert_eq!(error.dispatch, DispatchState::Unknown);
    assert_eq!(executor.calls().len(), 1);
    assert!(!format!("{error:?} {error}").contains("secret transport diagnostic"));
}

#[tokio::test]
async fn malformed_success_is_not_a_success_stub_and_is_never_replayed() {
    for original in [
        WorkloadResponse {
            status: 200,
            headers: BTreeMap::from([("Content-Type".into(), "text/plain".into())]),
            body: b"{}".to_vec(),
        },
        WorkloadResponse {
            status: 200,
            headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
            body: b"not json".to_vec(),
        },
        response(200, json!({})),
        response(201, json!({"name":"E"})),
    ] {
        let executor = Fixture::new(vec![original.clone()]);
        let error = client(&executor)
            .get::<EntityType>(&lineage("tag"), false)
            .await
            .unwrap_err();
        assert!(matches!(error.kind, WorkloadErrorKind::Protocol(_)));
        assert_eq!(error.response, Some(original));
        assert_eq!(executor.calls().len(), 1);
    }
    let executor = Fixture::new(vec![response(200, json!({"name":"E"}))]);
    let error = client(&executor)
        .create(&entity(json!({"name":"E"})))
        .await
        .unwrap_err();
    assert_eq!(error.dispatch, DispatchState::Unknown);
}

#[tokio::test]
async fn request_byte_boundary_and_streaming_response_bound_are_enforced() {
    let entity = entity(json!({"name":"E"}));
    let body_len = serde_json::to_vec(&entity).unwrap().len();
    let executor = Fixture::default();
    for (bound, valid) in [(body_len, true), (body_len - 1, false)] {
        let client = OntologyClient::new(
            target(features()),
            &executor,
            ByteLimits::new(bound, 100).unwrap(),
        )
        .unwrap();
        assert_eq!(client.create_request(&entity).is_ok(), valid);
    }
    assert!(executor.calls().is_empty());
    let executor = Fixture::new(vec![response(
        200,
        json!({"name":"E","padding":"oversize"}),
    )]);
    let client = OntologyClient::new(
        target(features()),
        &executor,
        ByteLimits::new(100, 10).unwrap(),
    )
    .unwrap();
    let error = client
        .get::<EntityType>(&lineage("tag"), false)
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkloadErrorKind::Limit);
    assert_eq!(error.response.unwrap().body.len(), 10);
    assert_eq!(executor.calls()[0].max_response_bytes(), 10);
}

#[tokio::test]
async fn response_duplicate_headers_are_rejected_without_leaking_body() {
    let mut original = response(200, json!({"name":"provider-private-value"}));
    original
        .headers
        .insert("content-type".into(), "text/plain".into());
    let executor = Fixture::new(vec![original.clone()]);
    let error = client(&executor)
        .get::<EntityType>(&lineage("tag"), false)
        .await
        .unwrap_err();
    assert!(matches!(error.kind, WorkloadErrorKind::Protocol(_)));
    assert_eq!(error.response, Some(original));
    assert!(!format!("{error:?}").contains("provider-private-value"));
}

fn script_cases() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "expression",
            json!({"name":"X","expression":"let x = 1 in x"}),
            json!({}),
        ),
        ("table", json!({"name":"T"}), json!({})),
        ("entity", json!({"name":"E"}), json!({})),
        (
            "entityRelationship",
            json!({"name":"ER","fromEntity":"E","toEntity":"F"}),
            json!({}),
        ),
        (
            "relationship",
            json!({"type":"singleColumn","name":"R","fromTable":"T","toTable":"U"}),
            json!({}),
        ),
        (
            "rule",
            json!({"name":"Rule","statement":"A rule"}),
            json!({}),
        ),
        ("namespace", json!({"name":"plant"}), json!({})),
        ("metric", json!({"type":"explicit","name":"M"}), json!({})),
        ("reusableProperty", json!({"name":"Reusable"}), json!({})),
        (
            "column",
            json!({"name":"Id","lineageTag":"keep-column","dataType":"int64"}),
            json!({"table":"T"}),
        ),
        (
            "partition",
            json!({"name":"Partition","source":{"type":"m","expressionSource":"X"}}),
            json!({"table":"T"}),
        ),
        (
            "measure",
            json!({"name":"Measure","expression":"EXTERNALMEASURE()","lineageTag":"keep-measure"}),
            json!({"table":"T"}),
        ),
        (
            "property",
            json!({"name":"Id","dataType":"int64","lineageTag":"keep-property"}),
            json!({"entity":"E"}),
        ),
        (
            "additionalBackingTable",
            json!({"table":"U","relationship":"R"}),
            json!({"entity":"E"}),
        ),
        (
            "referencedEntity",
            json!({"entity":"E","propertyScope":"none"}),
            json!({"rule":"Rule"}),
        ),
        (
            "referencedRelationship",
            json!({"entityRelationship":"ER"}),
            json!({"rule":"Rule"}),
        ),
    ]
}

#[test]
fn scripts_generate_exact_native_verbs_and_all_root_child_reference_shapes() {
    let fixture = Fixture::default();
    let client = client(&fixture);
    for (kind, body, parent) in script_cases() {
        let create = json!({"create":{"parentObject":parent,(kind):body}});
        let request = client
            .script_request(&ScriptRequest::from_value(create.clone()).unwrap(), false)
            .unwrap();
        assert_eq!(wire(&request), create);
        assert_eq!(request.content_type(), Some("application/json"));
        assert!(request.route().as_str().ends_with("/definition/scripts"));
        let identity_key = match kind {
            "additionalBackingTable" => "table",
            "referencedEntity" => "entity",
            "referencedRelationship" => "entityRelationship",
            _ => "name",
        };
        let mut reference = parent.clone();
        reference[kind] = body[identity_key].clone();
        for verb in ["createOrReplace", "alter", "delete"] {
            let script = if verb == "delete" {
                json!({verb:{"object":reference}})
            } else {
                json!({verb:{"object":reference,(kind):if verb=="alter" {json!({})} else {body.clone()}}})
            };
            if kind == "referencedRelationship" && verb == "alter" {
                assert!(ScriptRequest::from_value(script).is_err());
            } else {
                let request = client
                    .script_request(&ScriptRequest::from_value(script.clone()).unwrap(), false)
                    .unwrap();
                assert_eq!(wire(&request), script);
            }
        }
    }
    assert!(fixture.calls().is_empty());
}

#[test]
fn typed_script_builders_pair_parent_and_reference_types() {
    let entity = entity(json!({"name":"E"}));
    let root = ScriptReference::<EntityType>::root(name("E")).unwrap();
    let create = ScriptLeaf::create(ScriptParent::<EntityType>::root(), &entity).unwrap();
    let replace = ScriptLeaf::create_or_replace(&root, &entity).unwrap();
    let alter = ScriptLeaf::alter(
        &root,
        &MergePatch::<EntityType>::new(json!({"description":null})).unwrap(),
    )
    .unwrap();
    let delete = ScriptLeaf::delete(&root).unwrap();
    let sequence = ScriptRequest::sequence(vec![create, replace, alter, delete]).unwrap();
    assert_eq!(
        sequence.value()["sequence"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let property: EntityProperty =
        serde_json::from_value(json!({"name":"Id","dataType":"int64"})).unwrap();
    let create = ScriptLeaf::create(
        ScriptParent::<EntityProperty>::named(name("E")).unwrap(),
        &property,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(create).unwrap()["create"]["parentObject"],
        json!({"entity":"E"})
    );
    let child = ScriptReference::<EntityProperty>::child(name("E"), name("Id")).unwrap();
    assert_eq!(
        serde_json::to_value(ScriptLeaf::delete(&child).unwrap()).unwrap(),
        json!({"delete":{"object":{"entity":"E","property":"Id"}}})
    );
}

#[test]
fn strict_scripts_reject_bad_verbs_kinds_pairings_graph_writes_and_nested_sequences() {
    for invalid in [
        json!({}),
        json!({"create":{},"delete":{}}),
        json!({"sequence":{"operations":[]}}),
        json!({"sequence":{"operations":[{"sequence":{"operations":[]}}]}}),
        json!({"create":{"dataSource":{"name":"T"}}}),
        json!({"create":{"parentObject":{"entity":"E"},"column":{"name":"Id"}}}),
        json!({"create":{"parentObject":{"table":"T"},"entity":{"name":"E"}}}),
        json!({"alter":{"object":{"expression":"X"},"entity":{"description":"wrong-ref"}}}),
        json!({"alter":{"object":{"entity":"E","lineageTag":"tag"},"entity":{"description":"wrong-identity"}}}),
        json!({"create":{"entity":{"name":"E","lineageTag":"server-only"}}}),
        json!({"create":{"entity":{"name":"E","arbitraryGraph":{"nodes":[]}}}}),
        json!({"create":{"entity":{"name":"E","properties":[{"name":"Id"}]}}}),
        json!({"delete":{"object":{"entity":"E"},"entity":{}}}),
        json!({"delete":{"object":{"table":"T","property":"P"}}}),
        json!({"delete":{"object":{"entity":"E","property":"P","extra":"bad"}}}),
        json!({"alter":{"object":{"rule":"Rule","referencedRelationship":"ER"},"referencedRelationship":{"entityRelationship":"New"}}}),
    ] {
        assert!(
            ScriptRequest::from_value(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    let operation = ScriptLeaf::from_value(json!({"delete":{"object":{"entity":"E"}}})).unwrap();
    assert!(ScriptRequest::sequence(vec![operation.clone(); 1000]).is_ok());
    assert!(ScriptRequest::sequence(vec![operation; 1001]).is_err());
}

#[test]
fn script_features_cannot_bypass_reusable_or_inheritance_gates_including_delete() {
    let fixture = Fixture::default();
    let client = OntologyClient::new(
        target(Features {
            reusable_properties: false,
            inheritance: false,
            ..features()
        }),
        &fixture,
        ByteLimits::new(10000, 10000).unwrap(),
    )
    .unwrap();
    for script in [
        json!({"create":{"reusableProperty":{"name":"P"}}}),
        json!({"alter":{"object":{"reusableProperty":"P"},"reusableProperty":{"description":null}}}),
        json!({"delete":{"object":{"reusableProperty":"P"}}}),
        json!({"create":{"entity":{"name":"Derived","baseEntityType":"Base"}}}),
        json!({"alter":{"object":{"entity":"Derived"},"entity":{"overriddenMetadataFields":null}}}),
        json!({"create":{"parentObject":{"entity":"E"},"property":{"name":"P","dataType":"int64","overriddenMetadataFields":[]}}}),
    ] {
        assert!(
            client
                .script_request(&ScriptRequest::from_value(script).unwrap(), false)
                .is_err()
        );
    }
    let delete =
        ScriptRequest::from_value(json!({"delete":{"object":{"entity":"Derived"}}})).unwrap();
    assert!(client.script_request(&delete, false).is_ok());
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn transactional_sequence_is_one_submission_preserves_failure_and_does_not_replay() {
    let script = ScriptRequest::from_value(json!({"sequence":{"operations":[
        {"create":{"entity":{"name":"A"}}},{"create":{"entity":{"name":"A"}}}
    ]}}))
    .unwrap();
    let original = response(
        409,
        json!({"code":"Conflict","message":"operation 1 already exists; transaction aborted"}),
    );
    let fixture = Fixture::new(vec![original.clone()]);
    let error = client(&fixture)
        .execute_script(&script, true)
        .await
        .unwrap_err();
    assert_eq!(error.response, Some(original));
    let calls = fixture.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(wire(&calls[0]), script.value().clone());
    assert!(
        calls[0]
            .route()
            .as_str()
            .ends_with("/definition/scripts?deferValidation=true")
    );
    assert_eq!(calls[0].if_match(), None);
}

#[tokio::test]
async fn script_result_count_order_delete_and_rename_targets_follow_source_mapper() {
    let script = ScriptRequest::from_value(
        json!({"alter":{"object":{"entity":"Before"},"entity":{"name":"After"}}}),
    )
    .unwrap();
    let fixture = Fixture::new(vec![response(
        200,
        json!({"results":[{
            "opIndex":0,"verb":"alter","target":{"entity":"Before"},"object":{"name":"After","lineageTag":"stable","future":null}
        }]}),
    )]);
    let Outcome::Completed { value, .. } = client(&fixture)
        .execute_script(&script, false)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(value.results[0].target.name(), "Before");
    assert_eq!(value.results[0].object.as_ref().unwrap()["name"], "After");
    for body in [
        json!({"results":[]}),
        json!({"results":[{"opIndex":1,"verb":"alter","target":{"entity":"Before"},"object":{"name":"After"}}]}),
        json!({"results":[{"opIndex":0,"verb":"alter","target":{"entity":"After"},"object":{"name":"After"}}]}),
        json!({"results":[{"opIndex":0,"verb":"alter","target":{"entity":"Before"},"deleted":true}]}),
    ] {
        let fixture = Fixture::new(vec![response(200, body)]);
        let error = client(&fixture)
            .execute_script(&script, false)
            .await
            .unwrap_err();
        assert_eq!(error.dispatch, DispatchState::Unknown);
        assert_eq!(fixture.calls().len(), 1);
    }
    let delete = ScriptRequest::from_value(json!({"delete":{"object":{"entity":"E"}}})).unwrap();
    let fixture = Fixture::new(vec![response(
        200,
        json!({"results":[{"opIndex":0,"verb":"delete","target":{"entity":"E"},"deleted":true}]}),
    )]);
    client(&fixture)
        .execute_script(&delete, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn committed_script_can_return_null_or_omitted_final_state_object() {
    let script = ScriptRequest::from_value(json!({"sequence":{"operations":[
        {"create":{"entity":{"name":"E"}}},{"delete":{"object":{"entity":"E"}}}
    ]}}))
    .unwrap();
    for object in [Some(Value::Null), None] {
        let mut first = json!({"opIndex":0,"verb":"create","target":{"entity":"E"},"deleted":null});
        if let Some(object) = object.clone() {
            first["object"] = object;
        }
        let fixture = Fixture::new(vec![response(
            200,
            json!({"results":[first,
                {"opIndex":1,"verb":"delete","target":{"entity":"E"},"object":null,"deleted":true}
            ]}),
        )]);
        let Outcome::Completed { value, .. } = client(&fixture)
            .execute_script(&script, false)
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(value.results[0].object, object);
        assert!(value.results[0].typed_object.is_none());
        assert_eq!(fixture.calls().len(), 1);
    }
}

#[test]
fn table_measure_rename_effect_is_explicit_and_not_a_root_metric_operation() {
    let script = ScriptRequest::from_value(
        json!({"alter":{"object":{"table":"T","measure":"Before"},"measure":{"name":"After"}}}),
    )
    .unwrap();
    assert!(script.may_remove_authored_metric_metadata());
    assert!(script.value()["alter"]["object"].get("metric").is_none());
}

#[test]
fn rdf_only_advertises_actual_formats_and_safe_import_gap() {
    for bad in ["JsonLd", "JSON-LD", "NTriples", ".owl", ""] {
        assert!(RdfFormat::parse(bad).is_err());
    }
    assert!(serde_json::from_value::<RdfFormat>(json!("JsonLd")).is_err());
    assert_eq!(
        RdfFormat::from_extension(".owl").unwrap(),
        RdfFormat::RdfXml
    );
    assert!(RdfFormat::from_extension(".jsonld").is_err());
    for bad in [
        "relative",
        "file:///tmp/private",
        "https://",
        "urn:",
        "https://example.invalid/\n",
    ] {
        assert!(BaseIri::new(bad).is_err());
    }
    for good in [
        "https://example.invalid/ns#",
        "http://example.invalid/ns/",
        "urn:plant:machine",
        "urn:plant",
    ] {
        assert!(BaseIri::new(good).is_ok());
    }
    let fixture = Fixture::default();
    let error = client(&fixture).rdf_import_capability().unwrap_err();
    assert!(matches!(error.kind, WorkloadErrorKind::CapabilityGap(_)));
    assert_eq!(error.dispatch, DispatchState::NotSent);
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn rdf_export_generates_private_request_preserves_binary_and_reports_source_losses() {
    for format in [RdfFormat::Turtle, RdfFormat::RdfXml] {
        let bytes = vec![0, 255, 1, 2];
        let fixture = Fixture::new(vec![WorkloadResponse {
            status: 200,
            headers: BTreeMap::from([
                ("Content-Type".into(), format.content_type().into()),
                (
                    "Content-Disposition".into(),
                    "attachment; filename=\"untrusted.owl\"".into(),
                ),
            ]),
            body: bytes.clone(),
        }]);
        let export = RdfExportRequest {
            format,
            base_iri: Some(BaseIri::new("urn:plant:machine").unwrap()),
        };
        let Outcome::Completed { value, response } =
            client(&fixture).export_rdf(&export).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(value.bytes, bytes);
        assert_eq!(response.body, bytes);
        assert_eq!(
            value.fidelity,
            RdfFidelity::LossyProjectionWithoutCompleteDiagnostics
        );
        let calls = fixture.calls();
        assert!(calls[0].route().as_str().ends_with("/rdf/export"));
        assert_eq!(calls[0].method(), WorkloadMethod::Post);
        assert_eq!(
            wire(&calls[0]),
            json!({"format":format,"baseIri":"urn:plant:machine"})
        );
        assert_eq!(calls[0].max_response_bytes(), MAX_EXPORT_BYTES);
    }
}

#[tokio::test]
async fn rdf_bounds_exact_boundary_media_type_and_accepted_results_are_not_faked() {
    let export = RdfExportRequest {
        format: RdfFormat::Turtle,
        base_iri: None,
    };
    for size in [0, MAX_EXPORT_BYTES, MAX_EXPORT_BYTES + 1] {
        let fixture = Fixture::new(vec![WorkloadResponse {
            status: 200,
            headers: BTreeMap::from([("Content-Type".into(), "text/turtle".into())]),
            body: vec![b'a'; size],
        }]);
        let result = client(&fixture).export_rdf(&export).await;
        if size <= MAX_EXPORT_BYTES {
            assert!(matches!(result, Ok(Outcome::Completed { .. })));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, WorkloadErrorKind::Limit);
            assert_eq!(error.response.unwrap().body.len(), MAX_EXPORT_BYTES);
        }
        assert_eq!(fixture.calls().len(), 1);
    }
    let fixture = Fixture::new(vec![response(200, json!({"not":"turtle"}))]);
    assert!(client(&fixture).export_rdf(&export).await.is_err());
    let fixture = Fixture::new(vec![response(202, json!({"state":"accepted"}))]);
    assert!(matches!(
        client(&fixture).export_rdf(&export).await.unwrap(),
        Outcome::Accepted { .. }
    ));
    assert_eq!(fixture.calls().len(), 1);
}
