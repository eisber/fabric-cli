// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Physical resource copy through root arguments, real local files and offline HTTP.
//! Includes the authorized additive recursive/move behavior, not a rewritten Python oracle.

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
        "{:?}; evidence: {:?}",
        host.requests(),
        output.evidence
    );
    (output, ui)
}

fn files(host: &Harness) -> NativeResourceFileInputs {
    NativeResourceFileInputs::new(host.root.clone(), tokio::runtime::Handle::current())
}

fn write_reply(host: &Harness, status: u16) {
    host.headers(status, Value::Null, &[("etag", "\"target\"")]);
}

fn file_version(host: &Harness, length: usize) {
    host.headers(
        200,
        Value::Null,
        &[
            ("content-length", &length.to_string()),
            ("etag", "\"v1\""),
            ("x-ms-resource-type", "file"),
        ],
    );
}

fn file_range(host: &Harness, bytes: &str, offset: usize, total: usize) {
    host.headers(
        206,
        Value::String(bytes.into()),
        &[
            ("content-length", &bytes.len().to_string()),
            ("etag", "\"v1\""),
            (
                "content-range",
                &format!("bytes {offset}-{}/{total}", offset + bytes.len() - 1),
            ),
        ],
    );
}

#[tokio::test]
async fn root_upload_retains_source_file_and_ordered_storage_stage_evidence() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("input.bin"), b"plain\n").unwrap();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    write_reply(&host, 201);
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
    let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) = &output.evidence
    else {
        panic!("expected physical upload: {:?}", output.evidence)
    };
    assert_eq!(receipt.uploads[0].0.relative(), "Files/input.bin");
    assert!(receipt.uploads[0].1.append.is_some());
    assert_eq!(receipt.uploads[0].1.flush.as_ref().unwrap().status(), 200);
    assert_eq!(
        std::fs::read(host.root.join("input.bin")).unwrap(),
        b"plain\n"
    );
    assert!(
        ui.0.iter()
            .any(|message| message.contains("checking local file system"))
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[3]["method"], "PUT");
    assert_eq!(requests[4]["raw_body"], "plain\n");
    assert!(
        requests[5]["url"]
            .as_str()
            .unwrap()
            .ends_with("action=flush&position=6&close=true")
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
    file_version(&host, 4);
    file_range(&host, "new\n", 0, 4);
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "Demo.Workspace/Book.Lakehouse/Files/source.bin",
            "download.bin",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert!(matches!(
        &output.evidence, FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
        if receipt.local_writes[0].1 == 4
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
    file_version(&host, 4);
    file_range(&host, "new\n", 0, 4);
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
    host.reply(404, Value::Null);
    write_reply(&host, 201);
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
        output.evidence, FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
        if receipt.uploads[0].0.relative() == "Files/source file.bin"
    ));
}

#[tokio::test]
async fn local_moves_delete_the_source_only_after_the_destination_commits() {
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
        FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
        if receipt.local_deletes.len() == 1
    ));
    assert!(!host.root.join("input.bin").exists());
    assert_eq!(
        std::fs::read(host.root.join("target.bin")).unwrap(),
        b"keep"
    );
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

#[tokio::test]
async fn remote_file_copy_commits_exact_bytes_with_recursive_and_collision_flags() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.reply(404, Value::Null);
    file_version(&host, 13);
    write_reply(&host, 201);
    file_range(&host, "remote bytes\n", 0, 13);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let (output, _) = execute(
        &mut host,
        &[
            "copy",
            "Demo.Workspace/Book.Lakehouse/Files/source.txt",
            "Demo.Workspace/Book.Lakehouse/Files/copied.txt",
            "-r",
            "--block_on_path_collision",
        ],
        &ForbiddenLocal,
        pending(),
    )
    .await;
    success(&output);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
        if receipt.uploads[0].1.flush.unwrap().status() == 200
    ));
    let requests = host.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "GET", "GET", "HEAD", "HEAD", "HEAD", "PUT", "GET", "PATCH", "PATCH"
        ]
    );
    assert_eq!(requests[7]["raw_body"], "remote bytes\n");
    assert!(
        requests[5]["url"]
            .as_str()
            .unwrap()
            .contains("/Files/copied.txt/")
    );
    assert!(
        requests[8]["url"]
            .as_str()
            .unwrap()
            .ends_with("flush&position=13&close=true")
    );
}

#[tokio::test]
async fn recursive_remote_directory_move_is_one_rename_and_preserves_acceptance() {
    for status in [200, 202, 403] {
        let mut host = Harness::new();
        host.item("Lakehouse");
        host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        host.reply(404, Value::Null);
        let list = json!({"paths":[{"name":"n/Files/tree/empty","isDirectory":true}]});
        host.reply(200, list.clone());
        host.reply(200, list);
        host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        host.reply(404, Value::Null);
        host.reply(status, Value::Null);
        let (output, _) = execute(
            &mut host,
            &[
                "move",
                "Demo.Workspace/Book.Lakehouse/Files/tree",
                "Demo.Workspace/Book.Lakehouse/Files/renamed",
                "-r",
                "-f",
            ],
            &ForbiddenLocal,
            pending(),
        )
        .await;
        if status != 200 {
            assert_ne!(output.plan.exit_code, 0);
            assert!(matches!(
                &output.evidence,
                FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error)))
                if error.source_delete_may_have_occurred
            ));
        } else {
            success(&output);
            assert!(matches!(
                &output.evidence,
                FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
                if receipt.rename.unwrap().status() == status
            ));
        }
        let requests = host.requests();
        assert_eq!(requests.len(), 9);
        assert_eq!(requests[8]["method"], "PUT");
        assert!(
            requests[8]["url"]
                .as_str()
                .unwrap()
                .ends_with("/Files/renamed")
        );
        assert!(requests.iter().all(|request| request["method"] != "DELETE"));
    }
}

