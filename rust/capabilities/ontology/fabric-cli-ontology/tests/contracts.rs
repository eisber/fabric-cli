// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_cli_ontology::{
    ArgumentError, Backend, BodyContract, InputSource, OntologyTarget as Target,
    OntologyVerb as Verb, OutputDestination, SelectionError, UpdateMode, ValidationError, bindings,
    ontology_command, parse_common_args, parse_facade_args, select_binding, validate_arguments,
};
use std::{path::PathBuf, time::Duration};

#[test]
fn selected_aliases_are_exact_and_use_common_names() {
    let command = ontology_command();
    let names: Vec<_> = command
        .get_subcommands()
        .map(|command| command.get_name())
        .collect();
    assert_eq!(
        names,
        ["create", "delete", "get", "link", "list", "move", "update"]
    );
    for verb in Verb::ALL {
        assert!(bindings().iter().any(|binding| binding.verb == verb));
        assert!(!verb.common_name().contains("ontology"));
    }
    for unsupported in ["import", "export", "serve", "wait", "plan", "apply", "run"] {
        assert!(parse_facade_args(["ontology", unsupported, "/target"]).is_err());
    }
}

#[test]
fn common_and_facade_arguments_are_identical_for_each_selected_alias() {
    for (verb, options) in [
        (Verb::Create, vec!["--input", "source.json", "--wait"]),
        (Verb::Delete, vec!["--permanent", "--recursive"]),
        (Verb::Get, vec!["--max-bytes", "1048576", "--wait"]),
        (Verb::Link, vec![]),
        (
            Verb::List,
            vec!["--limit", "100", "--continuation", "opaque"],
        ),
        (Verb::Move, vec![]),
        (Verb::Update, vec!["--input", "-", "--mode", "patch"]),
    ] {
        let mut common = vec![verb.common_name(), "/explicit-target"];
        common.extend(options);
        let mut facade = vec!["ontology"];
        facade.extend(common.iter().copied());
        assert_eq!(
            parse_common_args(verb, common),
            parse_facade_args(facade),
            "{}",
            verb.common_name(),
        );
    }
}

#[test]
fn defaults_do_not_silently_adopt_draft_numeric_budgets() {
    for verb in [Verb::Get, Verb::List, Verb::Create, Verb::Delete] {
        let args = parse_common_args(verb, [verb.common_name(), "/target"]).unwrap();
        assert_eq!(args.timeout, None);
        assert_eq!(args.limit, None);
        assert_eq!(args.max_bytes, None);
        assert_eq!(args.output, OutputDestination::Stdout);
        assert!(!args.permanent);
        assert!(!args.recursive);
        assert!(!args.wait);
    }
}

#[test]
fn input_output_and_patch_replace_are_explicit() {
    let args = parse_facade_args([
        "ontology",
        "update",
        "/target/src/definition",
        "--input",
        "-",
        "--mode",
        "replace",
        "--output",
        "receipt.json",
        "--if-match",
        "opaque",
        "--profile",
        "chosen",
        "--project",
        "resource",
        "--timeout",
        "5m",
    ])
    .unwrap();
    assert_eq!(args.input, Some(InputSource::Stdin));
    assert_eq!(args.mode, Some(UpdateMode::Replace));
    assert_eq!(
        args.output,
        OutputDestination::File(PathBuf::from("receipt.json"))
    );
    assert_eq!(args.if_match.as_deref(), Some("opaque"));
    assert_eq!(args.profile.as_deref(), Some("chosen"));
    assert_eq!(args.project.as_deref(), Some("resource"));
    assert_eq!(args.timeout, Some(Duration::from_secs(300)));
    let args = parse_common_args(
        Verb::Update,
        [
            "update",
            "/target",
            "--input",
            "patch.json",
            "--mode",
            "patch",
        ],
    )
    .unwrap();
    assert_eq!(
        args.input,
        Some(InputSource::File(PathBuf::from("patch.json")))
    );
    assert_eq!(args.mode, Some(UpdateMode::Patch));
}

