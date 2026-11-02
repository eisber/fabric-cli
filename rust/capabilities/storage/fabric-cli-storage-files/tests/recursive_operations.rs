// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive owner-authorized contracts, not changes to the immutable Python oracle.

#![deny(unnameable_test_items)]

mod support;

use azure_core::http::{AsyncRawResponse, StatusCode, headers::Headers};
use fabric_cli_context::{
    Element, ItemType, LocalKind as ContextKind, LocalMetadata, NodeKind, OneLakeKind, PathElement,
    PathKind, ResolveOptions, WorkspaceType,
};
use fabric_cli_mutations::{
    CommandFuture, MutationError, MutationUi, NoticeKind, OneLakeWriteHost,
    frontdoor::{ResourceArgs, ResourceCommand, ResourceContext},
};
use fabric_cli_storage_files::{
    LocalEntry, LocalError, LocalErrorKind, LocalFileReader, LocalFileSystem, LocalFileWriter,
    LocalKind, LocalReadFile, OsFileSystem, ResourceFileHost, TreeFailure, TreeStage,
};
use fabric_sdk_onelake::{OneLakeLimits, OneLakeWriteLimits};
use serde_json::json;
use std::{
    fs,
    future::{pending, ready},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::{Fixture, Ui, response, setup};

struct Context;
impl ResourceContext for Context {
    fn resolve(
        &mut self,
        _: &[String],
        _: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError> {
        panic!("resolved additive transfers must not resolve again")
    }
}

struct ApprovedUi;
impl MutationUi for ApprovedUi {
    fn notice(&mut self, _: NoticeKind, _: &str) {}
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(ready(true))
    }
}

fn local(path: &Path) -> Arc<PathElement> {
    PathElement::local(LocalMetadata {
        path: path.to_str().unwrap().into(),
        name: path
            .file_name()
            .unwrap_or_default()
            .to_str()
            .unwrap()
            .into(),
        id: "local".into(),
        kind: ContextKind::Other,
    })
}

fn remote(workspace_id: &str, relative: &str, kind: OneLakeKind) -> Arc<PathElement> {
    let workspace = Element::child(
        "Team",
        Some(workspace_id.into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    let item = Element::child(
        "Lake",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("Lakehouse").unwrap()),
        workspace,
    )
    .unwrap();
    let mut node = PathElement::from_fabric(item);
    let parts: Vec<_> = relative.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        let selected = if index + 1 == parts.len() {
            kind
        } else {
            OneLakeKind::Folder
        };
        node = PathElement::child(
            *part,
            if selected == OneLakeKind::Undefined {
                None
            } else {
                Some("resolved".into())
            },
            PathKind::OneLake(selected),
            node,
        )
        .unwrap();
    }
    node
}

fn command(moving: bool, recursive: bool, force: bool, block: bool) -> ResourceCommand {
    let args = ResourceArgs {
        path: vec!["source".into()],
        output_format: None,
        skill: None,
    };
    if moving {
        ResourceCommand::Move {
            args,
            destination: vec!["target".into()],
            force,
            recursive,
        }
    } else {
        ResourceCommand::Copy {
            args,
            destination: vec!["target".into()],
            force,
            recursive,
            block_on_path_collision: block,
        }
    }
}

fn reply(status: u16, body: serde_json::Value, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut values = Headers::new();
    for (name, value) in headers {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let bytes = match body {
        serde_json::Value::String(value) => value.into_bytes(),
        serde_json::Value::Null => Vec::new(),
        value => serde_json::to_vec(&value).unwrap(),
    };
    AsyncRawResponse::from_bytes(StatusCode::from(status), values, bytes)
}

fn listing() -> serde_json::Value {
    json!({"paths":[{"name":"item/Files/tree/a.bin","isDirectory":false,"etag":"\"v1\""},{"name":"item/Files/tree/empty","isDirectory":true}]})
}

fn created() -> AsyncRawResponse {
    reply(201, serde_json::Value::Null, &[("etag", "\"target\"")])
}

fn version(length: usize) -> AsyncRawResponse {
    reply(
        200,
        serde_json::Value::Null,
        &[
            ("content-length", &length.to_string()),
            ("etag", "\"v1\""),
            ("x-ms-resource-type", "file"),
        ],
    )
}

fn range(bytes: &[u8]) -> AsyncRawResponse {
    let mut headers = Headers::new();
    headers.insert("content-length", bytes.len().to_string());
    headers.insert("etag", "\"v1\"");
    headers.insert(
        "content-range",
        format!("bytes 0-{}/{}", bytes.len() - 1, bytes.len()),
    );
    AsyncRawResponse::from_bytes(StatusCode::from(206), headers, bytes.to_vec())
}

#[tokio::test]
async fn local_recursive_copy_and_move_preserve_nested_empty_directories_and_zero_byte_files() {
    for moving in [false, true] {
        let fixture = Fixture::new();
        let from = fixture.path().join("tree");
        let parent = fixture.path().join("output");
        fs::create_dir_all(from.join("nested").join("empty")).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(from.join("nested").join("data"), b"\0\xff\n").unwrap();
        fs::write(from.join("zero"), b"").unwrap();
        let files = fixture.adapter().await;
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let result = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(moving, !moving, true, false),
            &local(&from),
            &local(&parent),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read(parent.join("tree").join("nested").join("data")).unwrap(),
            b"\0\xff\n"
        );
        assert!(parent.join("tree").join("nested").join("empty").is_dir());
        assert_eq!(fs::read(parent.join("tree").join("zero")).unwrap(), b"");
        assert_eq!(result.local_writes.len(), 2);
        assert_eq!(from.exists(), !moving);
        assert_eq!(result.local_deletes.is_empty(), !moving);
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn directory_upload_creates_empty_directories_and_flushes_zero_byte_files() {
    let fixture = Fixture::new();
    let from = fixture.path().join("tree");
    fs::create_dir_all(from.join("empty")).unwrap();
    fs::write(from.join("payload"), b"data").unwrap();
    fs::write(from.join("zero"), b"").unwrap();
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![
        response(201, b""),
        response(201, b""),
        created(),
        response(202, b""),
        response(200, b""),
        created(),
        response(200, b""),
    ]);
    let mut ui = Ui::default();
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(false, true, true, false),
        &local(&from),
        &remote("workspace", "Files/new", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.directories.len(), 2);
    assert_eq!(result.uploads.len(), 2);
    assert_eq!(result.uploads[1].1.append, None);
    assert_eq!(
        network.requests.lock().unwrap().last().unwrap()["url"]
            .as_str()
            .unwrap()
            .split('?')
            .next_back()
            .unwrap(),
        "action=flush&position=0&close=true"
    );
    assert!(from.join("empty").is_dir());
}

#[tokio::test]
async fn paginated_download_materializes_implicit_parents_and_empty_directories() {
    let fixture = Fixture::new();
    let target = fixture.path().join("download");
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![
        reply(
            200,
            json!({"paths":[{"name":"item/Files/tree/nested/data","isDirectory":false}]}),
            &[("x-ms-continuation", "next+/=")],
        ),
        reply(
            200,
            json!({"paths":[{"name":"item/Files/tree/empty","isDirectory":true},{"name":"item/Files/tree/zero","isDirectory":false}]}),
            &[],
        ),
        version(2),
        range(b"\0\xff"),
        version(0),
        reply(
            200,
            json!({"paths":[{"name":"item/Files/tree/nested/data","isDirectory":false},{"name":"item/Files/tree/empty","isDirectory":true},{"name":"item/Files/tree/zero","isDirectory":false}]}),
            &[],
        ),
    ]);
    let mut ui = Ui::default();
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(false, true, true, false),
        &remote("workspace", "Files/tree", OneLakeKind::Folder),
        &local(&target),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.local_writes.len(), 2);
    assert_eq!(
        fs::read(target.join("nested").join("data")).unwrap(),
        b"\0\xff"
    );
    assert_eq!(fs::read(target.join("zero")).unwrap(), b"");
    assert!(target.join("empty").is_dir());
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(
        requests[1]["url"]
            .as_str()
            .unwrap()
            .contains("continuationToken=next%2B%2F%3D")
    );
}

