// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource-command physical-copy routing with offline storage and local files.

#![deny(unnameable_test_items)]

mod support;

use fabric_cli_context::{
    Element, ItemType, LocalKind as ContextLocalKind, LocalMetadata, NodeKind, OneLakeKind,
    PathElement, PathKind, ResolveOptions, WorkspaceType,
};
use fabric_cli_mutations::{
    CommandFuture, MutationError, OneLakeWriteError, OneLakeWriteHost,
    frontdoor::{ResourceArgs, ResourceCommand, ResourceContext},
};
use fabric_cli_storage_files::{
    CopyErrorKind, CopyStage, LocalError, LocalFileSystem, LocalKind, ResourceFileError,
    ResourceFileHost, ResourceFileResult, SelectedFileSystem,
};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeExecutor, OneLakeLimits, OneLakeWriteLimits, OneLakeWriteOutcome,
};
use serde_json::json;
use std::{fs, future::pending, num::NonZeroUsize, path::Path, sync::Arc};
use support::{FaultLocal, Fixture, Ui, response, setup};

struct Context {
    source: Arc<PathElement>,
    target: Arc<PathElement>,
    calls: Vec<(Vec<String>, ResolveOptions)>,
    fail: Option<usize>,
}

impl Context {
    fn new(source: Arc<PathElement>, target: Arc<PathElement>) -> Self {
        Self {
            source,
            target,
            calls: Vec::new(),
            fail: None,
        }
    }
}

impl ResourceContext for Context {
    fn resolve(
        &mut self,
        tokens: &[String],
        options: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError> {
        self.calls.push((tokens.to_vec(), options));
        if self.fail == Some(self.calls.len()) {
            return Err(MutationError::new(
                "Forbidden",
                "Synthetic metadata failure",
            ));
        }
        Ok(if tokens == ["from"] {
            self.source.clone()
        } else {
            self.target.clone()
        })
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
        id: "local-fixture".into(),
        // The handler must use fresh native predicates, not trust a stale snapshot.
        kind: ContextLocalKind::Other,
    })
}

fn fabric() -> Arc<PathElement> {
    let workspace = Element::child(
        "Team",
        Some("workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    PathElement::from_fabric(
        Element::child(
            "Lake",
            Some("item".into()),
            NodeKind::Item(ItemType::parse("Lakehouse").unwrap()),
            workspace,
        )
        .unwrap(),
    )
}

fn remote(kind: OneLakeKind) -> Arc<PathElement> {
    let root = PathElement::child(
        "Files",
        Some("resolved".into()),
        PathKind::OneLake(OneLakeKind::Folder),
        fabric(),
    )
    .unwrap();
    if kind == OneLakeKind::Folder {
        return root;
    }
    PathElement::child(
        "payload.bin",
        if kind == OneLakeKind::Undefined {
            None
        } else {
            Some("resolved".into())
        },
        PathKind::OneLake(kind),
        root,
    )
    .unwrap()
}

fn command(moving: bool, flags: bool) -> ResourceCommand {
    let args = ResourceArgs {
        path: vec!["from".into()],
        output_format: None,
        skill: None,
    };
    if moving {
        ResourceCommand::Move {
            args,
            destination: vec!["to".into()],
            force: flags,
            recursive: flags,
        }
    } else {
        ResourceCommand::Copy {
            args,
            destination: vec!["to".into()],
            force: flags,
            recursive: flags,
            block_on_path_collision: flags,
        }
    }
}

fn host<'a>(
    context: &'a mut Context,
    client: &'a OneLakeClient,
    adapter: &'a dyn OneLakeExecutor,
    ui: &'a mut Ui,
    files: Option<&'a dyn LocalFileSystem>,
) -> ResourceFileHost<'a> {
    ResourceFileHost {
        context,
        storage: OneLakeWriteHost {
            client,
            executor: adapter,
            ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files,
    }
}

