// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline external-consumer create contracts; fixture schemas are not native adapters.

#![deny(unnameable_test_items)]

use fabric_sdk_items::create::{
    AcceptedHandle, ConflictPolicy, CreateAction, CreateCapabilities, CreateError,
    CreateInputValidator, CreatePlan, CreateSupport, CreateTarget, DuplicateSupport,
    ExecutionOutcome, InputDisposition, LeafState, MutationEffect, NativeId, ObservationEvidence,
    Preflight, ReplacementEvidence, ReplacementUnit, ResourceIdentity, ResourceResult, SchemaRef,
    SkipSupport, TargetScope, VerifiedOutcome, plan_create, verify_outcome,
};
use serde_json::{Value, json};

struct FixtureSchema;

impl CreateInputValidator for FixtureSchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.item_type() != "Fixture.Item" || schema.as_str() != "fixture/item/v1" {
            return Err(CreateError::InputRejected);
        }
        let Some(supplied) = supplied else {
            return Ok(InputDisposition::CreateOnly);
        };
        let object = supplied.as_object().ok_or(CreateError::InputRejected)?;
        if object.is_empty()
            || object
                .keys()
                .any(|key| key != "content" && key != "description")
            || object.values().any(|value| !value.is_string())
        {
            return Err(CreateError::InputRejected);
        }
        if object.contains_key("content") {
            Ok(InputDisposition::CreateAndReplace(unit()))
        } else {
            Ok(InputDisposition::CreateOnly)
        }
    }
}

struct MetadataSchema;

impl CreateInputValidator for MetadataSchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.item_type() != "Fixture.Item" || schema.as_str() != "fixture/metadata/v1" {
            return Err(CreateError::InputRejected);
        }
        let object = supplied
            .and_then(Value::as_object)
            .ok_or(CreateError::InputRejected)?;
        if object.len() != 1 || !object.get("description").is_some_and(Value::is_string) {
            return Err(CreateError::InputRejected);
        }
        Ok(InputDisposition::CreateAndReplace(metadata_unit()))
    }
}

struct ReplacementOnlySchema;

impl CreateInputValidator for ReplacementOnlySchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        match FixtureSchema.validate(target, schema, supplied)? {
            InputDisposition::CreateAndReplace(unit) => Ok(InputDisposition::ReplaceOnly(unit)),
            _ => Err(CreateError::InputRejected),
        }
    }
}

struct DefaultContentSchema;

impl CreateInputValidator for DefaultContentSchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let disposition = FixtureSchema.validate(target, schema, supplied)?;
        if supplied.is_none() {
            Ok(InputDisposition::CreateAndReplace(unit()))
        } else {
            Ok(disposition)
        }
    }
}

struct SkipOnlySchema;

impl CreateInputValidator for SkipOnlySchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        match FixtureSchema.validate(target, schema, supplied)? {
            InputDisposition::CreateOnly if supplied.is_some() => Ok(InputDisposition::SkipOnly),
            _ => Err(CreateError::InputRejected),
        }
    }
}

fn id(value: &str) -> NativeId {
    NativeId::new(value).expect("fixture native ID")
}

fn unit() -> ReplacementUnit {
    ReplacementUnit::new("content/v1").expect("fixture replacement unit")
}

fn metadata_unit() -> ReplacementUnit {
    ReplacementUnit::new("metadata/v1").expect("fixture metadata unit")
}

fn target(scope: TargetScope) -> CreateTarget {
    CreateTarget::new(scope, id("parent:東京"), "résumé/分析", "Fixture.Item")
        .expect("fixture target")
}

fn identity(value: &str) -> ResourceIdentity {
    ResourceIdentity::new(id(value), id("parent:東京"), "Fixture.Item")
        .expect("fixture resource identity")
}

fn capabilities() -> CreateCapabilities {
    CreateCapabilities {
        schema: SchemaRef::new("fixture/item/v1").expect("fixture schema"),
        create: CreateSupport::RejectExisting,
        replacement: Some(unit()),
        skip: SkipSupport::ExactExistingUnchanged,
        duplicate: DuplicateSupport::FreshIdentityWithRepeatedNames,
    }
}

fn complete_input() -> Option<Value> {
    Some(json!({"content": "complete advertised content"}))
}

fn existing() -> Preflight {
    Preflight::Leaf(LeafState::Existing(identity("native:existing")))
}

fn missing() -> Preflight {
    Preflight::Leaf(LeafState::Missing)
}

fn plan(
    policy: ConflictPolicy,
    preflight: Preflight,
    supplied: Option<Value>,
) -> Result<CreatePlan, CreateError> {
    plan_create(
        policy,
        target(TargetScope::NamedLeaf),
        preflight,
        &capabilities(),
        supplied,
        &FixtureSchema,
    )
}

