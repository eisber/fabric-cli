// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public-boundary contracts for the unmounted CLI v2 create surface.

#![deny(unnameable_test_items)]

use clap::error::ErrorKind;
use fabric_cli_mutations::v2_create::{
    ConflictPolicy, CreateCapabilities, CreateCommand, DiagnosticFact, DiagnosticInput,
    InputSource, InputState, OutputDestination, OutputFormat, command, from_matches, parse_args,
    parse_timeout, render_diagnostic,
};
use fabric_sdk_items::create::{
    CreateAction, CreateError, CreateInputValidator, CreateSupport, CreateTarget, DuplicateSupport,
    InputDisposition, LeafState, NativeId, Preflight, ReplacementUnit, ResourceIdentity, SchemaRef,
    SkipSupport, TargetScope, plan_create,
};
use serde_json::{Value, json};
use std::{ffi::OsString, path::PathBuf, time::Duration};

struct FixtureValidator;

impl CreateInputValidator for FixtureValidator {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != "surface-fixture/v1"
            || target.item_type() != "Surface.Fixture"
            || target.parent_id().as_str() != "fixture-parent"
            || target.name() != "fixture-name"
        {
            return Err(CreateError::InputRejected);
        }
        match supplied {
            None => Ok(InputDisposition::CreateOnly),
            Some(value) if value == &json!({"fixture": "create-only"}) => {
                Ok(InputDisposition::CreateOnly)
            }
            Some(value) if value == &json!({"fixture": "complete-content"}) => {
                Ok(InputDisposition::CreateAndReplace(replacement_unit()))
            }
            _ => Err(CreateError::InputRejected),
        }
    }
}

fn parse(arguments: &[&str]) -> CreateCommand {
    parse_args(std::iter::once("create").chain(arguments.iter().copied()))
        .expect("valid create arguments")
}

fn capabilities() -> CreateCapabilities {
    CreateCapabilities {
        schema: SchemaRef::new("surface-fixture/v1").expect("fixture schema"),
        create: CreateSupport::RejectExisting,
        replacement: Some(replacement_unit()),
        skip: SkipSupport::ExactExistingUnchanged,
        duplicate: DuplicateSupport::FreshIdentityWithRepeatedNames,
    }
}

fn replacement_unit() -> ReplacementUnit {
    ReplacementUnit::new("surface-content/v1").expect("fixture replacement unit")
}

fn fixture_target(scope: TargetScope) -> CreateTarget {
    CreateTarget::new(
        scope,
        NativeId::new("fixture-parent").expect("fixture parent"),
        "fixture-name",
        "Surface.Fixture",
    )
    .expect("fixture target")
}

fn fixture_identity(id: &str) -> ResourceIdentity {
    ResourceIdentity::new(
        NativeId::new(id).expect("fixture resource ID"),
        NativeId::new("fixture-parent").expect("fixture parent"),
        "Surface.Fixture",
    )
    .expect("fixture identity")
}

#[test]
fn defaults_are_error_json_stdout_and_thirty_seconds() {
    let request = parse(&["/example/target"]);
    assert_eq!(request.destination, OsString::from("/example/target"));
    assert_eq!(request.policy, ConflictPolicy::Error);
    assert_eq!(request.format, OutputFormat::Json);
    assert_eq!(request.output, OutputDestination::Stdout);
    assert_eq!(request.timeout, Duration::from_secs(30));
    assert_eq!(request.input, None);
    assert_eq!(request.project, None);
    assert_eq!(request.profile, None);
    assert!(!request.wait);
}

#[test]
fn policy_is_the_engine_type_and_all_four_values_are_available() {
    for (name, policy) in [
        ("error", ConflictPolicy::Error),
        ("overwrite", ConflictPolicy::Overwrite),
        ("skip", ConflictPolicy::Skip),
        ("duplicate", ConflictPolicy::Duplicate),
    ] {
        let engine_policy: fabric_sdk_items::create::ConflictPolicy =
            parse(&["target", "--if-exists", name]).policy;
        assert_eq!(engine_policy, policy);
    }
    assert_eq!(
        parse(&["target", "--force"]),
        parse(&["target", "--if-exists", "overwrite"])
    );
}

