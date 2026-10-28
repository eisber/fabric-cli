// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real filesystem operations on the production current-thread worker adapter.

#![cfg(feature = "native-read")]

use fabric_cli::families::transfer_files::{TokioTransferFiles, TransferWorkerError};
use fabric_cli_transfer::{LocalTransferFiles, TransferFiles};
use fabric_deploy::definitions::{
    DefinitionErrorKind, DefinitionFile, DefinitionImportFileSystem, DefinitionLimits, ImportTarget,
};
use fabric_deploy::{
    deployment_config::ConfigurationError,
    repository::{NotebookRepository, RepositoryError},
};
use serde_json::Value;
use std::{
    fs,
    future::{Future, poll_fn},
    io,
    num::NonZeroUsize,
    path::PathBuf,
    pin::{Pin, pin},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    task::Poll,
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            ".async-transfer-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    async fn adapter(&self, limits: DefinitionLimits) -> TokioTransferFiles {
        TokioTransferFiles::new(
            self.0.clone(),
            None,
            limits,
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("cleanup only this test's exact owned directory");
    }
}

fn file(path: &str, bytes: &[u8]) -> DefinitionFile {
    DefinitionFile {
        path: path.into(),
        bytes: bytes.into(),
    }
}

fn worker_kind(error: &io::Error) -> &TransferWorkerError {
    error
        .get_ref()
        .unwrap()
        .downcast_ref::<TransferWorkerError>()
        .unwrap()
}

