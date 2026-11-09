// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SDK facade reexports preserve one underlying contract and no CLI dependency.

#![deny(unnameable_test_items)]

use fabric_sdk::{ontology::OntologyTarget, v2};

#[test]
fn neutral_contracts_and_public_targets_are_original_producer_types() {
    let registry = v2::Registry::default();
    let core_registry: fabric_v2_contracts::Registry = registry;
    assert_eq!(core_registry.operations().count(), 0);
    let target = OntologyTarget::new(
        "11111111-2222-3333-4444-555555555555",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
    )
    .unwrap();
    let original: fabric_sdk_ontology::OntologyTarget = target;
    assert_eq!(original.item_id(), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
}