#[tokio::test]
async fn remote_tree_copy_reuses_listing_and_uploads_without_deleting_source() {
    let (client, adapter, network) = setup(vec![
        reply(200, listing(), &[]),
        response(201, b""),
        version(4),
        created(),
        range(b"data"),
        response(202, b""),
        response(200, b""),
        response(201, b""),
        reply(200, listing(), &[]),
    ]);
    let mut ui = Ui::default();
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(false, true, true, false),
        &remote("workspace", "Files/tree", OneLakeKind::Folder),
        &remote("workspace", "Files/copied", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.uploads.len(), 1);
    assert_eq!(result.directories.len(), 2);
    assert!(
        network
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["method"] != "delete")
    );
}

#[tokio::test]
async fn failed_or_accepted_upload_retains_the_complete_local_move_source() {
    for flush in [202, 403] {
        let fixture = Fixture::new();
        let from = fixture.path().join("tree");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("payload"), b"keep").unwrap();
        let files = fixture.adapter().await;
        let (client, adapter, network) = setup(vec![
            response(201, b""),
            created(),
            response(202, b""),
            response(flush, b""),
        ]);
        let mut ui = Ui::default();
        let error = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(true, true, true, false),
            &local(&from),
            &remote("workspace", "Files/new", OneLakeKind::Undefined),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(!error.source_delete_may_have_occurred);
        assert!(error.partial.local_deletes.is_empty());
        assert_eq!(fs::read(from.join("payload")).unwrap(), b"keep");
        assert_eq!(network.requests.lock().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn remote_directory_move_deletes_only_after_all_copies_commit_with_etag_and_nonrecursive_guards()
 {
    let fixture = Fixture::new();
    let target = fixture.path().join("download");
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![
        reply(200, listing(), &[]),
        version(4),
        range(b"data"),
        reply(200, listing(), &[]),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory"), ("etag", "\"root\"")],
        ),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory"), ("etag", "\"empty\"")],
        ),
        response(204, b""),
        response(204, b""),
        response(204, b""),
    ]);
    let mut ui = Ui::default();
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(true, true, true, false),
        &remote("workspace", "Files/tree", OneLakeKind::Folder),
        &local(&target),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.source_deletes.len(), 3);
    assert_eq!(fs::read(target.join("a.bin")).unwrap(), b"data");
    assert!(target.join("empty").is_dir());
    let requests = network.requests.lock().unwrap();
    assert!(
        requests[..4]
            .iter()
            .all(|request| request["method"] == "get" || request["method"] == "head")
    );
    assert!(
        requests[4..6]
            .iter()
            .all(|request| request["method"] == "head")
    );
    assert!(requests[6..].iter().all(|request| {
        request["method"] == "delete"
            && request["url"]
                .as_str()
                .unwrap()
                .ends_with("recursive=false")
    }));
    assert_eq!(requests[7]["headers"]["if-match"], "\"v1\"");
}

