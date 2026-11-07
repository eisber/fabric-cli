// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SDK-to-CLI metadata and exact-file composition without network or identity I/O.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::get_metadata::{
    MetadataError, MetadataFileRequest, MetadataHost, MetadataLimits, MetadataTarget,
};
use fabric_cli_storage_files::{ExactOutputErrorKind, ExactOutputStage};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use std::{
    collections::BTreeMap,
    fs,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-explicit-get",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Executor {
    requests: Mutex<Vec<String>>,
    status: u16,
}

const BODY: &[u8] = br#"{"id":"item","displayName":"A","future":{"empty":[],"value":null}}"#;

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            self.requests
                .lock()
                .unwrap()
                .push(request.url().to_string());
            Ok(ItemResponse {
                status: self.status,
                body: BODY.to_vec(),
                headers: BTreeMap::new(),
            })
        })
    }
}

impl WorkspaceExecutor for Executor {
    fn execute<'a>(
        &'a self,
        _: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        panic!("item read must not enumerate or enrich its workspace")
    }
}

struct Fixture {
    root: PathBuf,
    credential: Arc<Credential>,
    client: Client,
    executor: Executor,
}

impl Fixture {
    fn new(status: u16) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fabric-explicit-get-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        fs::create_dir(&root).expect("private test directory must not already exist");
        let credential = Arc::new(Credential::default());
        let client = Client::new(
            credential.clone(),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        );
        Self {
            root,
            credential,
            client,
            executor: Executor {
                requests: Mutex::new(Vec::new()),
                status,
            },
        }
    }

    fn host(&self) -> MetadataHost<'_> {
        MetadataHost {
            client: &self.client,
            items: &self.executor,
            workspaces: &self.executor,
        }
    }

    fn path(&self) -> PathBuf {
        self.root.join("exact.chosen-name")
    }

    fn assert_no_read(&self) {
        assert_eq!(self.credential.calls.load(Ordering::SeqCst), 0);
        assert!(self.executor.requests.lock().unwrap().is_empty());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove only the test-owned directory");
    }
}

fn target() -> MetadataTarget<'static> {
    MetadataTarget::Item {
        workspace_id: "ws",
        item_id: "item",
    }
}

fn limits(bytes: usize) -> MetadataLimits {
    MetadataLimits {
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_result_bytes: NonZeroUsize::new(bytes).unwrap(),
    }
}

#[tokio::test]
async fn cli_composes_the_sdk_operation_with_exact_file_delivery() {
    let fixture = Fixture::new(200);
    let path = fixture.path();
    let request = MetadataFileRequest::preflight(&path, limits(BODY.len())).unwrap();
    assert!(!path.exists());
    fixture.assert_no_read();
    let output = request
        .acquire(&fixture.host(), target(), pending())
        .await
        .unwrap();
    assert_eq!(output.read().target(), target());
    assert_eq!(output.read().json_bytes(), BODY);
    assert!(!path.exists());
    let receipt = output.commit().unwrap();
    assert_eq!(receipt.path, path);
    assert_eq!(receipt.bytes_written, BODY.len());
    assert_eq!(fs::read(path).unwrap(), BODY);
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
    assert_eq!(
        fixture.executor.requests.lock().unwrap().as_slice(),
        &["https://api.fabric.microsoft.com/v1/workspaces/ws/items/item"]
    );
}

#[test]
fn existing_output_fails_before_credentials_or_remote_reads() {
    let fixture = Fixture::new(200);
    let path = fixture.path();
    fs::write(&path, b"preserve").unwrap();
    let error = MetadataFileRequest::preflight(&path, limits(4096)).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Preflight);
    assert_eq!(fs::read(path).unwrap(), b"preserve");
    fixture.assert_no_read();
}

#[tokio::test]
async fn racing_output_is_preserved_after_the_remote_read() {
    let fixture = Fixture::new(200);
    let path = fixture.path();
    let request = MetadataFileRequest::preflight(&path, limits(4096)).unwrap();
    let output = request
        .acquire(&fixture.host(), target(), pending())
        .await
        .unwrap();
    fs::write(&path, b"competitor").unwrap();
    let error = output.commit().unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Create);
    assert!(!error.path_created);
    assert_eq!(fs::read(path).unwrap(), b"competitor");
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn denied_read_and_over_budget_metadata_never_create_a_file() {
    for (status, cap) in [(403, 4096), (200, BODY.len() - 1)] {
        let fixture = Fixture::new(status);
        let path = fixture.path();
        let request = MetadataFileRequest::preflight(&path, limits(cap)).unwrap();
        let error = request
            .acquire(&fixture.host(), target(), pending())
            .await
            .unwrap_err();
        if status == 200 {
            assert!(matches!(error, MetadataError::BudgetExceeded));
        }
        assert!(!path.exists());
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn cancellation_or_abandoned_delivery_does_not_create_output() {
    let fixture = Fixture::new(200);
    let path = fixture.path();
    let request = MetadataFileRequest::preflight(&path, limits(4096)).unwrap();
    assert!(
        request
            .acquire(&fixture.host(), target(), ready(()))
            .await
            .is_err()
    );
    fixture.assert_no_read();
    assert!(!path.exists());
    let request = MetadataFileRequest::preflight(&path, limits(4096)).unwrap();
    let output = request
        .acquire(&fixture.host(), target(), pending())
        .await
        .unwrap();
    drop(output);
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn relative_or_missing_parent_destination_cannot_start_a_read() {
    let fixture = Fixture::new(200);
    assert!(MetadataFileRequest::preflight(Path::new("relative.json"), limits(4096)).is_err());
    let error =
        MetadataFileRequest::preflight(&fixture.root.join("missing").join("file"), limits(4096))
            .unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::MissingParent);
    fixture.assert_no_read();
}
