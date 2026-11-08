// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Offline native Domain synchronization and exact approval/evidence contracts.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_catalog::service::{
    self, DomainTarget, InputError, OperationExecutor, OperationLimits, SyncError,
    v2::{
        ApprovalPolicy, Availability, Effect, Outcome, Precondition, PreparedOperation, Registry,
        ResolvedTarget, Target, TargetKind, TenantIdentity,
    },
};
use fabric_sdk_operations::{OperationExecutionError, OperationRequest, OperationResponse};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::{self, Future},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const DOMAIN: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Debug)]
struct Credential(Arc<AtomicUsize>);
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
            "offline-test-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone)]
struct Executor {
    sends: Arc<AtomicUsize>,
    body: Arc<Mutex<Vec<u8>>>,
    reply: Result<OperationResponse, OperationExecutionError>,
    cancel_after_send: Option<Arc<AtomicUsize>>,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.sends.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method(), ResourceMethod::Post);
        assert_eq!(
            request.url().as_str(),
            format!(
                "https://api.fabric.microsoft.com/v1/admin/domains/{DOMAIN}/roleAssignments/syncToSubdomains"
            )
        );
        *self.body.lock().unwrap() = request.body().to_vec();
        if let Some(cancel) = &self.cancel_after_send {
            cancel.store(1, Ordering::SeqCst);
            Box::pin(future::pending())
        } else {
            Box::pin(future::ready(self.reply.clone()))
        }
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("synchronous Domain sync must never poll or wait")
    }
}

struct Policy;
impl ApprovalPolicy for Policy {
    type Error = &'static str;
    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(prepared.operation_id(), service::OPERATION_ID);
        assert_eq!(
            prepared.input(),
            &serde_json::json!({"domainId":DOMAIN,"role":"Contributor"})
        );
        assert_eq!(prepared.effects(), [Effect::RemoteWrite]);
        assert_eq!(
            prepared.preconditions(),
            [
                Precondition::ExistingOnly,
                Precondition::UnguardedConcurrency
            ]
        );
        assert!(
            matches!(prepared.availability(), Availability::Conditional(requirements) if requirements.len() == 2)
        );
        Ok(())
    }
}

fn target(owner: &str) -> ResolvedTarget {
    ResolvedTarget::tenant_collection(
        "/.domains/Contoso.Domain",
        TenantIdentity::new(TENANT).unwrap(),
        Target {
            kind: TargetKind::TenantCollection,
            owner_type: owner.into(),
            facet: "roleAssignments".into(),
            representation: "native".into(),
            generation: None,
        },
    )
    .unwrap()
}
fn prepared() -> PreparedOperation {
    let mut registry = Registry::default();
    service::register(&mut registry).unwrap();
    let domain = DomainTarget::new(target("Domain"), DOMAIN).unwrap();
    service::prepare(&registry, &domain, "Contributor").unwrap()
}
fn context(calls: Arc<AtomicUsize>, tenant: &str) -> RequestContext {
    RequestContext::new(
        Arc::new(Credential(calls)),
        CredentialSelection::new("offline", tenant, Audience::Fabric).unwrap(),
    )
}
fn executor(reply: Result<OperationResponse, OperationExecutionError>) -> Executor {
    Executor {
        sends: Arc::new(AtomicUsize::new(0)),
        body: Arc::new(Mutex::new(Vec::new())),
        reply,
        cancel_after_send: None,
    }
}
fn response(status: u16, body: &[u8]) -> OperationResponse {
    OperationResponse {
        status,
        body: body.to_vec(),
        headers: BTreeMap::new(),
    }
}

#[test]
fn wrong_owner_role_and_malformed_uuid_are_local_rejections() {
    for owner in ["Gateway", "Capacity", "Workspace"] {
        assert_eq!(
            DomainTarget::new(target(owner), DOMAIN).unwrap_err(),
            InputError::Target
        );
    }
    for id in [
        "",
        "name",
        "../route",
        "22222222-2222-2222-2222-22222222222z",
    ] {
        assert_eq!(
            DomainTarget::new(target("Domain"), id).unwrap_err(),
            InputError::Target
        );
    }
    let mut registry = Registry::default();
    service::register(&mut registry).unwrap();
    let original = registry.clone();
    assert!(service::register(&mut registry).is_err());
    assert_eq!(registry.bindings(), original.bindings());
    let domain = DomainTarget::new(target("Domain"), DOMAIN).unwrap();
    for role in ["Admin", "contributor", "", "Viewer"] {
        assert_eq!(
            service::prepare(&registry, &domain, role).unwrap_err(),
            InputError::Role
        );
    }
    assert_eq!(
        service::prepare(&Registry::default(), &domain, "Contributor").unwrap_err(),
        InputError::Binding
    );
}

