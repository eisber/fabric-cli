// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline exact-target help through the public SDK boundary.

#![deny(unnameable_test_items)]

use fabric_sdk_discovery::{
    DiscoveryError, Operation, TargetSelector, discover, help::TargetHelp, registrations,
};

#[test]
fn exact_target_help_retains_only_registered_shared_contracts_in_id_order() {
    for path in ["/New.Workspace", "/Sales.Workspace/Analysis.Notebook"] {
        let target = TargetSelector::parse(path).unwrap();
        let help = TargetHelp::new(&target, None).unwrap();
        let mut expected: Vec<_> = registrations()
            .iter()
            .filter(|entry| entry.target_kind() == target.kind())
            .map(|entry| entry.canonical_id())
            .collect();
        expected.sort();
        assert_eq!(help.target(), &target);
        assert_eq!(
            help.operations()
                .iter()
                .map(|entry| entry.canonical_id())
                .collect::<Vec<_>>(),
            expected
        );
        for contract in help.operations() {
            let original = discover(contract.operation(), &target).unwrap();
            assert_eq!(contract.descriptor(), original.descriptor());
            assert_eq!(contract.input_schema(), original.input_schema());
            assert_eq!(contract.mcp_tool(), original.mcp_tool());
            assert!(help.render().contains(&original.help()));
        }
    }
}

#[test]
fn verb_filter_never_exposes_another_operation_or_mutates_the_machine_schema() {
    let target = TargetSelector::parse("/Prospective.Workspace").unwrap();
    let before = discover(Operation::Create, &target).unwrap().descriptor();
    let help = TargetHelp::new(&target, Some(Operation::Create)).unwrap();
    assert_eq!(help.operations().len(), 1);
    assert_eq!(help.operations()[0].operation(), Operation::Create);
    assert_eq!(help.operations()[0].descriptor(), before);
    assert!(!help.render().contains("fabric.core.workspace.delete.v1"));
    assert!(
        !help
            .render()
            .contains("fabric.core.workspace.update-metadata.v1")
    );
    assert!(matches!(
        TargetHelp::new(&target, Some(Operation::Run)),
        Err(DiscoveryError::UnsupportedCapability { .. })
    ));
}

#[test]
fn rendering_is_repeatable_human_only_and_truthful_about_corpus_and_effects() {
    let target = TargetSelector::parse("/Sales.Workspace/Analysis.Notebook").unwrap();
    let help = TargetHelp::new(&target, None).unwrap();
    let text = help.render();
    assert_eq!(text, help.render());
    assert!(text.ends_with('\n'));
    for boundary in [
        target.path(),
        "resolution unverified; authorization not evaluated",
        "Discovery effects: none",
        "V2 mutation execution: unconfigured",
        "not complete Fabric coverage",
        "Docs corpus: not indexed",
        "no documentation search or fetch performed",
        "Notebook executionData and parameters are unsupported",
        "notebook_code_may_mutate_data_or_external_systems",
        "Help text is for people",
        "run <path> --schema",
    ] {
        assert!(text.contains(boundary), "{boundary}: {text}");
    }
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
}

#[test]
fn target_names_are_data_not_interpolated_shell_command_examples() {
    let path = "/Sales space;private-marker.Workspace";
    let help = TargetHelp::new(&TargetSelector::parse(path).unwrap(), None).unwrap();
    let text = help.render();
    assert_eq!(text.matches(path).count(), 1);
    assert!(text.contains("create <path> --schema"));
    assert!(text.contains("as one argument, quoted for your shell"));
    assert!(!text.contains(&format!("create {path} --schema")));
}

#[test]
fn representations_unknown_kinds_and_unsafe_selectors_do_not_borrow_parent_help() {
    for path in [
        "/Sales.Workspace/Analysis.Notebook/src/definition",
        "/Sales.Workspace/Analysis.Notebook/docs/schema",
        "/Sales.Workspace/Analysis.Notebook/functions/private-marker",
        "/Sales.Workspace/Other.Unknown",
        "/Sales.Workspace/../private-marker.Notebook",
        "/Sales.Workspace/private-marker\u{1b}[31m.Notebook",
    ] {
        let error = TargetSelector::parse(path).unwrap_err();
        assert!(!error.to_string().contains("private-marker"));
    }
}