#[test]
fn force_conflicts_with_every_explicit_policy_in_both_orders() {
    for policy in ["error", "overwrite", "skip", "duplicate"] {
        for arguments in [
            vec!["create", "target", "--force", "--if-exists", policy],
            vec!["create", "target", "--if-exists", policy, "--force"],
        ] {
            assert_eq!(
                parse_args(arguments).expect_err("explicit conflict").kind(),
                ErrorKind::ArgumentConflict
            );
        }
        let attached = format!("--if-exists={policy}");
        for arguments in [
            vec!["create", "target", "--force", attached.as_str()],
            vec!["create", "target", attached.as_str(), "--force"],
        ] {
            assert_eq!(
                parse_args(arguments).expect_err("attached conflict").kind(),
                ErrorKind::ArgumentConflict
            );
        }
    }
}

#[test]
fn repeated_flags_are_not_silently_overridden() {
    for arguments in [
        vec!["target", "--force", "--force"],
        vec!["target", "--wait", "--wait"],
        vec!["target", "--if-exists", "error", "--if-exists", "error"],
        vec!["target", "--if-exists", "skip", "--if-exists", "duplicate"],
        vec!["target", "--input", "a.json", "--input", "b.json"],
        vec!["target", "--format", "json", "--format", "json"],
        vec!["target", "--output", "a.json", "-o", "b.json"],
        vec!["target", "--project", "id", "-q", "name"],
        vec!["target", "--profile", "first", "--profile", "second"],
        vec!["target", "--timeout", "1s", "--timeout", "2s"],
    ] {
        assert!(
            parse_args(std::iter::once("create").chain(arguments.iter().copied())).is_err(),
            "must reject repeated arguments: {arguments:?}"
        );
    }
}

#[test]
fn destination_is_one_required_nonempty_control_free_token() {
    for arguments in [
        vec!["create"],
        vec!["create", ""],
        vec!["create", "first", "second"],
        vec!["create", "line\nbreak"],
        vec!["create", "tab\tpath"],
        vec!["create", "nul\0path"],
        vec!["create", "escape\u{1b}path"],
        vec!["create", "delete\u{7f}path"],
        vec!["create", "unicode\u{85}control"],
    ] {
        assert!(parse_args(arguments).is_err());
    }
}

#[test]
fn destination_has_no_workspace_or_collection_suffix_heuristics() {
    for target in [
        "/",
        "relative.collection",
        "noncanonical.Workspace/anything",
        "opaque:collection",
        "id.without.type",
    ] {
        assert_eq!(parse(&[target]).destination, OsString::from(target));
    }
    let request = parse(&["opaque:collection", "--if-exists", "duplicate"]);
    assert_eq!(request.destination, OsString::from("opaque:collection"));
}

#[test]
fn delimiter_preserves_flag_like_destination_as_data() {
    assert_eq!(
        parse(&["--input", "content.json", "--", "--force"]).destination,
        OsString::from("--force")
    );
    assert_eq!(
        parse(&["--", "--help"]).destination,
        OsString::from("--help")
    );
    assert!(parse_args(["create", "--input", "content.json", "--unknown"]).is_err());
    assert!(parse_args(["create", "target", "--", "--force"]).is_err());
}

#[test]
fn option_like_native_files_can_use_attached_values() {
    let request = parse(&[
        "--input=--content.json",
        "--output=--result.json",
        "--",
        "-target",
    ]);
    assert_eq!(
        request.input,
        Some(InputSource::File(PathBuf::from("--content.json")))
    );
    assert_eq!(
        request.output,
        OutputDestination::File(PathBuf::from("--result.json"))
    );
    assert_eq!(request.destination, OsString::from("-target"));
}

#[test]
fn unicode_spaces_and_quoted_tokens_are_preserved_exactly() {
    let target = "/工作区/\"quoted name\"; $(not-a-command)";
    let input = "內容 'native'.json";
    let output = "résultat \"final\".json";
    let project = "{名字: displayName, note: 'a b'}";
    let request = parse(&[
        target,
        "--input",
        input,
        "-o",
        output,
        "-q",
        project,
        "--profile",
        "研究 profile",
    ]);
    assert_eq!(request.destination, OsString::from(target));
    assert_eq!(request.input, Some(InputSource::File(PathBuf::from(input))));
    assert_eq!(
        request.output,
        OutputDestination::File(PathBuf::from(output))
    );
    assert_eq!(request.project.as_deref(), Some(project));
    assert_eq!(request.profile.as_deref(), Some("研究 profile"));
}