#[tokio::test]
async fn directory_copies_require_recursive_without_mutating_sources() {
    for local_source in [false, true] {
        let mut host = Harness::new();
        std::fs::create_dir(host.root.join("tree")).unwrap();
        std::fs::create_dir(host.root.join("tree").join("empty")).unwrap();
        std::fs::write(host.root.join("tree").join("keep"), b"keep").unwrap();
        host.item("Lakehouse");
        if !local_source {
            host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        }
        let files = files(&host);
        let (output, _) = execute(
            &mut host,
            &[
                "cp",
                if local_source {
                    "tree"
                } else {
                    "Demo.Workspace/Book.Lakehouse/Files/tree"
                },
                "Demo.Workspace/Book.Lakehouse/Files",
                "--block_on_path_collision",
            ],
            &files,
            pending(),
        )
        .await;
        assert_ne!(output.plan.exit_code, 0);
        let mut stdout = Vec::new();
        output.write_to(&mut stdout, &mut Vec::new()).unwrap();
        let envelope: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], "NotSupported");
        assert_eq!(
            envelope["result"]["message"],
            "Directory copy requires --recursive"
        );
        assert!(
            host.requests()
                .iter()
                .all(|request| { request["method"] == "GET" || request["method"] == "HEAD" })
        );
        assert_eq!(
            std::fs::read(host.root.join("tree").join("keep")).unwrap(),
            b"keep"
        );
        assert!(host.root.join("tree").join("empty").is_dir());
    }
}

#[tokio::test]
async fn onelake_directory_create_and_recursive_delete_reach_the_production_storage_host() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    host.reply(201, Value::Null);
    let output = host
        .execute(&["mkdir", "Demo.Workspace/Book.Lakehouse/Files/new"])
        .await;
    success(&output);
    assert!(matches!(
        &output.evidence, FamilyEvidence::Storage(Ok(receipt))
        if receipt.outcome.unwrap().status() == 201
    ));
    let requests = host.requests();
    assert_eq!(requests.last().unwrap()["method"], "PUT");
    assert!(
        requests.last().unwrap()["url"]
            .as_str()
            .unwrap()
            .ends_with("/Files/new/?resource=directory")
    );

    let mut host = Harness::new();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
    host.reply(202, Value::Null);
    let output = host
        .execute(&["del", "Demo.Workspace/Book.Lakehouse/Files/tree", "-f"])
        .await;
    success(&output);
    assert!(matches!(
        &output.evidence, FamilyEvidence::Storage(Ok(receipt))
        if receipt.outcome == Some(fabric_sdk_onelake::OneLakeWriteOutcome::Accepted)
    ));
    let requests = host.requests();
    assert_eq!(requests.last().unwrap()["method"], "DELETE");
    assert!(
        requests.last().unwrap()["url"]
            .as_str()
            .unwrap()
            .ends_with("/Files/tree/?recursive=true")
    );
}

#[tokio::test]
async fn shortcut_removal_uses_fabric_metadata_not_recursive_storage_deletion() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.headers(
        200,
        Value::Null,
        &[
            ("x-ms-resource-type", "directory"),
            ("x-ms-onelake-shortcut-path", "true"),
        ],
    );
    host.reply(200, Value::Null);
    let output = host
        .execute(&[
            "rm",
            "Demo.Workspace/Book.Lakehouse/Files/link.Shortcut",
            "-f",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.last().unwrap()["method"], "DELETE");
    assert_eq!(
        requests.last().unwrap()["url"],
        "https://api.fabric.microsoft.com/v1/workspaces/w/items/n/shortcuts/Files/link"
    );
    assert!(
        host.credential
            .0
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .iter()
            .all(|scope| scope == "https://api.fabric.microsoft.com/.default")
    );
}

#[tokio::test]
async fn root_recursive_local_copy_and_move_use_both_explicit_endpoint_capabilities() {
    for moving in [false, true] {
        let mut host = Harness::new();
        std::fs::create_dir_all(host.root.join("tree").join("nested").join("empty")).unwrap();
        std::fs::create_dir(host.root.join("output")).unwrap();
        std::fs::write(
            host.root.join("tree").join("nested").join("payload"),
            b"binary\n",
        )
        .unwrap();
        std::fs::write(host.root.join("tree").join("zero"), b"").unwrap();
        let files = files(&host);
        let args = if moving {
            vec!["mv", "tree", "output", "-f"]
        } else {
            vec!["cp", "tree", "output", "-r", "-f"]
        };
        let (output, _) = execute(&mut host, &args, &files, pending()).await;
        success(&output);
        assert_eq!(
            std::fs::read(
                host.root
                    .join("output")
                    .join("tree")
                    .join("nested")
                    .join("payload")
            )
            .unwrap(),
            b"binary\n"
        );
        assert!(
            host.root
                .join("output")
                .join("tree")
                .join("nested")
                .join("empty")
                .is_dir()
        );
        assert_eq!(
            std::fs::read(host.root.join("output").join("tree").join("zero")).unwrap(),
            b""
        );
        assert_eq!(host.root.join("tree").exists(), !moving);
        assert!(host.requests().is_empty());
    }
}

#[tokio::test]
async fn root_empty_file_copy_accepts_a_missing_relative_destination() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("zero"), b"").unwrap();
    let files = files(&host);
    let (output, _) = execute(&mut host, &["cp", "zero", "empty.out"], &files, pending()).await;
    success(&output);
    assert_eq!(std::fs::read(host.root.join("empty.out")).unwrap(), b"");
    assert!(
        matches!(output.evidence,FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) if receipt.local_writes[0].1 == 0)
    );
    assert!(host.requests().is_empty());
}