fn duplicate_plan(known_matches: Vec<ResourceIdentity>) -> CreatePlan {
    plan_create(
        ConflictPolicy::Duplicate,
        target(TargetScope::ParentCollection),
        Preflight::Collection { known_matches },
        &capabilities(),
        complete_input(),
        &FixtureSchema,
    )
    .expect("duplicate plan")
}

fn result(value: &str) -> ResourceResult {
    ResourceResult {
        native_id: Some(id(value)),
        parent_id: Some(id("parent:東京")),
        item_type: Some("Fixture.Item".into()),
        name: Some("résumé/分析".into()),
        replacement: ReplacementEvidence::Unverified,
    }
}

fn observation(resource: Option<ResourceResult>) -> ObservationEvidence {
    ObservationEvidence {
        operation_handle: None,
        resource,
        effect: MutationEffect::MayHaveOccurred,
    }
}

fn completed(evidence: ResourceResult) -> ExecutionOutcome<&'static str> {
    ExecutionOutcome::Completed(observation(Some(evidence)))
}

fn assert_no_mutation_completion_rejected(planned: &CreatePlan, resource: ResourceResult) {
    let evidence = ObservationEvidence {
        operation_handle: Some(
            AcceptedHandle::new("provider-operation:contradictory-completion").expect("handle"),
        ),
        resource: Some(resource),
        effect: MutationEffect::NotOccurred,
    };
    let original: ExecutionOutcome<&str> = ExecutionOutcome::Completed(evidence.clone());
    let rejected = verify_outcome(planned, original.clone()).expect_err("no mutation occurred");
    assert_eq!(rejected.error, CreateError::ContradictoryMutationEvidence);
    assert_eq!(
        rejected.error.to_string(),
        "Completed mutation contradicts evidence that no mutation occurred."
    );
    assert_eq!(*rejected.outcome, original);
    assert_eq!(rejected.outcome.evidence(), Some(&evidence));
}

#[test]
fn policy_default_and_exact_spellings() {
    assert_eq!(ConflictPolicy::default(), ConflictPolicy::Error);
    for (spelling, policy) in [
        ("error", ConflictPolicy::Error),
        ("overwrite", ConflictPolicy::Overwrite),
        ("skip", ConflictPolicy::Skip),
        ("duplicate", ConflictPolicy::Duplicate),
    ] {
        assert_eq!(ConflictPolicy::parse(spelling), Ok(policy));
        assert_eq!(spelling.parse::<ConflictPolicy>(), Ok(policy));
        assert_eq!(policy.as_str(), spelling);
    }
    for invalid in ["force", "", "ERROR", " skip", "duplicate "] {
        assert_eq!(
            ConflictPolicy::parse(invalid),
            Err(CreateError::InvalidPolicy)
        );
    }
}

#[test]
fn native_identifiers_are_not_assumed_to_be_guids() {
    for value in ["native:東京/分析", "opaque id", "urn:fixture:résumé"] {
        assert_eq!(id(value).as_str(), value);
    }
    for invalid in ["", "  ", "native\nid", "native\u{7f}id", "\0"] {
        assert_eq!(NativeId::new(invalid), Err(CreateError::InvalidIdentifier));
    }
}

#[test]
fn target_preserves_unicode_native_selectors_and_path_spelling() {
    let name = "ré\u{301}sumé/分析\\資料.Item";
    let selected = CreateTarget::new(
        TargetScope::NamedLeaf,
        id("opaque parent:東京"),
        name,
        "Future.種別",
    )
    .expect("Unicode selector");
    assert_eq!(selected.name(), name);
    assert_eq!(selected.parent_id().as_str(), "opaque parent:東京");
    assert_eq!(selected.item_type(), "Future.種別");
    assert_eq!(selected.scope(), TargetScope::NamedLeaf);
    assert_eq!(
        CreateTarget::new(TargetScope::NamedLeaf, id("parent"), "", "Kind"),
        Err(CreateError::InvalidTarget)
    );
    assert_eq!(
        ResourceIdentity::new(id("resource"), id("parent"), "Kind\n"),
        Err(CreateError::InvalidTarget)
    );
}

#[test]
fn schema_and_replacement_references_are_bounded() {
    for invalid in ["".to_owned(), "x".repeat(257), "schema\n".to_owned()] {
        assert_eq!(
            SchemaRef::new(invalid.clone()),
            Err(CreateError::InvalidSchema)
        );
        assert_eq!(
            ReplacementUnit::new(invalid),
            Err(CreateError::InvalidReplacementUnit)
        );
    }
    assert_eq!(unit().as_str(), "content/v1");
    assert!(SchemaRef::new("x".repeat(256)).is_ok());
}

#[test]
fn default_missing_leaf_plans_creation_and_retains_validated_binding() {
    let planned = plan(ConflictPolicy::default(), missing(), None).expect("missing leaf");
    assert_eq!(planned.policy(), ConflictPolicy::Error);
    assert_eq!(planned.target(), &target(TargetScope::NamedLeaf));
    let CreateAction::Create { input } = planned.action() else {
        panic!("missing leaf must create");
    };
    assert_eq!(input.target(), planned.target());
    assert_eq!(input.schema().as_str(), "fixture/item/v1");
    assert_eq!(input.supplied(), None);
    assert_eq!(input.disposition(), &InputDisposition::CreateOnly);
}