#[tokio::test]
async fn upload_uses_fresh_predicates_and_preserves_binary_wire_order_acceptance_and_source() {
    for flags in [false, true] {
        let fixture = Fixture::new();
        let file = fixture.path().join("input.bin");
        let bytes = b"\0\xff\r\n{}\n";
        fs::write(&file, bytes).unwrap();
        let files = SelectedFileSystem::defer(
            fixture.path().into(),
            file.clone(),
            tokio::runtime::Handle::current(),
        );
        let (client, adapter, network) = setup(vec![
            response(201, b""),
            response(202, b""),
            response(200, b""),
        ]);
        *network.held_source.lock().unwrap() = Some(file.clone());
        let mut ui = Ui::default();
        let mut context = Context::new(local(&file), remote(OneLakeKind::Folder));
        let result = host(&mut context, &client, &adapter, &mut ui, Some(&files))
            .execute(&command(false, flags), pending())
            .await
            .unwrap();
        assert_eq!(context.calls.len(), 2);
        assert!(context.calls[0].1.raise_error);
        assert!(!context.calls[1].1.raise_error);
        assert!(
            context
                .calls
                .iter()
                .all(|(_, options)| options.supports_local_path)
        );
        assert_eq!(result.outputs()[0].command.as_deref(), Some("cp"));
        let ResourceFileResult::Upload(result) = result else {
            panic!("expected upload receipt")
        };
        assert_eq!(result.storage_path.relative(), "Files/input.bin");
        assert_eq!(result.upload.append, Some(OneLakeWriteOutcome::Accepted));
        assert_eq!(result.upload.flush.unwrap().status(), 200);
        assert_eq!(fs::read(&file).unwrap(), bytes);
        assert!(ui.prompts.is_empty());
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["method"], "put");
        assert!(
            requests[0]["url"]
                .as_str()
                .unwrap()
                .ends_with("?resource=file")
        );
        assert_eq!(requests[1]["method"], "patch");
        assert!(
            requests[1]["url"]
                .as_str()
                .unwrap()
                .ends_with("?action=append&position=0")
        );
        assert_eq!(requests[1]["body"], json!(bytes));
        assert!(
            requests[2]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("?action=flush&position={}", bytes.len()))
        );
    }
}

#[tokio::test]
async fn download_truncates_only_selected_file_or_directory_child_regardless_of_copy_flags() {
    for directory in [false, true] {
        for flags in [false, true] {
            let fixture = Fixture::new();
            let selected = if directory {
                fixture.path().to_owned()
            } else {
                fixture.path().join("out")
            };
            let output = if directory {
                selected.join("payload.bin")
            } else {
                selected.clone()
            };
            fs::write(&output, b"old longer bytes").unwrap();
            fs::write(fixture.path().join("keep"), b"unrelated").unwrap();
            let files = SelectedFileSystem::defer(
                fixture.path().into(),
                selected.clone(),
                tokio::runtime::Handle::current(),
            );
            let (client, adapter, network) = setup(vec![response(200, b"\0\xff\n")]);
            let mut context = Context::new(remote(OneLakeKind::File), local(&selected));
            let mut ui = Ui::default();
            let result = host(&mut context, &client, &adapter, &mut ui, Some(&files))
                .execute(&command(false, flags), pending())
                .await
                .unwrap();
            let ResourceFileResult::Download(result) = result else {
                panic!("expected download receipt")
            };
            assert_eq!(result.local_path, output);
            assert_eq!(result.bytes_written, Some(3));
            assert_eq!(fs::read(output).unwrap(), b"\0\xff\n");
            assert_eq!(fs::read(fixture.path().join("keep")).unwrap(), b"unrelated");
            assert_eq!(network.requests.lock().unwrap().len(), 1);
            assert!(ui.prompts.is_empty());
        }
    }
}

