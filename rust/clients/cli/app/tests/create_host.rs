// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Helper-only tests, independent of any native artifact leaf.
#![deny(unnameable_test_items)]

use fabric_cli::create_command::{
    CreateBinding, CreateCommandError, PreparedCreateCommand, v2_create,
};
use fabric_sdk::items::create::{
    CreateCapabilities, CreateError, CreateInputValidator, CreatePlan, CreateSupport, CreateTarget,
    DuplicateSupport, InputDisposition, SchemaRef, SkipSupport,
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
