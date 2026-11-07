// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Physical local source -> public CLI -> SDK -> shared HTTP, never a service.

mod support;

use fabric_cli_context::ItemType;
use fabric_cli_mutations::{CommandFuture, OneLakeResourceKind, OneLakeWriteError};
use fabric_cli_storage_files::{
    CopyErrorKind, CopyStage, Interruption, LocalError, LocalErrorKind, LocalFileSystem, LocalKind,
    LocalReadFile, OsFileSystem, StorageFilesHost,
};
use fabric_sdk_onelake::{
    OneLakeLimits, OneLakeWriteLimits, OneLakeWriteOutcome, OneLakeWriteStep,
};
use std::{fs, future::pending, num::NonZeroUsize, path::Path, sync::Mutex};
use support::{FaultLocal, Fixture, Ui, failed_response, response, setup, source};

enum InjectedRead {
    Content(Vec<u8>),
    Pending,
}

struct InjectedLocal {
    kind: LocalKind,
    read: InjectedRead,
    reading: std::sync::atomic::AtomicBool,
}
impl LocalFileSystem for InjectedLocal {
    fn kind<'a>(&'a self, _: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        Box::pin(std::future::ready(Ok(self.kind)))
    }
    fn read<'a>(
        &'a self,
        _: &'a Path,
        _: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        panic!("upload must request a retained source lease");
    }
    fn read_for_copy<'a>(
        &'a self,
        _: &'a Path,
        _: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        Box::pin(async move {
            match &self.read {
                InjectedRead::Content(content) => Ok(LocalReadFile::new(content.clone(), ())),
                InjectedRead::Pending => {
                    self.reading
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    std::future::poll_fn(|context| {
                        context.waker().wake_by_ref();
                        std::task::Poll::Pending
                    })
                    .await
                }
            }
        })
    }
    fn write<'a>(
        &'a self,
        _: &'a Path,
        _: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        panic!("upload must not write local files");
    }
}

struct TraceLocal {
    inner: OsFileSystem,
    events: Mutex<Vec<&'static str>>,
}
impl LocalFileSystem for TraceLocal {
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        self.events.lock().unwrap().push("kind");
        self.inner.kind(path)
    }
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        self.events.lock().unwrap().push("read");
        self.inner.read(path, limit)
    }
    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        self.events.lock().unwrap().push("read_for_copy");
        self.inner.read_for_copy(path, limit)
    }
    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        panic!(
            "upload must not write local target {path:?}, bytes={}",
            content.len()
        )
    }
}

#[tokio::test]
async fn exact_binary_file_upload_uses_source_order_and_keeps_native_handle_through_all_requests() {
    let fixture = Fixture::new();
    let local = TraceLocal {
        inner: fixture.adapter().await,
        events: Mutex::new(Vec::new()),
    };
    let file = fixture.path().join("source.csv");
    let bytes = b"\0\xff\xc3\xa9\r\n{}\n";
    fs::write(&file, bytes).unwrap();
    fs::write(fixture.path().join("unrelated"), b"keep").unwrap();
    let (client, adapter, network) = setup(vec![
        response(201, b""),
        response(202, b""),
        response(200, b""),
    ]);
    *network.held_source.lock().unwrap() = Some(file.clone());
    let mut ui = Ui::default();
    let result = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .upload(
        &file,
        &source("Files", OneLakeResourceKind::Directory),
        OneLakeWriteLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        *local.events.lock().unwrap(),
        ["kind", "kind", "read_for_copy"]
    );
    assert_eq!(result.storage_path.relative(), "Files/source.csv");
    assert_eq!(result.outputs[0].message.as_deref(), Some("Done"));
    assert_eq!(result.upload.append, Some(OneLakeWriteOutcome::Accepted));
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["method"], "put");
    assert_eq!(requests[1]["method"], "patch");
    assert_eq!(requests[1]["body"], serde_json::json!(bytes));
    assert_eq!(requests[1]["headers"]["content-type"], "text/csv");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("action=flush&position={}", bytes.len()))
    );
    assert!(ui.prompts.is_empty());
    assert_eq!(ui.notices.len(), 1);
    assert_eq!(fs::read(&file).unwrap(), bytes);
    assert_eq!(fs::read(fixture.path().join("unrelated")).unwrap(), b"keep");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        assert_eq!(
            network
                .source_lease_checks
                .load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert!(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(file)
                .is_ok()
        );
    }
}

