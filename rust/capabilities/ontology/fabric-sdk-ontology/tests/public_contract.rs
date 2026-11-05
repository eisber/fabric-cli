// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public wire, identity, generation, source fidelity and operation-evidence tests.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk_items::{
    ItemClient, ItemExecutionError, ItemExecutor, ItemGetOptions, ItemRequest, ItemResponse,
};
use fabric_sdk_ontology::{
    DefinitionLimits, InputError, ModelForm, ModelReplacement, Observation, ObservedGeneration,
    OntologyTarget, PublicErrorKind, PublicOutcome, get_definition, get_metadata, get_model,
    replace_model,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OPERATION: &str = "99999999-8888-7777-6666-555555555555";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken {
            token: "offline-ontology-fixture".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

#[derive(Clone, Debug)]
struct Seen {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    waits: Arc<AtomicUsize>,
}

impl Executor {
    fn new(responses: Vec<OperationResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            ..Self::default()
        }
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.seen.lock().unwrap().push(Seen {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().into(),
            });
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected fixture response"))
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Ok(())))
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            self.seen.lock().unwrap().push(Seen {
                method: ResourceMethod::Get,
                url: request.url().to_string(),
                body: vec![],
            });
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected item fixture");
            Ok(ItemResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            })
        })
    }
}

fn context(credential: Arc<Credential>, audience: Audience) -> RequestContext {
    RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    )
}

