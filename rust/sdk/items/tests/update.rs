// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact offline typed metadata PATCH, approval and uncertainty contracts.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_items::update::{
    self, PreparedUpdate, UpdateError, UpdateMode, UpdateOptions,
    contract::{
        ApprovalPolicy, Coverage, ItemIdentity, Outcome, Precondition, PreparedOperation, Registry,
        ResolvedTarget, Target, TargetKind, Verb,
    },
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
const OP: &str = "cccccccc-dddd-eeee-ffff-111111111111";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-offline",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn context(credential: Arc<Credential>) -> RequestContext {
    RequestContext::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    )
}

#[derive(Clone, Default)]
struct Wire {
    calls: Arc<Mutex<Vec<(ResourceMethod, String, Value)>>>,
    results: Arc<Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>>,
}
impl OperationExecutor for Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.calls.lock().unwrap().push((
            request.method(),
            request.url().to_string(),
            serde_json::from_slice(request.body()).unwrap(),
        ));
        Box::pin(std::future::ready(
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request/replay"),
        ))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("synchronous metadata update must not wait")
    }
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn metadata(owner: &str) -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":owner,"displayName":"new","description":""})
}
fn target(owner: &str, item: &str, facet: &str) -> ResolvedTarget {
    ResolvedTarget::new(
        &format!("/{WS}.Workspace/{item}.{owner}"),
        ItemIdentity::new(WS, item).unwrap(),
        Target {
            kind: TargetKind::Item,
            owner_type: owner.into(),
            facet: facet.into(),
            representation: "json".into(),
            generation: None,
        },
        None,
    )
    .unwrap()
}
fn plan(owner: &str, bytes: &[u8], credential: Arc<Credential>) -> PreparedUpdate {
    PreparedUpdate::new(
        target(owner, ITEM, "metadata"),
        bytes,
        UpdateOptions::patch(),
    )
    .unwrap()
    .with_context(context(credential), OperationLimits::default())
    .unwrap()
}

struct Policy(bool);
impl ApprovalPolicy for Policy {
    type Error = &'static str;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(operation.operation_id(), update::OPERATION_ID);
        assert_eq!(
            operation.preconditions(),
            [
                Precondition::ExistingOnly,
                Precondition::UnguardedConcurrency
            ]
        );
        if self.0 { Ok(()) } else { Err("denied") }
    }
}