#[tokio::test]
async fn missing_etag_or_changed_remote_tree_never_deletes_move_sources() {
    for missing_etag in [true, false] {
        let fixture = Fixture::new();
        let files = fixture.adapter().await;
        let mut replies = vec![
            reply(200, listing(), &[]),
            if missing_etag {
                reply(200, serde_json::Value::Null, &[("content-length", "4")])
            } else {
                version(4)
            },
        ];
        if !missing_etag {
            replies.push(range(b"data"));
            replies.push(reply(200, json!({"paths":[]}), &[]));
        }
        let (client, adapter, network) = setup(replies);
        let mut ui = Ui::default();
        let error = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(true, true, true, false),
            &remote("workspace", "Files/tree", OneLakeKind::Folder),
            &local(&fixture.path().join("out")),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(!error.source_delete_may_have_occurred);
        assert!(
            matches!(&error.failure,TreeFailure::Command(error) if error.code == "SourceChanged")
                || matches!(
                    &error.failure,
                    TreeFailure::Read(fabric_sdk_onelake::OneLakeError::InvalidResponse)
                )
        );
        assert!(
            network
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request["method"] != "delete")
        );
    }
}

#[tokio::test]
async fn collision_block_and_unforced_overwrite_never_write_or_delete() {
    for block in [true, false] {
        let fixture = Fixture::new();
        let from = fixture.path().join("tree");
        let target = fixture.path().join("out");
        fs::create_dir(&from).unwrap();
        fs::create_dir_all(target.join("tree")).unwrap();
        fs::write(from.join("payload"), b"source").unwrap();
        fs::write(target.join("tree").join("payload"), b"destination").unwrap();
        let files = fixture.adapter().await;
        let (client, adapter, _) = setup(vec![]);
        let mut ui = Ui::default();
        let result = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(false, true, block, block),
            &local(&from),
            &local(&target),
            pending(),
        )
        .await;
        assert!(
            matches!(result.unwrap_err().failure,TreeFailure::Command(error) if error.code == "AlreadyExists")
        );
        assert!(ui.prompts.is_empty());
        assert_eq!(fs::read(from.join("payload")).unwrap(), b"source");
        assert_eq!(
            fs::read(target.join("tree").join("payload")).unwrap(),
            b"destination"
        );
    }
}