#[tokio::test]
async fn root_recursive_collision_policy_preflights_before_any_write_or_confirmation() {
    for (moving, collision) in [(false, false), (false, true), (true, true)] {
        let mut host = Harness::new();
        std::fs::create_dir_all(host.root.join("tree").join("a-new")).unwrap();
        std::fs::create_dir_all(host.root.join("output").join("tree")).unwrap();
        std::fs::write(host.root.join("tree").join("a-new").join("payload"), b"new").unwrap();
        std::fs::write(host.root.join("tree").join("z-file"), b"source").unwrap();
        std::fs::write(
            host.root.join("output").join("tree").join("unrelated"),
            b"keep",
        )
        .unwrap();
        if collision {
            std::fs::write(
                host.root.join("output").join("tree").join("z-file"),
                b"destination",
            )
            .unwrap();
        }
        let files = files(&host);
        let (output, _) = execute(
            &mut host,
            &[if moving { "mv" } else { "cp" }, "tree", "output", "-r"],
            &files,
            pending(),
        )
        .await;
        if collision {
            assert_ne!(output.plan.exit_code, 0);
            assert!(
                matches!(output.evidence,FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) if matches!(&error.failure,fabric_cli_storage_files::TreeFailure::Command(error) if error.code == "AlreadyExists") && error.partial.local_directories.is_empty() && error.partial.local_writes.is_empty() && !error.source_delete_may_have_occurred)
            );
            assert!(!host.root.join("output").join("tree").join("a-new").exists());
            assert_eq!(
                std::fs::read(host.root.join("output").join("tree").join("z-file")).unwrap(),
                b"destination"
            );
        } else {
            success(&output);
            assert_eq!(
                std::fs::read(
                    host.root
                        .join("output")
                        .join("tree")
                        .join("a-new")
                        .join("payload")
                )
                .unwrap(),
                b"new"
            );
        }
        assert_eq!(
            std::fs::read(host.root.join("tree").join("z-file")).unwrap(),
            b"source"
        );
        assert_eq!(
            std::fs::read(host.root.join("output").join("tree").join("unrelated")).unwrap(),
            b"keep"
        );
    }
}

#[tokio::test]
async fn root_single_file_overwrite_requires_force() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("source"), b"new").unwrap();
    std::fs::write(host.root.join("target"), b"keep").unwrap();
    let files = files(&host);
    let (output, _) = execute(&mut host, &["cp", "source", "target"], &files, pending()).await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(
        matches!(output.evidence,FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) if matches!(&error.failure,fabric_cli_storage_files::TreeFailure::Command(error) if error.code == "AlreadyExists"))
    );
    assert_eq!(std::fs::read(host.root.join("source")).unwrap(), b"new");
    assert_eq!(std::fs::read(host.root.join("target")).unwrap(), b"keep");
}

#[tokio::test]
async fn root_recursive_merge_honors_force_and_collision_block_without_removing_unrelated_files() {
    for block in [true, false] {
        let mut host = Harness::new();
        std::fs::create_dir(host.root.join("tree")).unwrap();
        std::fs::create_dir_all(host.root.join("output").join("tree")).unwrap();
        std::fs::write(host.root.join("tree").join("payload"), b"source").unwrap();
        std::fs::write(
            host.root.join("output").join("tree").join("payload"),
            b"old",
        )
        .unwrap();
        std::fs::write(
            host.root.join("output").join("tree").join("unrelated"),
            b"keep",
        )
        .unwrap();
        let files = files(&host);
        let mut args = vec!["cp", "tree", "output", "-r", "-f"];
        if block {
            args.push("--block_on_path_collision");
        }
        let (output, _) = execute(&mut host, &args, &files, pending()).await;
        if block {
            assert_ne!(output.plan.exit_code, 0);
            assert!(
                matches!(output.evidence,FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) if matches!(&error.failure,fabric_cli_storage_files::TreeFailure::Command(error) if error.code == "AlreadyExists"))
            );
        } else {
            success(&output);
        }
        assert_eq!(
            std::fs::read(host.root.join("tree").join("payload")).unwrap(),
            b"source"
        );
        assert_eq!(
            std::fs::read(host.root.join("output").join("tree").join("payload")).unwrap(),
            if block {
                b"old".as_slice()
            } else {
                b"source".as_slice()
            }
        );
        assert_eq!(
            std::fs::read(host.root.join("output").join("tree").join("unrelated")).unwrap(),
            b"keep"
        );
        assert!(host.requests().is_empty());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn root_native_case_alias_preserves_the_requested_destination_basename() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("input.bin"), b"data").unwrap();
    std::fs::create_dir(host.root.join("output")).unwrap();
    let files = files(&host);
    let (output, _) = execute(&mut host, &["cp", "INPUT.BIN", "output"], &files, pending()).await;
    success(&output);
    let name = std::fs::read_dir(host.root.join("output"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    assert_eq!(name, std::ffi::OsStr::new("INPUT.BIN"));
}

#[tokio::test]
async fn root_single_file_upload_preserves_explicit_shortcut_destination_support() {
    let mut host = Harness::new();
    std::fs::write(host.root.join("payload.bin"), b"data").unwrap();
    host.item("Lakehouse");
    host.headers(
        200,
        Value::Null,
        &[
            ("x-ms-resource-type", "directory"),
            ("x-ms-onelake-shortcut-path", "true"),
        ],
    );
    host.headers(
        200,
        Value::Null,
        &[
            ("x-ms-resource-type", "file"),
            ("x-ms-onelake-shortcut-path", "true"),
        ],
    );
    file_version(&host, 3);
    host.headers(
        200,
        Value::Null,
        &[
            ("x-ms-resource-type", "file"),
            ("x-ms-onelake-shortcut-path", "true"),
        ],
    );
    for status in [201, 202, 200] {
        write_reply(&host, status);
    }
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "payload.bin",
            "Demo.Workspace/Book.Lakehouse/Files/link.Shortcut",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert!(
        matches!(output.evidence,FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) if receipt.uploads[0].0.relative() == "Files/link/payload.bin")
    );
    assert_eq!(
        std::fs::read(host.root.join("payload.bin")).unwrap(),
        b"data"
    );
}