#[tokio::test]
async fn local_moves_and_invalid_copy_pairs_reject_before_any_filesystem_or_http_call() {
    let path = Path::new("unactivated");
    let fixtures: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("resource-routes.json")).unwrap();
    let element = |kind: &str| match kind {
        "local" => local(path),
        "onelake" => remote(OneLakeKind::File),
        "item" => fabric(),
        _ => panic!("unknown synthetic fixture kind"),
    };
    for case in fixtures {
        let moving = case["move"].as_bool().unwrap();
        let mut context = Context::new(
            element(case["from"].as_str().unwrap()),
            element(case["to"].as_str().unwrap()),
        );
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let result = host(&mut context, &client, &adapter, &mut ui, Some(&NeverLocal))
            .execute(&command(moving, true), pending())
            .await
            .unwrap_err();
        let output = result.output();
        assert_eq!(output.error_code.as_deref(), case["code"].as_str());
        assert_eq!(output.message.as_deref(), case["message"].as_str());
        assert!(network.requests.lock().unwrap().is_empty());
        assert!(ui.notices.is_empty());
        assert!(ui.prompts.is_empty());
    }
}

#[tokio::test]
async fn recursive_sources_reject_but_undefined_download_preserves_source_noop() {
    let fixture = Fixture::new();
    let files = SelectedFileSystem::defer(
        fixture.path().into(),
        fixture.path().into(),
        tokio::runtime::Handle::current(),
    );
    for (from, to, local_io) in [
        (
            local(fixture.path()),
            remote(OneLakeKind::File),
            &files as &dyn LocalFileSystem,
        ),
        (
            remote(OneLakeKind::Folder),
            local(fixture.path()),
            &NeverLocal,
        ),
        (
            remote(OneLakeKind::Table),
            local(fixture.path()),
            &NeverLocal,
        ),
        (
            remote(OneLakeKind::Shortcut),
            local(fixture.path()),
            &NeverLocal,
        ),
        (
            remote(OneLakeKind::Undefined),
            local(fixture.path()),
            &NeverLocal,
        ),
    ] {
        let noop = from.kind() == PathKind::OneLake(OneLakeKind::Undefined);
        let mut context = Context::new(from, to);
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let result = host(&mut context, &client, &adapter, &mut ui, Some(local_io))
            .execute(&command(false, true), pending())
            .await;
        if noop {
            assert!(result.unwrap().outputs().is_empty());
        } else {
            let output = result.unwrap_err().output();
            assert_eq!(output.error_code.as_deref(), Some("NotSupported"));
            assert_eq!(
                output.message.as_deref(),
                Some("Recursive copy not supported")
            );
        }
        assert!(network.requests.lock().unwrap().is_empty());
        assert!(ui.notices.is_empty());
    }
}

struct NeverLocal;
impl LocalFileSystem for NeverLocal {
    fn kind<'a>(&'a self, _: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        panic!("local method must remain deferred");
    }
    fn read<'a>(
        &'a self,
        _: &'a Path,
        _: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        panic!("unexpected local read");
    }
    fn write<'a>(
        &'a self,
        _: &'a Path,
        _: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        panic!("unexpected local write");
    }
}