#[tokio::test]
async fn overlap_bounds_and_ready_cancellation_fail_before_any_mutation() {
    let fixture = Fixture::new();
    let from = fixture.path().join("tree");
    fs::create_dir(&from).unwrap();
    fs::write(from.join("payload"), b"source").unwrap();
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    for (overlap, cancelled) in [(true, false), (false, true), (false, false)] {
        let mut host = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits {
                    max_paths: NonZeroUsize::new(1).unwrap(),
                    ..OneLakeLimits::default()
                },
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        };
        let destination = if overlap {
            from.join("nested")
        } else {
            fixture.path().join("out")
        };
        let error = host
            .execute_full_resolved(
                &command(false, true, true, false),
                &local(&from),
                &local(&destination),
                async move {
                    if cancelled {
                        ready(()).await;
                    } else {
                        pending::<()>().await;
                    }
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.stage, TreeStage::Preflight);
        assert!(!destination.exists());
        assert_eq!(fs::read(from.join("payload")).unwrap(), b"source");
    }
    assert!(network.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malicious_remote_listing_and_uncertain_rename_never_trigger_fallback_deletions() {
    let (client, adapter, network) = setup(vec![reply(
        200,
        json!({"paths":[{"name":"item/Files/other/escape","isDirectory":false}]}),
        &[],
    )]);
    let mut ui = Ui::default();
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(false, true, true, false),
        &remote("workspace", "Files/tree", OneLakeKind::Folder),
        &remote("workspace", "Files/new", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.stage, TreeStage::Preflight);
    assert_eq!(network.requests.lock().unwrap().len(), 1);
    let (client, adapter, network) = setup(vec![
        reply(200, json!({"paths":[]}), &[]),
        reply(200, json!({"paths":[]}), &[]),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory")],
        ),
        response(404, b""),
    ]);
    network
        .replies
        .lock()
        .unwrap()
        .push_back(Err(support::failed_response()));
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(true, true, true, false),
        &remote("workspace", "Files/日本語", OneLakeKind::Folder),
        &remote("workspace", "Files/new", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.stage, TreeStage::Rename);
    assert!(error.source_delete_may_have_occurred);
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[4]["method"], "put");
    assert!(
        requests[4]["headers"]["x-ms-rename-source"]
            .as_str()
            .unwrap()
            .contains("%E6%97%A5")
    );
    assert_eq!(requests[4]["headers"]["if-none-match"], "*");
}

#[tokio::test]
async fn existing_directories_merge_without_force_when_no_file_is_overwritten() {
    for moving in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.path().join("tree");
        let target = fixture.path().join("output");
        fs::create_dir_all(source.join("empty")).unwrap();
        fs::create_dir_all(target.join("tree")).unwrap();
        fs::write(source.join("payload"), b"new").unwrap();
        fs::write(target.join("tree").join("unrelated"), b"keep").unwrap();
        let files = fixture.adapter().await;
        let (client, executor, network) = setup(vec![]);
        let result = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &executor,
                ui: &mut ApprovedUi,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(moving, true, false, false),
            &local(&source),
            &local(&target),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.local_writes.len(), 1);
        assert_eq!(
            fs::read(target.join("tree").join("payload")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(target.join("tree").join("unrelated")).unwrap(),
            b"keep"
        );
        assert!(target.join("tree").join("empty").is_dir());
        assert_eq!(source.exists(), !moving);
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_late_tree_collision_is_found_before_any_destination_creation_or_move_confirmation() {
    for moving in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.path().join("tree");
        let target = fixture.path().join("output");
        fs::create_dir_all(source.join("a-new")).unwrap();
        fs::create_dir_all(target.join("tree")).unwrap();
        fs::write(source.join("a-new").join("payload"), b"new").unwrap();
        fs::write(source.join("z-conflict"), b"source").unwrap();
        fs::write(target.join("tree").join("z-conflict"), b"destination").unwrap();
        let files = fixture.adapter().await;
        let (client, executor, _) = setup(vec![]);
        let mut ui = Ui::default();
        let error = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &executor,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(moving, true, false, false),
            &local(&source),
            &local(&target),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error.failure,TreeFailure::Command(error) if error.code == "AlreadyExists")
        );
        assert!(!error.source_delete_may_have_occurred);
        assert!(
            error.partial.local_writes.is_empty() && error.partial.local_directories.is_empty()
        );
        assert!(!target.join("tree").join("a-new").exists());
        assert_eq!(
            fs::read(source.join("a-new").join("payload")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(target.join("tree").join("z-conflict")).unwrap(),
            b"destination"
        );
        assert!(ui.prompts.is_empty());
    }
}