#[tokio::test]
async fn bad_writable_root_precedes_local_inspection_and_directory_rejects_before_target_naming() {
    let fixture = Fixture::new();
    let local = TraceLocal {
        inner: fixture.adapter().await,
        events: Mutex::new(Vec::new()),
    };
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
        .upload(
            fixture.path(),
            &source("Tables/table", OneLakeResourceKind::Table),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.output().error_code.as_deref(), Some("NotSupported"));
    assert!(
        error
            .output()
            .message
            .unwrap()
            .starts_with("Cannot write in folder 'Tables'")
    );
    assert!(local.events.lock().unwrap().is_empty());
    let error = host
        .upload(
            fixture.path(),
            &source("Files/table", OneLakeResourceKind::Table),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.output().message.as_deref(),
        Some("Recursive copy not supported")
    );
    assert_eq!(*local.events.lock().unwrap(), ["kind"]);
    assert!(network.requests.lock().unwrap().is_empty());
    assert!(ui.notices.is_empty());
}

#[tokio::test]
async fn missing_source_precedes_invalid_destination_kind_and_never_creates_local_or_remote_data() {
    let fixture = Fixture::new();
    let local = TraceLocal {
        inner: fixture.adapter().await,
        events: Mutex::new(Vec::new()),
    };
    let file = fixture.path().join("missing");
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let error = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .upload(
        &file,
        &source("Files/table", OneLakeResourceKind::Table),
        OneLakeWriteLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.output().message.as_deref(),
        Some("Invalid source, expected file")
    );
    assert_eq!(*local.events.lock().unwrap(), ["kind", "kind"]);
    assert!(network.requests.lock().unwrap().is_empty());
    assert!(!file.exists());
}

#[tokio::test]
async fn invalid_destination_after_file_classification_never_opens_reads_or_uploads_source() {
    let fixture = Fixture::new();
    let local = TraceLocal {
        inner: fixture.adapter().await,
        events: Mutex::new(Vec::new()),
    };
    let file = fixture.path().join("source");
    fs::write(&file, b"bytes").unwrap();
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let error = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .upload(
        &file,
        &source("Files/table", OneLakeResourceKind::Table),
        OneLakeWriteLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.output().error_code.as_deref(), Some("InvalidPath"));
    assert_eq!(*local.events.lock().unwrap(), ["kind", "kind"]);
    assert!(network.requests.lock().unwrap().is_empty());
    assert!(ui.notices.is_empty());
}

#[tokio::test]
async fn empty_read_fault_and_byte_bound_fail_after_progress_but_before_create() {
    let fixture = Fixture::new();
    let file = fixture.path().join("source");
    for (bytes, read_failure, max, code) in [
        (&b""[..], false, 4, "InvalidInput"),
        (&b"content"[..], true, 8, "InvalidPath"),
        (&b"content"[..], false, 4, "Limit"),
    ] {
        fs::write(&file, bytes).unwrap();
        let local = FaultLocal {
            inner: fixture.adapter().await,
            read_failure,
            write_after: None,
            short_success: false,
        };
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let error = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &file,
            &source("Files/out", OneLakeResourceKind::Undefined),
            OneLakeWriteLimits {
                max_request_bytes: NonZeroUsize::new(max).unwrap(),
                ..OneLakeWriteLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.stage, CopyStage::LocalRead);
        assert_eq!(error.output().error_code.as_deref(), Some(code));
        assert_eq!(ui.notices.len(), 1);
        assert!(network.requests.lock().unwrap().is_empty());
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
}

#[tokio::test]
async fn create_status_other_than_201_does_not_append_flush_or_print_done() {
    let fixture = Fixture::new();
    let file = fixture.path().join("source");
    fs::write(&file, b"content").unwrap();
    let local = fixture.adapter().await;
    for status in [200, 202, 204] {
        let (client, adapter, network) = setup(vec![response(status, b"")]);
        let mut ui = Ui::default();
        let result = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &file,
            &source("Files/out", OneLakeResourceKind::File),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.upload.create.status(), status);
        assert!(result.upload.append.is_none() && result.upload.flush.is_none());
        assert!(result.outputs.is_empty());
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn failure_at_every_remote_stage_retains_mutation_uncertainty_without_replay_or_cleanup() {
    let fixture = Fixture::new();
    let file = fixture.path().join("source");
    fs::write(&file, b"content").unwrap();
    let local = fixture.adapter().await;
    for stage in 0..3 {
        let replies = (0..stage)
            .map(|index| response(if index == 0 { 201 } else { 202 }, b""))
            .collect();
        let (client, adapter, network) = setup(replies);
        *network.held_source.lock().unwrap() = Some(file.clone());
        network
            .replies
            .lock()
            .unwrap()
            .push_back(Err(failed_response()));
        let mut ui = Ui::default();
        let error = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &file,
            &source("Files/out", OneLakeResourceKind::File),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.stage, CopyStage::StorageWrite);
        let CopyErrorKind::Upload { error: cause, .. } = error.kind else {
            panic!("lost upload error");
        };
        let OneLakeWriteError::Storage(cause) = *cause else {
            panic!("lost mutation uncertainty");
        };
        assert!(cause.mutation_may_have_occurred);
        assert_eq!(
            cause.completed_steps,
            [OneLakeWriteStep::CreateFile, OneLakeWriteStep::Append][..stage]
        );
        assert_eq!(network.requests.lock().unwrap().len(), stage + 1);
        assert_eq!(fs::read(&file).unwrap(), b"content");
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&file)
                    .is_ok()
            );
        }
    }
}

