// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real CLI -> SDK -> shared HTTP -> OS-filesystem offline consumers.

mod support;

use fabric_cli_mutations::OneLakeResourceKind;
use fabric_cli_storage_files::{
    CopyErrorKind, CopyStage, Interruption, LocalErrorKind, LocalFileSystem, LocalKind,
    StorageFilesHost,
};
use fabric_sdk_onelake::{OneLakeError, OneLakeLimits};
use std::{fs, future::pending, num::NonZeroUsize};
use support::{FaultLocal, Fixture, Ui, response, setup, source};

#[tokio::test]
async fn downloads_exact_bytes_overwrites_without_prompt_and_selects_native_directory_name() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let payload = b"\0\xff\xc3\xa9\r\n{}\n";
    fs::write(fixture.path().join("data.txt"), b"old-longer-data").unwrap();
    let (client, adapter, network) = setup(vec![response(200, payload), response(200, payload)]);
    let mut ui = Ui::default();
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    for target in [fixture.path().to_owned(), fixture.path().join("data.txt")] {
        let result = host
            .download(
                &source("Files/data.txt", OneLakeResourceKind::File),
                &target,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(result.bytes_written, Some(payload.len()));
        assert_eq!(result.outputs[0].message.as_deref(), Some("Done"));
        assert_eq!(fs::read(&result.local_path).unwrap(), payload);
    }
    assert!(ui.prompts.is_empty());
    assert_eq!(network.requests.lock().unwrap().len(), 2);
    assert!(network.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_recursive_source_kinds_and_preserves_undefined_source_noop() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    for kind in [
        OneLakeResourceKind::Directory,
        OneLakeResourceKind::Shortcut,
        OneLakeResourceKind::Table,
    ] {
        let error = host
            .download(&source("Files/source", kind), fixture.path(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.output().error_code.as_deref(), Some("NotSupported"));
        assert_eq!(
            error.output().message.as_deref(),
            Some("Recursive copy not supported")
        );
        assert!(!error.local_write_may_have_occurred);
    }
    let result = host
        .download(
            &source("Files/source", OneLakeResourceKind::Undefined),
            fixture.path(),
            pending(),
        )
        .await
        .unwrap();
    assert!(result.outputs.is_empty());
    assert!(network.requests.lock().unwrap().is_empty());
    assert!(ui.notices.is_empty());
}

#[tokio::test]
async fn empty_destination_gets_source_then_fails_without_copying_into_the_local_boundary() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![response(200, b"bytes")]);
    let mut ui = Ui::default();
    let error = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .download(
        &source("Files/source", OneLakeResourceKind::File),
        std::path::Path::new(""),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.stage, CopyStage::LocalWrite);
    assert!(error.local_path.as_os_str().is_empty());
    assert_eq!(error.output().error_code.as_deref(), Some("InvalidPath"));
    assert_eq!(network.requests.lock().unwrap().len(), 1);
    assert!(matches!(error.kind, CopyErrorKind::Local(ref cause) if !cause.destination_opened));
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn empty_failed_and_oversized_remote_reads_never_truncate_existing_file() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let target = fixture.path().join("keep.txt");
    fs::write(&target, b"keep").unwrap();
    for reply in [
        response(200, b""),
        response(403, b""),
        response(404, b""),
        response(200, b"oversized"),
        response(202, b"new"),
        response(204, b"new"),
    ] {
        let (client, adapter, _) = setup(vec![reply]);
        let mut ui = Ui::default();
        let mut host = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits {
                max_response_bytes: NonZeroUsize::new(4).unwrap(),
                ..OneLakeLimits::default()
            },
        };
        let error = host
            .download(
                &source("Files/source", OneLakeResourceKind::File),
                &target,
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.stage, CopyStage::StorageRead);
        assert!(!error.local_write_may_have_occurred);
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}