#[tokio::test]
async fn exact_payload_single_post_and_empty_documented_completion() {
    let calls = Arc::new(AtomicUsize::new(0));
    let context = context(calls.clone(), TENANT);
    let executor = executor(Ok(response(200, b"")));
    let prepared = prepared();
    let approved = prepared.authorize(&mut Policy).unwrap();
    let report = service::synchronize(
        &context,
        &approved,
        executor.clone(),
        OperationLimits::default(),
        future::pending(),
    )
    .await
    .unwrap();
    assert_eq!(executor.sends.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*executor.body.lock().unwrap(), br#"{"role":"Contributor"}"#);
    assert_eq!(
        report.receipt.outcome(),
        &Outcome::Succeeded { data: Value::Null }
    );
    assert_eq!(report.receipt.evidence().http_status, Some(200));
    assert!(report.receipt.evidence().handles.is_empty());
    assert_eq!(report.receipt.target().identity().tenant_id(), Some(TENANT));
    assert_eq!(
        report.receipt.sources()[0].details.as_ref().unwrap(),
        &serde_json::json!({"domainId":DOMAIN,"role":"Contributor"})
    );
    assert!(report.receipt.target().identity().item_id().is_none());
    assert!(report.response.unwrap().body.is_empty());
}

#[tokio::test]
async fn native_errors_preserve_exact_code_body_and_never_retry() {
    for (status, code) in [
        (400, "SyncingAdminsToSubdomainsIsNotSupported"),
        (401, "Unauthorized"),
        (403, "InsufficientPermissions"),
        (404, "DomainNotFound"),
        (429, "TooManyRequests"),
        (500, "UnknownError"),
    ] {
        let native = serde_json::json!({"errorCode":code,"message":"private-provider-message"});
        let executor = executor(Ok(response(status, &serde_json::to_vec(&native).unwrap())));
        let context = context(Arc::new(AtomicUsize::new(0)), TENANT);
        let prepared = prepared();
        let approved = prepared.authorize(&mut Policy).unwrap();
        let report = service::synchronize(
            &context,
            &approved,
            executor.clone(),
            OperationLimits::default(),
            future::pending(),
        )
        .await
        .unwrap();
        assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
        assert_eq!(report.receipt.evidence().http_status, Some(status));
        assert_eq!(
            report.error.as_ref().unwrap().raw_service_error.as_deref(),
            Some(&native)
        );
        assert_eq!(executor.sends.load(Ordering::SeqCst), 1);
        assert!(!format!("{report:?}").contains("private-provider-message"));
        assert!(
            !serde_json::to_string(&report.receipt)
                .unwrap()
                .contains("private-provider-message")
        );
    }
}

#[tokio::test]
async fn response_loss_and_undocumented_success_are_unknown_without_polling() {
    for reply in [
        Err(OperationExecutionError::Failed),
        Err(OperationExecutionError::TimedOut),
        Err(OperationExecutionError::ResponseLimit),
        Ok(response(201, b"{}")),
        Ok(response(204, b"")),
        Ok(response(202, b"")),
        Ok(response(200, b"[]")),
        Ok(response(200, b"{}")),
        Ok(response(200, b"not-json")),
        Ok(response(200, br#"{"errorCode":"DomainNotFound"}"#)),
        Ok(response(200, br#"{"status":"Running"}"#)),
        Ok(response(206, b"")),
        Ok(response(302, b"")),
    ] {
        let executor = executor(reply.clone());
        let prepared = prepared();
        let approved = prepared.authorize(&mut Policy).unwrap();
        let report = service::synchronize(
            &context(Arc::new(AtomicUsize::new(0)), TENANT),
            &approved,
            executor.clone(),
            OperationLimits::default(),
            future::pending(),
        )
        .await
        .unwrap();
        assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
        assert!(report.receipt.evidence().mutation_may_have_occurred);
        assert!(report.receipt.evidence().handles.is_empty());
        assert_eq!(executor.sends.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn receipt_construction_failure_retains_domain_input_and_effect_evidence() {
    for status in [0, 600] {
        let native = serde_json::json!({"errorCode":"InvalidProviderStatus"});
        let executor = executor(Ok(response(status, &serde_json::to_vec(&native).unwrap())));
        let prepared = prepared();
        let approved = prepared.authorize(&mut Policy).unwrap();
        let failure = service::synchronize(
            &context(Arc::new(AtomicUsize::new(0)), TENANT),
            &approved,
            executor.clone(),
            OperationLimits::default(),
            future::pending(),
        )
        .await
        .unwrap_err();
        let SyncError::Receipt {
            cause,
            prepared: retained,
            source,
            error,
            ..
        } = failure
        else {
            panic!("invalid HTTP evidence must fail receipt validation");
        };
        assert_eq!(*retained, prepared);
        assert_eq!(
            retained.target().canonical_path(),
            "/.domains/Contoso.Domain"
        );
        assert_eq!(
            retained.input(),
            &serde_json::json!({"domainId":DOMAIN,"role":"Contributor"})
        );
        assert_eq!(retained.effects(), [Effect::RemoteWrite]);
        assert_eq!(
            source.details,
            Some(serde_json::json!({"domainId":DOMAIN,"role":"Contributor"}))
        );
        assert_eq!(source.http_status, Some(status));
        assert!(matches!(cause.outcome, Outcome::Unknown { .. }));
        assert!(cause.evidence.mutation_may_have_occurred);
        assert_eq!(cause.evidence.http_status, Some(status));
        assert_eq!(error.unwrap().raw_service_error.as_deref(), Some(&native));
        assert_eq!(executor.sends.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn tenant_audience_origin_and_approval_evidence_fail_before_credentials() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = executor(Ok(response(200, b"")));
    let prepared = prepared();
    let approved = prepared.authorize(&mut Policy).unwrap();
    let contexts = [
        context(calls.clone(), DOMAIN),
        RequestContext::new(
            Arc::new(Credential(calls.clone())),
            CredentialSelection::new("offline", TENANT, Audience::Arm).unwrap(),
        ),
        RequestContext::with_endpoint(
            Arc::new(Credential(calls.clone())),
            CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
            EndpointBinding::public(ServiceOrigin::PowerBi),
        ),
    ];
    for context in contexts {
        assert!(matches!(
            service::synchronize(
                &context,
                &approved,
                executor.clone(),
                OperationLimits::default(),
                future::pending()
            )
            .await,
            Err(SyncError::Input(InputError::Context))
        ));
    }
    let changed = prepared.clone().with_preconditions(vec![]).unwrap();
    let changed = changed.authorize(&mut Permissive).unwrap();
    assert!(matches!(
        service::synchronize(
            &context(calls.clone(), TENANT),
            &changed,
            executor.clone(),
            OperationLimits::default(),
            future::pending()
        )
        .await,
        Err(SyncError::Input(InputError::Binding))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.sends.load(Ordering::SeqCst), 0);
}

struct Permissive;
impl ApprovalPolicy for Permissive {
    type Error = ();
    fn authorize(&mut self, _: &PreparedOperation) -> Result<(), ()> {
        Ok(())
    }
}

#[tokio::test]
async fn cancellation_distinguishes_pre_dispatch_from_lost_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = executor(Ok(response(200, b"")));
    let prepared = prepared();
    let approved = prepared.authorize(&mut Policy).unwrap();
    let report = service::synchronize(
        &context(calls.clone(), TENANT),
        &approved,
        executor.clone(),
        OperationLimits::default(),
        future::ready(()),
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
    assert!(!report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.sends.load(Ordering::SeqCst), 0);

    let cancel = Arc::new(AtomicUsize::new(0));
    let executor = Executor {
        cancel_after_send: Some(cancel.clone()),
        ..executor
    };
    let cancelled = future::poll_fn(move |cx| {
        if cancel.load(Ordering::SeqCst) == 1 {
            std::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    });
    let report = service::synchronize(
        &context(calls, TENANT),
        &approved,
        executor.clone(),
        OperationLimits::default(),
        cancelled,
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(executor.sends.load(Ordering::SeqCst), 1);
}