#[tokio::test]
async fn root_large_local_move_streams_every_file_before_source_cleanup() {
    let mut host = Harness::new();
    std::fs::create_dir_all(host.root.join("tree").join("empty")).unwrap();
    std::fs::write(host.root.join("tree").join("a-small"), b"small").unwrap();
    std::fs::write(
        host.root.join("tree").join("z-large"),
        vec![b'x'; 4 * 1024 * 1024 + 1],
    )
    .unwrap();
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &["mv", "tree", "output", "-r", "-f"],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert!(!host.root.join("tree").exists());
    assert!(
        matches!(output.evidence, FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt)))
        if receipt.destination_copy_complete && receipt.local_writes.len() == 2)
    );
    assert_eq!(
        std::fs::metadata(host.root.join("output").join("z-large"))
            .unwrap()
            .len(),
        4 * 1024 * 1024 + 1
    );
    assert!(host.root.join("output").join("empty").is_dir());
    assert_eq!(
        std::fs::read(host.root.join("output").join("a-small")).unwrap(),
        b"small"
    );
}

#[tokio::test]
async fn root_large_file_copy_and_move_stream_all_four_endpoint_pairs() {
    let chunk = "x".repeat(4 * 1024 * 1024);
    let tail = "tail\n\0large-file";
    let length = chunk.len() + tail.len();
    for moving in [false, true] {
        for (local_source, local_target) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let mut host = Harness::new();
            if local_source {
                let mut file = std::fs::File::create(host.root.join("source.bin")).unwrap();
                use std::io::Write;
                file.write_all(chunk.as_bytes()).unwrap();
                file.write_all(tail.as_bytes()).unwrap();
            }
            if !local_source || !local_target {
                host.metadata(json!([
                    {"id":"n","displayName":"Book","type":"Lakehouse"},
                    {"id":"m","displayName":"Other","type":"Lakehouse"}
                ]));
                if !local_source {
                    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
                }
                if !local_target {
                    host.reply(404, Value::Null);
                }
            }
            if !local_source {
                file_version(&host, length);
            }
            if !local_target {
                write_reply(&host, 201);
            }
            for (offset, bytes) in [(0, chunk.as_str()), (chunk.len(), tail)] {
                if !local_source {
                    file_range(&host, bytes, offset, length);
                }
                if !local_target {
                    write_reply(&host, 202);
                }
            }
            if !local_target {
                write_reply(&host, 200);
            }
            if moving && !local_source {
                host.reply(204, Value::Null);
            }
            let files = files(&host);
            let (output, _) = execute(
                &mut host,
                &[
                    if moving { "mv" } else { "cp" },
                    if local_source {
                        "source.bin"
                    } else {
                        "Demo.Workspace/Book.Lakehouse/Files/source.bin"
                    },
                    if local_target {
                        "target.bin"
                    } else {
                        "Demo.Workspace/Other.Lakehouse/Files/target.bin"
                    },
                    "-f",
                ],
                &files,
                pending(),
            )
            .await;
            success(&output);
            let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) =
                output.evidence
            else {
                panic!("missing incremental production receipt");
            };
            assert!(receipt.destination_copy_complete);
            if local_target {
                use std::io::Read;
                let mut file = std::fs::File::open(host.root.join("target.bin")).unwrap();
                assert_eq!(file.metadata().unwrap().len(), length as u64);
                let mut bytes = vec![0; chunk.len()];
                file.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, chunk.as_bytes());
                let mut final_bytes = Vec::new();
                file.read_to_end(&mut final_bytes).unwrap();
                assert_eq!(final_bytes, tail.as_bytes());
            } else {
                assert_eq!(receipt.upload_progress[0].1, length as u64);
                assert_eq!(receipt.upload_progress[0].2, 2);
                let requests = host.requests();
                let appends: Vec<_> = requests
                    .iter()
                    .filter(|request| request["url"].as_str().unwrap().contains("action=append"))
                    .collect();
                assert_eq!(appends.len(), 2);
                assert_eq!(appends[0]["raw_body"].as_str().unwrap(), chunk);
                assert_eq!(appends[1]["raw_body"].as_str().unwrap(), tail);
                assert!(
                    requests
                        .iter()
                        .filter(|request| request["method"] == "PATCH")
                        .all(|request| request["raw_body"].as_str().unwrap().len()
                            <= 4 * 1024 * 1024)
                );
            }
            if local_source {
                assert_eq!(host.root.join("source.bin").exists(), !moving);
            }
            if moving && !local_source {
                assert_eq!(receipt.source_deletes.len(), 1);
                assert_eq!(host.requests().last().unwrap()["method"], "DELETE");
            }
        }
    }
}