#[tokio::test]
async fn remote_paginated_collision_preflight_never_creates_earlier_new_directories() {
    let (client, executor, network) = setup(vec![
        reply(
            200,
            json!({"paths":[{"name":"item/Files/tree/a-new/payload","isDirectory":false}]}),
            &[("x-ms-continuation", "next")],
        ),
        reply(
            200,
            json!({"paths":[{"name":"item/Files/tree/z-conflict","isDirectory":false}]}),
            &[],
        ),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory")],
        ),
        response(404, b""),
        response(404, b""),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "file")],
        ),
    ]);
    let mut ui = Ui::default();
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(false, true, false, false),
        &remote("source", "Files/tree", OneLakeKind::Folder),
        &remote("destination", "Files", OneLakeKind::Folder),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.failure,TreeFailure::Command(error) if error.code == "AlreadyExists"));
    assert!(error.partial.uploads.is_empty() && error.partial.directories.is_empty());
    assert!(
        network
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["method"] == "get" || request["method"] == "head")
    );
    assert_eq!(network.requests.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn same_item_directory_merge_moves_copy_every_file_before_deleting_sources() {
    let list = json!({"paths":[{"name":"item/Files/from/a","isDirectory":false,"etag":"\"v1\""}]});
    let (client, executor, network) = setup(vec![
        reply(200, list.clone(), &[]),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory")],
        ),
        response(404, b""),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory")],
        ),
        version(4),
        created(),
        range(b"data"),
        response(202, b""),
        response(200, b""),
        reply(200, list, &[]),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory"), ("etag", "\"root\"")],
        ),
        response(204, b""),
        response(204, b""),
    ]);
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ApprovedUi,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(true, true, false, false),
        &remote("workspace", "Files/from", OneLakeKind::Folder),
        &remote("workspace", "Files/container", OneLakeKind::Folder),
        pending(),
    )
    .await
    .unwrap();
    assert!(result.rename.is_none());
    assert_eq!(result.uploads.len(), 1);
    assert_eq!(result.source_deletes.len(), 2);
    let requests = network.requests.lock().unwrap();
    let first_delete = requests
        .iter()
        .position(|request| request["method"] == "delete")
        .unwrap();
    let flush = requests
        .iter()
        .position(|request| request["url"].as_str().unwrap().contains("action=flush"))
        .unwrap();
    assert!(first_delete > flush);
}

