// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public process boundaries for the separate, opt-in discovery executable.

#![deny(unnameable_test_items)]

use std::process::Command;

fn legacy(args: &[&str]) -> std::process::Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    process.env_clear().env("NO_COLOR", "1").args(args);
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    process.output().unwrap()
}

#[test]
fn feature_does_not_mount_discovery_in_the_existing_binary() {
    let output = legacy(&["--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Usage: fab-rs"));
    assert!(!help.contains("Discovery preview"));
    assert!(!help.contains("fab-cli"));
    let output = legacy(&["create", "/Prospective.Workspace", "--schema"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[cfg(feature = "self-update")]
#[test]
fn existing_self_update_help_is_not_the_preview_resource_update() {
    let output = legacy(&["update", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("Inspect this operation"));
    assert!(!help.contains("--schema"));
}

#[cfg(feature = "v2-discovery")]
mod preview {
    use fabric_sdk_discovery::{Operation, TargetSelector, discover, registrations};
    use serde_json::Value;
    use std::process::{Command, Output, Stdio};

    fn invoke(args: &[&str]) -> Output {
        let mut process = Command::new(env!("CARGO_BIN_EXE_fab-cli-discovery"));
        process
            .env_clear()
            .env("NO_COLOR", "1")
            .env("FAB_RUST_CONFIG_ROOT", "private-relative-unusable-config")
            .env("FAB_RUST_ACCOUNT", "private-unusable-account")
            .env("FAB_RUST_TENANT", "private-unusable-tenant")
            .env("FAB_RUST_PROFILE", "private-unusable-profile")
            .env("AZURE_CONFIG_DIR", "private-unusable-azure-config")
            .stdin(Stdio::null())
            .args(args);
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                process.env(name, value);
            }
        }
        process.output().expect("preview must start without a host")
    }

    fn success(args: &[&str]) -> Value {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        assert_eq!(output.stdout.last(), Some(&b'\n'));
        let text = String::from_utf8(output.stdout).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn failure(args: &[&str], exit: i32) -> Value {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(exit), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(output.stderr.last(), Some(&b'\n'));
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private-"), "{text}");
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn all_four_roots_return_exact_sdk_descriptors_without_payloads_or_a_profile() {
        for (operation, target) in [
            (Operation::Create, "/Prospective.Workspace"),
            (Operation::Update, "/Existing.Workspace"),
            (Operation::Delete, "/Existing.Workspace"),
            (Operation::Run, "/Existing.Workspace/Analysis.Notebook"),
        ] {
            let actual = success(&[operation.as_str(), target, "--schema"]);
            let expected = discover(operation, &TargetSelector::parse(target).unwrap()).unwrap();
            assert_eq!(actual, serde_json::to_value(&expected).unwrap());
            let mcp = expected.mcp_tool();
            assert_eq!(actual["operationId"], mcp["name"]);
            assert_eq!(
                actual["input"]["schema"],
                mcp["inputSchema"]["properties"]["input"]
            );
            assert_eq!(actual["input"]["schema"], expected.input_schema());
            assert_eq!(actual["target"]["resolution"], "unverified");
            assert_eq!(actual["target"]["authorization"], "not_evaluated");
            assert_eq!(
                actual["availability"]["v2MutationExecution"],
                "unconfigured"
            );
            assert!(actual.is_object());
        }
    }

    #[test]
    fn unregistered_operation_kind_pairs_fail_instead_of_using_another_contract() {
        for operation in Operation::ALL {
            for target in [
                "/Existing.Workspace",
                "/Existing.Workspace/Analysis.Notebook",
            ] {
                let selector = TargetSelector::parse(target).unwrap();
                if let Err(expected) = discover(operation, &selector) {
                    let error = failure(&[operation.as_str(), target, "--schema"], 1);
                    assert_eq!(error["error"]["code"], expected.code());
                }
            }
        }
    }

    #[test]
    fn notebook_default_run_does_not_require_a_body_or_dynamic_parameters() {
        let target = TargetSelector::parse("/Existing.Workspace/Analysis.Notebook").unwrap();
        let contract = discover(Operation::Run, &target).unwrap();
        assert!(contract.validate_input(None).is_ok());
        assert_eq!(
            success(&["run", target.path(), "--schema"]),
            serde_json::to_value(&contract).unwrap()
        );
        assert_eq!(contract.input_schema(), Value::Bool(false));
    }

    #[test]
    fn supported_facade_aliases_equal_roots_and_wrong_owners_fail() {
        for (kind, operation, target) in [
            ("workspace", "create", "/Prospective.Workspace"),
            ("workspace", "update", "/Existing.Workspace"),
            ("workspace", "delete", "/Existing.Workspace"),
            ("notebook", "run", "/Existing.Workspace/Analysis.Notebook"),
        ] {
            assert_eq!(
                success(&[operation, target, "--schema"]),
                success(&["artifact", kind, operation, target, "--schema"])
            );
            let wrong_kind = if kind == "workspace" {
                "notebook"
            } else {
                "workspace"
            };
            failure(&["artifact", wrong_kind, operation, target, "--schema"], 1);
        }
    }

    #[test]
    fn paths_are_required_but_no_execution_file_payload_or_parameters_are_registered() {
        for operation in Operation::ALL {
            failure(&[operation.as_str(), "--schema"], 2);
            let output = invoke(&[operation.as_str(), "--help"]);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("<path>"));
            assert!(help.contains("--schema"));
            for entry in registrations()
                .iter()
                .filter(|entry| entry.operation() == operation)
            {
                assert!(help.contains(entry.canonical_id()));
                assert!(help.contains(entry.summary()));
            }
            for flag in ["--file", "--input", "--verb", "--parameters"] {
                assert!(!help.contains(flag), "{help}");
            }
        }
        for flag in ["--file", "--input", "--verb", "--parameters", "--output"] {
            let error = failure(
                &[
                    "create",
                    "/Prospective.Workspace",
                    "--schema",
                    flag,
                    "private-secret-value",
                ],
                2,
            );
            assert_eq!(error["error"]["code"], "invalid_arguments");
        }
    }

    #[test]
    fn no_schema_is_explicitly_not_implemented_including_literal_delimiter_cases() {
        for operation in Operation::ALL {
            let error = failure(&[operation.as_str(), "/Existing.Workspace"], 1);
            assert_eq!(error["error"]["code"], "discovery_preview_only");
        }
        for args in [
            vec!["create", "--", "--schema"],
            vec!["create", "--", "/Existing.Workspace", "--schema"],
            vec!["create", "/Existing.Workspace", "--", "--schema"],
        ] {
            let output = invoke(&args);
            assert!(!output.status.success(), "{args:?}");
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(matches!(
                error["error"]["code"].as_str(),
                Some("discovery_preview_only" | "invalid_arguments")
            ));
        }
    }

    #[test]
    fn selectors_never_fall_back_to_a_parent_contract() {
        for target in [
            "/Existing.Workspace/../Other.Workspace",
            "/Existing.Workspace/*",
            "/Existing.Workspace/Analysis.Notebook/src/definition",
            "/Existing.Workspace/Analysis.Notebook/functions/private-secret",
            "/Existing.Workspace/private-secret.UnknownType",
            "/Existing.Workspace/Book.Lakehouse",
            "/Existing.Workspace//Analysis.Notebook",
            "/Existing.Workspace/%2e%2e",
        ] {
            failure(&["create", target, "--schema"], 1);
        }
        for target in [
            "/Existing.Workspace/Book.Warehouse/Tables/private-secret",
            "/Existing.Workspace/Book.Lakehouse/Files/private-secret",
            "/Existing.Workspace/Book.Lakehouse/docs/schema",
            "/Existing.Workspace/Book.Lakehouse/Tables/../private-secret",
            "/Existing.Workspace/Book.Lakehouse/Tables/*",
            "/Existing.Workspace/Analysis.Notebook",
        ] {
            let error = failure(&["schema", target], 1);
            assert_eq!(error["error"]["code"], "unsupported");
        }
    }

    #[test]
    fn unknown_arguments_and_rejected_values_do_not_echo_secrets() {
        for args in [
            vec!["private-secret-command"],
            vec!["--private-secret-flag", "private-secret-value"],
            vec![
                "create",
                "/Prospective.Workspace",
                "--schema=private-secret-value",
            ],
            vec!["schema", "/private-secret.UnknownType"],
        ] {
            let output = invoke(&args);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stderr.contains("private-"), "{stderr}");
            let _: Value = serde_json::from_str(&stderr).unwrap();
        }
    }

    #[test]
    fn help_and_version_are_local_and_explicit_about_preview_boundaries() {
        for args in [
            vec!["--help"],
            vec!["--version"],
            vec!["-V"],
            vec!["create", "--help"],
            vec!["update", "--help"],
            vec!["run", "--help"],
            vec!["delete", "--help"],
            vec!["schema", "--help"],
            vec!["artifact", "--help"],
            vec!["artifact", "workspace", "--help"],
            vec!["artifact", "notebook", "run", "--help"],
        ] {
            let output = invoke(&args);
            assert!(output.status.success(), "{args:?}");
            assert!(output.stderr.is_empty());
            let text = String::from_utf8(output.stdout).unwrap().to_lowercase();
            for boundary in [
                "discovery preview",
                "no mutation execution",
                "no mcp server",
                "live data host not configured",
            ] {
                assert!(text.contains(boundary), "{text}");
            }
        }
        let help = String::from_utf8(invoke(&["--help"]).stdout).unwrap();
        assert!(!help.contains("self-update"));
        assert!(!help.contains("REPL command"));
        assert!(help.contains("Mutation crates remain package dependencies"));
    }

    #[test]
    fn absent_backend_provider_is_not_success_or_an_empty_schema() {
        let error = failure(
            &["schema", "/Existing.Workspace/Book.Lakehouse/Tables/sales"],
            1,
        );
        assert_eq!(error["error"]["code"], "NotConfigured");
        failure(
            &[
                "schema",
                "/Existing.Workspace/Book.Lakehouse/Tables/sales",
                "--schema",
            ],
            2,
        );
    }

    #[test]
    fn literal_shell_quote_characters_surviving_argv_do_not_select_another_table() {
        let target = "/Existing.Workspace/Book.Lakehouse/Tables/private-secret";
        for path in [
            format!("'{target}"),
            format!("\"{target}"),
            format!("{target}'"),
            format!("{target}\""),
            format!("'{target}'"),
            format!("\"{target}\""),
            "'Existing.Workspace/Book.Lakehouse/Tables/private-secret".to_owned(),
            "\"Existing.Workspace/Book.Lakehouse/Tables/private-secret".to_owned(),
        ] {
            // Command::args supplies literal argv, without a shell removing quotes.
            let error = failure(&["schema", path.as_str()], 1);
            assert_eq!(error["error"]["code"], "unsupported");
        }
    }
}