fn target() -> OntologyTarget {
    OntologyTarget::new(WORKSPACE, ITEM).unwrap()
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn definition_limits(bytes: usize) -> DefinitionLimits {
    DefinitionLimits {
        max_parts: NonZeroUsize::new(4).unwrap(),
        max_decoded_bytes: NonZeroUsize::new(bytes).unwrap(),
    }
}

#[tokio::test]
async fn metadata_reuses_typed_item_client_and_preserves_generation_and_unknown_fields() {
    for generation in [
        None,
        Some(json!(null)),
        Some(json!(1)),
        Some(json!(2)),
        Some(json!(7)),
    ] {
        let credential = Arc::new(Credential::default());
        let context = context(credential.clone(), Audience::Fabric);
        let client = ItemClient::with_context(context);
        let mut properties = json!({"future":{"kept":true}});
        if let Some(generation) = &generation {
            properties["generation"] = generation.clone();
        }
        let raw = json!({"id":ITEM,"displayName":"Ontology","type":"Ontology","workspaceId":WORKSPACE,"properties":properties,"extra":null});
        let executor = Executor::new(vec![response(200, raw.clone())]);
        let result = get_metadata(
            &client,
            &executor,
            &target(),
            ItemGetOptions::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.metadata().as_value(), &raw);
        let expected = generation
            .and_then(|value| value.as_u64())
            .map(ObservedGeneration::Known)
            .unwrap_or(ObservedGeneration::Unknown);
        assert_eq!(result.generation(), expected);
        let seen = executor.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].url,
            format!(
                "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/ontologies/{ITEM}"
            )
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn malformed_generation_wrong_owner_and_identity_never_become_source_fallback() {
    for raw in [
        json!({"id":ITEM,"displayName":"Model","type":"SemanticModel"}),
        json!({"id":WORKSPACE,"displayName":"Wrong","type":"Ontology"}),
        json!({"id":ITEM,"displayName":"Bad","type":"Ontology","properties":{"generation":"2"}}),
    ] {
        let context = context(Arc::new(Credential::default()), Audience::Fabric);
        let executor = Executor::new(vec![response(200, raw)]);
        let client = ItemClient::with_context(context);
        let error = get_metadata(
            &client,
            &executor,
            &target(),
            ItemGetOptions::default(),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error.kind, PublicErrorKind::InvalidMetadata(_)));
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn public_model_forms_do_not_use_private_alias_or_normalize_future_source_fields() {
    for (form, query) in [
        (ModelForm::ProviderDefault, ""),
        (ModelForm::Minimized, "?form=Minimized"),
        (ModelForm::Expanded, "?form=Expanded"),
    ] {
        let context = context(Arc::new(Credential::default()), Audience::Fabric);
        let raw = json!({"name":"Ontology","model":{"entities":[],"futureExtension":{"kept":true}},"future":null});
        let executor = Executor::new(vec![response(200, raw.clone())]);
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let PublicOutcome::Completed { data, response, .. } =
            get_model(&mut session, &target(), form, Observation::ReturnAccepted)
                .await
                .unwrap()
        else {
            panic!("expected completion")
        };
        assert_eq!(data.as_value(), &raw);
        assert_eq!(
            serde_json::from_slice::<Value>(&response.body).unwrap(),
            raw
        );
        let seen = executor.seen.lock().unwrap();
        assert_eq!(seen[0].method, ResourceMethod::Get);
        assert!(seen[0].body.is_empty());
        assert_eq!(
            seen[0].url,
            format!(
                "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/ontologies/{ITEM}/model{query}"
            )
        );
    }
}

#[tokio::test]
async fn denied_definition_retains_provider_error_and_never_fetches_metadata() {
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let executor = Executor::new(vec![response(
        403,
        json!({"errorCode":"Denied","message":"provider source"}),
    )]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = get_definition(
        &mut session,
        &target(),
        definition_limits(32),
        Observation::ReturnAccepted,
    )
    .await
    .unwrap_err();
    assert!(matches!(error.kind, PublicErrorKind::Operation(_)));
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
    assert!(!format!("{error:?}").contains("provider source"));
}

#[tokio::test]
async fn definition_preserves_native_paths_payload_order_formats_and_future_types() {
    let raw = json!({"definition":{"format":"TMDL","parts":[
        {"path":"entities/sales#Sensor.tmdl","payload":"e30=","payloadType":"InlineBase64","future":1},
        {"path":".platform","payload":"e30=","payloadType":"InlineBase64"},
        {"path":"future.part","payload":"opaque-native-source","payloadType":"FutureNativeType"}
    ]}});
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let executor = Executor::new(vec![response(200, raw.clone())]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let PublicOutcome::Completed { data, .. } = get_definition(
        &mut session,
        &target(),
        definition_limits(4),
        Observation::ReturnAccepted,
    )
    .await
    .unwrap() else {
        panic!("expected completion")
    };
    assert_eq!(data.as_value(), &raw);
    assert_eq!(data.parts()[0]["path"], "entities/sales#Sensor.tmdl");
    assert_eq!(
        executor.seen.lock().unwrap()[0].method,
        ResourceMethod::Post
    );
}

#[tokio::test]
async fn malformed_duplicate_and_oversize_parts_fail_with_original_response_evidence() {
    for (parts, budget) in [
        (
            json!([{"path":"p","payload":"bad!","payloadType":"InlineBase64"}]),
            32,
        ),
        (
            json!([{"path":"p","payload":"e30=","payloadType":"InlineBase64"},{"path":"p","payload":"e30=","payloadType":"InlineBase64"}]),
            32,
        ),
        (
            json!([{"path":"p","payload":"e30=","payloadType":"InlineBase64"}]),
            1,
        ),
    ] {
        let raw = json!({"definition":{"parts":parts}});
        let context = context(Arc::new(Credential::default()), Audience::Fabric);
        let executor = Executor::new(vec![response(200, raw.clone())]);
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = get_definition(
            &mut session,
            &target(),
            definition_limits(budget),
            Observation::ReturnAccepted,
        )
        .await
        .unwrap_err();
        let PublicErrorKind::InvalidResponse { response, .. } = error.kind else {
            panic!("expected retained response")
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&response.body).unwrap(),
            raw
        );
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_lro_result_retains_the_original_operation_id_and_response() {
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let mut done = response(200, json!({"status":"Succeeded"}));
    done.headers.insert(
        "Location".into(),
        format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}/result"),
    );
    let invalid = json!({"definition":"not-a-definition"});
    let executor = Executor::new(vec![accepted(), done, response(200, invalid.clone())]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = get_definition(
        &mut session,
        &target(),
        definition_limits(32),
        Observation::WaitForResult,
    )
    .await
    .unwrap_err();
    let PublicErrorKind::InvalidResponse {
        response, evidence, ..
    } = error.kind
    else {
        panic!("expected response evidence")
    };
    assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
    assert!(evidence.mutation_may_have_occurred);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap(),
        invalid
    );
    assert_eq!(
        executor
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == ResourceMethod::Post)
            .count(),
        1
    );
}

fn accepted() -> OperationResponse {
    OperationResponse {
        status: 202,
        body: vec![],
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OPERATION.into()),
            (
                "Location".into(),
                format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
            ("Retry-After".into(), "1".into()),
        ]),
    }
}

#[tokio::test]
async fn acceptance_retains_original_scope_and_handle_without_polling_or_resubmission() {
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let executor = Executor::new(vec![accepted()]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let PublicOutcome::Accepted {
        target: original,
        operation_id,
        response,
    } = get_definition(
        &mut session,
        &target(),
        definition_limits(4),
        Observation::ReturnAccepted,
    )
    .await
    .unwrap()
    else {
        panic!("acceptance is not completion")
    };
    assert_eq!(original, target());
    assert_eq!(operation_id, OPERATION);
    assert_eq!(response.status, 202);
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
    assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_observation_reuses_operation_gets_not_another_definition_post() {
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let mut done = response(200, json!({"status":"Succeeded"}));
    done.headers.insert(
        "Location".into(),
        format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}/result"),
    );
    let executor = Executor::new(vec![
        accepted(),
        done,
        response(200, json!({"definition":{"parts":[]}})),
    ]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    assert!(matches!(
        get_definition(
            &mut session,
            &target(),
            definition_limits(4),
            Observation::WaitForResult
        )
        .await
        .unwrap(),
        PublicOutcome::Completed { .. }
    ));
    let seen = executor.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|request| request.method == ResourceMethod::Post)
            .count(),
        1
    );
    assert!(
        seen.iter()
            .skip(1)
            .all(|request| request.method == ResourceMethod::Get)
    );
}

#[tokio::test]
async fn caller_cancellation_and_wrong_audience_do_not_discover_another_identity() {
    for audience in [Audience::Fabric, Audience::OneLake] {
        let credential = Arc::new(Credential::default());
        let context = context(credential.clone(), audience);
        let executor = Executor::default();
        if audience == Audience::Fabric {
            let mut session = OperationSession::with_context(
                &context,
                executor.clone(),
                OperationLimits::default(),
                ready(()),
            );
            assert!(
                get_model(
                    &mut session,
                    &target(),
                    ModelForm::Minimized,
                    Observation::ReturnAccepted
                )
                .await
                .is_err()
            );
        } else {
            let mut session = OperationSession::with_context(
                &context,
                executor.clone(),
                OperationLimits::default(),
                pending(),
            );
            assert!(
                get_model(
                    &mut session,
                    &target(),
                    ModelForm::Minimized,
                    Observation::ReturnAccepted
                )
                .await
                .is_err()
            );
        }
        assert!(executor.seen.lock().unwrap().is_empty());
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn model_replacement_has_native_envelope_and_no_implicit_creation_or_replay() {
    let replacement =
        ModelReplacement::new(json!({"entities":[]}), Some("authored description".into())).unwrap();
    let expected = json!({"description":"authored description","model":{"entities":[]}});
    assert_eq!(
        serde_json::from_slice::<Value>(replacement.body()).unwrap(),
        expected
    );
    let context = context(Arc::new(Credential::default()), Audience::Fabric);
    let executor = Executor::new(vec![response(409, json!({"errorCode":"Conflict"}))]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = replace_model(
        &mut session,
        &target(),
        &replacement,
        Observation::ReturnAccepted,
    )
    .await
    .unwrap_err();
    assert!(matches!(error.kind, PublicErrorKind::Operation(_)));
    let seen = executor.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&seen[0].body).unwrap(),
        expected
    );
    assert!(seen[0].url.ends_with("/model"));
}

#[test]
fn invalid_identifiers_internal_model_envelopes_and_over_budget_input_fail_locally() {
    assert_eq!(
        OntologyTarget::new("../workspace", ITEM).unwrap_err(),
        InputError::InvalidIdentity
    );
    for model in [
        json!(null),
        json!({"model":{}}),
        json!({"compatibilityLevel":1605}),
        json!({"entities":[1]}),
    ] {
        assert_eq!(
            ModelReplacement::new(model, None).unwrap_err(),
            InputError::InvalidModel
        );
    }
    assert_eq!(
        ModelReplacement::new(
            json!({}),
            Some("x".repeat(fabric_sdk_ontology::MAX_MODEL_REQUEST_BYTES + 1))
        )
        .unwrap_err(),
        InputError::RequestLimit
    );
}