#[tokio::test]
async fn root_forced_cross_entry_hardlink_copy_preserves_every_source_identity() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join("src")).unwrap();
    std::fs::create_dir_all(host.root.join("out").join("src")).unwrap();
    std::fs::write(host.root.join("src").join("a"), b"AAAA").unwrap();
    std::fs::write(host.root.join("src").join("b"), b"BBBB").unwrap();
    std::fs::hard_link(
        host.root.join("src").join("b"),
        host.root.join("out").join("src").join("a"),
    )
    .unwrap();
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &["cp", "src", "out", "-r", "-f"],
        &files,
        pending(),
    )
    .await;
    success(&output);
    for (name, bytes) in [("a", b"AAAA"), ("b", b"BBBB")] {
        assert_eq!(
            std::fs::read(host.root.join("src").join(name)).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(host.root.join("out").join("src").join(name)).unwrap(),
            bytes
        );
    }
    assert!(host.requests().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn root_ambiguous_native_mapping_rejects_forced_copy_and_move_before_any_destination_effects()
{
    for moving in [false, true] {
        for existing in [false, true] {
            for (first, second, directories) in [
                ("A.bin", "a.bin", false),
                ("Ä.bin", "ä.bin", false),
                ("Dir/first.bin", "dir/second.bin", true),
                ("Δ/first.bin", "δ/second.bin", true),
            ] {
                let mut host = Harness::new();
                let output_root = host.root.join("out");
                let existing_tree = output_root.join("tree");
                let existing_name = second.split('/').next().unwrap();
                let existing_entry = existing_tree.join(existing_name);
                let sentinel = if directories {
                    existing_entry.join("keep")
                } else {
                    existing_entry.clone()
                };
                if existing {
                    std::fs::create_dir_all(if directories {
                        &existing_entry
                    } else {
                        &existing_tree
                    })
                    .unwrap();
                    std::fs::write(&sentinel, b"existing destination").unwrap();
                }
                host.item("Lakehouse");
                host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
                // Only metadata is queued: preflight must not fetch either
                // distinct payload or issue any remote mutation.
                let sources = [(first, "AAAA"), (second, "BBBB")];
                host.reply(
                    200,
                    json!({"paths":sources.iter().map(|(name, bytes)| json!({
                    "name":format!("n/Files/tree/{name}"),
                    "isDirectory":false,
                    "contentLength":bytes.len().to_string(),
                    "etag":format!("\"{bytes}\""),
                })).collect::<Vec<_>>()}),
                );
                let files = files(&host);
                let (output, _) = execute(
                    &mut host,
                    &[
                        if moving { "mv" } else { "cp" },
                        "Demo.Workspace/Book.Lakehouse/Files/tree",
                        "out",
                        "-r",
                        "-f",
                    ],
                    &files,
                    pending(),
                )
                .await;
                let mut stdout = Vec::new();
                assert_eq!(output.write_to(&mut stdout, &mut Vec::new()).unwrap(), 1);
                let envelope: Value = serde_json::from_slice(&stdout).unwrap();
                assert_eq!(envelope["result"]["error_code"], "AmbiguousDestination");
                let FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) =
                    output.evidence
                else {
                    panic!("expected full-preflight mapping rejection");
                };
                assert_eq!(error.stage, fabric_cli_storage_files::TreeStage::Preflight);
                assert!(
                    error.partial.local_writes.is_empty()
                        && error.partial.local_directories.is_empty()
                );
                assert!(
                    error.partial.local_deletes.is_empty()
                        && error.partial.source_deletes.is_empty()
                );
                assert!(
                    !error.source_delete_may_have_occurred && !error.local_write_may_have_occurred
                );
                assert!(!error.partial.destination_copy_complete);
                let requests = host.requests();
                assert_eq!(requests.len(), 4);
                assert!(
                    requests
                        .iter()
                        .all(|request| matches!(request["method"].as_str(), Some("GET" | "HEAD")))
                );
                if existing {
                    assert_eq!(std::fs::read(&sentinel).unwrap(), b"existing destination");
                    assert_eq!(std::fs::read_dir(&output_root).unwrap().count(), 1);
                    let names = std::fs::read_dir(&existing_tree)
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .collect::<Vec<_>>();
                    assert_eq!(names, [std::ffi::OsString::from(existing_name)]);
                    if directories {
                        assert_eq!(std::fs::read_dir(existing_entry).unwrap().count(), 1);
                    }
                } else {
                    assert!(!output_root.exists());
                }
            }
        }
    }
}

#[tokio::test]
async fn root_distinct_unicode_native_mappings_keep_the_requested_names_and_bytes() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
    let listing = json!({"paths":[
        {"name":"n/Files/tree/Ä.bin","isDirectory":false,"etag":"\"v1\""},
        {"name":"n/Files/tree/β.bin","isDirectory":false,"etag":"\"v1\""}
    ]});
    host.reply(200, listing.clone());
    for bytes in ["AAAA", "BBBB"] {
        file_version(&host, bytes.len());
        file_range(&host, bytes, 0, bytes.len());
    }
    host.reply(200, listing);
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "Demo.Workspace/Book.Lakehouse/Files/tree",
            "out",
            "-r",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    for (name, bytes) in [("Ä.bin", b"AAAA"), ("β.bin", b"BBBB")] {
        assert_eq!(
            std::fs::read(host.root.join("out").join(name)).unwrap(),
            bytes
        );
    }
    assert!(
        host.requests()
            .iter()
            .all(|request| matches!(request["method"].as_str(), Some("GET" | "HEAD")))
    );
}

