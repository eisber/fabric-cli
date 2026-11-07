// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Cross-head preparation, approval and execution-evidence regression contracts.

#![deny(unnameable_test_items)]

use fabric_v2_contracts::{
    ApprovalPolicy, Availability, Binding, Coverage, Effect, ExecutionEvidence, ExecutionReceipt,
    FailureInfo, Generation, ItemIdentity, OperationDescriptor, OperationHandle, Outcome,
    Precondition, PreparedOperation, Prerequisite, ReceiptError, Registry, ResolvedTarget, Target,
    Verb,
};
use serde_json::json;

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn selection(generation: u32) -> Target {
    Target {
        kind: fabric_v2_contracts::TargetKind::Item,
        owner_type: "Ontology".into(),
        facet: "model".into(),
        representation: "minimized".into(),
        generation: Some(generation),
    }
}

fn target(generation: u32) -> ResolvedTarget {
    ResolvedTarget::new(
        "/workspace.Workspace/item.Ontology/src/model",
        ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
        selection(generation),
        None,
    )
    .unwrap()
}

fn registry(availability: Availability) -> Registry {
    let mut registry = Registry::default();
    registry
        .register(
            OperationDescriptor::new(
                "model.get",
                Verb::Get,
                json!({"type": "object"}),
                json!({"type": "object"}),
                vec![Effect::RemoteRead],
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .bind(
            Binding::new(
                "Ontology",
                "model",
                "minimized",
                Generation::Exact(2),
                "model.get",
                availability,
            )
            .unwrap(),
        )
        .unwrap();
    registry
}

struct Policy {
    allow: bool,
    calls: usize,
}

impl ApprovalPolicy for Policy {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        self.calls += 1;
        assert_eq!(operation.operation_id(), "model.get");
        assert_eq!(operation.target().identity().item_id(), Some(ITEM));
        assert_eq!(operation.effects(), &[Effect::RemoteRead]);
        if self.allow {
            Ok(())
        } else {
            Err("host denied the exact request")
        }
    }
}

fn evidence(status: u16) -> ExecutionEvidence {
    ExecutionEvidence {
        http_status: Some(status),
        handles: vec![],
        coverage: Coverage::Complete,
        mutation_may_have_occurred: false,
    }
}

#[test]
fn approval_borrows_the_exact_immutable_request_without_a_default_allow() {
    let registry = registry(Availability::Conditional(vec![Prerequisite {
        id: "feature".into(),
        description: "Host must establish feature availability.".into(),
    }]));
    let resolution = registry.resolve(Verb::Get, &selection(2), None).unwrap();
    let request =
        PreparedOperation::new(resolution, target(2), json!({"form":"Minimized"})).unwrap();
    assert!(matches!(
        request.availability(),
        Availability::Conditional(_)
    ));
    let mut policy = Policy {
        allow: false,
        calls: 0,
    };
    assert_eq!(
        request.authorize(&mut policy).unwrap_err(),
        "host denied the exact request"
    );
    policy.allow = true;
    let approved = request.authorize(&mut policy).unwrap();
    assert!(std::ptr::eq(approved.prepared(), &request));
    assert_eq!(policy.calls, 2);
}

#[test]
fn unsupported_and_substituted_targets_cannot_be_prepared() {
    let registry = registry(Availability::Unsupported("Provider returns 501.".into()));
    let resolution = registry.resolve(Verb::Get, &selection(2), None).unwrap();
    assert_eq!(
        PreparedOperation::new(resolution, target(2), json!({})).unwrap_err(),
        ReceiptError::Unsupported
    );
    assert_eq!(
        PreparedOperation::new(resolution, target(1), json!({})).unwrap_err(),
        ReceiptError::TargetMismatch
    );
}

#[test]
fn accepted_receipts_require_a_real_handle_and_never_serialize_as_succeeded() {
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({}),
    )
    .unwrap();
    let approved = request
        .authorize(&mut Policy {
            allow: true,
            calls: 0,
        })
        .unwrap();
    assert_eq!(
        ExecutionReceipt::<serde_json::Value>::new(&approved, Outcome::Accepted, evidence(202))
            .unwrap_err()
            .kind,
        ReceiptError::MissingHandle
    );
    let mut observed = evidence(202);
    observed.handles.push(OperationHandle::FabricOperation {
        id: "native-operation-id".into(),
    });
    let receipt =
        ExecutionReceipt::<serde_json::Value>::new(&approved, Outcome::Accepted, observed.clone())
            .unwrap();
    let encoded = serde_json::to_value(&receipt).unwrap();
    assert_eq!(encoded["schemaVersion"], "0.1");
    assert_eq!(encoded["status"], "accepted");
    assert!(encoded.get("data").is_none());
    assert_eq!(receipt.evidence(), &observed);
    assert_eq!(receipt.target().identity().workspace_id(), Some(WORKSPACE));
}