#[test]
fn registration_and_plan_are_local_and_explicitly_unguarded() {
    let mut registry = Registry::default();
    update::register(&mut registry).unwrap();
    for owner in ["Notebook", "Lakehouse"] {
        let target = target(owner, ITEM, "metadata");
        let found = registry
            .resolve(Verb::Update, target.selection(), None)
            .unwrap();
        assert_eq!(found.operation.id(), update::OPERATION_ID);
        let plan =
            PreparedUpdate::new(target, br#"{"description":""}"#, UpdateOptions::patch()).unwrap();
        let document = plan.plan_document();
        assert_eq!(document["status"], "succeeded");
        assert_eq!(document["meta"]["executionMode"], "plan");
        assert_eq!(
            document["meta"]["preconditions"][1]["kind"],
            "unguardedConcurrency"
        );
        assert_eq!(document["data"]["changedFields"], json!(["description"]));
    }
    let snapshot = format!("{registry:?}");
    assert!(update::register(&mut registry).is_err());
    assert_eq!(format!("{registry:?}"), snapshot);
}

#[test]
fn strict_fields_preserve_omission_empty_string_and_reject_null_duplicates() {
    for input in [
        r#"{}"#,
        r#"null"#,
        r#"[]"#,
        r#"{"displayName":null}"#,
        r#"{"description":null}"#,
        r#"{"description":false}"#,
        r#"{"displayName":""}"#,
        r#"{"displayName":"  "}"#,
        r#"{"displayName":"a\nb"}"#,
        r#"{"id":"new"}"#,
        r#"{"properties":{}}"#,
        r#"{"definition":{}}"#,
        r#"{"description":"a","description":"b"}"#,
        r#"{"description":""} trailing"#,
    ] {
        assert!(
            matches!(
                PreparedUpdate::new(
                    target("Notebook", ITEM, "metadata"),
                    input.as_bytes(),
                    UpdateOptions::patch()
                ),
                Err(UpdateError::InvalidInput)
            ),
            "{input}"
        );
    }
    for input in [
        r#"{"displayName":"new"}"#,
        r#"{"description":""}"#,
        r#"{"description":"a\nb"}"#,
    ] {
        let plan = PreparedUpdate::new(
            target("Notebook", ITEM, "metadata"),
            input.as_bytes(),
            UpdateOptions::patch(),
        )
        .unwrap();
        assert_eq!(
            plan.prepared().input()["body"],
            serde_json::from_str::<Value>(input).unwrap()
        );
    }
    for length in [256, 257] {
        let input = serde_json::to_vec(&json!({"description":"x".repeat(length)})).unwrap();
        assert_eq!(
            PreparedUpdate::new(
                target("Notebook", ITEM, "metadata"),
                &input,
                UpdateOptions::patch()
            )
            .is_ok(),
            length == 256
        );
    }
}

#[test]
fn unsupported_targets_modes_conditions_and_wait_are_never_weakened() {
    for owner in ["SemanticModel", "SQLDatabase", "Report", "UnknownType"] {
        assert!(matches!(
            PreparedUpdate::new(
                target(owner, ITEM, "metadata"),
                br#"{"description":""}"#,
                UpdateOptions::patch()
            ),
            Err(UpdateError::UnsupportedCapability)
        ));
    }
    for facet in ["src", "docs", "schema", "settings"] {
        assert!(matches!(
            PreparedUpdate::new(
                target("Notebook", ITEM, facet),
                br#"{"description":""}"#,
                UpdateOptions::patch()
            ),
            Err(UpdateError::UnsupportedCapability)
        ));
    }
    for options in [
        UpdateOptions {
            mode: UpdateMode::Replace,
            ..UpdateOptions::patch()
        },
        UpdateOptions {
            if_match: Some("opaque".into()),
            ..UpdateOptions::patch()
        },
        UpdateOptions {
            if_match: Some(String::new()),
            ..UpdateOptions::patch()
        },
        UpdateOptions {
            wait: true,
            ..UpdateOptions::patch()
        },
    ] {
        assert!(matches!(
            PreparedUpdate::new(
                target("Notebook", ITEM, "metadata"),
                br#"{"description":""}"#,
                options
            ),
            Err(UpdateError::UnsupportedCapability)
        ));
    }
}

#[tokio::test]
async fn exact_type_specific_patch_has_no_read_create_or_replay() {
    for (owner, collection) in [("Notebook", "notebooks"), ("Lakehouse", "lakehouses")] {
        let credential = Arc::new(Credential::default());
        let plan = plan(
            owner,
            br#"{"displayName":"new","description":""}"#,
            credential.clone(),
        );
        let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
        let wire = Wire::default();
        wire.results
            .lock()
            .unwrap()
            .push_back(Ok(response(200, metadata(owner))));
        let result = update::execute(&plan, &approval, wire.clone(), pending())
            .await
            .unwrap();
        assert!(matches!(
            result.receipt.outcome(),
            Outcome::Succeeded { .. }
        ));
        assert_eq!(result.receipt.evidence().coverage, Coverage::Complete);
        assert_eq!(
            *wire.calls.lock().unwrap(),
            vec![(
                ResourceMethod::Patch,
                format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/{collection}/{ITEM}"),
                json!({"displayName":"new","description":""})
            )]
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        assert!(matches!(
            update::execute(&plan, &approval, wire.clone(), pending()).await,
            Err(UpdateError::AlreadyExecuted)
        ));
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn denial_context_and_approval_substitution_send_nothing() {
    let credential = Arc::new(Credential::default());
    let first = plan("Notebook", br#"{"description":""}"#, credential.clone());
    let second = plan(
        "Notebook",
        br#"{"description":"other"}"#,
        credential.clone(),
    );
    assert_eq!(
        first.prepared().authorize(&mut Policy(false)).unwrap_err(),
        "denied"
    );
    let approval = first.prepared().authorize(&mut Policy(true)).unwrap();
    let wire = Wire::default();
    assert!(matches!(
        update::execute(&second, &approval, wire.clone(), pending()).await,
        Err(UpdateError::ApprovalMismatch)
    ));
    let unbound = PreparedUpdate::new(
        target("Notebook", ITEM, "metadata"),
        br#"{"description":""}"#,
        UpdateOptions::patch(),
    )
    .unwrap();
    let approval = unbound.prepared().authorize(&mut Policy(true)).unwrap();
    assert!(matches!(
        update::execute(&unbound, &approval, wire.clone(), pending()).await,
        Err(UpdateError::NotConfigured)
    ));
    assert!(wire.calls.lock().unwrap().is_empty());
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn response_loss_timeout_and_cancellation_are_unknown_and_single_send() {
    for failure in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
        OperationExecutionError::ResponseLimit,
    ] {
        let plan = plan(
            "Notebook",
            br#"{"description":""}"#,
            Arc::new(Credential::default()),
        );
        let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
        let wire = Wire::default();
        wire.results.lock().unwrap().push_back(Err(failure));
        let result = update::execute(&plan, &approval, wire.clone(), pending())
            .await
            .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(result.receipt.evidence().mutation_may_have_occurred);
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_before_dispatch_is_failed_without_request() {
    let credential = Arc::new(Credential::default());
    let plan = plan("Notebook", br#"{"description":""}"#, credential.clone());
    let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
    let wire = Wire::default();
    let result = update::execute(&plan, &approval, wire.clone(), std::future::ready(()))
        .await
        .unwrap();
    assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
    assert!(!result.receipt.evidence().mutation_may_have_occurred);
    assert!(wire.calls.lock().unwrap().is_empty());
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn denied_not_found_throttle_and_precondition_are_not_success_or_create() {
    for status in [401, 403, 404, 412, 429, 500] {
        let plan = plan(
            "Notebook",
            br#"{"description":""}"#,
            Arc::new(Credential::default()),
        );
        let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
        let wire = Wire::default();
        wire.results.lock().unwrap().push_back(Ok(response(
            status,
            json!({"errorCode":"Denied","message":"private provider text"}),
        )));
        let result = update::execute(&plan, &approval, wire.clone(), pending())
            .await
            .unwrap();
        assert!(matches!(result.receipt.outcome(), Outcome::Failed { .. }));
        assert_eq!(result.receipt.evidence().http_status, Some(status));
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
        let json = serde_json::to_string(&result.receipt).unwrap();
        assert!(!json.contains("private provider text"));
        if status == 412 {
            assert!(json.contains("PreconditionFailed"));
        }
    }
}

#[tokio::test]
async fn acceptance_retains_handle_and_never_polls_or_claims_completed() {
    for with_handle in [true, false] {
        let plan = plan(
            "Notebook",
            br#"{"description":""}"#,
            Arc::new(Credential::default()),
        );
        let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
        let wire = Wire::default();
        let mut accepted = response(202, Value::Null);
        if with_handle {
            accepted
                .headers
                .insert("x-ms-operation-id".into(), OP.into());
        }
        wire.results.lock().unwrap().push_back(Ok(accepted));
        let result = update::execute(&plan, &approval, wire.clone(), pending())
            .await
            .unwrap();
        assert_eq!(
            matches!(result.receipt.outcome(), Outcome::Accepted),
            with_handle
        );
        if !with_handle {
            assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        }
        assert_eq!(
            result.receipt.evidence().handles.len(),
            usize::from(with_handle)
        );
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn wrong_identity_type_and_racing_metadata_never_become_complete_success() {
    for body in [
        json!({"id":OTHER,"type":"Notebook","displayName":"new","description":""}),
        json!({"id":ITEM,"type":"Lakehouse","displayName":"new","description":""}),
        json!({"id":ITEM,"workspaceId":OTHER,"type":"Notebook","displayName":"new","description":""}),
        json!({"id":ITEM,"type":"Notebook","displayName":"concurrent","description":""}),
    ] {
        let plan = plan(
            "Notebook",
            br#"{"displayName":"new","description":""}"#,
            Arc::new(Credential::default()),
        );
        let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
        let wire = Wire::default();
        wire.results
            .lock()
            .unwrap()
            .push_back(Ok(response(200, body.clone())));
        let result = update::execute(&plan, &approval, wire.clone(), pending())
            .await
            .unwrap();
        assert!(!matches!(
            result.receipt.outcome(),
            Outcome::Succeeded { .. }
        ));
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
        if body["displayName"] == "concurrent" {
            assert!(matches!(result.receipt.outcome(), Outcome::Partial { .. }));
            assert!(matches!(
                result.receipt.evidence().coverage,
                Coverage::Incomplete { .. }
            ));
        } else {
            assert_eq!(result.unconfirmed_data, Some(body));
            assert!(matches!(result.receipt.outcome(), Outcome::Unknown { .. }));
        }
    }
}

#[derive(Clone, Default)]
struct SuspendedWire(Arc<AtomicUsize>);
impl OperationExecutor for SuspendedWire {
    fn execute<'a>(
        &'a self,
        _: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(pending())
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("unexpected observer")
    }
}

#[tokio::test]
async fn dropped_and_competing_execution_futures_never_replenish_submission() {
    use std::task::{Context, Poll, Waker};
    let plan = plan(
        "Notebook",
        br#"{"description":""}"#,
        Arc::new(Credential::default()),
    );
    let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
    let wire = SuspendedWire::default();
    let mut future = Box::pin(update::execute(&plan, &approval, wire.clone(), pending()));
    assert!(matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert_eq!(wire.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        update::execute(&plan, &approval, wire.clone(), pending()).await,
        Err(UpdateError::AlreadyExecuted)
    ));
    drop(future);
    assert!(matches!(
        update::execute(&plan, &approval, wire.clone(), pending()).await,
        Err(UpdateError::AlreadyExecuted)
    ));
    assert_eq!(wire.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_interruption_of_suspended_patch_retains_unknown_without_replay() {
    use std::{
        sync::atomic::AtomicBool,
        task::{Context, Poll, Waker},
    };
    let plan = plan(
        "Notebook",
        br#"{"description":""}"#,
        Arc::new(Credential::default()),
    );
    let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
    let wire = SuspendedWire::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let interruption = std::future::poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = Box::pin(update::execute(
        &plan,
        &approval,
        wire.clone(),
        interruption,
    ));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    cancelled.store(true, Ordering::SeqCst);
    let Poll::Ready(Ok(report)) = future.as_mut().poll(&mut cx) else {
        panic!("interruption was not observed")
    };
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(wire.0.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Default)]
struct SuspendedCredential(AtomicUsize);
#[async_trait]
impl TokenCredential for SuspendedCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        pending().await
    }
}

#[tokio::test]
async fn real_interruption_of_credential_acquisition_never_dispatches_patch() {
    use std::{
        sync::atomic::AtomicBool,
        task::{Context, Poll, Waker},
    };
    let credential = Arc::new(SuspendedCredential::default());
    let context = RequestContext::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let plan = PreparedUpdate::new(
        target("Notebook", ITEM, "metadata"),
        br#"{"description":""}"#,
        UpdateOptions::patch(),
    )
    .unwrap()
    .with_context(context, OperationLimits::default())
    .unwrap();
    let approval = plan.prepared().authorize(&mut Policy(true)).unwrap();
    let wire = SuspendedWire::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let interruption = std::future::poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = Box::pin(update::execute(
        &plan,
        &approval,
        wire.clone(),
        interruption,
    ));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    cancelled.store(true, Ordering::SeqCst);
    let Poll::Ready(Ok(report)) = future.as_mut().poll(&mut cx) else {
        panic!("interruption was not observed")
    };
    assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
    assert!(!report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}