#[tokio::test]
async fn missing_parent_and_file_directory_collision_fail_after_get_without_creating_parents() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    fs::create_dir(fixture.path().join("source")).unwrap();
    for target in [
        fixture.path().join("missing").join("out"),
        fixture.path().to_owned(),
    ] {
        let (client, adapter, network) = setup(vec![response(200, b"new")]);
        let mut ui = Ui::default();
        let mut host = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        };
        let error = host
            .download(
                &source("Files/source", OneLakeResourceKind::File),
                &target,
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.stage, CopyStage::LocalWrite);
        assert_eq!(error.output().error_code.as_deref(), Some("InvalidPath"));
        assert_eq!(network.requests.lock().unwrap().len(), 1);
        assert!(!fixture.path().join("missing").exists());
        assert!(fixture.path().join("source").is_dir());
    }
}

#[tokio::test]
async fn partial_write_error_retains_exact_path_count_and_file_without_deleting_unrelated_data() {
    let fixture = Fixture::new();
    let local = FaultLocal {
        inner: fixture.adapter().await,
        read_failure: false,
        write_after: Some(2),
        short_success: false,
    };
    let target = fixture.path().join("out");
    fs::write(&target, b"old-content").unwrap();
    fs::write(fixture.path().join("unrelated"), b"leave alone").unwrap();
    let (client, adapter, _) = setup(vec![response(200, b"new-content")]);
    let mut ui = Ui::default();
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let error = host
        .download(
            &source("Files/source", OneLakeResourceKind::File),
            &target,
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.local_path, target);
    assert!(error.local_write_may_have_occurred);
    assert!(
        matches!(error.kind, CopyErrorKind::Local(ref cause) if cause.destination_opened && cause.bytes_written == 2)
    );
    assert_eq!(fs::read(&target).unwrap(), b"ne");
    assert_eq!(
        fs::read(fixture.path().join("unrelated")).unwrap(),
        b"leave alone"
    );
    assert!(
        error
            .output()
            .message
            .unwrap()
            .starts_with("Error writing file: injected write failure")
    );
}

#[tokio::test]
async fn short_success_from_injected_adapter_is_not_done() {
    let fixture = Fixture::new();
    let local = FaultLocal {
        inner: fixture.adapter().await,
        read_failure: false,
        write_after: Some(1),
        short_success: true,
    };
    let target = fixture.path().join("out");
    let (client, adapter, _) = setup(vec![response(200, b"complete")]);
    let mut ui = Ui::default();
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let error = host
        .download(
            &source("Files/source", OneLakeResourceKind::File),
            &target,
            pending(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind, CopyErrorKind::Local(ref cause) if cause.destination_opened && cause.bytes_written == 1)
    );
    assert_eq!(fs::read(target).unwrap(), b"c");
}

#[tokio::test]
async fn cancellation_after_http_dispatch_keeps_target_untouched_and_retains_deadline_reason() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let target = fixture.path().join("keep");
    fs::write(&target, b"old").unwrap();
    let (client, adapter, network) = setup(vec![response(200, b"new")]);
    let mut ui = Ui::default();
    let interrupted = std::future::poll_fn(|_| {
        if network.requests.lock().unwrap().is_empty() {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(Interruption::Deadline)
        }
    });
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let error = host
        .download(
            &source("Files/source", OneLakeResourceKind::File),
            &target,
            interrupted,
        )
        .await
        .unwrap_err();
    assert_eq!(error.stage, CopyStage::StorageRead);
    assert!(matches!(
        error.kind,
        CopyErrorKind::Interrupted(Interruption::Deadline)
    ));
    assert_eq!(fs::read(target).unwrap(), b"old");
}