#[test]
fn invalid_or_conflicting_arguments_fail_without_echoing_supplied_values() {
    for args in [
        vec!["get"],
        vec!["get", ""],
        vec!["get", "/target", "second"],
        vec!["get", "/target", "--format", "raw"],
        vec!["get", "/target", "--input", "source.json"],
        vec!["get", "/target", "--max-bytes", "0"],
        vec!["get", "/target", "--format", "json", "--format", "json"],
        vec!["get", "/target", "--force"],
    ] {
        let error = parse_common_args(Verb::Get, args).unwrap_err();
        assert!(!error.to_string().contains("/target"));
        assert!(!error.to_string().contains("source.json"));
    }
    for options in [
        vec![],
        vec!["--input", "-"],
        vec!["--mode", "patch"],
        vec!["--input", "-", "--mode", "upsert"],
    ] {
        let mut args = vec!["update", "/target"];
        args.extend(options);
        assert_eq!(
            parse_common_args(Verb::Update, args),
            Err(ArgumentError::InvalidSyntax)
        );
    }
    assert_eq!(
        parse_common_args(Verb::Get, ["get", " \t"]),
        Err(ArgumentError::EmptyPath),
    );
}

#[test]
fn duration_and_collection_bounds_are_positive_and_bounded() {
    for value in ["1ms", "1s", "1m", "24h", "86400000ms"] {
        assert!(parse_common_args(Verb::Get, ["get", "/target", "--timeout", value]).is_ok());
    }
    for value in [
        "0s",
        "-1s",
        "+1s",
        "1.5s",
        "1",
        "25h",
        "86400001ms",
        "999999999999999999999h",
    ] {
        assert!(parse_common_args(Verb::Get, ["get", "/target", "--timeout", value]).is_err());
    }
    for value in ["0", "-1", "1.5", "999999999999999999999999"] {
        assert!(parse_common_args(Verb::List, ["list", "/target", "--limit", value]).is_err());
    }
}

#[test]
fn unresolved_link_and_move_contracts_never_become_parse_success() {
    for verb in [Verb::Link, Verb::Move] {
        assert_eq!(
            parse_common_args(verb, [verb.common_name(), "/target"]),
            Err(ArgumentError::UnresolvedCommandContract),
        );
    }
}

#[test]
fn catalog_is_unique_and_keeps_representations_separate() {
    for (index, binding) in bindings().iter().enumerate() {
        assert!(!bindings()[..index].iter().any(|previous| {
            previous.verb == binding.verb && previous.target == binding.target
        }));
    }
    let definition = select_binding(Verb::Update, Target::Definition, "Ontology", Some(2)).unwrap();
    let metadata = select_binding(Verb::Update, Target::Item, "Ontology", Some(2)).unwrap();
    let public = select_binding(
        Verb::Update,
        Target::PublicModelMinimized,
        "Ontology",
        Some(2),
    )
    .unwrap();
    let internal =
        select_binding(Verb::Update, Target::NativeModelStored, "Ontology", Some(2)).unwrap();
    assert_eq!(metadata.body, BodyContract::MetadataPatch);
    assert_eq!(definition.body, BodyContract::DefinitionReplace);
    assert_eq!(public.body, BodyContract::PublicModelReplace);
    assert_eq!(internal.body, BodyContract::NativeModelReplace);
    assert_ne!(public.backend, internal.backend);
    assert_eq!(definition.backend, Backend::PublicDefinition);
}