#[test]
fn default_existing_leaf_is_conflict_even_with_complete_input() {
    assert_eq!(
        plan(ConflictPolicy::Error, existing(), complete_input()),
        Err(CreateError::AlreadyExists)
    );
}

#[test]
fn overwrite_missing_creates_instead_of_upserting_or_requiring_replacement() {
    for supplied in [
        None,
        Some(json!({"description": "creation defaults are allowed"})),
    ] {
        let planned = plan(ConflictPolicy::Overwrite, missing(), supplied).expect("force missing");
        assert_eq!(planned.policy(), ConflictPolicy::Overwrite);
        assert!(matches!(planned.action(), CreateAction::Create { .. }));
    }
}

#[test]
fn overwrite_existing_replaces_exact_full_unit_and_identity() {
    let planned =
        plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("full replacement");
    let CreateAction::Replace {
        existing,
        unit: advertised,
        input,
    } = planned.action()
    else {
        panic!("force existing must replace in place");
    };
    assert_eq!(existing, &identity("native:existing"));
    assert_eq!(advertised, &unit());
    assert_eq!(input.supplied(), complete_input().as_ref());
    assert_eq!(
        input.disposition(),
        &InputDisposition::CreateAndReplace(unit())
    );
}

#[test]
fn overwrite_rejects_no_input_partial_input_and_invalid_empty_input() {
    for supplied in [None, Some(json!({"description": "partial"}))] {
        assert_eq!(
            plan(ConflictPolicy::Overwrite, existing(), supplied),
            Err(CreateError::IncompleteReplacement)
        );
    }
    for supplied in [json!({}), Value::Null, json!({"content": 7})] {
        assert_eq!(
            plan(ConflictPolicy::Overwrite, existing(), Some(supplied)),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn replacement_validation_must_match_the_advertised_unit() {
    let mut support = capabilities();
    support.replacement = Some(metadata_unit());
    assert_eq!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            existing(),
            &support,
            complete_input(),
            &FixtureSchema,
        ),
        Err(CreateError::IncompleteReplacement)
    );
}

#[test]
fn metadata_only_resource_requires_explicit_complete_metadata_schema() {
    let mut support = capabilities();
    support.schema = SchemaRef::new("fixture/metadata/v1").expect("metadata schema");
    support.replacement = Some(metadata_unit());
    let planned = plan_create(
        ConflictPolicy::Overwrite,
        target(TargetScope::NamedLeaf),
        existing(),
        &support,
        Some(json!({"description": ""})),
        &MetadataSchema,
    )
    .expect("complete metadata replacement, including an explicit empty description");
    assert!(matches!(
        planned.action(),
        CreateAction::Replace { unit, .. } if unit == &metadata_unit()
    ));
    for supplied in [
        None,
        Some(json!({})),
        Some(json!({"content": "not metadata"})),
    ] {
        assert_eq!(
            plan_create(
                ConflictPolicy::Overwrite,
                target(TargetScope::NamedLeaf),
                existing(),
                &support,
                supplied,
                &MetadataSchema,
            ),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn skip_existing_retains_identity_without_claiming_content_equality() {
    let planned = plan(
        ConflictPolicy::Skip,
        existing(),
        Some(json!({"content": "deliberately different"})),
    )
    .expect("skip does not compare contents");
    assert_eq!(
        planned.action(),
        &CreateAction::Skip {
            existing: identity("native:existing"),
        }
    );
}

#[test]
fn skip_validates_explicit_input_before_any_conflict_handling() {
    for observed in [
        existing(),
        missing(),
        Preflight::MissingParent,
        Preflight::Leaf(LeafState::Ambiguous),
    ] {
        assert_eq!(
            plan(
                ConflictPolicy::Skip,
                observed,
                Some(json!({"unknown_property": "invalid schema input"})),
            ),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn skip_missing_leaf_creates_using_validated_creation_input() {
    let planned = plan(ConflictPolicy::Skip, missing(), None).expect("skip missing");
    assert_eq!(planned.policy(), ConflictPolicy::Skip);
    assert!(matches!(planned.action(), CreateAction::Create { .. }));
}

#[test]
fn duplicate_is_always_fresh_collection_creation_for_zero_one_or_many_matches() {
    for matches in [
        vec![],
        vec![identity("first")],
        vec![identity("first"), identity("second")],
    ] {
        let planned = duplicate_plan(matches.clone());
        assert_eq!(planned.policy(), ConflictPolicy::Duplicate);
        assert_eq!(planned.target().scope(), TargetScope::ParentCollection);
        let CreateAction::Duplicate {
            known_matches,
            input,
        } = planned.action()
        else {
            panic!("duplicate must never switch action based on match count");
        };
        assert_eq!(known_matches, &matches);
        assert_eq!(input.target().name(), "résumé/分析");
    }
}

#[test]
fn named_duplicate_is_rejected_even_when_absent_or_ambiguous() {
    for observed in [missing(), existing(), Preflight::Leaf(LeafState::Ambiguous)] {
        assert_eq!(
            plan(ConflictPolicy::Duplicate, observed, complete_input()),
            Err(CreateError::DuplicateRequiresCollection)
        );
    }
}

#[test]
fn duplicate_requires_explicit_repeated_name_fresh_identity_guarantee() {
    let mut support = capabilities();
    support.duplicate = DuplicateSupport::Unsupported;
    for matches in [vec![], vec![identity("existing")]] {
        assert_eq!(
            plan_create(
                ConflictPolicy::Duplicate,
                target(TargetScope::ParentCollection),
                Preflight::Collection {
                    known_matches: matches,
                },
                &support,
                complete_input(),
                &FixtureSchema,
            ),
            Err(CreateError::UnsupportedPolicy)
        );
    }
}

#[test]
fn unsupported_policies_fail_even_for_missing_targets() {
    for policy in [
        ConflictPolicy::Error,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Skip,
        ConflictPolicy::Duplicate,
    ] {
        let mut support = capabilities();
        match policy {
            ConflictPolicy::Error => support.create = CreateSupport::Unsupported,
            ConflictPolicy::Overwrite => support.replacement = None,
            ConflictPolicy::Skip => support.skip = SkipSupport::Unsupported,
            ConflictPolicy::Duplicate => support.duplicate = DuplicateSupport::Unsupported,
        }
        let (selected, observed) = if policy == ConflictPolicy::Duplicate {
            (
                target(TargetScope::ParentCollection),
                Preflight::Collection {
                    known_matches: vec![],
                },
            )
        } else {
            (target(TargetScope::NamedLeaf), missing())
        };
        assert_eq!(
            plan_create(
                policy,
                selected,
                observed,
                &support,
                complete_input(),
                &FixtureSchema,
            ),
            Err(CreateError::UnsupportedPolicy)
        );
    }
}

#[test]
fn missing_leaf_does_not_imply_provider_creation_guarantees() {
    let mut support = capabilities();
    support.create = CreateSupport::Unsupported;
    for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Skip] {
        assert_eq!(
            plan_create(
                policy,
                target(TargetScope::NamedLeaf),
                missing(),
                &support,
                complete_input(),
                &FixtureSchema,
            ),
            Err(CreateError::UnsupportedCreation)
        );
    }
    assert!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            existing(),
            &support,
            complete_input(),
            &FixtureSchema,
        )
        .is_ok()
    );
}

#[test]
fn missing_parent_is_never_created_skipped_or_forced() {
    for policy in [
        ConflictPolicy::Error,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Skip,
        ConflictPolicy::Duplicate,
    ] {
        let scope = if policy == ConflictPolicy::Duplicate {
            TargetScope::ParentCollection
        } else {
            TargetScope::NamedLeaf
        };
        assert_eq!(
            plan_create(
                policy,
                target(scope),
                Preflight::MissingParent,
                &capabilities(),
                complete_input(),
                &FixtureSchema,
            ),
            Err(CreateError::MissingParent)
        );
    }
}

#[test]
fn ambiguity_and_wrong_kind_are_never_skipped_or_forced() {
    for policy in [
        ConflictPolicy::Error,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Skip,
    ] {
        assert_eq!(
            plan(
                policy,
                Preflight::Leaf(LeafState::Ambiguous),
                complete_input(),
            ),
            Err(CreateError::AmbiguousLeaf)
        );
        assert_eq!(
            plan(
                policy,
                Preflight::Leaf(LeafState::WrongKind),
                complete_input(),
            ),
            Err(CreateError::WrongKind)
        );
        let wrong_kind = ResourceIdentity::new(id("existing"), id("parent:東京"), "Different.Kind")
            .expect("wrong-kind identity");
        assert_eq!(
            plan(
                policy,
                Preflight::Leaf(LeafState::Existing(wrong_kind)),
                complete_input(),
            ),
            Err(CreateError::WrongKind)
        );
    }
}

#[test]
fn preflight_identity_must_belong_to_exact_native_parent() {
    let elsewhere = ResourceIdentity::new(id("existing"), id("other-parent"), "Fixture.Item")
        .expect("other parent identity");
    for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Skip] {
        assert_eq!(
            plan(
                policy,
                Preflight::Leaf(LeafState::Existing(elsewhere.clone())),
                complete_input(),
            ),
            Err(CreateError::ParentMismatch)
        );
    }
    assert_eq!(
        plan_create(
            ConflictPolicy::Duplicate,
            target(TargetScope::ParentCollection),
            Preflight::Collection {
                known_matches: vec![elsewhere],
            },
            &capabilities(),
            complete_input(),
            &FixtureSchema,
        ),
        Err(CreateError::ParentMismatch)
    );
}

#[test]
fn selection_intent_and_preflight_shape_must_match() {
    for policy in [
        ConflictPolicy::Error,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Skip,
    ] {
        assert_eq!(
            plan_create(
                policy,
                target(TargetScope::ParentCollection),
                Preflight::Collection {
                    known_matches: vec![],
                },
                &capabilities(),
                complete_input(),
                &FixtureSchema,
            ),
            Err(CreateError::SelectionMismatch)
        );
        assert_eq!(
            plan(
                policy,
                Preflight::Collection {
                    known_matches: vec![],
                },
                complete_input(),
            ),
            Err(CreateError::SelectionMismatch)
        );
    }
    assert_eq!(
        plan_create(
            ConflictPolicy::Duplicate,
            target(TargetScope::ParentCollection),
            missing(),
            &capabilities(),
            complete_input(),
            &FixtureSchema,
        ),
        Err(CreateError::DuplicateRequiresCollection)
    );
}

#[test]
fn replacement_only_validated_input_cannot_create() {
    assert_eq!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            missing(),
            &capabilities(),
            complete_input(),
            &ReplacementOnlySchema,
        ),
        Err(CreateError::InputCannotCreate)
    );
    assert!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            existing(),
            &capabilities(),
            complete_input(),
            &ReplacementOnlySchema,
        )
        .is_ok()
    );
}