#[test]
fn incomplete_denied_and_acceptance_only_evidence_cannot_be_healthy_success() {
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({}),
    )
    .unwrap();
    let approved = request
        .authorize(&mut Policy {
            allow: true,
            calls: 0,
        })
        .unwrap();
    for status in [100, 103, 199, 202, 300, 302, 307, 399, 401, 403, 429, 500] {
        assert_eq!(
            ExecutionReceipt::new(
                &approved,
                Outcome::Succeeded { data: json!([]) },
                evidence(status)
            )
            .unwrap_err()
            .kind,
            ReceiptError::InvalidEvidence
        );
    }
    let mut partial = evidence(200);
    partial.coverage = Coverage::Incomplete {
        reasons: vec!["The provider did not return all requested parts.".into()],
        continuation: None,
    };
    assert_eq!(
        ExecutionReceipt::new(
            &approved,
            Outcome::Succeeded { data: json!([]) },
            partial.clone()
        )
        .unwrap_err()
        .kind,
        ReceiptError::InvalidEvidence
    );
    let receipt = ExecutionReceipt::new(
        &approved,
        Outcome::Partial {
            data: json!([]),
            error: FailureInfo {
                code: "Incomplete".into(),
                message: "Some requested definition parts are unavailable.".into(),
            },
        },
        partial,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(receipt).unwrap()["status"], "partial");
}

#[test]
fn unknown_outcome_preserves_original_handles_and_possible_mutation() {
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({}),
    )
    .unwrap();
    let approved = request
        .authorize(&mut Policy {
            allow: true,
            calls: 0,
        })
        .unwrap();
    let observed = ExecutionEvidence {
        http_status: None,
        handles: vec![OperationHandle::FabricOperation {
            id: "original-handle".into(),
        }],
        coverage: Coverage::Unknown,
        mutation_may_have_occurred: true,
    };
    let receipt = ExecutionReceipt::<serde_json::Value>::new(
        &approved,
        Outcome::Unknown {
            error: FailureInfo {
                code: "ObservationTimeout".into(),
                message: "The final service outcome is unknown.".into(),
            },
        },
        observed.clone(),
    )
    .unwrap();
    assert_eq!(receipt.evidence(), &observed);
    let encoded = serde_json::to_value(receipt).unwrap();
    assert_eq!(encoded["status"], "unknown");
    assert_eq!(
        encoded["meta"]["evidence"]["handles"][0]["id"],
        "original-handle"
    );
}

#[test]
fn malformed_identity_and_input_debug_do_not_leak_request_content() {
    assert_eq!(
        ItemIdentity::new("not-a-workspace", ITEM).unwrap_err(),
        ReceiptError::InvalidIdentity
    );
    let identity = ItemIdentity::new(WORKSPACE, ITEM).unwrap();
    assert_eq!(
        ResolvedTarget::new("https://example.test", identity, selection(2), None).unwrap_err(),
        ReceiptError::InvalidIdentity
    );
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({"content":"customer-authored-content"}),
    )
    .unwrap();
    assert!(!format!("{request:?}").contains("customer-authored-content"));
}

#[test]
fn receipt_errors_retain_original_accepted_or_completed_execution_evidence() {
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({}),
    )
    .unwrap();
    let approved = request
        .authorize(&mut Policy {
            allow: true,
            calls: 0,
        })
        .unwrap();
    let mut observed = evidence(403);
    observed.handles.push(OperationHandle::FabricOperation {
        id: "original-handle".into(),
    });
    observed.mutation_may_have_occurred = true;
    let failure =
        ExecutionReceipt::<serde_json::Value>::new(&approved, Outcome::Accepted, observed.clone())
            .unwrap_err();
    assert_eq!(failure.kind, ReceiptError::InvalidEvidence);
    assert_eq!(failure.outcome, Outcome::Accepted);
    assert_eq!(failure.evidence, observed);
}

#[test]
fn native_revision_conditions_are_preserved_and_contradictions_rejected() {
    let registry = registry(Availability::Available);
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({}),
    )
    .unwrap();
    let revision = Precondition::NativeRevision {
        authority: "ontology-model".into(),
        value: "W/\"exact-native-revision\"".into(),
    };
    for conditions in [
        vec![Precondition::CreateOnly, Precondition::ExistingOnly],
        vec![revision.clone(), Precondition::UnguardedConcurrency],
        vec![revision.clone(), revision.clone()],
    ] {
        assert_eq!(
            request.clone().with_preconditions(conditions).unwrap_err(),
            ReceiptError::InvalidPrecondition
        );
    }
    let request = request.with_preconditions(vec![revision.clone()]).unwrap();
    let approved = request
        .authorize(&mut Policy {
            allow: true,
            calls: 0,
        })
        .unwrap();
    let receipt = ExecutionReceipt::new(
        &approved,
        Outcome::Succeeded { data: json!({}) },
        evidence(200),
    )
    .unwrap();
    assert_eq!(
        approved.prepared().preconditions(),
        std::slice::from_ref(&revision)
    );
    assert_eq!(receipt.preconditions(), &[revision]);
    let encoded = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        encoded["meta"]["preconditions"][0]["value"],
        "W/\"exact-native-revision\""
    );
}