#[tokio::test]
async fn same_path_directory_moves_are_noops_and_local_native_names_are_not_remote_encoded() {
    let fixture = Fixture::new();
    let source = fixture.path().join("tree");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("100% #.txt"), b"value").unwrap();
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let mut host = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &adapter,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    };
    let result = host
        .execute_full_resolved(
            &command(true, true, true, false),
            &local(&source),
            &local(&source),
            pending(),
        )
        .await
        .unwrap();
    assert!(result.unchanged);
    assert!(result.local_deletes.is_empty());
    host.execute_full_resolved(
        &command(false, true, true, false),
        &local(&source),
        &local(&fixture.path().join("out")),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(fixture.path().join("out").join("100% #.txt")).unwrap(),
        b"value"
    );
    assert!(network.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cross_workspace_moves_require_committed_flush_and_preserve_delete_acceptance() {
    for (flush, delete) in [(202, 204), (200, 202), (200, 204)] {
        let mut replies = vec![
            version(4),
            created(),
            range(b"data"),
            response(202, b""),
            response(flush, b""),
        ];
        if flush == 200 {
            replies.push(response(delete, b""));
        }
        let (client, adapter, network) = setup(replies);
        let mut ui = Ui::default();
        let result = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &adapter,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: None,
        }
        .execute_full_resolved(
            &command(true, false, true, false),
            &remote("from", "Files/source", OneLakeKind::File),
            &remote("to", "Files/destination", OneLakeKind::Undefined),
            pending(),
        )
        .await;
        if flush == 202 {
            let error = result.unwrap_err();
            assert!(!error.source_delete_may_have_occurred);
            assert_eq!(network.requests.lock().unwrap().len(), 5);
        } else if delete == 202 {
            let error = result.unwrap_err();
            assert_eq!(error.stage, TreeStage::SourceDelete);
            assert!(error.source_delete_may_have_occurred);
            assert_eq!(error.partial.source_deletes.len(), 1);
        } else {
            assert_eq!(result.unwrap().source_deletes.len(), 1);
        }
        let requests = network.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .take(4)
                .all(|request| request["method"] != "delete")
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_device_and_normalized_names_fail_before_copy_or_source_deletion() {
    let fixture = Fixture::new();
    let files = fixture.adapter().await;
    for (index, name) in [
        "NUL", "nul.txt", "CON", "PRN.csv", "AUX", "COM1", "COM¹", "LPT9", "CONOUT$", "name.",
        "name ",
    ]
    .iter()
    .enumerate()
    {
        let target = fixture.path().join(format!("target-{index}"));
        let (client, executor, network) = setup(vec![reply(
            200,
            json!({"paths":[{"name":format!("item/Files/tree/{name}"),"isDirectory":false}]}),
            &[],
        )]);
        let mut ui = Ui::default();
        let error = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &executor,
                ui: &mut ui,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(true, true, true, false),
            &remote("workspace", "Files/tree", OneLakeKind::Folder),
            &local(&target),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.stage, TreeStage::Preflight);
        assert!(!error.source_delete_may_have_occurred);
        assert!(!target.exists());
        assert!(
            matches!(error.failure,TreeFailure::Local(error) if matches!(error.kind,fabric_cli_storage_files::LocalErrorKind::InvalidName))
        );
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
    let error = files
        .write_checked(
            &fixture.path().join("NUL"),
            b"must not disappear".to_vec(),
            true,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        fabric_cli_storage_files::LocalErrorKind::InvalidName
    ));
    assert!(!error.destination_opened);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_junction_source_is_rejected_without_traversing_or_deleting_outside_data() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let source = fixture.path().join("tree");
    let target = fixture.path().join("copy");
    fs::create_dir(&source).unwrap();
    fs::write(outside.path().join("keep"), b"outside").unwrap();
    let output = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(source.join("linked"))
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = fixture.adapter().await;
    let (client, executor, network) = setup(vec![]);
    let mut ui = Ui::default();
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(true, true, true, false),
        &local(&source),
        &local(&target),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.failure,TreeFailure::Local(error) if matches!(error.kind,fabric_cli_storage_files::LocalErrorKind::OutsideBoundary))
    );
    assert!(!error.source_delete_may_have_occurred);
    assert!(!target.exists());
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
    assert!(network.requests.lock().unwrap().is_empty());
}

struct DestinationRace {
    files: OsFileSystem,
    destination: PathBuf,
    checks: AtomicUsize,
}
impl LocalFileSystem for DestinationRace {
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        Box::pin(async move {
            if path == self.destination && self.checks.fetch_add(1, Ordering::SeqCst) == 1 {
                fs::remove_dir(path).unwrap();
                fs::write(path, b"raced target").unwrap();
            }
            self.files.kind(path).await
        })
    }
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        self.files.read(path, limit)
    }
    fn write<'a>(
        &'a self,
        path: &'a Path,
        bytes: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        self.files.write(path, bytes)
    }
    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        self.files.snapshot_tree(path, limit)
    }
    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        self.files.canonical_path(path)
    }
}

#[tokio::test]
async fn destination_kind_races_stop_before_any_planned_write() {
    let fixture = Fixture::new();
    let source = fixture.path().join("tree");
    let target = fixture.path().join("output");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), b"source").unwrap();
    fs::create_dir_all(target.join("tree")).unwrap();
    let files = DestinationRace {
        files: fixture.adapter().await,
        destination: target.join("tree"),
        checks: AtomicUsize::new(0),
    };
    let (client, executor, _) = setup(vec![]);
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ApprovedUi,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(false, true, false, false),
        &local(&source),
        &local(&target),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.failure,TreeFailure::Command(error) if error.code == "DestinationChanged")
    );
    assert!(error.partial.local_writes.is_empty() && error.partial.local_directories.is_empty());
    assert!(!error.source_delete_may_have_occurred);
    assert_eq!(fs::read(source.join("payload")).unwrap(), b"source");
    assert_eq!(fs::read(target.join("tree")).unwrap(), b"raced target");
}

#[tokio::test]
async fn rename_target_creation_races_use_a_conditional_request_without_copy_delete_fallback() {
    let (client, executor, network) = setup(vec![
        reply(200, json!({"paths":[]}), &[]),
        reply(200, json!({"paths":[]}), &[]),
        reply(
            200,
            serde_json::Value::Null,
            &[("x-ms-resource-type", "directory")],
        ),
        response(404, b""),
        response(412, b""),
    ]);
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ApprovedUi,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: None,
    }
    .execute_full_resolved(
        &command(true, true, false, false),
        &remote("workspace", "Files/from", OneLakeKind::Folder),
        &remote("workspace", "Files/to", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.stage, TreeStage::Rename);
    assert!(error.partial.uploads.is_empty() && error.partial.source_deletes.is_empty());
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests.last().unwrap()["headers"]["if-none-match"], "*");
    assert!(
        requests
            .iter()
            .all(|request| request["method"] != "delete" && request["method"] != "patch")
    );
}