#[test]
fn current_thread_heartbeat_runs_while_actual_file_write_is_queued() {
    let directory = Directory::new();
    let runtime = Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut files = directory.adapter(Default::default()).await;
        let (entered, started) = oneshot::channel();
        let (release, gate) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            // A deadlock guard, not a filesystem-speed/performance assertion.
            gate.recv_timeout(Duration::from_secs(30)).unwrap();
        });
        started.await.unwrap();
        let mut write =
            pin!(files.write_async(&directory.0, file("bytes.bin", &[0, 255, 13, 10]),));
        poll_fn(|cx| {
            assert!(write.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let heartbeat = tokio::spawn(async {
            tokio::task::yield_now().await;
            1
        });
        assert_eq!(heartbeat.await.unwrap(), 1);
        poll_fn(|cx| {
            assert!(write.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(!directory.0.join("bytes.bin").exists());
        release.send(()).unwrap();
        write.await.unwrap();
        blocker.await.unwrap();
        assert_eq!(
            fs::read(directory.0.join("bytes.bin")).unwrap(),
            [0, 255, 13, 10],
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn dropped_queued_write_retains_one_permit_and_does_not_touch_the_tree() {
    let directory = Directory::new();
    let runtime = Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut files = directory.adapter(Default::default()).await;
        let (entered, started) = oneshot::channel();
        let (release, gate) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(30)).unwrap();
        });
        started.await.unwrap();
        {
            let mut write = pin!(files.write_async(&directory.0, file("never.bin", b"no")));
            poll_fn(|cx| {
                assert!(write.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        let mut next = pin!(files.is_empty_async(&directory.0));
        poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        release.send(()).unwrap();
        assert!(next.await.unwrap());
        blocker.await.unwrap();
        assert!(!directory.0.join("never.bin").exists());
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[tokio::test]
async fn native_errors_preflight_and_finite_bytes_remain_explicit() {
    let directory = Directory::new();
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(4).unwrap(),
        max_files: NonZeroUsize::new(2).unwrap(),
        ..Default::default()
    };
    let mut files = directory.adapter(limits).await;
    let error = files.resolve_directory(".").unwrap_err();
    assert!(matches!(
        worker_kind(&error),
        TransferWorkerError::AsyncRequired
    ));
    assert_eq!(
        files
            .resolve_directory_async("../escape")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput,
    );
    assert_eq!(
        files
            .resolve_directory_async("missing")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
    );
    let error = files
        .write_async(&directory.0, file("too-large.bin", b"12345"))
        .await
        .unwrap_err();
    assert!(matches!(worker_kind(&error), TransferWorkerError::Limit));
    assert!(!directory.0.join("too-large.bin").exists());
    let error = files
        .preflight_async(
            &directory.0,
            vec![file("a", b""), file("b", b""), file("c", b"")],
        )
        .await
        .unwrap_err();
    assert!(matches!(worker_kind(&error), TransferWorkerError::Limit));
    fs::write(directory.0.join("existing.bin"), b"keep").unwrap();
    fs::create_dir(directory.0.join("collision")).unwrap();
    assert!(
        files
            .preflight_async(
                &directory.0,
                vec![file("existing.bin", b"x"), file("collision", b"y")],
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(directory.0.join("existing.bin")).unwrap(), b"keep");
    let plan = vec![file("z.bin", b"\0"), file("a.bin", b"\xff")];
    let checked = files.preflight_async(&directory.0, plan).await.unwrap();
    assert_eq!(checked[0].path, "z.bin");
    assert_eq!(checked[1].path, "a.bin");
}

#[tokio::test]
async fn worker_import_keeps_typed_source_gaps_and_byte_bounds_before_mutation() {
    let directory = Directory::new();
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(4).unwrap(),
        ..Default::default()
    };
    fs::write(directory.0.join("data.bin"), b"12345").unwrap();
    let mut files = directory.adapter(limits).await;
    let target = ImportTarget {
        workspace_id: "11111111-1111-1111-1111-111111111111",
        item_id: None,
        item_type: "DataPipeline",
        display_name: "Synthetic",
        folder_id: None,
    };
    let error = files
        .prepare_import_async(&directory.0, target, None, Default::default())
        .await
        .unwrap_err();
    assert!(matches!(error.kind, DefinitionErrorKind::FileSystem(_)));
    let error = files
        .prepare_import_async(
            &directory.0,
            ImportTarget {
                item_type: "Environment",
                ..target
            },
            None,
            Default::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        DefinitionErrorKind::Unsupported("Environment readiness/publish lifecycle")
    ));
    assert_eq!(fs::read(directory.0.join("data.bin")).unwrap(), b"12345");
    assert!(error.completed_paths.is_empty());
}

#[test]
fn unavailable_worker_is_typed_without_a_synchronous_fallback() {
    let directory = Directory::new();
    let worker = Builder::new_current_thread().build().unwrap();
    let mut files = worker.block_on(directory.adapter(Default::default()));
    worker.shutdown_timeout(Duration::ZERO);
    let driver = Builder::new_current_thread().build().unwrap();
    driver.block_on(async {
        let error = files
            .write_async(&directory.0, file("never.bin", b"no"))
            .await
            .unwrap_err();
        assert!(matches!(
            worker_kind(&error),
            TransferWorkerError::Unavailable
        ));
        assert!(!directory.0.join("never.bin").exists());
    });
    driver.shutdown_timeout(Duration::from_secs(1));
}

#[cfg(unix)]
#[tokio::test]
async fn worker_retains_link_rejection_without_resolving_individual_paths() {
    let directory = Directory::new();
    fs::create_dir(directory.0.join("real")).unwrap();
    std::os::unix::fs::symlink(directory.0.join("real"), directory.0.join("linked")).unwrap();
    let mut files = directory.adapter(Default::default()).await;
    assert!(files.resolve_directory_async("linked").await.is_err());
    assert!(
        files
            .write_async(&directory.0, file("linked/never.bin", b"no"))
            .await
            .is_err()
    );
    assert!(!directory.0.join("real").join("never.bin").exists());
}

fn repository(directory: &Directory) -> PathBuf {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/publish_source.json"
    ))
    .unwrap();
    let root = directory.0.join("repository");
    let notebook = root.join("Book.Notebook");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&notebook).unwrap();
    for file in fixture["cases"][0]["files"].as_array().unwrap() {
        fs::write(
            notebook.join(file[0].as_str().unwrap()),
            file[1].as_str().unwrap(),
        )
        .unwrap();
    }
    root
}

async fn occupy_worker() -> (mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (entered, started) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(30)).unwrap();
    });
    started.await.unwrap();
    (release, blocker)
}

async fn pending_with_heartbeat<T>(mut work: Pin<&mut impl Future<Output = T>>) {
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::spawn(async { tokio::task::yield_now().await })
        .await
        .unwrap();
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[test]
fn current_thread_read_and_deploy_repository_preparation_use_the_actual_worker() {
    let directory = Directory::new();
    let repository = repository(&directory);
    fs::write(directory.0.join("raw.bin"), [0, 255, 13, 10]).unwrap();
    let runtime = Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut files = directory.adapter(Default::default()).await;
        {
            let (release, blocker) = occupy_worker().await;
            let mut read = pin!(files.read_async(&directory.0, "raw.bin", 4));
            pending_with_heartbeat(read.as_mut()).await;
            release.send(()).unwrap();
            assert_eq!(read.await.unwrap(), [0, 255, 13, 10]);
            blocker.await.unwrap();
        }
        {
            let (release, blocker) = occupy_worker().await;
            let mut prepare =
                pin!(files.prepare_repository_async(&repository, Default::default(), 32,));
            pending_with_heartbeat(prepare.as_mut()).await;
            release.send(()).unwrap();
            let prepared = prepare.await.unwrap();
            blocker.await.unwrap();
            assert_eq!(prepared.notebooks.len(), 1);
            assert_eq!(prepared.notebooks[0].name(), "Book");
        }
        assert_eq!(
            fs::read(directory.0.join("raw.bin")).unwrap(),
            [0, 255, 13, 10]
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn dropped_deploy_preparation_shares_the_transfer_worker_permit_with_reads() {
    let directory = Directory::new();
    let repository = repository(&directory);
    fs::write(directory.0.join("raw.bin"), b"keep").unwrap();
    let runtime = Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut files = directory.adapter(Default::default()).await;
        let (release, blocker) = occupy_worker().await;
        {
            let mut prepare =
                pin!(files.prepare_repository_async(&repository, Default::default(), 32,));
            pending_with_heartbeat(prepare.as_mut()).await;
        }
        let mut read = pin!(files.read_async(&directory.0, "raw.bin", 4));
        pending_with_heartbeat(read.as_mut()).await;
        release.send(()).unwrap();
        assert_eq!(read.await.unwrap(), b"keep");
        blocker.await.unwrap();
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[tokio::test]
async fn deployment_preparation_preserves_bounds_native_errors_and_sync_local_compatibility() {
    let directory = Directory::new();
    let repository = repository(&directory);
    fs::write(directory.0.join("raw.bin"), b"keep").unwrap();
    let mut files = directory.adapter(Default::default()).await;
    assert_eq!(
        files
            .read_async(&directory.0, "../escape", 4)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput,
    );
    assert!(files.read_async(&directory.0, "raw.bin", 3).await.is_err());
    let Err(error) = files
        .prepare_repository_async(&repository, Default::default(), 0)
        .await
    else {
        panic!("aggregate repository entry bound must fail");
    };
    assert!(matches!(
        error,
        RepositoryError::Configuration(ConfigurationError::Limit)
    ));
    let mut local = LocalTransferFiles::new(directory.0.clone(), None, Default::default()).unwrap();
    assert_eq!(local.read(&directory.0, "raw.bin", 4).unwrap(), b"keep");
    let synchronous =
        NotebookRepository::discover(&mut local, &repository, Default::default(), 32).unwrap();
    let asynchronous = files
        .prepare_repository_async(&repository, Default::default(), 32)
        .await
        .unwrap();
    assert_eq!(
        synchronous.notebooks[0].name(),
        asynchronous.notebooks[0].name()
    );
    let fallback = local
        .prepare_repository_async(&repository, Default::default(), 32)
        .await
        .unwrap();
    assert_eq!(
        fallback.notebooks[0].name(),
        synchronous.notebooks[0].name()
    );
}