#[test]
fn adapter_defaults_cannot_make_omitted_input_a_complete_replacement() {
    assert_eq!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            existing(),
            &capabilities(),
            None,
            &DefaultContentSchema,
        ),
        Err(CreateError::IncompleteReplacement)
    );
    assert!(
        plan_create(
            ConflictPolicy::Overwrite,
            target(TargetScope::NamedLeaf),
            missing(),
            &capabilities(),
            None,
            &DefaultContentSchema,
        )
        .is_ok()
    );
}

#[test]
fn valid_skip_only_input_does_not_authorize_creation_or_replacement() {
    for (policy, observed, expected) in [
        (
            ConflictPolicy::Skip,
            missing(),
            CreateError::InputCannotCreate,
        ),
        (
            ConflictPolicy::Overwrite,
            existing(),
            CreateError::IncompleteReplacement,
        ),
    ] {
        assert_eq!(
            plan_create(
                policy,
                target(TargetScope::NamedLeaf),
                observed,
                &capabilities(),
                Some(json!({"description": "valid but not mutation input"})),
                &SkipOnlySchema,
            ),
            Err(expected)
        );
    }
    let planned = plan_create(
        ConflictPolicy::Skip,
        target(TargetScope::NamedLeaf),
        existing(),
        &capabilities(),
        Some(json!({"description": "valid but not mutation input"})),
        &SkipOnlySchema,
    )
    .expect("valid input still allows exact unchanged skip");
    assert!(matches!(planned.action(), CreateAction::Skip { .. }));
}

