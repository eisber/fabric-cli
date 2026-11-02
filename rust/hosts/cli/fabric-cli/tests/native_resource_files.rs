// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Physical resource copy through root arguments, real local files and offline HTTP.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

#[path = "family_support/metadata.rs"]
mod family_metadata;
mod family_support;

use fabric_cli::{
    families::{
        FamilyEvidence, FamilyOutput, FamilySession,
        native_resource_files::NativeResourceFileInputs,
        resource_files::{ResourceFileInputs, ResourceLocalSnapshot},
    },
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, LocalKind, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_mutations::{CommandFuture, MutationError, MutationUi, NoticeKind};
use fabric_cli_output::Timestamp;
use fabric_cli_storage_files::{ResourceFileError, ResourceFileResult};
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use family_support::{Harness, success};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, ready},
    sync::Arc,
    time::Duration,
};

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

#[derive(Default)]
struct Ui(Vec<String>);
impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, message: &str) {
        self.0.push(message.into());
    }
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        panic!("source physical-copy handlers must not ask for confirmation")
    }
}

async fn execute(
    host: &mut Harness,
    args: &[&str],
    files: &dyn ResourceFileInputs,
    cancelled: impl Future<Output = ()> + Send,
) -> (FamilyOutput, Ui) {
    let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
    let sdk = Client::new(
        host.credential.clone(),
        CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = HttpExecutor::new(
        host.http.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )
    .unwrap();
    let tenant = Element::tenant("Tenant", "tenant");
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: tenant.clone(),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2026-10-29T00:00:00Z").unwrap(),
        config: Default::default(),
    };
    let store = ConfigStore::new(&host.root);
    let mut navigation = NavigationState::new(
        PathElement::from_fabric(tenant),
        HomeContext::Unspecified,
        host.root.to_string_lossy().into_owned(),
        37,
        true,
    )
    .unwrap();
    let mut ui = Ui::default();
    let output = FamilySession {
        reads: &reads,
        http: &executor,
        store: &store,
        navigation: &mut navigation,
        local: &mut host.effects,
        mutations: &mut ui,
        confirmation: None,
        transfer_ui: &family_support::TransferUiCapture::default(),
        transfer_files: None,
        resource_files: None,
    }
    .dispatch_with_physical_files(&invocation, files, cancelled)
    .await
    .unwrap();
    assert!(
        host.http.replies.lock().unwrap().is_empty(),
        "{:?}",
        host.requests()
    );
    (output, ui)
}

fn files(host: &Harness) -> NativeResourceFileInputs {
    NativeResourceFileInputs::new(host.root.clone(), tokio::runtime::Handle::current())
}

#[tokio::test]
async fn root_upload_retains_source_file_and_ordered_storage_stage_evidence() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("input.bin"), b"plain\n").unwrap();
    host.item("Lakehouse");
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let files = files(&host);
    let (output, ui) = execute(
        &mut host,
        &[
            "cp",
            "input.bin",
            "Demo.Workspace/Book.Lakehouse/Files",
            "-f",
            "-r",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Upload(receipt))) = &output.evidence
    else {
        panic!("expected physical upload: {:?}", output.evidence)
    };
    assert_eq!(receipt.storage_path.relative(), "Files/input.bin");
    assert!(receipt.upload.append.is_some());
    assert_eq!(receipt.upload.flush.as_ref().unwrap().status(), 200);
    assert_eq!(
        std::fs::read(host.root.join("input.bin")).unwrap(),
        b"plain\n"
    );
    assert!(
        ui.0.iter()
            .any(|message| message.contains("checking local file system"))
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[2]["method"], "PUT");
    assert_eq!(requests[3]["raw_body"], "plain\n");
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with("action=flush&position=6")
    );
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(host.requests().len(), requests.len());
}

#[tokio::test]
async fn existing_dispatch_does_not_infer_a_local_filesystem_capability() {
    let mut host = Harness::new();
    let output = host.execute(&["cp", "input.bin", "target.bin"]).await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(host.requests().is_empty());
}

#[tokio::test]
async fn root_download_overwrites_only_the_selected_existing_file() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("download.bin"), b"old value").unwrap();
    std::fs::write(host.root.join("untouched.bin"), b"keep").unwrap();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.reply(200, Value::String("new\n".into()));
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "Demo.Workspace/Book.Lakehouse/Files/source.bin",
            "download.bin",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert!(matches!(
        &output.evidence, FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Download(receipt)))
        if receipt.bytes_written == Some(4)
    ));
    assert_eq!(
        std::fs::read(host.root.join("download.bin")).unwrap(),
        b"new\n"
    );
    assert_eq!(
        std::fs::read(host.root.join("untouched.bin")).unwrap(),
        b"keep"
    );
    assert_eq!(host.requests().last().unwrap()["method"], "GET");
}

