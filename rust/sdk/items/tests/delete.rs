// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline exact Notebook deletion contracts through the public focused SDK.

#![deny(unnameable_test_items)]

use fabric_core::{Audience, CredentialSelection, EndpointBinding, ServiceOrigin};
use fabric_sdk_items::delete::{
    ApprovalRequirement, CapabilitySupport, CompletionEvidence, DeleteApproval, DeleteAssessment,
    DeleteOptions, DeleteTarget, DeletionMode, DispatchEvidence, ObservationMode,
    ObservationSupport, OperationEffectKind, OperationPrecondition, OperationResourceType,
    OperationStatus, RecoverySemantics, RequiredWorkspaceRole, TargetTypeEnforcement,
    TraversalMode, assess, execute,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::json;
use std::{future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER_ITEM: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";

fn target() -> DeleteTarget {
    DeleteTarget::notebook(WORKSPACE, ITEM).unwrap()
}

fn permanent(wait: bool) -> DeleteOptions {
    DeleteOptions {
        permanent: true,
        wait,
        ..DeleteOptions::default()
    }
}

fn assessment(wait: bool) -> DeleteAssessment {
    assess(target(), permanent(wait)).unwrap()
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("test-account", "test-tenant", audience).unwrap()
}

fn resource_error(status: Option<u16>, mutation: bool) -> ResourceError {
    ResourceError {
        code: "UntrustedProviderCode".into(),
        message: "private provider message must not be output".into(),
        status,
        service_error: None,
        raw_service_error: Some(Box::new(json!({"private": "provider details"}))),
        operation_id: mutation.then(|| "known-operation".into()),
        mutation_may_have_occurred: mutation,
    }
}

struct Capture {
    calls: Vec<ResourceOperation>,
    outcome: Result<ResourceOutcome, ResourceError>,
}

impl Capture {
    fn new(outcome: Result<ResourceOutcome, ResourceError>) -> Self {
        Self {
            calls: Vec::new(),
            outcome,
        }
    }
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.calls.push(operation);
        Box::pin(std::future::ready(self.outcome.clone()))
    }
}

#[test]
fn target_requires_exact_guids_and_preserves_no_name_lookup_surface() {
    let uppercase = DeleteTarget::notebook(WORKSPACE, &ITEM.to_ascii_uppercase()).unwrap();
    assert_eq!(uppercase, target());
    assert_eq!(uppercase.workspace_id(), WORKSPACE);
    assert_eq!(uppercase.item_id(), ITEM);
    assert_eq!(uppercase.resource_type(), OperationResourceType::Notebook);
    for invalid in [
        "",
        "Notebook",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/",
        "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
        "aaaaaaaa_bbbb-cccc-dddd-eeeeeeeeeeee",
        "gaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeé",
    ] {
        for result in [
            DeleteTarget::notebook(invalid, ITEM),
            DeleteTarget::notebook(WORKSPACE, invalid),
        ] {
            let failure = result.unwrap_err();
            assert_eq!(failure.code(), "InvalidInput");
            assert!(failure.metadata().is_none());
            assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
            assert!(failure.resource_error().is_none());
            assert_eq!(
                failure.message(),
                "Notebook deletion requires exact workspace and item GUIDs"
            );
        }
    }
}

#[test]
fn recoverable_recursive_and_conditional_requirements_cannot_dispatch() {
    let executor = Capture::new(Ok(ResourceOutcome::Completed {
        status: 200,
        data: None,
    }));
    let default = DeleteOptions::default();
    assert!(!default.permanent);
    assert!(!default.recursive);
    assert!(default.if_match.is_none());
    assert!(!default.wait);
    let cases = [
        (
            default,
            DeletionMode::Recoverable,
            TraversalMode::ExactTarget,
            OperationPrecondition::Unconditional,
        ),
        (
            DeleteOptions {
                recursive: true,
                ..permanent(false)
            },
            DeletionMode::Permanent,
            TraversalMode::RecursiveRequested,
            OperationPrecondition::Unconditional,
        ),
        (
            DeleteOptions {
                if_match: Some("private-etag".into()),
                ..permanent(false)
            },
            DeletionMode::Permanent,
            TraversalMode::ExactTarget,
            OperationPrecondition::IfMatchRequested,
        ),
        (
            DeleteOptions {
                if_match: Some(String::new()),
                ..permanent(false)
            },
            DeletionMode::Permanent,
            TraversalMode::ExactTarget,
            OperationPrecondition::IfMatchRequested,
        ),
    ];
    for (options, deletion_mode, traversal, precondition) in cases {
        let failure = assess(target(), options).unwrap_err();
        assert_eq!(failure.code(), "UnsupportedCapability");
        let metadata = failure.metadata().unwrap();
        assert_eq!(metadata.target.workspace_id, WORKSPACE);
        assert_eq!(metadata.target.item_id, ITEM);
        assert_eq!(metadata.deletion_mode, deletion_mode);
        assert_eq!(metadata.traversal, traversal);
        assert_eq!(metadata.precondition, precondition);
        assert_eq!(failure.evidence().status, OperationStatus::Failed);
        assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
        assert!(!failure.evidence().mutation_may_have_occurred);
        assert!(failure.resource_error().is_none());
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("private-etag")
        );
    }
    assert!(executor.calls.is_empty());
}