#[test]
fn wrong_type_unknown_generation_and_unsupported_facets_are_explicit() {
    assert_eq!(
        select_binding(Verb::Get, Target::Item, "SemanticModel", Some(2)),
        Err(SelectionError::WrongOwnerType),
    );
    for generation in [None, Some(0), Some(3), Some(u64::MAX)] {
        assert_eq!(
            select_binding(Verb::Get, Target::Definition, "Ontology", generation),
            Err(SelectionError::UnknownGeneration),
        );
    }

    for target in [
        Target::Owl,
        Target::InternalTmdl,
        Target::Docs,
        Target::Context,
    ] {
        for verb in Verb::ALL {
            assert_eq!(
                select_binding(verb, target, "Ontology", Some(2)),
                Err(SelectionError::UnsupportedTarget),
            );
        }
    }
    assert_eq!(
        select_binding(Verb::List, Target::Entities, "Ontology", Some(1)),
        Err(SelectionError::UnsupportedGeneration),
    );
    assert!(select_binding(Verb::Get, Target::Definition, "Ontology", Some(1)).is_ok());
    assert_eq!(
        select_binding(Verb::List, Target::Item, "Ontology", Some(2)),
        Err(SelectionError::UnsupportedTarget),
    );
}

#[test]
fn representation_validation_rejects_patch_replace_and_condition_fallbacks() {
    for (mode, target, expected) in [
        ("patch", Target::Item, Ok(BodyContract::MetadataPatch)),
        (
            "replace",
            Target::Item,
            Err(ValidationError::UnsupportedUpdateMode),
        ),
        (
            "patch",
            Target::Definition,
            Err(ValidationError::UnsupportedUpdateMode),
        ),
        (
            "replace",
            Target::Definition,
            Ok(BodyContract::DefinitionReplace),
        ),
        (
            "replace",
            Target::PublicModelExpanded,
            Err(ValidationError::Selection(
                SelectionError::UnsupportedTarget,
            )),
        ),
    ] {
        let args = parse_common_args(
            Verb::Update,
            ["update", "/target", "--input", "-", "--mode", mode],
        )
        .unwrap();
        assert_eq!(
            validate_arguments(&args, target, "Ontology", Some(2)).map(|binding| binding.body),
            expected,
        );
    }
    let args = parse_common_args(
        Verb::Update,
        [
            "update",
            "/target",
            "--input",
            "-",
            "--mode",
            "patch",
            "--if-match",
            "opaque",
        ],
    )
    .unwrap();
    assert_eq!(
        validate_arguments(&args, Target::Item, "Ontology", Some(2)),
        Err(ValidationError::UnsupportedCondition),
    );
}

#[test]
fn hard_only_deletion_requires_intent_not_implicit_cascades_or_observation() {
    let args = parse_common_args(Verb::Delete, ["delete", "/entity"]).unwrap();
    assert_eq!(
        validate_arguments(&args, Target::Entity, "Ontology", Some(2)),
        Err(ValidationError::PermanentIntentRequired),
    );
    let mut args = args;
    args.permanent = true;
    assert!(validate_arguments(&args, Target::Entity, "Ontology", Some(2)).is_ok());
    args.recursive = true;
    assert_eq!(
        validate_arguments(&args, Target::Entity, "Ontology", Some(2)),
        Err(ValidationError::UnsupportedRecursion),
    );
    args.recursive = false;
    args.wait = true;
    assert_eq!(
        validate_arguments(&args, Target::Entity, "Ontology", Some(2)),
        Err(ValidationError::UnsupportedWait),
    );
}

#[test]
fn create_needs_typed_input_and_metadata_does_not_inherit_definition_wait() {
    let args = parse_common_args(Verb::Create, ["create", "/destination"]).unwrap();
    assert_eq!(
        validate_arguments(&args, Target::Item, "Ontology", Some(2)),
        Err(ValidationError::InputRequired),
    );
    let args = parse_common_args(Verb::Get, ["get", "/target", "--wait"]).unwrap();
    assert_eq!(
        validate_arguments(&args, Target::Item, "Ontology", Some(2)),
        Err(ValidationError::UnsupportedWait),
    );
    assert!(validate_arguments(&args, Target::Definition, "Ontology", Some(2)).is_ok());
}
