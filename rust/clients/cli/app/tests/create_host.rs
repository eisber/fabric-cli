// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Helper-only tests, independent of any native artifact leaf.
#![deny(unnameable_test_items)]

use fabric_cli::create_command::{
    CreateBinding, CreateCommandError, PreparedCreateCommand, command_for_binding, v2_create,
};
use fabric_sdk::items::create::{
    CreateCapabilities, CreateError, CreateInputValidator, CreatePlan, CreateSupport, CreateTarget,
    DuplicateSupport, InputDisposition, ReplacementUnit, SchemaRef, SkipSupport,
};
use fabric_sdk_operations::FabricOperation;
use serde_json::Value;
use std::io::{self, Read};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.TestItem";
struct Binding;
impl CreateInputValidator for Binding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        input: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.item_type() != "TestItem"
            || schema.as_str() != "test.create/v1"
            || input.is_some_and(|input| input != &serde_json::json!({"displayName":"Exact"}))
        {
            return Err(CreateError::InputRejected);
        }
        Ok(InputDisposition::CreateOnly)
    }
}
impl CreateBinding for Binding {
    fn item_type(&self) -> &'static str {
        "TestItem"
    }
    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new("test.create/v1").unwrap(),
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        }
    }
    fn operation(
        &self,
        _: &CreatePlan,
        _: Option<&[u8]>,
        _: bool,
    ) -> Result<FabricOperation, CreateError> {
        panic!("local preflight must not build or dispatch a native operation")
    }
}
struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("stdin belongs to the caller, not an implicit consumer")
    }
}

struct HelpBinding {
    owner: &'static str,
    create: CreateSupport,
    skip: SkipSupport,
}

impl CreateInputValidator for HelpBinding {
    fn validate(
        &self,
        _: &CreateTarget,
        _: &SchemaRef,
        _: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        panic!("help must not invoke schema validation")
    }
}

impl CreateBinding for HelpBinding {
    fn item_type(&self) -> &'static str {
        self.owner
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new("test.help/v1").unwrap(),
            create: self.create,
            replacement: Some(ReplacementUnit::new("test.full/v1").unwrap()),
            skip: self.skip,
            duplicate: DuplicateSupport::FreshIdentityWithRepeatedNames,
        }
    }

    fn operation(
        &self,
        _: &CreatePlan,
        _: Option<&[u8]>,
        _: bool,
    ) -> Result<FabricOperation, CreateError> {
        panic!("help must not prepare a native operation")
    }
}

#[test]
fn shared_help_uses_only_the_selected_owner_and_its_typed_path() {
    for owner in ["Notebook", "Lakehouse", "Report"] {
        let binding = HelpBinding {
            owner,
            create: CreateSupport::RejectExisting,
            skip: SkipSupport::ExactExistingUnchanged,
        };
        let help = command_for_binding(&binding).render_long_help().to_string();
        assert!(help.contains(&format!("Selected native owner: {owner}.")));
        assert!(help.contains(&format!(
            "/<workspaceGUID>.Workspace/<literal-name>.{owner}"
        )));
        assert!(help.contains("Enabled policies: error and skip."));
        assert!(
            help.contains(
                "No overwrite, duplicate, collection, profile or artifact facade binding."
            )
        );
        assert!(help.contains("--force never bypasses it."));
        for unrelated in ["Notebook", "Lakehouse", "Report"] {
            if unrelated != owner {
                assert!(!help.contains(unrelated));
            }
        }
    }
}

#[test]
fn shared_help_never_advertises_provider_policies_outside_the_host_subset() {
    for (create, skip, expected) in [
        (
            CreateSupport::RejectExisting,
            SkipSupport::Unsupported,
            "error",
        ),
        (
            CreateSupport::Unsupported,
            SkipSupport::ExactExistingUnchanged,
            "skip",
        ),
        (CreateSupport::Unsupported, SkipSupport::Unsupported, "none"),
    ] {
        let binding = HelpBinding {
            owner: "TestItem",
            create,
            skip,
        };
        let help = command_for_binding(&binding).render_long_help().to_string();
        assert!(help.contains(&format!("Enabled policies: {expected}.")));
        assert!(!help.contains("Enabled policies: overwrite"));
        assert!(!help.contains("Enabled policies: duplicate"));
        assert!(!help.contains("Notebook"));
    }
}

#[test]
fn omitted_input_and_file_input_never_consume_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let command = v2_create::parse_args(["create", TARGET]).unwrap();
    assert!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, false)
            .is_ok()
    );
    std::fs::write(
        temp.path().join("input.json"),
        br#"{"displayName":"Exact"}"#,
    )
    .unwrap();
    let command = v2_create::parse_args(["create", TARGET, "--input", "input.json"]).unwrap();
    assert!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, false)
            .is_ok()
    );
}

#[test]
fn owner_parent_and_profile_are_checked_before_input_access() {
    let temp = tempfile::tempdir().unwrap();
    for path in [
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        "/Name.Workspace/Exact.TestItem",
        "/11111111-1111-1111-1111-111111111111.Workspace",
    ] {
        let command = v2_create::parse_args(["create", path, "--input", "-"]).unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &Binding,
                temp.path(),
                &mut ForbiddenRead,
                false
            ),
            Err(CreateCommandError::Invalid)
        ));
    }
    let command =
        v2_create::parse_args(["create", TARGET, "--profile", "named", "--input", "-"]).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, false),
        Err(CreateCommandError::Invalid)
    ));
}

#[test]
fn exclusive_stdin_and_strict_json_are_enforced_locally() {
    let temp = tempfile::tempdir().unwrap();
    let command = v2_create::parse_args(["create", TARGET, "--input", "-"]).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, true),
        Err(CreateCommandError::Invalid)
    ));
    for bytes in [
        b"".as_slice(),
        b"null",
        b"{",
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact","extra":null}"#,
    ] {
        let command =
            v2_create::parse_args(["create", TARGET, "--input", "-", "--if-exists", "skip"])
                .unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &Binding,
                temp.path(),
                &mut &bytes[..],
                false
            )
            .is_err()
        );
    }
}

#[test]
fn unsupported_policies_fail_before_any_host_execution_even_for_missing_target() {
    let temp = tempfile::tempdir().unwrap();
    for policy in ["overwrite", "duplicate"] {
        let command = v2_create::parse_args(["create", TARGET, "--if-exists", policy]).unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &Binding,
                temp.path(),
                &mut ForbiddenRead,
                false
            ),
            Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
        ));
    }
}

#[test]
fn output_preflight_never_creates_or_overwrites_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    let args = ["create", TARGET, "--output", path.to_str().unwrap()];
    let command = v2_create::parse_args(args).unwrap();
    assert!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, false)
            .is_ok()
    );
    assert!(!path.exists());
    std::fs::write(&path, b"existing").unwrap();
    let command = v2_create::parse_args(args).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(command, &Binding, temp.path(), &mut ForbiddenRead, false),
        Err(CreateCommandError::File(_))
    ));
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
}
