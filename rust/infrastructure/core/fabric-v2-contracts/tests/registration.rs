// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deterministic registration, alias and positive-capability regression contracts.

#![deny(unnameable_test_items)]

use fabric_v2_contracts::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Prerequisite,
    Registry, Target, Verb,
};
use serde_json::json;

fn descriptor(id: &str, verb: Verb) -> OperationDescriptor {
    OperationDescriptor::new(
        id,
        verb,
        json!({"type": "object"}),
        json!({"type": "object"}),
        vec![Effect::RemoteRead],
    )
    .unwrap()
}

fn target(generation: Option<u32>) -> Target {
    Target {
        kind: fabric_v2_contracts::TargetKind::Item,
        owner_type: "Ontology".into(),
        facet: "definition".into(),
        representation: "native".into(),
        generation,
    }
}

fn binding(generation: Generation, id: &str) -> Binding {
    Binding::new(
        "Ontology",
        "definition",
        "native",
        generation,
        id,
        Availability::Available,
    )
    .unwrap()
}

#[test]
fn facade_and_generic_heads_resolve_the_same_operation_and_schema() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("item.definition.get", Verb::Get))
        .unwrap();
    registry.alias("ontology", "Ontology").unwrap();
    registry
        .bind(binding(Generation::Any, "item.definition.get"))
        .unwrap();
    let item = target(Some(2));
    let generic = registry.resolve(Verb::Get, &item, None).unwrap();
    let facade = registry
        .resolve(Verb::Get, &item, Some("ontology"))
        .unwrap();
    assert!(std::ptr::eq(generic.operation, facade.operation));
    assert_eq!(registry.operations().count(), 1);
    assert_eq!(generic.operation.input_schema(), &json!({"type": "object"}));
    assert_eq!(
        generic.operation.output_schema(),
        &json!({"type": "object"})
    );
    assert_eq!(generic.operation.effects(), &[Effect::RemoteRead]);
}

#[test]
fn unknown_generation_does_not_select_an_older_schema() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("ontology.entity.get.v1", Verb::Get))
        .unwrap();
    registry
        .bind(binding(Generation::Exact(1), "ontology.entity.get.v1"))
        .unwrap();
    for generation in [None, Some(0), Some(2), Some(u32::MAX)] {
        assert_eq!(
            registry
                .resolve(Verb::Get, &target(generation), None)
                .unwrap_err(),
            ContractError::UnsupportedTarget
        );
    }
    assert!(registry.resolve(Verb::Get, &target(Some(1)), None).is_ok());
}

#[test]
fn alias_rejects_wrong_owner_before_capability_selection() {
    let mut registry = Registry::default();
    registry.alias("ontology", "Ontology").unwrap();
    let mut item = target(Some(2));
    item.owner_type = "SemanticModel".into();
    assert_eq!(
        registry
            .resolve(Verb::Get, &item, Some("ontology"))
            .unwrap_err(),
        ContractError::WrongOwnerType
    );
    assert_eq!(
        registry
            .resolve(Verb::Get, &item, Some("unregistered"))
            .unwrap_err(),
        ContractError::UnknownAlias
    );
}

#[test]
fn ambiguous_generation_bindings_never_use_first_match() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.get", Verb::Get))
        .unwrap();
    registry
        .register(descriptor("definition.get.new", Verb::Get))
        .unwrap();
    registry
        .bind(binding(Generation::Exact(1), "definition.get"))
        .unwrap();
    registry
        .bind(binding(Generation::Exact(2), "definition.get.new"))
        .unwrap();
    assert_eq!(
        registry
            .bind(binding(Generation::Any, "definition.get"))
            .unwrap_err(),
        ContractError::AmbiguousBinding
    );
    assert_eq!(
        registry
            .bind(binding(Generation::Exact(1), "definition.get.new"))
            .unwrap_err(),
        ContractError::AmbiguousBinding
    );
    assert_eq!(registry.bindings().len(), 2);
}

#[test]
fn different_actions_can_address_the_same_facet() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.get", Verb::Get))
        .unwrap();
    registry
        .register(descriptor("definition.replace", Verb::Update))
        .unwrap();
    registry
        .bind(binding(Generation::Exact(2), "definition.get"))
        .unwrap();
    registry
        .bind(binding(Generation::Exact(2), "definition.replace"))
        .unwrap();
    assert_eq!(
        registry
            .resolve(Verb::Update, &target(Some(2)), None)
            .unwrap()
            .operation
            .id(),
        "definition.replace"
    );
    assert_eq!(
        registry
            .resolve(Verb::Create, &target(Some(2)), None)
            .unwrap_err(),
        ContractError::UnsupportedTarget
    );
}

#[test]
fn duplicate_and_dangling_registrations_fail_without_replacing_content() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.get", Verb::Get))
        .unwrap();
    assert_eq!(
        registry
            .register(descriptor("definition.get", Verb::Delete))
            .unwrap_err(),
        ContractError::DuplicateOperation
    );
    assert_eq!(
        registry
            .bind(binding(Generation::Any, "missing"))
            .unwrap_err(),
        ContractError::UnknownOperation
    );
    registry.alias("ontology", "Ontology").unwrap();
    assert_eq!(
        registry.alias("ontology", "Report").unwrap_err(),
        ContractError::DuplicateAlias
    );
    assert_eq!(registry.operations().next().unwrap().verb(), Verb::Get);
}