#[tokio::test]
async fn download_to_an_existing_directory_uses_only_the_remote_basename() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join("output")).unwrap();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.reply(200, Value::String("new\n".into()));
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "Demo.Workspace/Book.Lakehouse/Files/source.bin",
            "output",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert_eq!(
        std::fs::read(host.root.join("output/source.bin")).unwrap(),
        b"new\n"
    );
    assert_eq!(
        std::fs::read_dir(host.root.join("output")).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn quoted_local_alias_and_native_spaces_preserve_the_selected_source() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("source file.bin"), b"value").unwrap();
    host.item("Lakehouse");
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "\"source file.bin\"",
            "Demo.Workspace/Book.Lakehouse/Files",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert_eq!(
        std::fs::read(host.root.join("source file.bin")).unwrap(),
        b"value"
    );
    assert!(matches!(
        output.evidence, FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Upload(receipt)))
        if receipt.storage_path.relative() == "Files/source file.bin"
    ));
}

#[tokio::test]
async fn local_moves_are_rejected_without_reading_or_deleting_the_source() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("input.bin"), b"keep").unwrap();
    std::fs::write(host.root.join("target.bin"), b"old").unwrap();
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &["mv", "input.bin", "target.bin", "-f"],
        &files,
        pending(),
    )
    .await;
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::ResourceFiles(Err(ResourceFileError::Command(error)))
        if error.code == "NotSupported"
    ));
    assert_eq!(std::fs::read(host.root.join("input.bin")).unwrap(), b"keep");
    assert_eq!(std::fs::read(host.root.join("target.bin")).unwrap(), b"old");
    assert!(host.requests().is_empty());
}

struct ForbiddenLocal;
impl ResourceFileInputs for ForbiddenLocal {
    fn snapshot<'a>(
        &'a self,
        _: &'a str,
    ) -> CommandFuture<'a, Result<ResourceLocalSnapshot, MutationError>> {
        panic!("remote failure or prior cancellation must not touch the local filesystem")
    }
}

#[tokio::test]
async fn remote_permission_failure_and_ready_cancellation_do_not_activate_local_io() {
    for cancelled in [false, true] {
        let mut host = Harness::new();
        if !cancelled {
            host.reply(
                403,
                json!({"error":{"code":"Forbidden","message":"denied"}}),
            );
        }
        let signal = async move {
            if cancelled {
                ready(()).await
            } else {
                pending::<()>().await
            }
        };
        let (output, _) = execute(
            &mut host,
            &[
                "cp",
                "Demo.Workspace/Book.Lakehouse/Files/source.bin",
                "download.bin",
            ],
            &ForbiddenLocal,
            signal,
        )
        .await;
        assert_ne!(output.plan.exit_code, 0);
        assert_eq!(host.requests().len(), usize::from(!cancelled));
        assert!(!host.root.join("download.bin").exists());
    }
}

#[tokio::test]
async fn unavailable_virtual_metadata_never_falls_back_to_a_same_named_local_file() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join(".connections")).unwrap();
    std::fs::write(
        host.root.join(".connections/Test.Connection"),
        b"must not upload",
    )
    .unwrap();
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            ".connections/Test.Connection",
            "Demo.Workspace/Book.Lakehouse/Files",
        ],
        &ForbiddenLocal,
        pending(),
    )
    .await;
    assert_ne!(output.plan.exit_code, 0);
    let mut stdout = Vec::new();
    output.write_to(&mut stdout, &mut Vec::new()).unwrap();
    let envelope: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(envelope["result"]["error_code"], "NotSupported");
    assert!(host.requests().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn explicit_drive_root_metadata_keeps_only_exact_alias_authority() {
    let host = Harness::new();
    let root = host.root.ancestors().last().unwrap().to_str().unwrap();
    let files = files(&host);
    let snapshot = files.snapshot(root).await.unwrap();
    assert_eq!(snapshot.metadata.kind, LocalKind::Directory);
    assert!(snapshot.exists && snapshot.readable);
    assert!(
        snapshot
            .files
            .kind(std::path::Path::new("unrelated"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_metadata_preserves_original_alias_kind_and_zero_io_construction() {
    let host = Harness::new();
    std::fs::write(host.root.join("input.bin"), b"value").unwrap();
    let files = files(&host);
    let snapshot = files.snapshot("input.bin").await.unwrap();
    assert_eq!(snapshot.metadata.path, "input.bin");
    assert_eq!(snapshot.metadata.name, "input.bin");
    assert_eq!(snapshot.metadata.kind, LocalKind::File);
    assert!(snapshot.exists && snapshot.readable);
    assert!(files.snapshot("../outside.bin").await.is_err());
}