#[tokio::test]
async fn root_cleanup_failures_render_complete_copy_partial_removal_and_active_uncertainty() {
    for failed_delete in [202, 403] {
        let mut host = Harness::new();
        host.item("Lakehouse");
        host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        let listing = json!({"paths":[
            {"name":"n/Files/tree/a.bin","isDirectory":false,"etag":"\"v1\""},
            {"name":"n/Files/tree/z-empty","isDirectory":true}
        ]});
        host.reply(200, listing.clone());
        file_version(&host, 4);
        file_range(&host, "data", 0, 4);
        host.reply(200, listing);
        for etag in ["\"root\"", "\"empty\""] {
            host.headers(
                200,
                Value::Null,
                &[("x-ms-resource-type", "directory"), ("etag", etag)],
            );
        }
        host.reply(204, Value::Null);
        host.reply(
            failed_delete,
            json!({"error":{"code":"PrivateCode","message":"secret-token C:\\private\\source"}}),
        );
        let files = files(&host);
        let (output, _) = execute(
            &mut host,
            &[
                "mv",
                "Demo.Workspace/Book.Lakehouse/Files/tree",
                "download",
                "-r",
                "-f",
            ],
            &files,
            pending(),
        )
        .await;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
        let rendered = String::from_utf8(stdout).unwrap();
        assert!(rendered.contains("destination copy complete"), "{rendered}");
        assert!(rendered.contains("phase: source cleanup"), "{rendered}");
        assert!(
            rendered.contains("source removal partial (1 confirmed entries)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("active source cleanup uncertain"),
            "{rendered}"
        );
        assert!(rendered.contains("no rollback performed"), "{rendered}");
        for private in ["secret-token", "private", "Demo.Workspace", "PrivateCode"] {
            assert!(!rendered.contains(private), "{rendered}");
        }
        assert_eq!(
            std::fs::read(host.root.join("download").join("a.bin")).unwrap(),
            b"data"
        );
        let FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) = output.evidence
        else {
            panic!("missing cleanup failure evidence");
        };
        assert!(error.partial.destination_copy_complete);
        assert!(error.source_delete_may_have_occurred);
        if failed_delete == 403 {
            assert!(
                matches!(error.failure, fabric_cli_storage_files::TreeFailure::Storage(error)
                if matches!(error.kind, fabric_sdk_onelake::OneLakeError::Http { status: 403, .. }))
            );
        } else {
            assert!(
                matches!(error.failure, fabric_cli_storage_files::TreeFailure::Command(error) if error.code == "Incomplete")
            );
        }
    }
}

#[tokio::test]
async fn root_cancelled_large_upload_retains_the_complete_source_and_never_flushes_or_deletes() {
    let mut host = Harness::new();
    std::fs::write(
        host.root.join("source.bin"),
        vec![b'x'; 4 * 1024 * 1024 + 17],
    )
    .unwrap();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    write_reply(&host, 201);
    write_reply(&host, 202);
    let http = host.http.clone();
    let cancelled = std::future::poll_fn(move |context| {
        if http
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["url"].as_str().unwrap().contains("action=append"))
        {
            std::task::Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    });
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "mv",
            "source.bin",
            "Demo.Workspace/Book.Lakehouse/Files/target.bin",
            "-f",
        ],
        &files,
        cancelled,
    )
    .await;
    assert_eq!(output.plan.exit_code, 1);
    assert_eq!(
        std::fs::metadata(host.root.join("source.bin"))
            .unwrap()
            .len(),
        4 * 1024 * 1024 + 17
    );
    assert!(
        matches!(output.evidence, FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error)))
        if !error.source_delete_may_have_occurred && error.partial.local_deletes.is_empty() && !error.partial.destination_copy_complete)
    );
    assert!(
        host.requests()
            .iter()
            .all(|request| request["method"] != "DELETE"
                && !request["url"].as_str().unwrap().contains("action=flush"))
    );
}

#[tokio::test]
async fn root_empty_directory_upload_commits_without_an_invented_file_payload() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join("empty")).unwrap();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    host.reply(201, Value::Null);
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "empty",
            "Demo.Workspace/Book.Lakehouse/Files",
            "-r",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert!(
        matches!(output.evidence,FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) if receipt.uploads.is_empty() && receipt.directories.len() == 1)
    );
    assert!(host.root.join("empty").is_dir());
    assert_eq!(host.requests().len(), 4);
}

#[tokio::test]
async fn root_large_failed_append_and_accepted_flush_retain_all_sources_and_bounded_receipts() {
    let length = 4 * 1024 * 1024 + 17;
    for failed_append in [false, true] {
        let mut host = Harness::new();
        std::fs::write(host.root.join("source.bin"), vec![b'x'; length]).unwrap();
        host.item("Lakehouse");
        host.reply(404, Value::Null);
        write_reply(&host, 201);
        write_reply(&host, 202);
        write_reply(&host, if failed_append { 403 } else { 202 });
        if !failed_append {
            write_reply(&host, 202);
        }
        let files = files(&host);
        let (output, _) = execute(
            &mut host,
            &[
                "mv",
                "source.bin",
                "Demo.Workspace/Book.Lakehouse/Files/target.bin",
                "-f",
            ],
            &files,
            pending(),
        )
        .await;
        assert_eq!(output.plan.exit_code, 1);
        let mut stdout = Vec::new();
        output.write_to(&mut stdout, &mut Vec::new()).unwrap();
        assert!(
            String::from_utf8(stdout)
                .unwrap()
                .contains("destination copy incomplete or unverified")
        );
        let FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error))) = output.evidence
        else {
            panic!("missing incomplete upload evidence");
        };
        assert!(std::error::Error::source(error.as_ref()).is_some());
        assert!(!error.source_delete_may_have_occurred);
        assert!(error.partial.local_deletes.is_empty());
        assert!(!error.partial.destination_copy_complete);
        assert_eq!(
            error.partial.upload_progress[0].1,
            if failed_append {
                4 * 1024 * 1024
            } else {
                length as u64
            }
        );
        assert_eq!(
            error.partial.upload_progress[0].2,
            if failed_append { 1 } else { 2 }
        );
        assert!(error.partial.uploads[0].1.flush.is_none());
        assert!(
            matches!(error.failure, fabric_cli_storage_files::TreeFailure::Chunk(error)
            if error.mutation_may_have_occurred && error.response.as_ref().is_some_and(|response| response.status == 202)
                || matches!(error.kind, fabric_sdk_onelake::OneLakeError::Http { status: 403, .. }))
        );
        assert_eq!(
            std::fs::metadata(host.root.join("source.bin"))
                .unwrap()
                .len(),
            length as u64
        );
        assert!(
            host.requests()
                .iter()
                .all(|request| request["method"] != "DELETE")
        );
    }
}