#[test]
fn descriptor_and_assessment_expose_only_explicit_bounded_effects() {
    let assessment = assessment(true);
    assert_eq!(assessment.target(), &target());
    assert_eq!(assessment.options(), &permanent(true));
    let metadata = assessment.metadata();
    let descriptor = assessment.descriptor();
    assert_eq!(&metadata.descriptor, descriptor);
    assert_eq!(descriptor.id, "fabric.items.notebook.delete.v2");
    assert_eq!(
        descriptor.target_type_enforcement,
        TargetTypeEnforcement::ServiceEndpoint
    );
    assert_eq!(
        descriptor.required_workspace_role,
        RequiredWorkspaceRole::Admin
    );
    assert!(metadata.invocation_id > 0);
    assert!(metadata.context.is_none());
    assert_eq!(
        metadata.target.resource_type,
        OperationResourceType::Notebook
    );
    assert_eq!(metadata.deletion_mode, DeletionMode::Permanent);
    assert_eq!(metadata.traversal, TraversalMode::ExactTarget);
    assert_eq!(metadata.precondition, OperationPrecondition::Unconditional);
    assert_eq!(metadata.observation, ObservationMode::WaitForCompletion);
    assert_eq!(descriptor.recursive_support, CapabilitySupport::Unsupported);
    assert_eq!(
        descriptor.conditional_support,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        descriptor.observation_support,
        ObservationSupport::TerminalResponseOnly
    );
    assert_eq!(
        descriptor.recovery,
        RecoverySemantics::UnknownServiceDefined
    );
    assert_eq!(
        descriptor.approval,
        ApprovalRequirement::ExplicitHostAuthorization
    );
    let effects: Vec<_> = descriptor
        .effects
        .iter()
        .map(|effect| (effect.id, effect.kind))
        .collect();
    assert_eq!(
        effects,
        [
            ("notebook-lifecycle", OperationEffectKind::NotebookLifecycle),
            ("notebook-content", OperationEffectKind::NotebookContent),
        ]
    );
    let serialized = serde_json::to_value(&assessment).unwrap();
    assert_eq!(serialized["target"]["workspace_id"], WORKSPACE);
    assert_eq!(serialized["target"]["item_id"], ITEM);
    assert_eq!(serialized["options"]["permanent"], true);
    assert_eq!(serialized["options"]["recursive"], false);
    assert_eq!(serialized["options"]["if_match"], serde_json::Value::Null);
    assert_eq!(serialized["options"]["wait"], true);
    assert_eq!(
        serialized["metadata"]["invocation_id"],
        metadata.invocation_id
    );
    assert!(serialized.get("executed").is_none());
}