#[tokio::test]
async fn in_flight_local_write_is_dropped_with_explicit_uncertainty_not_rollback() {
    use fabric_cli_mutations::CommandFuture;
    use fabric_cli_storage_files::LocalError;
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, Ordering},
        task::Poll,
    };
    struct HeldLocal {
        inner: fabric_cli_storage_files::OsFileSystem,
        entered: AtomicBool,
        dropped: AtomicBool,
    }
    struct OnDrop<'a>(&'a AtomicBool);
    impl Drop for OnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl LocalFileSystem for HeldLocal {
        fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
            self.inner.kind(path)
        }
        fn read<'a>(
            &'a self,
            path: &'a Path,
            limit: NonZeroUsize,
        ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
            self.inner.read(path, limit)
        }
        fn write<'a>(
            &'a self,
            _: &'a Path,
            _: Vec<u8>,
        ) -> CommandFuture<'a, Result<usize, LocalError>> {
            Box::pin(async move {
                let _guard = OnDrop(&self.dropped);
                self.entered.store(true, Ordering::SeqCst);
                std::future::poll_fn(|context| {
                    context.waker().wake_by_ref();
                    Poll::Pending
                })
                .await
            })
        }
    }
    let fixture = Fixture::new();
    let local = HeldLocal {
        inner: fixture.adapter().await,
        entered: AtomicBool::new(false),
        dropped: AtomicBool::new(false),
    };
    let target = fixture.path().join("keep");
    fs::write(&target, b"old").unwrap();
    let (client, adapter, _) = setup(vec![response(200, b"new")]);
    let mut ui = Ui::default();
    let interrupted = std::future::poll_fn(|_| {
        if local.entered.load(Ordering::SeqCst) {
            Poll::Ready(Interruption::Cancelled)
        } else {
            Poll::Pending
        }
    });
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let error = host
        .download(
            &source("Files/source", OneLakeResourceKind::File),
            &target,
            interrupted,
        )
        .await
        .unwrap_err();
    assert_eq!(error.stage, CopyStage::LocalWrite);
    assert_eq!(
        error.storage_path.as_ref(),
        &source("Files/source", OneLakeResourceKind::File).path
    );
    assert!(error.local_write_may_have_occurred);
    assert!(local.dropped.load(Ordering::SeqCst));
    assert_eq!(fs::read(target).unwrap(), b"old");
}

#[tokio::test]
async fn cancellation_and_whole_transfer_deadline_before_io_do_not_start_get_or_write() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    for reason in [Interruption::Cancelled, Interruption::Deadline] {
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let mut host = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        };
        let error = host
            .download(
                &source("Files/source", OneLakeResourceKind::File),
                fixture.path(),
                std::future::ready(reason),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.kind, CopyErrorKind::Interrupted(actual) if actual == reason));
        assert!(!error.local_write_may_have_occurred);
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn real_adapter_bounds_reads_and_refuses_outgoing_boundary_without_directory_traversal() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let local = fixture.adapter().await;
    let path = fixture.path().join("binary");
    fs::write(&path, [0, 255, 10]).unwrap();
    assert_eq!(local.kind(&path).await.unwrap(), LocalKind::File);
    assert_eq!(
        local
            .read(&path, NonZeroUsize::new(3).unwrap())
            .await
            .unwrap(),
        [0, 255, 10]
    );
    assert!(matches!(
        local
            .read(&path, NonZeroUsize::new(2).unwrap())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Limit
    ));
    assert!(matches!(
        local
            .write(&other.path().join("no"), b"bad".to_vec())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::OutsideBoundary
    ));
    assert!(!other.path().join("no").exists());
    assert_eq!(
        local.kind(&fixture.path().join("missing")).await.unwrap(),
        LocalKind::Missing
    );
}

#[cfg(unix)]
#[tokio::test]
async fn outgoing_symlink_cannot_write_outside_selected_local_boundary() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let local = fixture.adapter().await;
    let outside = other.path().join("file");
    fs::write(&outside, b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.path().join("link")).unwrap();
    assert!(matches!(
        local
            .write(&fixture.path().join("link"), b"bad".to_vec())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::OutsideBoundary
    ));
    assert_eq!(fs::read(outside).unwrap(), b"keep");
}

#[test]
fn source_path_rejects_remote_traversal_before_local_join() {
    assert!(matches!(
        fabric_sdk_onelake::OneLakePath::new("workspace", "item", "Files/../out"),
        Err(OneLakeError::InvalidInput)
    ));
}

#[cfg(windows)]
#[tokio::test]
async fn remote_drive_and_ads_name_cannot_replace_directory_destination() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    for name in ["Files/C:escape", "Files/source:alternate"] {
        let error = host
            .download(
                &source(name, OneLakeResourceKind::File),
                fixture.path(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.stage, CopyStage::Validate);
        assert!(
            matches!(error.kind, CopyErrorKind::Local(ref cause) if matches!(cause.kind, LocalErrorKind::OutsideBoundary))
        );
    }
    assert!(network.requests.lock().unwrap().is_empty());
}