#[tokio::test]
async fn root_large_version_change_never_deletes_the_remote_source_or_completes_partial_local_output()
 {
    let mut host = Harness::new();
    let chunk = "x".repeat(4 * 1024 * 1024);
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    file_version(&host, chunk.len() + 17);
    file_range(&host, &chunk, 0, chunk.len() + 17);
    host.reply(
        412,
        json!({"error":{"message":"private-source secret-token"}}),
    );
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "mv",
            "Demo.Workspace/Book.Lakehouse/Files/source.bin",
            "download.bin",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    assert_eq!(output.plan.exit_code, 1);
    let mut stdout = Vec::new();
    output.write_to(&mut stdout, &mut Vec::new()).unwrap();
    let rendered = String::from_utf8(stdout).unwrap();
    assert!(
        rendered.contains("HTTP 412") && rendered.contains("destination write may have occurred"),
        "{rendered}"
    );
    assert!(!rendered.contains("private-source") && !rendered.contains("secret-token"));
    assert!(
        matches!(output.evidence, FamilyEvidence::ResourceFiles(Err(ResourceFileError::Tree(error)))
        if !error.source_delete_may_have_occurred && error.partial.local_deletes.is_empty()
            && error.local_write_may_have_occurred && !error.partial.destination_copy_complete)
    );
    assert_eq!(
        std::fs::metadata(host.root.join("download.bin"))
            .unwrap()
            .len(),
        chunk.len() as u64
    );
    assert!(
        host.requests()
            .iter()
            .all(|request| request["method"] != "DELETE")
    );
}

#[tokio::test]
async fn root_recursive_upload_reaches_every_nested_and_empty_entry() {
    let mut host = Harness::new();
    std::fs::create_dir_all(host.root.join("tree").join("empty")).unwrap();
    std::fs::write(host.root.join("tree").join("payload.bin"), b"binary\n").unwrap();
    std::fs::write(host.root.join("tree").join("zero"), b"").unwrap();
    host.item("Lakehouse");
    host.reply(404, Value::Null);
    for status in [201, 201, 201, 202, 200, 201, 200] {
        write_reply(&host, status);
    }
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "tree",
            "Demo.Workspace/Book.Lakehouse/Files",
            "-r",
            "-f",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) = output.evidence
    else {
        panic!("missing tree receipt")
    };
    assert_eq!(receipt.directories.len(), 2);
    assert_eq!(receipt.uploads.len(), 2);
    assert_eq!(receipt.uploads[0].0.relative(), "Files/tree/payload.bin");
    assert_eq!(receipt.uploads[1].1.append, None);
    assert!(host.root.join("tree").join("empty").is_dir());
    assert_eq!(host.requests().len(), 10);
}

#[tokio::test]
async fn root_recursive_download_pages_before_copying_and_preserves_empty_directories() {
    let mut host = Harness::new();
    host.item("Lakehouse");
    host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
    host.headers(
        200,
        json!({"paths":[{"name":"n/Files/tree/nested/payload","isDirectory":false}]}),
        &[("x-ms-continuation", "next+/=")],
    );
    host.reply(
        200,
        json!({"paths":[{"name":"n/Files/tree/empty","isDirectory":true}]}),
    );
    file_version(&host, 7);
    file_range(&host, "binary\n", 0, 7);
    host.reply(200,json!({"paths":[{"name":"n/Files/tree/nested/payload","isDirectory":false},{"name":"n/Files/tree/empty","isDirectory":true}]}));
    let files = files(&host);
    let (output, _) = execute(
        &mut host,
        &[
            "cp",
            "Demo.Workspace/Book.Lakehouse/Files/tree",
            "download",
            "-r",
        ],
        &files,
        pending(),
    )
    .await;
    success(&output);
    assert_eq!(
        std::fs::read(host.root.join("download").join("nested").join("payload")).unwrap(),
        b"binary\n"
    );
    assert!(host.root.join("download").join("empty").is_dir());
    let requests = host.requests();
    assert_eq!(requests.len(), 8);
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .contains("continuationToken=next%2B%2F%3D")
    );
}