#[test]
fn source_implementation_is_not_permission_or_deployment_proof() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("model.get", Verb::Get))
        .unwrap();
    let availability = Availability::Conditional(vec![Prerequisite {
        id: "public-model-feature".into(),
        description: "The caller's host verifies public model availability.".into(),
    }]);
    registry
        .bind(
            Binding::new(
                "Ontology",
                "definition",
                "native",
                Generation::Exact(2),
                "model.get",
                availability.clone(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        registry
            .resolve(Verb::Get, &target(Some(2)), None)
            .unwrap()
            .binding
            .availability(),
        &availability
    );
    assert_eq!(
        serde_json::to_value(registry.bindings()).unwrap()[0]["availability"]["kind"],
        "conditional"
    );
}

#[test]
fn explicitly_unsupported_binding_is_preserved_in_discovery() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.tmdl.get", Verb::Get))
        .unwrap();
    registry
        .bind(
            Binding::new(
                "Ontology",
                "definition",
                "tmdl",
                Generation::Exact(2),
                "definition.tmdl.get",
                Availability::Unsupported("The internal provider returns 501.".into()),
            )
            .unwrap(),
        )
        .unwrap();
    let mut item = target(Some(2));
    item.representation = "tmdl".into();
    assert!(matches!(
        registry
            .resolve(Verb::Get, &item, None)
            .unwrap()
            .binding
            .availability(),
        Availability::Unsupported(_)
    ));
}

#[test]
fn malformed_selectors_and_schemas_do_not_enter_registry() {
    for id in [
        "",
        "../get",
        "https://example.test",
        "with spaces",
        "-get",
        "a/b",
    ] {
        assert_eq!(
            OperationDescriptor::new(id, Verb::Get, json!({}), json!({}), vec![]).unwrap_err(),
            ContractError::InvalidSelector
        );
    }
    assert_eq!(
        OperationDescriptor::new("get", Verb::Get, json!(null), json!({}), vec![]).unwrap_err(),
        ContractError::InvalidSchema
    );
    assert_eq!(
        OperationDescriptor::new(
            "get",
            Verb::Get,
            json!({}),
            json!({}),
            vec![Effect::RemoteRead, Effect::RemoteRead]
        )
        .unwrap_err(),
        ContractError::DuplicateEffect
    );
    let bad = Binding::new(
        "Ontology",
        "definition",
        "native",
        Generation::Any,
        "get",
        Availability::Conditional(vec![]),
    );
    assert_eq!(bad.unwrap_err(), ContractError::InvalidAvailability);
    let mut registry = Registry::default();
    let mut item = target(Some(2));
    item.facet = "../definition".into();
    assert_eq!(
        registry.resolve(Verb::Get, &item, None).unwrap_err(),
        ContractError::InvalidSelector
    );
    assert!(registry.alias("ontology", "Ontology").is_ok());
}

#[test]
fn discovery_order_is_independent_of_registration_order_and_locale() {
    fn registry(reverse: bool) -> Registry {
        let mut result = Registry::default();
        let mut entries = vec![
            ("z.get", Generation::Exact(2)),
            ("a.get", Generation::Exact(1)),
        ];
        if reverse {
            entries.reverse();
        }
        for (id, generation) in entries {
            result.register(descriptor(id, Verb::Get)).unwrap();
            result.bind(binding(generation, id)).unwrap();
        }
        result
    }
    let left = registry(false);
    let right = registry(true);
    assert_eq!(
        serde_json::to_value(left.operations().collect::<Vec<_>>()).unwrap(),
        serde_json::to_value(right.operations().collect::<Vec<_>>()).unwrap()
    );
    assert_eq!(left.bindings(), right.bindings());
}

#[test]
fn any_generation_registered_first_still_rejects_an_overlapping_exact_binding() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.get", Verb::Get))
        .unwrap();
    registry
        .bind(binding(Generation::Any, "definition.get"))
        .unwrap();
    assert_eq!(
        registry
            .bind(binding(Generation::Exact(2), "definition.get"))
            .unwrap_err(),
        ContractError::AmbiguousBinding
    );
    assert!(registry.resolve(Verb::Get, &target(None), None).is_ok());
}

#[test]
fn empty_and_duplicate_prerequisites_cannot_look_available() {
    let requirement = Prerequisite {
        id: "feature".into(),
        description: "The host must verify feature availability.".into(),
    };
    for availability in [
        Availability::Conditional(vec![]),
        Availability::Conditional(vec![requirement.clone(), requirement]),
        Availability::Conditional(vec![Prerequisite {
            id: "feature".into(),
            description: " \t".into(),
        }]),
        Availability::Unsupported(" \n".into()),
    ] {
        assert_eq!(
            Binding::new(
                "Ontology",
                "definition",
                "native",
                Generation::Any,
                "definition.get",
                availability,
            )
            .unwrap_err(),
            ContractError::InvalidAvailability
        );
    }
}

#[test]
fn unadvertised_facets_and_representations_never_inherit_generic_support() {
    let mut registry = Registry::default();
    registry
        .register(descriptor("definition.get", Verb::Get))
        .unwrap();
    registry
        .bind(binding(Generation::Any, "definition.get"))
        .unwrap();
    for (facet, representation) in [
        ("context", "native"),
        ("definition", "owl"),
        ("definition", "Native"),
        ("docs", "markdown"),
    ] {
        let mut item = target(Some(2));
        item.facet = facet.into();
        item.representation = representation.into();
        assert_eq!(
            registry.resolve(Verb::Get, &item, None).unwrap_err(),
            ContractError::UnsupportedTarget
        );
    }
}