#[test]
fn schema_binding_rejects_unrecognized_target_and_revision() {
    let mut support = capabilities();
    support.schema = SchemaRef::new("fixture/item/v2").expect("unknown revision");
    assert_eq!(
        plan_create(
            ConflictPolicy::Skip,
            target(TargetScope::NamedLeaf),
            existing(),
            &support,
            complete_input(),
            &FixtureSchema,
        ),
        Err(CreateError::InputRejected)
    );
    assert_eq!(
        plan_create(
            ConflictPolicy::Error,
            CreateTarget::new(
                TargetScope::NamedLeaf,
                id("parent:東京"),
                "résumé/分析",
                "Unknown.Kind",
            )
            .expect("unknown target kind"),
            missing(),
            &capabilities(),
            complete_input(),
            &FixtureSchema,
        ),
        Err(CreateError::InputRejected)
    );
}

#[test]
fn completed_creation_requires_final_identity_and_name() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    let mut evidence = result("new");
    evidence.native_id = None;
    let rejected = verify_outcome(&planned, completed(evidence.clone())).expect_err("missing ID");
    assert_eq!(rejected.error, CreateError::MissingFinalIdentity);
    assert_eq!(*rejected.outcome, completed(evidence));
    let mut evidence = result("new");
    evidence.name = None;
    assert_eq!(
        verify_outcome(&planned, completed(evidence))
            .expect_err("missing name")
            .error,
        CreateError::MissingFinalName
    );
    let verified = verify_outcome(&planned, completed(result("new"))).expect("completed creation");
    let VerifiedOutcome::Completed(resource) = verified else {
        panic!("verified completion");
    };
    assert_eq!(resource.identity(), &identity("new"));
    assert_eq!(resource.name(), "résumé/分析");
    assert_eq!(resource.evidence(), &observation(Some(result("new"))));
}

