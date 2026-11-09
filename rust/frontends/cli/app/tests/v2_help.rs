// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual preview output and injected-host purity for local human help.

#![cfg(feature = "v2-discovery")]
#![deny(unnameable_test_items)]

use fabric_cli::v2_discovery::{
    DataSchemaProvider, DataSchemaRequest, DiscoveryFailure, DiscoveryFuture, DiscoveryHost,
};
use fabric_sdk::discovery::{Operation, TargetSelector, help::TargetHelp};
use fabric_sdk_tables::schema_document::DataSchemaDocument;
use futures_util::FutureExt;
use std::{
    future::{pending, ready},
    io::{self, Write},
    num::NonZeroUsize,
    process::{Command, Output, Stdio},
};

fn invoke(args: &[&str]) -> Output {
    let profile = tempfile::tempdir().unwrap();
    let mut process = Command::new(env!("CARGO_BIN_EXE_fab-cli-discovery"));
    process
        .env_clear()
        .env("NO_COLOR", "1")
        .env("FAB_RUST_CONFIG_ROOT", profile.path())
        .env("FAB_RUST_TENANT", "private-invalid-tenant")
        .env("FAB_RUST_ACCOUNT", "private-invalid-account")
        .env("FAB_RUST_AUTH_MODE", "private-invalid-mode")
        .env("FAB_RUST_UPDATE_POLICY", "install")
        .env("FAB_RUST_UPDATE_STATE_ROOT", "private-invalid-update-root")
        .stdin(Stdio::null())
        .args(args);
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            process.env(key, value);
        }
    }
    let output = process.output().unwrap();
    assert_eq!(std::fs::read_dir(profile.path()).unwrap().count(), 0);
    output
}

#[test]
fn cli_human_output_equals_sdk_projection_for_exact_targets_and_verb_filter() {
    for (path, operation) in [
        ("/Prospective.Workspace", None),
        ("/Sales.Workspace", Some(Operation::Delete)),
        ("/Sales.Workspace/Analysis.Notebook", Some(Operation::Run)),
    ] {
        let mut args = vec!["help", path];
        if let Some(operation) = operation {
            args.extend(["--verb", operation.as_str()]);
        }
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty());
        let expected = TargetHelp::new(&TargetSelector::parse(path).unwrap(), operation).unwrap();
        assert_eq!(output.stdout, expected.render().as_bytes());
    }
}

#[test]
fn wrong_target_operation_and_unregistered_representations_fail_with_local_guidance() {
    for args in [
        vec!["help", "/Sales.Workspace", "--verb", "run"],
        vec![
            "help",
            "/Sales.Workspace/private-marker.Notebook",
            "--verb",
            "create",
        ],
        vec!["help", "/Sales.Workspace/Analysis.Notebook/src/definition"],
        vec!["help", "/Sales.Workspace/Analysis.Notebook/docs/schema"],
        vec!["help", "/Sales.Workspace/private-marker.Unknown"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "unsupported_capability");
        let text = error["error"]["message"].as_str().unwrap();
        assert!(text.contains("help --help"));
        assert!(text.contains("help <path>"));
        assert!(text.contains("do not use a parent contract"));
        assert!(!text.contains("private-marker"));
    }
}

#[test]
fn invalid_or_unimplemented_options_fail_before_any_host_and_never_echo_values() {
    for args in [
        vec!["help"],
        vec!["help", "/Sales.Workspace", "--verb"],
        vec!["help", "/Sales.Workspace", "--verb", "private-invalid-verb"],
        vec!["help", "/Sales.Workspace", "--search", "private-search"],
        vec!["help", "/Sales.Workspace", "--input", "private-input"],
        vec!["help", "/Sales.Workspace", "--schema"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private-"));
        assert!(text.contains("invalid_arguments"));
    }
    let output = invoke(&["help", "--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("--verb"));
    assert!(text.contains("Docs search is not implemented"));
}

struct ForbiddenProvider;

impl DataSchemaProvider for ForbiddenProvider {
    fn max_output_bytes(&self) -> NonZeroUsize {
        panic!("local help must not consult provider budgets");
    }

    fn schema<'a>(
        &'a self,
        _: &'a DataSchemaRequest,
        _: DiscoveryFuture<'a, ()>,
    ) -> DiscoveryFuture<'a, Result<DataSchemaDocument, DiscoveryFailure>> {
        panic!("local help must not invoke data-schema providers");
    }
}

#[test]
fn local_help_is_immediately_ready_without_data_auth_http_or_runtime_setup() {
    let host = DiscoveryHost {
        data_schema: Some(&ForbiddenProvider),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = host
        .run(
            ["fab-cli-discovery", "help", "/Prospective.Workspace"],
            pending(),
            &mut stdout,
            &mut stderr,
        )
        .now_or_never()
        .expect("pure local help needs no runtime")
        .unwrap();
    assert_eq!(exit, 0);
    assert!(stderr.is_empty());
    assert!(!stdout.is_empty());
}

#[test]
fn cancellation_has_no_success_output_and_does_not_consult_a_provider() {
    let host = DiscoveryHost {
        data_schema: Some(&ForbiddenProvider),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = host
        .run(
            ["fab-cli-discovery", "help", "/Prospective.Workspace"],
            ready(()),
            &mut stdout,
            &mut stderr,
        )
        .now_or_never()
        .unwrap()
        .unwrap();
    assert_eq!(exit, 1);
    assert!(stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error"]["code"], "cancelled");
}

struct FailedWriter;

impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn failed_help_output_is_an_error_not_success_or_a_replayed_operation() {
    let error = DiscoveryHost::default()
        .run(
            ["fab-cli-discovery", "help", "/Prospective.Workspace"],
            pending(),
            &mut FailedWriter,
            &mut Vec::new(),
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
}