#[tokio::test]
async fn cancellation_after_each_upload_request_retains_exact_stage_uncertainty_and_deadline() {
    let fixture = Fixture::new();
    let file = fixture.path().join("source");
    fs::write(&file, b"content").unwrap();
    let local = fixture.adapter().await;
    for after in 1..=3 {
        let (client, adapter, network) = setup(vec![
            response(201, b""),
            response(202, b""),
            response(200, b""),
        ]);
        *network.held_source.lock().unwrap() = Some(file.clone());
        let mut ui = Ui::default();
        let signal = std::future::poll_fn(|_| {
            if network.requests.lock().unwrap().len() >= after {
                std::task::Poll::Ready(Interruption::Deadline)
            } else {
                std::task::Poll::Pending
            }
        });
        let error = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &file,
            &source("Files/out", OneLakeResourceKind::File),
            OneLakeWriteLimits::default(),
            signal,
        )
        .await
        .unwrap_err();
        let CopyErrorKind::Upload {
            error: cause,
            interruption,
        } = error.kind
        else {
            panic!("lost interruption evidence");
        };
        assert_eq!(interruption, Some(Interruption::Deadline));
        assert!(
            matches!(*cause, OneLakeWriteError::Storage(ref cause) if cause.mutation_may_have_occurred && cause.completed_steps.len() == after - 1)
        );
        assert_eq!(network.requests.lock().unwrap().len(), after);
        assert_eq!(fs::read(&file).unwrap(), b"content");
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&file)
                    .is_ok()
            );
        }
    }
}

#[tokio::test]
async fn destination_mime_and_shortcut_basename_follow_existing_case_sensitive_policy() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let file = fixture.path().join("a b \u{e9}.txt");
    fs::write(&file, b"not-json").unwrap();
    for (path, kind, expected, mime) in [
        (
            "Files/link",
            OneLakeResourceKind::Shortcut,
            "Files/link/a b \u{e9}.txt",
            "text/plain",
        ),
        (
            "Files/out.json",
            OneLakeResourceKind::File,
            "Files/out.json",
            "application/json",
        ),
        (
            "Files/out.TXT",
            OneLakeResourceKind::Undefined,
            "Files/out.TXT",
            "application/octet-stream",
        ),
    ] {
        let (client, adapter, network) = setup(vec![
            response(201, b""),
            response(202, b""),
            response(200, b""),
        ]);
        let mut ui = Ui::default();
        let result = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &file,
            &source(path, kind),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.storage_path.relative(), expected);
        assert_eq!(
            network.requests.lock().unwrap()[1]["headers"]["content-type"],
            mime
        );
    }
}

