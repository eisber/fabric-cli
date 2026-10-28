// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![allow(dead_code)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    CommandFuture, MutationUi, NoticeKind, OneLakeResourceKind, OneLakeWriteTarget,
};
use fabric_cli_storage_files::{
    LocalError, LocalErrorKind, LocalFileSystem, LocalKind, LocalReadFile, OsFileSystem,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_onelake::{OneLakeClient, OneLakePath};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    future::pending,
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        Ok(AccessToken::new(
            "offline-fixture",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
pub struct Network {
    pub replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    pub requests: Mutex<Vec<Value>>,
    pub held_source: Mutex<Option<PathBuf>>,
    pub source_lease_checks: AtomicUsize,
}
#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        #[cfg(windows)]
        if let Some(path) = self.held_source.lock().unwrap().as_ref() {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(path)
                    .is_err()
            );
            self.source_lease_checks.fetch_add(1, Ordering::SeqCst);
        }
        let headers: BTreeMap<_, _> = ["content-type", "x-ms-content-type"]
            .into_iter()
            .filter_map(|name| {
                request
                    .headers()
                    .get_str(&name.into())
                    .ok()
                    .map(|v| (name, v.to_owned()))
            })
            .collect();
        self.requests.lock().unwrap().push(json!({
            "method": match request.method() {
                Method::Get => "get", Method::Put => "put", Method::Patch => "patch",
                _ => panic!("unexpected request"),
            },
            "url": request.url().as_str(), "headers": headers,
            "body": Bytes::from(request.body()).as_ref(),
        }));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or replay")
    }
}
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(delay, Duration::from_secs(240));
        Box::pin(pending())
    }
}

pub fn response(status: u16, body: &[u8]) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(StatusCode::from(status), Headers::new(), body.to_vec())
}

pub fn setup(replies: Vec<AsyncRawResponse>) -> (OneLakeClient, OneLakeHttpExecutor, Arc<Network>) {
    let network = Arc::new(Network {
        replies: Mutex::new(replies.into_iter().map(Ok).collect()),
        ..Network::default()
    });
    let adapter = OneLakeHttpExecutor::new(Arc::new(
        HttpExecutor::new(
            network.clone(),
            Arc::new(Timer),
            ExecutionOptions::default(),
        )
        .unwrap(),
    ));
    let client = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience()).unwrap(),
    );
    (client, adapter, network)
}

pub fn failed_response() -> azure_core::Error {
    azure_core::Error::with_message(ErrorKind::Other, "synthetic lost response")
}

pub fn source(path: &str, kind: OneLakeResourceKind) -> OneLakeWriteTarget {
    OneLakeWriteTarget {
        path: OneLakePath::new("workspace", "item", path).unwrap(),
        item_type: ItemType::parse("Lakehouse").unwrap(),
        kind,
        display_path: format!("/Team.Workspace/Lake.Lakehouse/{path}"),
    }
}

#[derive(Default)]
pub struct Ui {
    pub notices: Vec<Value>,
    pub prompts: Vec<String>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.notices.push(json!({"kind": match kind {
            NoticeKind::Progress => "progress", NoticeKind::Warning => "warning",
            NoticeKind::Help => "help",
        }, "message": message}));
    }
    fn confirm<'a>(&'a mut self, text: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(false))
    }
}

pub struct Fixture(PathBuf);
impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let parent = std::env::var_os("FABRIC_STORAGE_FILES_FIXTURES")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = parent.join(format!(
            "fabric-storage-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub async fn adapter(&self) -> OsFileSystem {
        OsFileSystem::new(self.0.clone(), tokio::runtime::Handle::current())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

pub struct FaultLocal {
    pub inner: OsFileSystem,
    pub read_failure: bool,
    pub write_after: Option<usize>,
    pub short_success: bool,
}
impl LocalFileSystem for FaultLocal {
    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        if self.read_failure {
            Box::pin(async {
                Err(LocalError::new(LocalErrorKind::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read failure",
                ))))
            })
        } else {
            self.inner.read_for_copy(path, limit)
        }
    }
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        self.inner.kind(path)
    }
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        if self.read_failure {
            Box::pin(async {
                Err(LocalError::new(LocalErrorKind::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read failure",
                ))))
            })
        } else {
            self.inner.read(path, limit)
        }
    }
    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move {
            if let Some(after) = self.write_after {
                let size = self
                    .inner
                    .write(path, content[..after.min(content.len())].to_vec())
                    .await?;
                if self.short_success {
                    return Ok(size);
                }
                return Err(LocalError {
                    kind: LocalErrorKind::Io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected write failure",
                    )),
                    destination_opened: true,
                    bytes_written: size,
                });
            }
            self.inner.write(path, content).await
        })
    }
}