#[test]
fn input_and_output_dash_are_distinct_stream_identities() {
    let request = parse(&["target", "--input", "-", "--output", "-"]);
    assert_eq!(request.input, Some(InputSource::Stdin));
    assert_eq!(request.output, OutputDestination::Stdout);
    assert_eq!(request.validate_stdin(false), Ok(()));
    assert_eq!(
        request.validate_stdin(true),
        Err(DiagnosticFact::StdinConflict)
    );
    let request = parse(&["target", "--input", "./-", "--output", "./-"]);
    assert_eq!(request.input, Some(InputSource::File(PathBuf::from("./-"))));
    assert_eq!(
        request.output,
        OutputDestination::File(PathBuf::from("./-"))
    );
    assert_eq!(request.validate_stdin(true), Ok(()));
    assert_eq!(parse(&["target"]).validate_stdin(true), Ok(()));
}

#[test]
fn format_is_only_json_and_does_not_select_native_schema() {
    assert_eq!(
        parse(&["target", "--format", "json"]).format,
        OutputFormat::Json
    );
    for format in ["text", "yaml", "native", "JSON", ""] {
        assert!(parse_args(["create", "target", "--format", format]).is_err());
    }
}

#[test]
fn input_transport_does_not_assume_json_or_read_native_files() {
    for path in [
        "notebook.py",
        "report.rdl",
        "native.bin",
        "content-without-suffix",
    ] {
        let request = parse(&["target", "--input", path]);
        assert_eq!(request.input, Some(InputSource::File(PathBuf::from(path))));
        assert_eq!(request.format, OutputFormat::Json);
    }
}

#[test]
fn unsupported_flags_alias_inference_and_unknown_policy_are_rejected() {
    for arguments in [
        vec!["target", "--name", "new-name"],
        vec!["target", "--type", "Notebook"],
        vec!["target", "-f"],
        vec!["target", "--for"],
        vec!["target", "--inp", "content.json"],
        vec!["target", "--output-format", "json"],
        vec!["target", "--if-exists", "replace"],
        vec!["target", "--if-exists", "ERROR"],
        vec!["target", "--input"],
        vec!["target", "--profile", ""],
    ] {
        assert!(
            parse_args(std::iter::once("create").chain(arguments.iter().copied())).is_err(),
            "must reject unsupported arguments: {arguments:?}"
        );
    }
}

#[test]
fn wait_defaults_to_five_minutes_but_explicit_timeout_wins() {
    let request = parse(&["target", "--wait"]);
    assert!(request.wait);
    assert_eq!(request.timeout, Duration::from_secs(300));
    for arguments in [
        vec!["target", "--wait", "--timeout", "7s"],
        vec!["target", "--timeout", "7s", "--wait"],
        vec!["target", "--timeout", "7s"],
    ] {
        assert_eq!(parse(&arguments).timeout, Duration::from_secs(7));
    }
}

#[test]
fn integer_durations_are_exact_and_allow_all_maximum_spellings() {
    for (value, millis) in [
        ("1ms", 1),
        ("123ms", 123),
        ("2s", 2_000),
        ("3m", 180_000),
        ("1h", 3_600_000),
        ("24h", 86_400_000),
        ("1440m", 86_400_000),
        ("86400s", 86_400_000),
        ("86400000ms", 86_400_000),
    ] {
        let duration = Duration::from_millis(millis);
        assert_eq!(parse_timeout(value), Ok(duration));
        assert_eq!(parse(&["target", "--timeout", value]).timeout, duration);
    }
}

#[test]
fn timeout_rejects_zero_fraction_sign_overflow_unknown_units_and_over_max() {
    for value in [
        "",
        "ms",
        "1",
        "0ms",
        "0s",
        "0m",
        "0h",
        "00s",
        "0.5s",
        "1.5h",
        "-1s",
        "+1s",
        " 1s",
        "1s ",
        "1 s",
        "1S",
        "1d",
        "1us",
        "1s1ms",
        "１s",
        "25h",
        "1441m",
        "86401s",
        "86400001ms",
        "18446744073709551615h",
        "18446744073709551616ms",
        "999999999999999999999999999999999999999999s",
    ] {
        assert!(parse_timeout(value).is_err(), "must reject {value:?}");
        let argument = format!("--timeout={value}");
        assert!(parse_args(["create", "target", argument.as_str()]).is_err());
    }
}