#[test]
fn create_collection_keeps_real_parent_identity_without_fabricating_an_item() {
    use fabric_v2_contracts::{TargetKind, WorkspaceIdentity};

    struct CollectionPolicy;
    impl ApprovalPolicy for CollectionPolicy {
        type Error = &'static str;
        fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
            assert_eq!(
                operation.target().identity().workspace_id(),
                Some(WORKSPACE)
            );
            assert_eq!(operation.target().identity().item_id(), None);
            assert_eq!(operation.preconditions(), &[Precondition::CreateOnly]);
            Ok(())
        }
    }

    let mut registry = Registry::default();
    registry
        .register(
            OperationDescriptor::new(
                "item.create",
                Verb::Create,
                json!({"type":"object"}),
                json!({"type":"object"}),
                vec![Effect::RemoteWrite],
            )
            .unwrap(),
        )
        .unwrap();
    registry.alias("ontology", "Ontology").unwrap();
    registry
        .bind(
            Binding::new(
                "Ontology",
                "items",
                "json",
                Generation::Any,
                "item.create",
                Availability::Available,
            )
            .unwrap()
            .with_target_kind(TargetKind::WorkspaceCollection),
        )
        .unwrap();
    let collection = Target {
        kind: TargetKind::WorkspaceCollection,
        owner_type: "Ontology".into(),
        facet: "items".into(),
        representation: "json".into(),
        generation: None,
    };
    let resolved = ResolvedTarget::workspace_collection(
        "/workspace.Workspace/new.Ontology",
        WorkspaceIdentity::new(WORKSPACE).unwrap(),
        collection.clone(),
    )
    .unwrap();
    let prepared = PreparedOperation::new(
        registry
            .resolve(Verb::Create, &collection, Some("ontology"))
            .unwrap(),
        resolved,
        json!({"displayName":"new"}),
    )
    .unwrap()
    .with_preconditions(vec![Precondition::CreateOnly])
    .unwrap();
    let approved = prepared.authorize(&mut CollectionPolicy).unwrap();
    let mut evidence = evidence(202);
    evidence.handles.push(OperationHandle::FabricOperation {
        id: "original-operation".into(),
    });
    let receipt =
        ExecutionReceipt::<serde_json::Value>::new(&approved, Outcome::Accepted, evidence).unwrap();
    let encoded = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        encoded["meta"]["target"]["identity"]["kind"],
        "workspaceCollection"
    );
    assert!(
        encoded["meta"]["target"]["identity"]
            .get("itemId")
            .is_none()
    );
    let mut item_selector = collection.clone();
    item_selector.kind = TargetKind::Item;
    assert!(
        registry
            .resolve(Verb::Create, &item_selector, None)
            .is_err()
    );
    assert_eq!(
        ResolvedTarget::new(
            "/workspace.Workspace/item.Ontology",
            ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
            collection,
            None,
        )
        .unwrap_err(),
        ReceiptError::TargetMismatch
    );
}

#[test]
fn explicit_file_effect_is_approved_without_removing_required_remote_effects() {
    struct FilePolicy;
    impl ApprovalPolicy for FilePolicy {
        type Error = &'static str;
        fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
            assert_eq!(
                operation.effects(),
                &[Effect::RemoteRead, Effect::LocalWrite]
            );
            assert_eq!(operation.input()["output"]["path"], "explicit-output.json");
            Ok(())
        }
    }

    let mut registry = Registry::default();
    registry
        .register(
            OperationDescriptor::new(
                "model.get",
                Verb::Get,
                json!({"type":"object"}),
                json!({"type":"object"}),
                vec![Effect::RemoteRead],
            )
            .unwrap()
            .with_optional_effects(vec![Effect::LocalWrite])
            .unwrap(),
        )
        .unwrap();
    registry
        .bind(
            Binding::new(
                "Ontology",
                "model",
                "minimized",
                Generation::Exact(2),
                "model.get",
                Availability::Available,
            )
            .unwrap(),
        )
        .unwrap();
    let request = PreparedOperation::new(
        registry.resolve(Verb::Get, &selection(2), None).unwrap(),
        target(2),
        json!({"output":{"kind":"file","path":"explicit-output.json"}}),
    )
    .unwrap();
    assert_eq!(request.effects(), &[Effect::RemoteRead]);
    assert_eq!(
        request
            .clone()
            .with_optional_effects(vec![Effect::RemoteWrite])
            .unwrap_err(),
        ReceiptError::InvalidEffects
    );
    assert_eq!(
        request
            .clone()
            .with_optional_effects(vec![Effect::LocalWrite, Effect::LocalWrite])
            .unwrap_err(),
        ReceiptError::InvalidEffects
    );
    let file = request
        .with_optional_effects(vec![Effect::LocalWrite])
        .unwrap();
    let approved = file.authorize(&mut FilePolicy).unwrap();
    assert!(std::ptr::eq(approved.prepared(), &file));
    let stdout = file.with_optional_effects(vec![]).unwrap();
    assert_eq!(stdout.effects(), &[Effect::RemoteRead]);
}