#[test]
fn completed_creation_checks_parent_kind_and_exact_name() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    for (parent, kind, expected) in [
        ("other", "Fixture.Item", CreateError::ParentMismatch),
        ("parent:東京", "Other.Kind", CreateError::WrongKind),
    ] {
        let mut evidence = result("new");
        evidence.parent_id = Some(id(parent));
        evidence.item_type = Some(kind.into());
        assert_eq!(
            verify_outcome(&planned, completed(evidence))
                .expect_err("mismatched result")
                .error,
            expected
        );
    }
    let mut evidence = result("new");
    evidence.name = Some("résumé/分析 (2)".into());
    assert_eq!(
        verify_outcome(&planned, completed(evidence))
            .expect_err("auto-renaming forbidden")
            .error,
        CreateError::NameChanged
    );
}

#[test]
fn replacement_changed_id_is_rejected_with_original_evidence() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    let mut evidence = result("delete-recreate-id");
    evidence.replacement = ReplacementEvidence::Verified(unit());
    let rejected =
        verify_outcome(&planned, completed(evidence.clone())).expect_err("identity changed");
    assert_eq!(rejected.error, CreateError::IdentityChanged);
    assert_eq!(*rejected.outcome, completed(evidence));
}

#[test]
fn replacement_changed_parent_or_kind_is_not_success() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    for (parent, kind, expected) in [
        ("moved-parent", "Fixture.Item", CreateError::ParentMismatch),
        ("parent:東京", "Different.Kind", CreateError::WrongKind),
    ] {
        let mut evidence = result("native:existing");
        evidence.parent_id = Some(id(parent));
        evidence.item_type = Some(kind.into());
        evidence.replacement = ReplacementEvidence::Verified(unit());
        assert_eq!(
            verify_outcome(&planned, completed(evidence))
                .expect_err("replacement identity guard")
                .error,
            expected
        );
    }
}

#[test]
fn replacement_requires_full_exact_unit_and_preservation_evidence() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    for verification in [
        ReplacementEvidence::Unverified,
        ReplacementEvidence::Verified(metadata_unit()),
    ] {
        let mut evidence = result("native:existing");
        evidence.replacement = verification;
        assert_eq!(
            verify_outcome(&planned, completed(evidence))
                .expect_err("not full verified replacement")
                .error,
            CreateError::ReplacementUnverified
        );
    }
    let mut evidence = result("native:existing");
    evidence.replacement = ReplacementEvidence::Verified(unit());
    let verified = verify_outcome(&planned, completed(evidence)).expect("verified replacement");
    assert!(matches!(
        verified,
        VerifiedOutcome::Completed(resource) if resource.identity() == &identity("native:existing")
    ));
}

#[test]
fn duplicate_rejects_every_observed_id_and_accepts_a_new_identity() {
    let planned = duplicate_plan(vec![identity("first"), identity("second")]);
    for reused in ["first", "second"] {
        assert_eq!(
            verify_outcome(&planned, completed(result(reused)))
                .expect_err("duplicate cannot reuse identity")
                .error,
            CreateError::DuplicateIdentityReused
        );
    }
    assert!(matches!(
        verify_outcome(&planned, completed(result("third"))).expect("fresh identity"),
        VerifiedOutcome::Completed(_)
    ));
}

#[test]
fn duplicate_never_accepts_auto_rename_even_with_no_prior_matches() {
    let planned = duplicate_plan(vec![]);
    let mut evidence = result("fresh");
    evidence.name = Some("résumé/分析 (copy)".into());
    assert_eq!(
        verify_outcome(&planned, completed(evidence))
            .expect_err("no automatic renaming")
            .error,
        CreateError::NameChanged
    );
}

#[test]
fn accepted_is_not_completed_or_a_resource_identifier() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    let handle = AcceptedHandle::new("provider-operation:123").expect("accepted reference");
    let evidence = ObservationEvidence {
        operation_handle: Some(handle.clone()),
        resource: None,
        effect: MutationEffect::MayHaveOccurred,
    };
    let observed: ExecutionOutcome<&str> = ExecutionOutcome::Accepted(evidence.clone());
    assert_eq!(observed.evidence(), Some(&evidence));
    assert_eq!(
        verify_outcome(&planned, observed),
        Ok(VerifiedOutcome::Accepted(evidence))
    );
    assert_eq!(handle.as_str(), "provider-operation:123");
    assert_ne!(format!("{handle:?}"), handle.as_str());
    for invalid in ["".to_owned(), "x".repeat(2049), "op\n".to_owned()] {
        assert_eq!(
            AcceptedHandle::new(invalid),
            Err(CreateError::InvalidAcceptedHandle)
        );
    }
}