#[tokio::test]
async fn ready_cancellation_wins_before_eager_local_methods_in_both_directions() {
    for upload in [false, true] {
        let native = local(Path::new("must-not-open"));
        let storage = remote(OneLakeKind::File);
        let mut context = if upload {
            Context::new(native, storage)
        } else {
            Context::new(storage, native)
        };
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let error = host(&mut context, &client, &adapter, &mut ui, Some(&NeverLocal))
            .execute(&command(false, false), std::future::ready(()))
            .await
            .unwrap_err();
        assert_eq!(error.output().error_code.as_deref(), Some("Cancelled"));
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn metadata_failures_and_missing_capabilities_never_become_local_fallback_success() {
    for fail in [Some(1), Some(2), None] {
        let mut context = Context::new(local(Path::new("file")), remote(OneLakeKind::File));
        context.fail = fail;
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let error = host(&mut context, &client, &adapter, &mut ui, None)
            .execute(&command(false, false), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.output().error_code.as_deref(),
            Some(if fail.is_some() {
                "Forbidden"
            } else {
                "NotSupported"
            })
        );
        assert_eq!(context.calls.len(), fail.unwrap_or(2));
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn original_partial_upload_and_local_write_failures_survive_the_resource_adapter() {
    let fixture = Fixture::new();
    let file = fixture.path().join("input.bin");
    fs::write(&file, b"abcdef").unwrap();
    let files = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![response(201, b""), response(403, b"")]);
    let mut context = Context::new(local(&file), remote(OneLakeKind::File));
    let mut ui = Ui::default();
    let error = host(&mut context, &client, &adapter, &mut ui, Some(&files))
        .execute(&command(false, false), pending())
        .await
        .unwrap_err();
    let ResourceFileError::Copy(error) = error else {
        panic!("expected copy error")
    };
    assert_eq!(error.stage, CopyStage::StorageWrite);
    let CopyErrorKind::Upload { error, .. } = error.kind else {
        panic!("expected upload error")
    };
    let OneLakeWriteError::Storage(error) = *error else {
        panic!("expected original mutation receipt")
    };
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.completed_steps.len(), 1);
    assert_eq!(network.requests.lock().unwrap().len(), 2);
    assert_eq!(fs::read(&file).unwrap(), b"abcdef");

    let files = FaultLocal {
        inner: files,
        read_failure: false,
        write_after: Some(2),
        short_success: false,
    };
    let (client, adapter, network) = setup(vec![response(200, b"replace")]);
    let mut context = Context::new(remote(OneLakeKind::File), local(&file));
    let error = host(&mut context, &client, &adapter, &mut ui, Some(&files))
        .execute(&command(false, true), pending())
        .await
        .unwrap_err();
    let ResourceFileError::Copy(error) = error else {
        panic!("expected copy error")
    };
    assert_eq!(error.stage, CopyStage::LocalWrite);
    assert!(error.local_write_may_have_occurred);
    let CopyErrorKind::Local(error) = error.kind else {
        panic!("expected local receipt")
    };
    assert!(error.destination_opened);
    assert_eq!(error.bytes_written, 2);
    assert_eq!(fs::read(file).unwrap(), b"re");
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn missing_empty_and_over_bound_sources_fail_before_storage_mutation() {
    for (content, code) in [
        (None, "InvalidPath"),
        (Some(Vec::new()), "InvalidInput"),
        (Some(vec![0_u8; 5]), "Limit"),
    ] {
        let fixture = Fixture::new();
        let file = fixture.path().join("input.bin");
        if let Some(content) = &content {
            fs::write(&file, content).unwrap();
        }
        let files = SelectedFileSystem::defer(
            fixture.path().into(),
            file.clone(),
            tokio::runtime::Handle::current(),
        );
        let mut context = Context::new(local(&file), remote(OneLakeKind::File));
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let mut host = host(&mut context, &client, &adapter, &mut ui, Some(&files));
        host.storage.write_limits.max_request_bytes = NonZeroUsize::new(4).unwrap();
        let error = host
            .execute(&command(false, true), pending())
            .await
            .unwrap_err();
        assert_eq!(error.output().error_code.as_deref(), Some(code));
        assert!(network.requests.lock().unwrap().is_empty());
        match content {
            Some(content) => assert_eq!(fs::read(file).unwrap(), content),
            None => assert!(!file.exists()),
        }
    }
}

#[tokio::test]
async fn remote_move_delegates_once_without_local_io_or_acceptance_promotion() {
    let source = remote(OneLakeKind::File);
    let target = PathElement::child(
        "renamed.bin",
        None,
        PathKind::OneLake(OneLakeKind::Undefined),
        source.parent().unwrap().clone(),
    )
    .unwrap();
    let mut context = Context::new(source, target);
    let (client, adapter, network) = setup(vec![response(202, b"")]);
    let mut ui = Ui::default();
    let mut moving = command(true, false);
    if let ResourceCommand::Move { force, .. } = &mut moving {
        *force = true;
    }
    let result = host(&mut context, &client, &adapter, &mut ui, Some(&NeverLocal))
        .execute(&moving, pending())
        .await
        .unwrap();
    let ResourceFileResult::Storage(result) = result else {
        panic!("expected original storage receipt")
    };
    assert_eq!(result.outcome, Some(OneLakeWriteOutcome::Accepted));
    assert!(result.outputs.is_empty());
    assert_eq!(context.calls.len(), 2);
    assert_eq!(network.requests.lock().unwrap().len(), 1);
    assert!(ui.prompts.is_empty());
}
