// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Tenant-scoped search identity does not fabricate a workspace or item.

#![deny(unnameable_test_items)]

use fabric_v2_contracts::{
    ApprovalPolicy, Availability, Binding, Coverage, Effect, ExecutionEvidence, ExecutionReceipt,
    FailureInfo, Generation, OperationDescriptor, Outcome, PreparedOperation, ReceiptError,
    Registry, ResolvedTarget, SourceEvidence, Target, TargetKind, TenantIdentity, Verb,
};
use serde_json::json;

const TENANT: &str = "11111111-2222-4333-8444-555555555555";

fn selection(kind: TargetKind) -> Target {
    Target {
        kind,
        owner_type: "Tenant".into(),
        facet: "catalog".into(),
        representation: "metadata".into(),
        generation: None,
    }
}

#[test]
fn failed_source_cannot_be_hidden_by_a_successful_aggregate_receipt() {
    struct ReadPolicy;
    impl ApprovalPolicy for ReadPolicy {
        type Error = &'static str;
        fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
            if operation.operation_id() == "catalog.search"
                && operation.effects() == [Effect::RemoteRead]
            {
                Ok(())
            } else {
                Err("Unsupported operation.")
            }
        }
    }
    let mut registry = Registry::default();
    registry
        .register(
            OperationDescriptor::new(
                "catalog.search",
                Verb::Search,
                json!({"type":"object"}),
                json!({"type":"object"}),
                vec![Effect::RemoteRead],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .bind(
            Binding::new(
                "Tenant",
                "catalog",
                "metadata",
                Generation::Any,
                "catalog.search",
                Availability::Available,
            )
            .unwrap()
            .with_target_kind(TargetKind::TenantCollection),
        )
        .unwrap();
    let selected = selection(TargetKind::TenantCollection);
    let prepared = PreparedOperation::new(
        registry.resolve(Verb::Search, &selected, None).unwrap(),
        ResolvedTarget::tenant_collection("/", TenantIdentity::new(TENANT).unwrap(), selected)
            .unwrap(),
        json!({"mode":"catalog","text":"sales"}),
    )
    .unwrap();
    let approved = prepared.authorize(&mut ReadPolicy).unwrap();
    let receipt = ExecutionReceipt::new(
        &approved,
        Outcome::Succeeded { data: json!([]) },
        ExecutionEvidence {
            http_status: Some(200),
            handles: vec![],
            coverage: Coverage::Complete,
            mutation_may_have_occurred: false,
        },
    )
    .unwrap();
    let failed = SourceEvidence {
        source: "relevance".into(),
        http_status: Some(403),
        coverage: Coverage::Unknown,
        error: Some(FailureInfo {
            code: "AccessDenied".into(),
            message: "Source denied.".into(),
        }),
        details: None,
    };
    let error = receipt
        .with_source_evidence(vec![failed.clone()])
        .unwrap_err();
    assert_eq!(error.sources, vec![failed.clone()]);
    assert!(matches!(error.receipt.outcome(), Outcome::Succeeded { .. }));
    let accepted_source = SourceEvidence {
        source: "catalog".into(),
        http_status: Some(202),
        coverage: Coverage::Unknown,
        error: None,
        details: None,
    };
    let rejected_acceptance = error
        .receipt
        .clone()
        .with_source_evidence(vec![accepted_source.clone()])
        .unwrap_err();
    assert_eq!(rejected_acceptance.sources, vec![accepted_source]);
    assert!(matches!(
        rejected_acceptance.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    let partial = ExecutionReceipt::new(
        &approved,
        Outcome::Partial {
            data: json!([{"id":"retained"}]),
            error: FailureInfo {
                code: "PartialSearch".into(),
                message: "One source failed.".into(),
            },
        },
        ExecutionEvidence {
            http_status: None,
            handles: vec![],
            coverage: Coverage::Incomplete {
                reasons: vec!["relevance_failed".into()],
                continuation: None,
            },
            mutation_may_have_occurred: false,
        },
    )
    .unwrap()
    .with_source_evidence(vec![failed])
    .unwrap();
    let encoded = serde_json::to_value(partial).unwrap();
    assert_eq!(encoded["status"], "partial");
    assert_eq!(encoded["meta"]["sources"][0]["httpStatus"], 403);
    assert_eq!(encoded["data"][0]["id"], "retained");
}

#[test]
fn tenant_search_retains_real_identity_and_has_no_workspace_or_item() {
    let target = ResolvedTarget::tenant_collection(
        "/",
        TenantIdentity::new(TENANT).unwrap(),
        selection(TargetKind::TenantCollection),
    )
    .unwrap();
    assert_eq!(target.identity().tenant_id(), Some(TENANT));
    assert_eq!(target.identity().workspace_id(), None);
    assert_eq!(target.identity().item_id(), None);
    assert_eq!(target.identity().kind(), TargetKind::TenantCollection);
    assert_eq!(
        serde_json::to_value(&target).unwrap()["identity"]["tenantId"],
        TENANT
    );
}

#[test]
fn tenant_constructor_rejects_mismatched_selection_and_malformed_identity() {
    assert_eq!(
        TenantIdentity::new("not-a-tenant").unwrap_err(),
        ReceiptError::InvalidIdentity
    );
    assert_eq!(
        ResolvedTarget::tenant_collection(
            "/",
            TenantIdentity::new(TENANT).unwrap(),
            selection(TargetKind::WorkspaceCollection),
        )
        .unwrap_err(),
        ReceiptError::TargetMismatch
    );
}

#[test]
fn tenant_search_registration_cannot_be_substituted_for_workspace_search() {
    let mut registry = Registry::default();
    registry
        .register(
            OperationDescriptor::new(
                "catalog.search",
                Verb::Search,
                json!({"type":"object"}),
                json!({"type":"object"}),
                vec![Effect::RemoteRead],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .bind(
            Binding::new(
                "Tenant",
                "catalog",
                "metadata",
                Generation::Any,
                "catalog.search",
                Availability::Available,
            )
            .unwrap()
            .with_target_kind(TargetKind::TenantCollection),
        )
        .unwrap();
    let selected = selection(TargetKind::TenantCollection);
    let resolution = registry.resolve(Verb::Search, &selected, None).unwrap();
    let target =
        ResolvedTarget::tenant_collection("/", TenantIdentity::new(TENANT).unwrap(), selected)
            .unwrap();
    let prepared =
        PreparedOperation::new(resolution, target, json!({"mode":"catalog","text":"sales"}))
            .unwrap();
    assert_eq!(prepared.operation_id(), "catalog.search");
    assert!(
        registry
            .resolve(
                Verb::Search,
                &selection(TargetKind::WorkspaceCollection),
                None
            )
            .is_err()
    );
}