#[test]
fn skip_accepts_only_unchanged_outcome_not_a_mutation_claim() {
    let skipped = plan(ConflictPolicy::Skip, existing(), None).expect("skip");
    let unchanged: ExecutionOutcome<&str> = ExecutionOutcome::Skipped;
    assert_eq!(
        verify_outcome(&skipped, unchanged),
        Ok(VerifiedOutcome::Skipped(identity("native:existing")))
    );
    assert_eq!(
        verify_outcome(&skipped, completed(result("native:existing")))
            .expect_err("skip must not claim a completed mutation")
            .error,
        CreateError::UnexpectedOutcome
    );
    let accepted: ExecutionOutcome<&str> = ExecutionOutcome::Accepted(ObservationEvidence {
        operation_handle: Some(AcceptedHandle::new("operation:skip").expect("reference")),
        resource: None,
        effect: MutationEffect::MayHaveOccurred,
    });
    let rejected =
        verify_outcome(&skipped, accepted.clone()).expect_err("skip cannot accept a mutation");
    assert_eq!(rejected.error, CreateError::UnexpectedOutcome);
    assert_eq!(*rejected.outcome, accepted);
    let created = plan(ConflictPolicy::Skip, missing(), None).expect("skip missing creates");
    let unchanged: ExecutionOutcome<&str> = ExecutionOutcome::Skipped;
    assert_eq!(
        verify_outcome(&created, unchanged)
            .expect_err("missing leaf cannot be reported skipped")
            .error,
        CreateError::UnexpectedOutcome
    );
}

#[test]
fn partial_unknown_and_typed_failures_remain_evidence_not_success() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    let partial = ResourceResult {
        native_id: None,
        parent_id: None,
        item_type: None,
        name: None,
        replacement: ReplacementEvidence::Unverified,
    };
    let evidence = observation(Some(partial));
    let observed: ExecutionOutcome<(u16, &'static str)> =
        ExecutionOutcome::Partial(evidence.clone());
    assert_eq!(
        verify_outcome(&planned, observed),
        Ok(VerifiedOutcome::Partial(evidence.clone()))
    );
    let unknown: ExecutionOutcome<(u16, &'static str)> =
        ExecutionOutcome::Unknown(evidence.clone());
    assert_eq!(
        verify_outcome(&planned, unknown),
        Ok(VerifiedOutcome::Unknown(evidence.clone()))
    );
    for effect in [MutationEffect::NotOccurred, MutationEffect::MayHaveOccurred] {
        let mut failure_evidence = evidence.clone();
        failure_evidence.effect = effect;
        let unknown: ExecutionOutcome<&str> = ExecutionOutcome::Unknown(failure_evidence.clone());
        assert_eq!(
            verify_outcome(&planned, unknown),
            Ok(VerifiedOutcome::Unknown(failure_evidence.clone()))
        );
        assert_eq!(
            verify_outcome(
                &planned,
                ExecutionOutcome::Failed {
                    error: (409, "structured-fixture-code"),
                    evidence: failure_evidence.clone(),
                },
            ),
            Ok(VerifiedOutcome::Failed {
                error: (409, "structured-fixture-code"),
                evidence: failure_evidence,
            })
        );
    }
}

#[test]
fn accepted_then_timeout_retains_exact_handle_resource_and_effect() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    let evidence = ObservationEvidence {
        operation_handle: Some(
            AcceptedHandle::new("provider-operation:accepted-before-timeout").expect("handle"),
        ),
        resource: Some(result("native:known-before-completion")),
        effect: MutationEffect::MayHaveOccurred,
    };
    let accepted: ExecutionOutcome<(u16, &str)> = ExecutionOutcome::Accepted(evidence.clone());
    let accepted = verify_outcome(&planned, accepted).expect("accepted remains accepted");
    assert_eq!(accepted, VerifiedOutcome::Accepted(evidence.clone()));
    let retained = accepted.evidence().expect("accepted evidence").clone();
    let timeout = ExecutionOutcome::Failed {
        error: (408, "fixture_timeout"),
        evidence: retained,
    };
    let timeout = verify_outcome(&planned, timeout).expect("typed timeout is retained");
    assert_eq!(
        timeout,
        VerifiedOutcome::Failed {
            error: (408, "fixture_timeout"),
            evidence: evidence.clone(),
        }
    );
    assert_eq!(timeout.evidence(), Some(&evidence));
    let unknown: ExecutionOutcome<(u16, &str)> = ExecutionOutcome::Unknown(evidence.clone());
    let unknown = verify_outcome(&planned, unknown).expect("unknown is not completion");
    assert_eq!(unknown, VerifiedOutcome::Unknown(evidence.clone()));
    assert_eq!(unknown.evidence(), Some(&evidence));
}