struct ChangingLocal {
    source: PathBuf,
    files: OsFileSystem,
    snapshots: AtomicUsize,
}
impl LocalFileSystem for ChangingLocal {
    fn open_reader<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        self.files.open_reader(path, expected)
    }
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        self.files.kind(path)
    }
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        self.files.read(path, limit)
    }
    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        self.files.read_for_copy(path, limit)
    }
    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        self.files.write(path, content)
    }
    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        self.files.canonical_path(path)
    }
    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        Box::pin(async move {
            if self.snapshots.fetch_add(1, Ordering::SeqCst) == 1 {
                let modified = fs::metadata(&self.source).unwrap().modified().unwrap();
                fs::write(&self.source, b"modified").unwrap();
                fs::File::options()
                    .write(true)
                    .open(&self.source)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
            self.files.snapshot_tree(path, limit).await
        })
    }
}

#[tokio::test]
async fn a_local_source_changed_after_copy_is_never_deleted() {
    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    fs::write(&source, b"original").unwrap();
    let files = ChangingLocal {
        source: source.clone(),
        files: fixture.adapter().await,
        snapshots: AtomicUsize::new(0),
    };
    let (client, executor, network) =
        setup(vec![created(), response(202, b""), response(200, b"")]);
    let mut ui = Ui::default();
    let error = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(true, false, true, false),
        &local(&source),
        &remote("workspace", "Files/destination", OneLakeKind::Undefined),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error.failure,TreeFailure::Command(error) if error.code == "SourceChanged"),
        "{error:?}"
    );
    assert!(!error.source_delete_may_have_occurred);
    assert_eq!(error.partial.uploads.len(), 1);
    assert_eq!(fs::read(source).unwrap(), b"modified");
    assert_eq!(
        network.requests.lock().unwrap()[1]["body"],
        json!(b"original")
    );
}

#[tokio::test]
async fn a_hardlinked_destination_is_replaced_without_truncating_the_source_identity() {
    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    fs::write(&source, b"original bytes").unwrap();
    fs::hard_link(&source, &target).unwrap();
    let files = fixture.adapter().await;
    let (client, executor, network) = setup(vec![]);
    let mut ui = Ui::default();
    let result = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(true, false, true, false),
        &local(&source),
        &local(&target),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        result.local_deletes.as_slice(),
        std::slice::from_ref(&source)
    );
    assert!(!source.exists());
    assert_eq!(fs::read(target).unwrap(), b"original bytes");
    assert!(network.requests.lock().unwrap().is_empty());
}

struct GuardedFiles {
    files: OsFileSystem,
    fail_path: Option<PathBuf>,
    reads: Arc<AtomicUsize>,
}

struct GuardedReader {
    inner: Box<dyn LocalFileReader>,
    reads: Arc<AtomicUsize>,
}

impl LocalFileReader for GuardedReader {
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn read_at(
        &mut self,
        offset: u64,
        limit: NonZeroUsize,
    ) -> CommandFuture<'_, Result<Vec<u8>, LocalError>> {
        assert!(limit.get() <= 4 * 1024 * 1024);
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_at(offset, limit)
    }
}

struct FailingWriter {
    inner: Box<dyn LocalFileWriter>,
    written: usize,
}

impl LocalFileWriter for FailingWriter {
    fn append_at(
        &mut self,
        position: u64,
        bytes: Vec<u8>,
    ) -> CommandFuture<'_, Result<(), LocalError>> {
        Box::pin(async move {
            if self.written > 0 {
                return Err(LocalError {
                    kind: LocalErrorKind::Io(std::io::Error::other(
                        "synthetic private-path failure",
                    )),
                    destination_opened: true,
                    bytes_written: self.written,
                });
            }
            let count = bytes.len();
            self.inner.append_at(position, bytes).await?;
            self.written += count;
            Ok(())
        })
    }
    fn finish(&mut self) -> CommandFuture<'_, Result<u64, LocalError>> {
        self.inner.finish()
    }
}

