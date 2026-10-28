// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Worker-bound consumers of the released d86ca1d generic repository API.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

use fabric_cli::families::transfer_files::TokioTransferFiles;
use fabric_cli_transfer::TransferFiles;
use fabric_deploy::{
    definitions::DefinitionLimits, deployment_config::ConfigurationError,
    notebook::DefinitionLimits as NotebookDefinitionLimits, repository::RepositoryError,
};
use serde_json::json;
use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            ".deployment-worker-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn item(&self, relative: impl AsRef<Path>, name: &str, kind: &str, id: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(".platform"),
            json!({
                "metadata":{"displayName":name,"type":kind},
                "config":{"logicalId":id},
            })
            .to_string(),
        )
        .unwrap();
        path
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn worker(root: &Path, limits: DefinitionLimits) -> TokioTransferFiles {
    TokioTransferFiles::new(
        root.to_owned(),
        None,
        limits,
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap()
}

fn pipeline(root: &Root) {
    let path = root.item(
        "Pipeline.DataPipeline",
        "Pipeline",
        "DataPipeline",
        "11111111-1111-1111-1111-111111111111",
    );
    fs::write(
        path.join("pipeline-content.json"),
        br#"{"name":"synthetic"}"#,
    )
    .unwrap();
}

#[tokio::test]
async fn full_snapshot_worker_preserves_generic_types_folders_and_binary_parts() {
    let root = Root::new();
    let pipeline = root.item(
        Path::new("Folder").join("Pipeline.DataPipeline"),
        "Pipeline",
        "DataPipeline",
        "11111111-1111-1111-1111-111111111111",
    );
    fs::write(
        pipeline.join("pipeline-content.json"),
        br#"{"name":"synthetic"}"#,
    )
    .unwrap();
    fs::create_dir(pipeline.join("Resources")).unwrap();
    fs::write(pipeline.join("Resources").join("b.whl"), [0, 255, 13, 10]).unwrap();
    let environment = root.item(
        "Shared.Environment",
        "Shared",
        "Environment",
        "22222222-2222-2222-2222-222222222222",
    );
    fs::write(
        environment.join("environment.yml"),
        b"dependencies:\r\n  - numpy\n",
    )
    .unwrap();
    let mut files = worker(&root.0, DefinitionLimits::default()).await;
    let snapshot = files
        .prepare_deployment_repository_async(&root.0, NotebookDefinitionLimits::default(), 100)
        .await
        .unwrap();
    assert_eq!(snapshot.items.len(), 2);
    assert!(snapshot.folders.iter().any(|folder| folder == "/Folder"));
    let item = snapshot
        .items
        .iter()
        .find(|item| item.item_type == "DataPipeline")
        .unwrap();
    assert_eq!(item.name, "Pipeline");
    assert_eq!(item.path, "Folder/Pipeline.DataPipeline");
    assert_eq!(item.folder_path, "/Folder");
    let binary = item.file("Resources/b.whl").unwrap();
    assert_eq!(binary.bytes, [0, 255, 13, 10]);
    assert!(!binary.text);
    assert!(
        snapshot
            .items
            .iter()
            .any(|item| item.item_type == "Environment")
    );
}

#[tokio::test]
async fn full_snapshot_worker_enforces_minimum_host_and_caller_byte_limits() {
    for (host, caller) in [(32, 4096), (4096, 32)] {
        let root = Root::new();
        pipeline(&root);
        let limits = DefinitionLimits {
            max_bytes: NonZeroUsize::new(host).unwrap(),
            ..DefinitionLimits::default()
        };
        let mut files = worker(&root.0, limits).await;
        assert!(
            files
                .prepare_deployment_repository_async(
                    &root.0,
                    NotebookDefinitionLimits {
                        max_bytes: NonZeroUsize::new(caller).unwrap()
                    },
                    100,
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn full_snapshot_worker_enforces_minimum_host_and_caller_aggregate_entry_limits() {
    for (host, caller) in [(2, 100), (100, 2)] {
        let root = Root::new();
        pipeline(&root);
        let limits = DefinitionLimits {
            max_files: NonZeroUsize::new(host).unwrap(),
            ..DefinitionLimits::default()
        };
        let mut files = worker(&root.0, limits).await;
        assert!(matches!(
            files
                .prepare_deployment_repository_async(
                    &root.0,
                    NotebookDefinitionLimits::default(),
                    caller
                )
                .await,
            Err(RepositoryError::Configuration(ConfigurationError::Limit))
        ));
    }
}

#[tokio::test]
async fn full_snapshot_worker_retains_original_filesystem_and_containment_failures() {
    let root = Root::new();
    let mut files = worker(&root.0, DefinitionLimits::default()).await;
    for (path, expected) in [
        (root.0.join("missing"), io::ErrorKind::NotFound),
        (
            root.0.join("..").join("outside"),
            io::ErrorKind::InvalidInput,
        ),
    ] {
        let error = files
            .prepare_deployment_repository_async(&path, NotebookDefinitionLimits::default(), 100)
            .await
            .unwrap_err();
        let RepositoryError::FileSystem(error) = error else {
            panic!("original filesystem evidence must survive")
        };
        assert_eq!(error.kind(), expected);
    }
}

#[tokio::test]
async fn generic_snapshot_capability_does_not_replace_legacy_notebook_or_raw_read_phases() {
    let root = Root::new();
    let notebook = root.item(
        "Book.Notebook",
        "Book",
        "Notebook",
        "11111111-1111-1111-1111-111111111111",
    );
    fs::write(
        notebook.join("notebook-content.py"),
        b"# Fabric notebook source\r\nprint('synthetic')\r\n",
    )
    .unwrap();
    fs::write(root.0.join("settings.txt"), b"raw\r\n").unwrap();
    let mut files = worker(&root.0, DefinitionLimits::default()).await;
    assert_eq!(
        files.read_async(&root.0, "settings.txt", 32).await.unwrap(),
        b"raw\r\n"
    );
    let legacy = files
        .prepare_repository_async(&root.0, NotebookDefinitionLimits::default(), 100)
        .await
        .unwrap();
    assert_eq!(legacy.notebooks.len(), 1);
    assert_eq!(legacy.notebooks[0].name(), "Book");
    let generic = files
        .prepare_deployment_repository_async(&root.0, NotebookDefinitionLimits::default(), 100)
        .await
        .unwrap();
    assert_eq!(generic.items.len(), 1);
    assert_eq!(generic.items[0].item_type, "Notebook");
}