#[test]
fn partial_then_failure_retains_known_id_and_name_without_parent_or_kind() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    let evidence = ObservationEvidence {
        operation_handle: Some(
            AcceptedHandle::new("provider-operation:partial-before-failure").expect("handle"),
        ),
        resource: Some(ResourceResult {
            native_id: Some(id("native:partial-分析")),
            parent_id: None,
            item_type: None,
            name: Some("résumé/分析".into()),
            replacement: ReplacementEvidence::Unverified,
        }),
        effect: MutationEffect::MayHaveOccurred,
    };
    let partial: ExecutionOutcome<(u16, &str)> = ExecutionOutcome::Partial(evidence.clone());
    let partial = verify_outcome(&planned, partial).expect("partial stays partial");
    assert_eq!(partial, VerifiedOutcome::Partial(evidence.clone()));
    let failure = ExecutionOutcome::Failed {
        error: (503, "fixture_service_failure"),
        evidence: partial.evidence().expect("partial evidence").clone(),
    };
    let failure = verify_outcome(&planned, failure).expect("failure evidence is retained");
    assert_eq!(
        failure,
        VerifiedOutcome::Failed {
            error: (503, "fixture_service_failure"),
            evidence: evidence.clone(),
        }
    );
    assert_eq!(failure.evidence(), Some(&evidence));
}

#[test]
fn rejected_completed_retains_exact_observation_and_partial_identity_fields() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    let mut resource = result("native:existing");
    resource.item_type = None;
    resource.replacement = ReplacementEvidence::Verified(unit());
    let evidence = ObservationEvidence {
        operation_handle: Some(
            AcceptedHandle::new("provider-operation:incomplete-completion").expect("handle"),
        ),
        resource: Some(resource),
        effect: MutationEffect::MayHaveOccurred,
    };
    let original: ExecutionOutcome<(u16, &str)> = ExecutionOutcome::Completed(evidence.clone());
    let rejected = verify_outcome(&planned, original.clone()).expect_err("missing final kind");
    assert_eq!(rejected.error, CreateError::MissingFinalIdentity);
    assert_eq!(*rejected.outcome, original);
    assert_eq!(rejected.outcome.evidence(), Some(&evidence));
}

#[test]
fn acceptance_and_completion_require_their_own_evidence_without_fallback() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    let without_handle = observation(Some(result("native:known")));
    let accepted: ExecutionOutcome<&str> = ExecutionOutcome::Accepted(without_handle);
    let rejected = verify_outcome(&planned, accepted.clone()).expect_err("missing handle");
    assert_eq!(rejected.error, CreateError::MissingAcceptedHandle);
    assert_eq!(*rejected.outcome, accepted);

    let handle = AcceptedHandle::new("provider-operation:complete").expect("handle");
    let without_resource = ObservationEvidence {
        operation_handle: Some(handle.clone()),
        resource: None,
        effect: MutationEffect::MayHaveOccurred,
    };
    let completed: ExecutionOutcome<&str> = ExecutionOutcome::Completed(without_resource);
    let rejected = verify_outcome(&planned, completed.clone()).expect_err("missing resource");
    assert_eq!(rejected.error, CreateError::MissingResourceEvidence);
    assert_eq!(*rejected.outcome, completed);

    let complete = ObservationEvidence {
        operation_handle: Some(handle),
        resource: Some(result("native:completed")),
        effect: MutationEffect::MayHaveOccurred,
    };
    let completed: ExecutionOutcome<&str> = ExecutionOutcome::Completed(complete.clone());
    let verified = verify_outcome(&planned, completed).expect("guarded completed resource");
    let VerifiedOutcome::Completed(resource) = &verified else {
        panic!("only guarded completion proves success");
    };
    assert_eq!(resource.evidence(), &complete);
    assert_eq!(verified.evidence(), Some(&complete));
}

#[test]
fn create_completion_rejects_explicit_no_mutation_without_coercing_evidence() {
    let planned = plan(ConflictPolicy::Error, missing(), None).expect("create");
    assert!(matches!(planned.action(), CreateAction::Create { .. }));
    assert_no_mutation_completion_rejected(&planned, result("native:new"));
}

#[test]
fn duplicate_completion_rejects_explicit_no_mutation_without_coercing_evidence() {
    let planned = duplicate_plan(vec![identity("native:existing")]);
    assert!(matches!(planned.action(), CreateAction::Duplicate { .. }));
    assert_no_mutation_completion_rejected(&planned, result("native:fresh"));
}

#[test]
fn replacement_completion_rejects_explicit_no_mutation_without_coercing_evidence() {
    let planned = plan(ConflictPolicy::Overwrite, existing(), complete_input()).expect("replace");
    assert!(matches!(planned.action(), CreateAction::Replace { .. }));
    let mut resource = result("native:existing");
    resource.replacement = ReplacementEvidence::Verified(unit());
    assert_no_mutation_completion_rejected(&planned, resource);
}

#[test]
fn validated_input_debug_does_not_expose_supplied_content() {
    let planned = plan(
        ConflictPolicy::Overwrite,
        existing(),
        Some(json!({"content": "synthetic-private-content-marker"})),
    )
    .expect("replace");
    assert!(!format!("{planned:?}").contains("synthetic-private-content-marker"));
    assert!(!CreateError::InputRejected.to_string().contains("marker"));
}