#[tokio::test]
async fn root_recursive_remote_copy_and_remote_to_local_move_use_real_storage_stages() {
    for moving in [false, true] {
        let mut host = Harness::new();
        host.item("Lakehouse");
        host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        if !moving {
            host.reply(404, Value::Null);
        }

        let list = json!({"paths":[{"name":"n/Files/tree/a.bin","isDirectory":false,"etag":"\"v1\""},{"name":"n/Files/tree/empty","isDirectory":true}]});
        host.reply(200, list.clone());
        if !moving {
            host.reply(201, Value::Null);
        }
        file_version(&host, 7);
        if moving {
            file_range(&host, "binary\n", 0, 7);
            host.reply(200, list);
            host.headers(
                200,
                Value::Null,
                &[("x-ms-resource-type", "directory"), ("etag", "\"root\"")],
            );
            host.headers(
                200,
                Value::Null,
                &[("x-ms-resource-type", "directory"), ("etag", "\"empty\"")],
            );
            for _ in 0..3 {
                host.reply(204, Value::Null);
            }
        } else {
            write_reply(&host, 201);
            file_range(&host, "binary\n", 0, 7);
            for status in [202, 200, 201] {
                write_reply(&host, status);
            }
            host.reply(200, list);
        }
        let files = files(&host);
        let (output, _) = execute(
            &mut host,
            &[
                if moving { "mv" } else { "cp" },
                "Demo.Workspace/Book.Lakehouse/Files/tree",
                if moving {
                    "download"
                } else {
                    "Demo.Workspace/Book.Lakehouse/Files/copied"
                },
                "-r",
                "-f",
            ],
            &files,
            pending(),
        )
        .await;
        success(&output);
        let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) = output.evidence
        else {
            panic!("missing tree receipt")
        };
        if moving {
            assert_eq!(receipt.source_deletes.len(), 3);
            assert_eq!(
                std::fs::read(host.root.join("download").join("a.bin")).unwrap(),
                b"binary\n"
            );
            assert!(host.root.join("download").join("empty").is_dir());
        } else {
            assert_eq!(receipt.directories.len(), 2);
            assert_eq!(receipt.uploads.len(), 1);
            assert!(
                host.requests()
                    .iter()
                    .all(|request| request["method"] != "DELETE")
            );
        }
    }
}

#[tokio::test]
async fn root_cross_item_move_uses_committed_copy_before_conditional_source_delete() {
    let mut host = Harness::new();
    host.metadata(json!([
        {"id":"n","displayName":"Book","type":"Lakehouse"},
        {"id":"m","displayName":"Other","type":"Lakehouse"}
    ]));
    host.headers(200, Value::Null, &[("x-ms-resource-type", "file")]);
    host.reply(404, Value::Null);
    file_version(&host, 4);
    write_reply(&host, 201);
    file_range(&host, "data", 0, 4);
    for status in [202, 200, 204] {
        write_reply(&host, status);
    }
    let (output, _) = execute(
        &mut host,
        &[
            "mv",
            "Demo.Workspace/Book.Lakehouse/Files/source.bin",
            "Demo.Workspace/Other.Lakehouse/Files/moved.bin",
            "-f",
        ],
        &ForbiddenLocal,
        pending(),
    )
    .await;
    success(&output);
    let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) = output.evidence
    else {
        panic!("missing tree receipt")
    };
    assert!(receipt.rename.is_none());
    assert_eq!(receipt.uploads.len(), 1);
    assert_eq!(receipt.source_deletes.len(), 1);
    let requests = host.requests();
    assert!(requests.iter().any(|request| {
        request["url"]
            .as_str()
            .unwrap()
            .contains("/w/m/Files/moved.bin")
    }));
    assert_eq!(requests.last().unwrap()["method"], "DELETE");
}

#[tokio::test]
async fn root_cross_workspace_file_and_directory_moves_copy_then_conditionally_delete() {
    for directory in [false, true] {
        let mut host = Harness::new();
        host.reply(200,json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"},{"id":"v","displayName":"Other","type":"Workspace"}]}));
        host.reply(
            200,
            json!({"value":[{"id":"n","displayName":"Book","type":"Lakehouse"}]}),
        );
        host.headers(
            200,
            Value::Null,
            &[(
                "x-ms-resource-type",
                if directory { "directory" } else { "file" },
            )],
        );
        host.reply(
            200,
            json!({"value":[{"id":"m","displayName":"Book","type":"Lakehouse"}]}),
        );
        host.reply(404, Value::Null);
        let list = json!({"paths":[{"name":"n/Files/tree/a.bin","isDirectory":false,"etag":"\"v1\""},{"name":"n/Files/tree/empty","isDirectory":true}]});
        if directory {
            host.reply(200, list.clone());
            host.reply(201, Value::Null);
        }
        file_version(&host, 7);
        write_reply(&host, 201);
        file_range(&host, "binary\n", 0, 7);
        for status in [202, 200] {
            write_reply(&host, status);
        }
        if directory {
            host.reply(201, Value::Null);
            host.reply(200, list);
            host.headers(
                200,
                Value::Null,
                &[("x-ms-resource-type", "directory"), ("etag", "\"root\"")],
            );
            host.headers(
                200,
                Value::Null,
                &[("x-ms-resource-type", "directory"), ("etag", "\"empty\"")],
            );
            host.reply(204, Value::Null);
            host.reply(204, Value::Null);
        }
        host.reply(204, Value::Null);
        let (output, _) = execute(
            &mut host,
            &[
                "mv",
                if directory {
                    "Demo.Workspace/Book.Lakehouse/Files/tree"
                } else {
                    "Demo.Workspace/Book.Lakehouse/Files/source.bin"
                },
                "Other.Workspace/Book.Lakehouse/Files/moved",
                "-r",
                "-f",
            ],
            &ForbiddenLocal,
            pending(),
        )
        .await;
        success(&output);
        let FamilyEvidence::ResourceFiles(Ok(ResourceFileResult::Tree(receipt))) = output.evidence
        else {
            panic!("missing tree receipt")
        };
        assert!(receipt.rename.is_none());
        assert_eq!(receipt.uploads.len(), 1);
        assert_eq!(receipt.source_deletes.len(), if directory { 3 } else { 1 });
        let requests = host.requests();
        let first_delete = requests
            .iter()
            .position(|request| request["method"] == "DELETE")
            .unwrap();
        let flush = requests
            .iter()
            .position(|request| request["url"].as_str().unwrap().contains("action=flush"))
            .unwrap();
        assert!(first_delete > flush);
        assert!(requests.iter().any(|request| {
            request["url"]
                .as_str()
                .unwrap()
                .contains("/v/m/Files/moved")
        }));
    }
}