#[tokio::test]
async fn binding_context_invalidates_old_approval_and_propagates_through_receipt_json() {
    let assessment = assessment(false);
    let invocation = assessment.metadata().invocation_id;
    let old_approval = DeleteApproval::approve(&assessment);
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let assessment = assessment
        .with_selection(&selection(Audience::Fabric), &endpoint)
        .unwrap();
    assert_eq!(assessment.metadata().invocation_id, invocation);
    let bound = assessment.metadata().context.as_ref().unwrap();
    assert_eq!(bound.account, "test-account");
    assert_eq!(bound.tenant, "test-tenant");
    assert_eq!(bound.endpoint, "https://dailyapi.fabric.microsoft.com/");
    let metadata = assessment.metadata().clone();
    let mut executor = Capture::new(Ok(ResourceOutcome::Completed {
        status: 200,
        data: None,
    }));
    let mismatch = execute(&mut executor, &assessment, Some(&old_approval))
        .await
        .unwrap_err();
    assert_eq!(mismatch.code(), "ApprovalMismatch");
    assert_eq!(mismatch.metadata(), Some(&metadata));
    assert!(executor.calls.is_empty());
    let mismatch_json = serde_json::to_value(&mismatch).unwrap();
    assert_eq!(
        mismatch_json["metadata"]["context"]["account"],
        "test-account"
    );
    let approval = DeleteApproval::approve(&assessment);
    let receipt = execute(&mut executor, &assessment, Some(&approval))
        .await
        .unwrap();
    assert_eq!(receipt.metadata(), &metadata);
    assert_eq!(executor.calls.len(), 1);
    let serialized = serde_json::to_value(&receipt).unwrap();
    assert_eq!(serialized["metadata"]["context"]["account"], "test-account");
    assert_eq!(serialized["metadata"]["context"]["tenant"], "test-tenant");
    assert_eq!(
        serialized["metadata"]["context"]["endpoint"],
        "https://dailyapi.fabric.microsoft.com/"
    );
    let assessed = serde_json::to_value(&assessment).unwrap();
    assert_eq!(
        assessed["metadata"]["context"],
        serialized["metadata"]["context"]
    );
}

#[test]
fn context_binding_rejects_rebinding_and_non_fabric_service_or_audience() {
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let bound = assessment(false)
        .with_selection(&selection(Audience::Fabric), &endpoint)
        .unwrap();
    let metadata = bound.metadata().clone();
    let failure = bound
        .with_selection(&selection(Audience::Fabric), &endpoint)
        .unwrap_err();
    assert_eq!(failure.code(), "ContextAlreadyBound");
    assert_eq!(failure.metadata(), Some(&metadata));
    assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
    for (audience, origin) in [
        (Audience::Arm, ServiceOrigin::Fabric),
        (Audience::OneLake, ServiceOrigin::Fabric),
        (Audience::Fabric, ServiceOrigin::PowerBi),
        (Audience::Fabric, ServiceOrigin::OneLake),
        (Audience::Fabric, ServiceOrigin::Arm),
    ] {
        let unbound = assessment(false);
        let metadata = unbound.metadata().clone();
        let failure = unbound
            .with_selection(&selection(audience), &EndpointBinding::public(origin))
            .unwrap_err();
        assert_eq!(failure.code(), "InvalidContext");
        assert_eq!(failure.metadata(), Some(&metadata));
        assert!(failure.metadata().unwrap().context.is_none());
        assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
        assert!(!failure.evidence().mutation_may_have_occurred);
    }
}