#[test]
fn schema_help_returns_without_input_authentication_or_filesystem_access() {
    let help = parse_args(["create", "--help"]).expect_err("display help, do not exit");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("--input"));
    assert!(text.contains("--if-exists"));
    assert!(text.contains("fab-cli create"));
    assert!(text.contains("--timeout"));
    assert!(text.contains("advertised native"));
    assert!(!text.contains("native JSON"));
    assert!(!text.contains("--name"));
    assert_eq!(command().get_name(), "create");
    let request = parse(&[
        "unresolved-destination",
        "--input",
        "nonexistent-content.json",
        "--output",
        "nonexistent-parent/result.json",
        "--profile",
        "nonexistent-profile",
    ]);
    assert!(matches!(request.input, Some(InputSource::File(_))));
}

#[test]
fn matches_are_reusable_beneath_a_parent_without_executable_mounting() {
    let matches = clap::Command::new("fab-cli")
        .subcommand(command())
        .try_get_matches_from(["fab-cli", "create", "target", "--if-exists", "skip"])
        .expect("parent schema parses");
    let request = from_matches(
        matches
            .subcommand_matches("create")
            .expect("create matches"),
    )
    .expect("typed subcommand");
    assert_eq!(request.policy, ConflictPolicy::Skip);
    assert!(from_matches(&matches).is_err());
    assert!(from_matches(&clap::ArgMatches::default()).is_err());
}

#[test]
fn implicit_policy_and_capabilities_are_validated_by_the_sdk() {
    let mut capabilities: fabric_sdk_items::create::CreateCapabilities = capabilities();
    capabilities.create = CreateSupport::Unsupported;
    capabilities.replacement = None;
    capabilities.duplicate = DuplicateSupport::Unsupported;
    assert_eq!(
        parse(&["target"]).plan(
            fixture_target(TargetScope::NamedLeaf),
            Preflight::Leaf(LeafState::Existing(fixture_identity("existing"))),
            &capabilities,
            None,
            &FixtureValidator,
        ),
        Err(CreateError::UnsupportedPolicy)
    );
    let skip = parse(&["target", "--if-exists", "skip"])
        .plan(
            fixture_target(TargetScope::NamedLeaf),
            Preflight::Leaf(LeafState::Existing(fixture_identity("existing"))),
            &capabilities,
            None,
            &FixtureValidator,
        )
        .expect("SDK permits exact-existing skip without creation support");
    assert!(matches!(skip.action(), CreateAction::Skip { .. }));
    for policy in ["overwrite", "duplicate"] {
        assert_eq!(
            parse(&["target", "--if-exists", policy]).plan(
                fixture_target(TargetScope::NamedLeaf),
                Preflight::Leaf(LeafState::Missing),
                &capabilities,
                None,
                &FixtureValidator,
            ),
            Err(CreateError::UnsupportedPolicy)
        );
    }
}

#[test]
fn overwrite_missing_with_permitted_omitted_default_plans_create() {
    let request = parse(&["target", "--force"]);
    let target = fixture_target(TargetScope::NamedLeaf);
    let capabilities = capabilities();
    let plan = request
        .plan(
            target.clone(),
            Preflight::Leaf(LeafState::Missing),
            &capabilities,
            None,
            &FixtureValidator,
        )
        .expect("overwrite of missing target uses permitted creation defaults");
    assert_eq!(plan.policy(), ConflictPolicy::Overwrite);
    assert_eq!(
        plan,
        plan_create(
            request.policy,
            target,
            Preflight::Leaf(LeafState::Missing),
            &capabilities,
            None,
            &FixtureValidator,
        )
        .expect("same shared SDK plan")
    );
    let CreateAction::Create { input } = plan.action() else {
        panic!("missing target must create, not replace");
    };
    assert_eq!(input.supplied(), None);
    assert_eq!(input.disposition(), &InputDisposition::CreateOnly);
}

#[test]
fn overwrite_missing_with_create_only_input_plans_create() {
    let request = parse(&["target", "--force", "--input", "content.json"]);
    let supplied = json!({"fixture": "create-only"});
    let plan = request
        .plan(
            fixture_target(TargetScope::NamedLeaf),
            Preflight::Leaf(LeafState::Missing),
            &capabilities(),
            Some(supplied.clone()),
            &FixtureValidator,
        )
        .expect("creation input need not be complete replacement input");
    assert_eq!(plan.policy(), ConflictPolicy::Overwrite);
    let CreateAction::Create { input } = plan.action() else {
        panic!("missing target must create");
    };
    assert_eq!(input.supplied(), Some(&supplied));
    assert_eq!(input.disposition(), &InputDisposition::CreateOnly);
}