#[tokio::test]
async fn cancelled_before_local_inspection_and_source_root_policy_rejection_never_mutate() {
    let fixture = Fixture::new();
    let local = TraceLocal {
        inner: fixture.adapter().await,
        events: Mutex::new(Vec::new()),
    };
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let mut target = source("Files/out", OneLakeResourceKind::File);
    let mut host = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let error = host
        .upload(
            &fixture.path().join("missing"),
            &target,
            OneLakeWriteLimits::default(),
            std::future::ready(Interruption::Cancelled),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        CopyErrorKind::Interrupted(Interruption::Cancelled)
    ));
    let error = host
        .download(
            &target,
            fixture.path(),
            std::future::ready(Interruption::Cancelled),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        CopyErrorKind::Interrupted(Interruption::Cancelled)
    ));
    target.item_type = ItemType::parse("SemanticModel").unwrap();
    let error = host
        .upload(
            fixture.path(),
            &target,
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.output().error_code.as_deref(), Some("NotSupported"));
    assert!(local.events.lock().unwrap().is_empty());
    assert!(network.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn injected_nonfile_oversized_read_and_inflight_read_cancellation_cannot_dispatch_create() {
    let fixture = Fixture::new();
    let target = source("Files/out", OneLakeResourceKind::File);
    for (kind, read, expected_stage, expected_code) in [
        (
            LocalKind::Other,
            InjectedRead::Content(vec![1]),
            CopyStage::Validate,
            "InvalidPath",
        ),
        (
            LocalKind::File,
            InjectedRead::Content(vec![1; 5]),
            CopyStage::LocalRead,
            "Limit",
        ),
        (
            LocalKind::File,
            InjectedRead::Pending,
            CopyStage::LocalRead,
            "Cancelled",
        ),
    ] {
        let local = InjectedLocal {
            kind,
            read,
            reading: std::sync::atomic::AtomicBool::new(false),
        };
        let (client, adapter, network) = setup(vec![]);
        let mut ui = Ui::default();
        let signal = std::future::poll_fn(|_| {
            if local.reading.load(std::sync::atomic::Ordering::SeqCst) {
                std::task::Poll::Ready(Interruption::Cancelled)
            } else {
                std::task::Poll::Pending
            }
        });
        let error = StorageFilesHost {
            client: &client,
            executor: &adapter,
            local: &local,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
        }
        .upload(
            &fixture.path().join("injected-source"),
            &target,
            OneLakeWriteLimits {
                max_request_bytes: NonZeroUsize::new(4).unwrap(),
                ..OneLakeWriteLimits::default()
            },
            signal,
        )
        .await
        .unwrap_err();
        assert_eq!(error.stage, expected_stage);
        assert_eq!(error.output().error_code.as_deref(), Some(expected_code));
        assert!(!error.local_write_may_have_occurred);
        assert!(network.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unavailable_source_lease_fails_explicitly_instead_of_silently_reading_bytes() {
    struct NoLease(OsFileSystem);
    impl LocalFileSystem for NoLease {
        fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
            self.0.kind(path)
        }
        fn read<'a>(
            &'a self,
            _: &'a Path,
            _: NonZeroUsize,
        ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
            panic!("must not substitute an early-closing byte read");
        }
        fn write<'a>(
            &'a self,
            _: &'a Path,
            _: Vec<u8>,
        ) -> CommandFuture<'a, Result<usize, LocalError>> {
            panic!("must not write local files");
        }
    }
    let fixture = Fixture::new();
    let path = fixture.path().join("source");
    fs::write(&path, b"keep").unwrap();
    let local = NoLease(fixture.adapter().await);
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let error = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .upload(
        &path,
        &source("Files/out", OneLakeResourceKind::File),
        OneLakeWriteLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.kind, CopyErrorKind::Local(ref cause) if matches!(cause.kind, LocalErrorKind::SourceLeaseUnavailable))
    );
    assert!(network.requests.lock().unwrap().is_empty());
    assert_eq!(fs::read(path).unwrap(), b"keep");
}

#[tokio::test]
async fn public_physical_copy_futures_can_run_on_the_callers_send_executor() {
    fn send<T>(_: impl std::future::Future<Output = T> + Send) {}
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
    let path = fixture.path().join("source");
    let target = source("Files/out", OneLakeResourceKind::File);
    send(host.upload(&path, &target, OneLakeWriteLimits::default(), pending()));
    send(host.download(&target, &path, pending()));
    assert!(network.requests.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn native_socket_source_is_not_treated_as_file_or_recursively_traversed() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let path = fixture.path().join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert_eq!(local.kind(&path).await.unwrap(), LocalKind::Other);
    let (client, adapter, network) = setup(vec![]);
    let mut ui = Ui::default();
    let error = StorageFilesHost {
        client: &client,
        executor: &adapter,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    }
    .upload(
        &path,
        &source("Files/out", OneLakeResourceKind::File),
        OneLakeWriteLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.output().message.as_deref(),
        Some("Invalid source, expected file")
    );
    assert!(network.requests.lock().unwrap().is_empty());
}