#[tokio::test]
async fn consumed_assessment_cannot_be_context_bound() {
    let assessment = assessment(false);
    let approval = DeleteApproval::approve(&assessment);
    let metadata = assessment.metadata().clone();
    let mut executor = Capture::new(Err(resource_error(None, true)));
    execute(&mut executor, &assessment, Some(&approval))
        .await
        .unwrap_err();
    let failure = assessment
        .with_selection(
            &selection(Audience::Fabric),
            &EndpointBinding::public(ServiceOrigin::Fabric),
        )
        .unwrap_err();
    assert_eq!(failure.code(), "AlreadyExecuted");
    assert_eq!(failure.metadata(), Some(&metadata));
    assert_eq!(executor.calls.len(), 1);
}

#[tokio::test]
async fn missing_or_mismatched_full_assessment_approval_makes_zero_calls() {
    let original = assessment(false);
    let approval = DeleteApproval::approve(&original);
    let mut executor = Capture::new(Ok(ResourceOutcome::Completed {
        status: 200,
        data: None,
    }));
    let failure = execute(&mut executor, &original, None).await.unwrap_err();
    assert_eq!(failure.code(), "ApprovalRequired");
    assert_eq!(failure.metadata(), Some(original.metadata()));
    assert!(executor.calls.is_empty());
    let mismatches = [
        assessment(false),
        assessment(true),
        assess(
            DeleteTarget::notebook(WORKSPACE, OTHER_ITEM).unwrap(),
            permanent(false),
        )
        .unwrap(),
    ];
    for mismatch in mismatches {
        assert_ne!(
            mismatch.metadata().invocation_id,
            original.metadata().invocation_id
        );
        let failure = execute(&mut executor, &mismatch, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(failure.code(), "ApprovalMismatch");
        assert_eq!(failure.metadata(), Some(mismatch.metadata()));
        assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
        assert!(executor.calls.is_empty());
    }
    execute(&mut executor, &original, Some(&approval))
        .await
        .unwrap();
    assert_eq!(executor.calls.len(), 1);
}

#[tokio::test]
async fn exact_delete_uses_only_shared_executor_and_is_consumed_once() {
    for wait in [false, true] {
        let assessment = assessment(wait);
        let metadata = assessment.metadata().clone();
        let approval = DeleteApproval::approve(&assessment);
        let mut executor = Capture::new(Ok(ResourceOutcome::Completed {
            status: 200,
            data: None,
        }));
        let receipt = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap();
        assert_eq!(executor.calls.len(), 1);
        let call = &executor.calls[0];
        assert_eq!(call.method(), ResourceMethod::Delete);
        assert_eq!(
            call.path(),
            format!("/v1/workspaces/{WORKSPACE}/notebooks/{ITEM}?hardDelete=true")
        );
        assert!(call.body().is_none());
        assert_eq!(
            call.completion(),
            if wait {
                CompletionPolicy::WaitForCompletion
            } else {
                CompletionPolicy::ReturnAccepted
            }
        );
        assert_eq!(receipt.metadata(), &metadata);
        assert_eq!(assessment.metadata(), &metadata);
        assert_eq!(receipt.status(), OperationStatus::Succeeded);
        assert_eq!(receipt.evidence.completion, CompletionEvidence::Completed);
        assert_eq!(receipt.evidence.http_status, Some(200));
        assert!(receipt.evidence.mutation_may_have_occurred);
        assert_eq!(
            receipt.evidence.dispatch,
            DispatchEvidence::ResponseObserved
        );
        assert_eq!(
            receipt.evidence.effects.len(),
            metadata.descriptor.effects.len()
        );
        for (observed, expected) in receipt
            .evidence
            .effects
            .iter()
            .zip(metadata.descriptor.effects)
        {
            assert_eq!(observed.effect_id, expected.id);
            assert_eq!(observed.status, OperationStatus::Succeeded);
        }
        let serialized = serde_json::to_value(&receipt).unwrap();
        assert_eq!(serialized["evidence"]["status"], "succeeded");
        assert_eq!(serialized["evidence"]["completion"], "completed");
        assert_eq!(serialized["metadata"]["deletion_mode"], "permanent");
        let second = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(second.code(), "AlreadyExecuted");
        assert_eq!(executor.calls.len(), 1);
    }
}

#[tokio::test]
async fn accepted_with_or_without_headers_is_never_terminal_success_even_when_waiting() {
    for wait in [false, true] {
        for outcome in [
            ResourceOutcome::Accepted { operation_id: None },
            ResourceOutcome::Accepted {
                operation_id: Some("service-operation".into()),
            },
            ResourceOutcome::Completed {
                status: 202,
                data: None,
            },
        ] {
            let expected_id = match &outcome {
                ResourceOutcome::Accepted { operation_id } => operation_id.clone(),
                ResourceOutcome::Completed { .. } => None,
            };
            let assessment = assessment(wait);
            let approval = DeleteApproval::approve(&assessment);
            let mut executor = Capture::new(Ok(outcome));
            let receipt = execute(&mut executor, &assessment, Some(&approval))
                .await
                .unwrap();
            assert_eq!(executor.calls.len(), 1);
            assert_eq!(receipt.metadata(), assessment.metadata());
            assert_eq!(receipt.status(), OperationStatus::Accepted);
            assert_eq!(receipt.evidence.operation_id, expected_id);
            assert_eq!(receipt.evidence.http_status, Some(202));
            assert_eq!(receipt.evidence.completion, CompletionEvidence::Unknown);
            assert!(
                receipt
                    .evidence
                    .effects
                    .iter()
                    .all(|effect| effect.status == OperationStatus::Accepted)
            );
            let serialized = serde_json::to_value(&receipt).unwrap();
            assert_eq!(serialized["evidence"]["status"], "accepted");
            assert_eq!(serialized["evidence"]["completion"], "unknown");
        }
    }
}

#[tokio::test]
async fn initial_service_denials_not_found_and_precondition_rejection_are_failures() {
    for (status, code) in [
        (403, "AccessDenied"),
        (404, "NotFound"),
        (412, "PreconditionFailed"),
    ] {
        let assessment = assessment(false);
        let metadata = assessment.metadata().clone();
        let approval = DeleteApproval::approve(&assessment);
        let mut original = resource_error(Some(status), true);
        original.operation_id = None;
        let mut executor = Capture::new(Err(original.clone()));
        let failure = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(executor.calls.len(), 1);
        assert_eq!(failure.code(), code);
        assert_eq!(failure.resource_error(), Some(&original));
        assert_eq!(failure.metadata(), Some(&metadata));
        assert_eq!(assessment.metadata(), &metadata);
        assert_eq!(failure.evidence().status, OperationStatus::Failed);
        assert_eq!(failure.evidence().http_status, Some(status));
        assert_eq!(
            failure.evidence().dispatch,
            DispatchEvidence::ResponseObserved
        );
        assert_eq!(failure.evidence().completion, CompletionEvidence::Unknown);
        assert!(failure.evidence().operation_id.is_none());
        let serialized = serde_json::to_string(&failure).unwrap();
        assert!(!serialized.contains("private provider"));
        assert!(!serialized.contains("provider details"));
        assert!(!serialized.contains("UntrustedProviderCode"));
        assert!(!failure.to_string().contains("private provider"));
        assert_eq!(
            serde_json::to_value(&failure).unwrap()["evidence"]["status"],
            "failed"
        );
    }
}

#[tokio::test]
async fn post_acceptance_observation_denials_preserve_unknown_mutation_outcome() {
    for (status, code) in [
        (401, "Unauthenticated"),
        (403, "AccessDenied"),
        (404, "NotFound"),
        (412, "PreconditionFailed"),
    ] {
        let assessment = assessment(true)
            .with_selection(
                &selection(Audience::Fabric),
                &EndpointBinding::public(ServiceOrigin::Fabric),
            )
            .unwrap();
        let metadata = assessment.metadata().clone();
        let approval = DeleteApproval::approve(&assessment);
        let original = resource_error(Some(status), true);
        let mut executor = Capture::new(Err(original.clone()));
        let failure = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(failure.code(), code);
        assert_eq!(failure.evidence().status, OperationStatus::Unknown);
        assert_eq!(failure.evidence().completion, CompletionEvidence::Unknown);
        assert_eq!(failure.evidence().http_status, Some(status));
        assert_eq!(
            failure.evidence().operation_id.as_deref(),
            Some("known-operation")
        );
        assert!(failure.evidence().mutation_may_have_occurred);
        assert_eq!(failure.metadata(), Some(&metadata));
        assert_eq!(failure.resource_error(), Some(&original));
        assert!(
            failure
                .evidence()
                .effects
                .iter()
                .all(|effect| effect.status == OperationStatus::Unknown)
        );
        if matches!(status, 403 | 404) {
            assert!(failure.message().contains("observation"));
            assert!(failure.message().contains("outcome is unknown"));
        }
        let serialized = serde_json::to_value(&failure).unwrap();
        assert_eq!(serialized["evidence"]["status"], "unknown");
        assert_eq!(serialized["evidence"]["operation_id"], "known-operation");
        assert_eq!(serialized["metadata"]["context"]["account"], "test-account");
        assert_eq!(executor.calls.len(), 1);
        let retry = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(retry.code(), "AlreadyExecuted");
        assert_eq!(executor.calls.len(), 1);
    }
}

#[tokio::test]
async fn pre_dispatch_failure_and_lost_response_or_timeout_are_distinct_and_not_replayed() {
    for (status, mutation, expected, dispatch) in [
        (
            None,
            false,
            OperationStatus::Failed,
            DispatchEvidence::NotDispatched,
        ),
        (
            None,
            true,
            OperationStatus::Unknown,
            DispatchEvidence::Uncertain,
        ),
        (
            Some(504),
            true,
            OperationStatus::Unknown,
            DispatchEvidence::ResponseObserved,
        ),
    ] {
        let assessment = assessment(true);
        let metadata = assessment.metadata().clone();
        let approval = DeleteApproval::approve(&assessment);
        let original = resource_error(status, mutation);
        let mut executor = Capture::new(Err(original.clone()));
        let failure = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(executor.calls.len(), 1);
        assert_eq!(failure.metadata(), Some(&metadata));
        assert_eq!(failure.resource_error(), Some(&original));
        assert_eq!(failure.evidence().status, expected);
        assert_eq!(failure.evidence().dispatch, dispatch);
        assert_eq!(failure.evidence().mutation_may_have_occurred, mutation);
        assert_eq!(failure.evidence().completion, CompletionEvidence::Unknown);
        assert_eq!(failure.evidence().operation_id, original.operation_id);
        assert!(
            failure
                .evidence()
                .effects
                .iter()
                .all(|effect| effect.status == expected)
        );
        let retry = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(retry.code(), "AlreadyExecuted");
        assert_eq!(executor.calls.len(), 1);
    }
}

#[tokio::test]
async fn finite_stop_reasons_remain_distinct_from_mutation_outcome() {
    let cases = [
        ("Timeout", None, false, "Timeout", OperationStatus::Failed),
        ("TimedOut", None, false, "Timeout", OperationStatus::Failed),
        (
            "Timeout",
            Some(202),
            true,
            "Timeout",
            OperationStatus::Unknown,
        ),
        (
            "TimedOut",
            Some(202),
            true,
            "Timeout",
            OperationStatus::Unknown,
        ),
        (
            "Cancelled",
            None,
            false,
            "Cancelled",
            OperationStatus::Failed,
        ),
        (
            "Cancelled",
            Some(202),
            true,
            "Cancelled",
            OperationStatus::Unknown,
        ),
        (
            "UntrustedProviderCode",
            Some(401),
            false,
            "Unauthenticated",
            OperationStatus::Failed,
        ),
        (
            "UntrustedProviderCode",
            Some(429),
            false,
            "Throttled",
            OperationStatus::Failed,
        ),
        (
            "UntrustedProviderCode",
            Some(429),
            true,
            "Throttled",
            OperationStatus::Unknown,
        ),
    ];
    for (resource_code, status, mutation, expected_code, expected_status) in cases {
        let assessment = assessment(true);
        let metadata = assessment.metadata().clone();
        let approval = DeleteApproval::approve(&assessment);
        let mut original = resource_error(status, mutation);
        original.code = resource_code.into();
        let mut executor = Capture::new(Err(original.clone()));
        let failure = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(failure.code(), expected_code);
        assert_eq!(failure.evidence().status, expected_status);
        assert_eq!(failure.metadata(), Some(&metadata));
        assert_eq!(failure.resource_error(), Some(&original));
        assert_eq!(failure.evidence().http_status, original.status);
        assert_eq!(failure.evidence().operation_id, original.operation_id);
        assert_eq!(failure.evidence().mutation_may_have_occurred, mutation);
        assert_eq!(failure.evidence().completion, CompletionEvidence::Unknown);
        assert_eq!(executor.calls.len(), 1);
        let serialized = serde_json::to_value(&failure).unwrap();
        assert_eq!(serialized["code"], expected_code);
        if expected_status == OperationStatus::Unknown {
            assert_eq!(serialized["evidence"]["status"], "unknown");
            assert_eq!(serialized["evidence"]["operation_id"], "known-operation");
        } else if status.is_none() {
            assert_eq!(failure.evidence().dispatch, DispatchEvidence::NotDispatched);
        }
        let retry = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(retry.code(), "AlreadyExecuted");
        assert_eq!(executor.calls.len(), 1);
    }
}

#[tokio::test]
async fn unexpected_success_statuses_do_not_fabricate_terminal_proof() {
    for status in [201, 204, 206, 299] {
        let assessment = assessment(false);
        let approval = DeleteApproval::approve(&assessment);
        let mut executor = Capture::new(Ok(ResourceOutcome::Completed {
            status,
            data: Some(json!({"not": "terminal proof"})),
        }));
        let failure = execute(&mut executor, &assessment, Some(&approval))
            .await
            .unwrap_err();
        assert_eq!(executor.calls.len(), 1);
        assert_eq!(failure.code(), "OutcomeUnknown");
        assert_eq!(failure.metadata(), Some(assessment.metadata()));
        assert_eq!(failure.evidence().status, OperationStatus::Unknown);
        assert_eq!(failure.evidence().completion, CompletionEvidence::Unknown);
        assert_eq!(failure.evidence().http_status, Some(status));
        assert_eq!(failure.resource_error().unwrap().code, "UnexpectedResponse");
    }
}

#[tokio::test]
async fn dropping_a_dispatched_future_cannot_reuse_the_assessment() {
    struct PendingCapture {
        calls: usize,
    }

    impl ResourceOperations for PendingCapture {
        fn execute(
            &mut self,
            _operation: ResourceOperation,
        ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>>
        {
            self.calls += 1;
            Box::pin(std::future::pending())
        }
    }

    let assessment = assessment(false);
    let approval = DeleteApproval::approve(&assessment);
    let mut executor = PendingCapture { calls: 0 };
    let mut future = Box::pin(execute(&mut executor, &assessment, Some(&approval)));
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    assert!(future.as_mut().poll(&mut context).is_pending());
    drop(future);
    let failure = execute(&mut executor, &assessment, Some(&approval))
        .await
        .unwrap_err();
    assert_eq!(failure.code(), "AlreadyExecuted");
    assert_eq!(executor.calls, 1);
}