impl LocalFileSystem for GuardedFiles {
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        self.files.kind(path)
    }
    fn read<'a>(
        &'a self,
        _: &'a Path,
        _: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        panic!("tree transfers must not allocate an entire file")
    }
    fn write<'a>(
        &'a self,
        _: &'a Path,
        _: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        panic!("tree transfers must not write an entire file buffer")
    }
    fn open_reader<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        Box::pin(async move {
            Ok(Box::new(GuardedReader {
                inner: self.files.open_reader(path, expected).await?,
                reads: self.reads.clone(),
            }) as Box<dyn LocalFileReader>)
        })
    }
    fn open_writer<'a>(
        &'a self,
        path: &'a Path,
        length: u64,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileWriter>, LocalError>> {
        Box::pin(async move {
            let writer = self.files.open_writer(path, length, overwrite).await?;
            if self.fail_path.as_deref() == Some(path) {
                Ok(Box::new(FailingWriter {
                    inner: writer,
                    written: 0,
                }) as Box<dyn LocalFileWriter>)
            } else {
                Ok(writer)
            }
        })
    }
    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        self.files.snapshot_tree(path, limit)
    }
    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        self.files.canonical_path(path)
    }
    fn create_directory<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<(), LocalError>> {
        self.files.create_directory(path)
    }
    fn remove_copied<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        self.files.remove_copied(path, expected)
    }
}

#[tokio::test]
async fn cross_entry_hardlink_overwrites_never_modify_another_source_on_windows_or_unix() {
    for moving in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.path().join("src");
        let output = fixture.path().join("out");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(output.join("src")).unwrap();
        fs::write(source.join("a"), b"AAAA").unwrap();
        fs::write(source.join("b"), vec![b'B'; 4 * 1024 * 1024 + 17]).unwrap();
        fs::hard_link(source.join("b"), output.join("src").join("a")).unwrap();
        let files = GuardedFiles {
            files: fixture.adapter().await,
            fail_path: moving.then(|| output.join("src").join("b")),
            reads: Arc::new(AtomicUsize::new(0)),
        };
        let (client, executor, network) = setup(vec![]);
        let result = ResourceFileHost {
            context: &mut Context,
            storage: OneLakeWriteHost {
                client: &client,
                executor: &executor,
                ui: &mut ApprovedUi,
                read_limits: OneLakeLimits::default(),
                write_limits: OneLakeWriteLimits::default(),
            },
            files: Some(&files),
        }
        .execute_full_resolved(
            &command(moving, true, true, false),
            &local(&source),
            &local(&output),
            pending(),
        )
        .await;
        if moving {
            let error = result.unwrap_err();
            assert!(!error.source_delete_may_have_occurred);
            assert!(error.partial.local_deletes.is_empty());
            assert!(error.local_write_may_have_occurred);
            assert_eq!(error.partial.local_writes.len(), 1);
            assert!(
                matches!(error.failure, TreeFailure::Local(error) if error.destination_opened && error.bytes_written == 4 * 1024 * 1024)
            );
            assert_eq!(
                fs::metadata(output.join("src").join("b")).unwrap().len(),
                4 * 1024 * 1024
            );
        } else {
            assert!(result.unwrap().destination_copy_complete);
            assert_eq!(
                fs::read(output.join("src").join("b")).unwrap(),
                vec![b'B'; 4 * 1024 * 1024 + 17]
            );
        }
        assert_eq!(fs::read(source.join("a")).unwrap(), b"AAAA");
        assert_eq!(
            fs::read(source.join("b")).unwrap(),
            vec![b'B'; 4 * 1024 * 1024 + 17]
        );
        assert_eq!(fs::read(output.join("src").join("a")).unwrap(), b"AAAA");
        assert_eq!(files.reads.load(Ordering::SeqCst), 3);
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn large_native_move_fingerprints_are_incremental_on_both_copy_and_final_verification() {
    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    fs::write(&source, vec![b'v'; 4 * 1024 * 1024 + 17]).unwrap();
    let files = GuardedFiles {
        files: fixture.adapter().await,
        fail_path: None,
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let (client, executor, _) = setup(vec![]);
    let receipt = ResourceFileHost {
        context: &mut Context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ApprovedUi,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&files),
    }
    .execute_full_resolved(
        &command(true, false, true, false),
        &local(&source),
        &local(&target),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(files.reads.load(Ordering::SeqCst), 4);
    assert!(receipt.destination_copy_complete);
    assert_eq!(
        receipt.local_deletes.as_slice(),
        std::slice::from_ref(&source)
    );
    assert!(!source.exists());
    assert_eq!(fs::metadata(target).unwrap().len(), 4 * 1024 * 1024 + 17);
}