#[test]
fn overwrite_existing_rejects_omitted_or_incomplete_replacement_via_sdk() {
    for (request, supplied) in [
        (parse(&["target", "--force"]), None),
        (
            parse(&["target", "--force", "--input", "content.json"]),
            Some(json!({"fixture": "create-only"})),
        ),
    ] {
        assert_eq!(
            request.plan(
                fixture_target(TargetScope::NamedLeaf),
                Preflight::Leaf(LeafState::Existing(fixture_identity("existing"))),
                &capabilities(),
                supplied,
                &FixtureValidator,
            ),
            Err(CreateError::IncompleteReplacement)
        );
    }
}

#[test]
fn overwrite_existing_complete_input_plans_identity_preserving_replacement() {
    let request = parse(&["target", "--force", "--input", "content.json"]);
    let existing = fixture_identity("existing");
    let plan = request
        .plan(
            fixture_target(TargetScope::NamedLeaf),
            Preflight::Leaf(LeafState::Existing(existing.clone())),
            &capabilities(),
            Some(json!({"fixture": "complete-content"})),
            &FixtureValidator,
        )
        .expect("SDK permits complete replacement");
    let CreateAction::Replace {
        existing: retained,
        unit,
        ..
    } = plan.action()
    else {
        panic!("existing target must replace in place");
    };
    assert_eq!(retained, &existing);
    assert_eq!(unit, &replacement_unit());
}

#[test]
fn skip_still_rejects_invalid_explicit_input_via_the_sdk_validator() {
    let request = parse(&["target", "--if-exists", "skip", "--input", "content.json"]);
    for supplied in [
        Value::Null,
        json!({}),
        json!([]),
        json!(true),
        json!({"fixture": "complete-content", "unexpected": "field"}),
    ] {
        assert_eq!(
            request.plan(
                fixture_target(TargetScope::NamedLeaf),
                Preflight::Leaf(LeafState::Existing(fixture_identity("existing"))),
                &capabilities(),
                Some(supplied),
                &FixtureValidator,
            ),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn duplicate_collection_planning_accepts_zero_one_or_many_matches() {
    let request = parse(&[
        "collection",
        "--if-exists",
        "duplicate",
        "--input",
        "content.json",
    ]);
    for known_matches in [
        vec![],
        vec![fixture_identity("first")],
        vec![fixture_identity("first"), fixture_identity("second")],
    ] {
        let plan = request
            .plan(
                fixture_target(TargetScope::ParentCollection),
                Preflight::Collection {
                    known_matches: known_matches.clone(),
                },
                &capabilities(),
                Some(json!({"fixture": "create-only"})),
                &FixtureValidator,
            )
            .expect("SDK permits explicit collection duplicate");
        assert_eq!(plan.policy(), ConflictPolicy::Duplicate);
        let CreateAction::Duplicate {
            known_matches: retained,
            ..
        } = plan.action()
        else {
            panic!("collection duplicate must not turn into named create or overwrite");
        };
        assert_eq!(retained, &known_matches);
    }
}

#[test]
fn plan_preserves_exact_target_schema_binding_and_sdk_errors() {
    let request = parse(&["target"]);
    let target = fixture_target(TargetScope::NamedLeaf);
    let preflight = Preflight::Leaf(LeafState::Existing(fixture_identity("existing")));
    assert_eq!(
        request.plan(
            target.clone(),
            preflight.clone(),
            &capabilities(),
            None,
            &FixtureValidator,
        ),
        Err(CreateError::AlreadyExists)
    );
    let mut capabilities = capabilities();
    capabilities.schema = SchemaRef::new("unsupported-fixture/v1").expect("schema label");
    let delegated = request.plan(
        target.clone(),
        preflight.clone(),
        &capabilities,
        None,
        &FixtureValidator,
    );
    assert_eq!(
        delegated,
        plan_create(
            request.policy,
            target,
            preflight,
            &capabilities,
            None,
            &FixtureValidator,
        )
    );
    assert_eq!(delegated, Err(CreateError::InputRejected));
}

#[test]
fn known_collision_identifies_target_and_gates_force_on_capability_and_content() {
    let request = parse(&["target", "--input", "content.json"]);
    let fact = DiagnosticFact::AlreadyExists {
        target: OsString::from("resolved-target"),
    };
    let mut target_capabilities = capabilities();
    for input in [InputState::Missing, InputState::Incomplete] {
        let result = render_diagnostic(
            DiagnosticInput::Fact(&fact),
            &request,
            &target_capabilities,
            input,
        );
        assert_eq!(result.code, "AlreadyExists");
        assert!(result.message.contains("resolved-target"));
        assert!(result.message.contains("No change was made"));
        assert!(result.guidance.iter().any(|text| text.contains("update")));
        assert!(
            result
                .guidance
                .iter()
                .any(|text| text.contains("separate input contract"))
        );
        assert!(!result.guidance.iter().any(|text| text.contains("--input")));
        assert!(!result.guidance.iter().any(|text| text.contains("--force")));
        assert!(result.suggestions.is_empty());
    }
    target_capabilities.replacement = None;
    let result = render_diagnostic(
        DiagnosticInput::Fact(&fact),
        &request,
        &target_capabilities,
        InputState::Complete,
    );
    assert!(result.suggestions.is_empty());
    assert!(!result.guidance.iter().any(|text| text.contains("--force")));
    let result = render_diagnostic(
        DiagnosticInput::Fact(&fact),
        &parse(&["target"]),
        &capabilities(),
        InputState::Complete,
    );
    assert!(result.suggestions.is_empty());
}

#[test]
fn force_suggestion_is_structured_preserves_context_and_round_trips() {
    let request = parse(&[
        "--input=--body $(do-not-run).json",
        "--output=--result;not-shell.json",
        "--profile=-selected profile",
        "--project={value: 'quoted expression'}",
        "--wait",
        "--timeout=123ms",
        "--if-exists=error",
        "--",
        "--target \"引用\"; $(not-a-command)",
    ]);
    let fact = DiagnosticFact::AlreadyExists {
        target: request.destination.clone(),
    };
    let result = render_diagnostic(
        DiagnosticInput::Fact(&fact),
        &request,
        &capabilities(),
        InputState::Complete,
    );
    assert_eq!(result.suggestions.len(), 1);
    let suggestion = &result.suggestions[0];
    assert_eq!(suggestion.program, "fab-cli");
    assert!(!suggestion.args.iter().any(|arg| arg == "--if-exists"));
    let retry =
        parse_args(suggestion.args.clone()).expect("suggestion argv parses without a shell");
    let mut expected = request;
    expected.policy = ConflictPolicy::Overwrite;
    assert_eq!(retry, expected);
    assert!(result.guidance.iter().any(|text| text.contains("--force")));
    assert!(
        !result
            .guidance
            .iter()
            .any(|text| text.contains("do-not-run"))
    );
}

#[test]
fn stdin_force_suggestion_retains_explicit_stdin_source() {
    let request = parse(&["target", "--input", "-"]);
    let result = render_diagnostic(
        DiagnosticInput::Fact(&DiagnosticFact::AlreadyExists {
            target: request.destination.clone(),
        }),
        &request,
        &capabilities(),
        InputState::Complete,
    );
    let retry = parse_args(result.suggestions[0].args.clone()).expect("retry parses");
    assert_eq!(retry.input, Some(InputSource::Stdin));
    assert_eq!(retry.policy, ConflictPolicy::Overwrite);
}

#[test]
fn ambiguity_and_provider_text_never_imply_no_change_or_suggest_force() {
    let request = parse(&["target", "--input", "content.json"]);
    let fact = DiagnosticFact::AmbiguousTarget {
        target: OsString::from("ambiguous-target"),
    };
    for input in [
        DiagnosticInput::Fact(&fact),
        DiagnosticInput::ProviderText(
            "AlreadyExists: secret=do-not-disclose; No change was made; retry --force",
        ),
    ] {
        let result = render_diagnostic(input, &request, &capabilities(), InputState::Complete);
        assert!(!result.message.contains("No change"));
        assert!(!result.message.contains("secret"));
        assert!(!result.message.contains("do-not-disclose"));
        assert!(!result.message.contains("--force"));
        assert!(result.guidance.is_empty());
        assert!(result.suggestions.is_empty());
    }
    let result = render_diagnostic(
        DiagnosticInput::ProviderText("AlreadyExists"),
        &request,
        &capabilities(),
        InputState::Complete,
    );
    assert_eq!(result.code, "ProviderError");
}

#[test]
fn target_identity_is_escaped_in_human_diagnostics_not_composed_as_a_command() {
    let request = parse(&["target", "--input", "content.json"]);
    for fact in [
        DiagnosticFact::AlreadyExists {
            target: OsString::from("name\n\u{1b}[31m\"; $(not-a-command)"),
        },
        DiagnosticFact::AmbiguousTarget {
            target: OsString::from("name\n\u{1b}[31m\"; $(not-a-command)"),
        },
    ] {
        let result = render_diagnostic(
            DiagnosticInput::Fact(&fact),
            &request,
            &capabilities(),
            InputState::Incomplete,
        );
        assert!(!result.message.contains('\n'));
        assert!(!result.message.contains('\u{1b}'));
        assert!(result.message.contains("\\n"));
        assert!(!result.message.contains("fab-cli create"));
        assert!(result.suggestions.is_empty());
    }
}

#[test]
fn validation_facts_have_stable_separate_diagnostics() {
    let request = parse(&["target"]);
    for (fact, code) in [
        (
            DiagnosticFact::UnsupportedPolicy {
                policy: ConflictPolicy::Duplicate,
            },
            "UnsupportedPolicy",
        ),
        (DiagnosticFact::InputRequired, "InputRequired"),
        (
            DiagnosticFact::IncompleteReplacement,
            "IncompleteReplacement",
        ),
        (DiagnosticFact::StdinConflict, "StdinConflict"),
    ] {
        let result = render_diagnostic(
            DiagnosticInput::Fact(&fact),
            &request,
            &capabilities(),
            InputState::Complete,
        );
        assert_eq!(result.code, code);
        assert!(!result.message.contains("native JSON"));
        if code == "InputRequired" {
            assert!(result.message.contains("advertised native content"));
        }
        assert!(!result.message.contains("No change"));
        assert!(result.suggestions.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_native_paths_are_preserved_in_parse_and_retry() {
    use std::os::unix::ffi::OsStringExt;

    let destination = OsString::from_vec(b"target-\xff".to_vec());
    let input = OsString::from_vec(b"content-\xfe.json".to_vec());
    let output = OsString::from_vec(b"result-\xfd.json".to_vec());
    let request = parse_args([
        OsString::from("create"),
        destination.clone(),
        OsString::from("--input"),
        input.clone(),
        OsString::from("--output"),
        output.clone(),
    ])
    .expect("native paths need not be UTF-8");
    assert_eq!(request.destination, destination);
    assert_eq!(request.input, Some(InputSource::File(PathBuf::from(input))));
    assert_eq!(
        request.output,
        OutputDestination::File(PathBuf::from(output))
    );
    let result = render_diagnostic(
        DiagnosticInput::Fact(&DiagnosticFact::AlreadyExists {
            target: request.destination.clone(),
        }),
        &request,
        &capabilities(),
        InputState::Complete,
    );
    let retry = parse_args(result.suggestions[0].args.clone()).expect("native retry");
    let mut expected = request;
    expected.policy = ConflictPolicy::Overwrite;
    assert_eq!(retry, expected);
}

#[cfg(windows)]
#[test]
fn non_utf8_windows_paths_are_preserved_in_parse_and_retry() {
    use std::os::windows::ffi::OsStringExt;

    let destination = OsString::from_wide(&[0x74, 0xd800]);
    let input = OsString::from_wide(&[0x69, 0xd801]);
    let output = OsString::from_wide(&[0x6f, 0xd802]);
    let request = parse_args([
        OsString::from("create"),
        destination.clone(),
        OsString::from("--input"),
        input.clone(),
        OsString::from("--output"),
        output.clone(),
    ])
    .expect("native Windows paths need not be UTF-8");
    assert_eq!(request.destination, destination);
    assert_eq!(request.input, Some(InputSource::File(PathBuf::from(input))));
    assert_eq!(
        request.output,
        OutputDestination::File(PathBuf::from(output))
    );
    let result = render_diagnostic(
        DiagnosticInput::Fact(&DiagnosticFact::AlreadyExists {
            target: request.destination.clone(),
        }),
        &request,
        &capabilities(),
        InputState::Complete,
    );
    let retry = parse_args(result.suggestions[0].args.clone()).expect("native Windows retry");
    let mut expected = request;
    expected.policy = ConflictPolicy::Overwrite;
    assert_eq!(retry, expected);
}
