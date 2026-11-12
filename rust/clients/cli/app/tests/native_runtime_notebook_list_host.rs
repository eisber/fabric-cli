// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Released native host + Notebook list leaf, including bounded child-pipe tests.
//! Scripted native execution is not an approved Notebook HTTP/auth adapter.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, Url},
};
use fabric_cli::runtime_v2::{
    ParsedInvocation, ReadErrorKind, ReadOptions, ReadReceipt, ResolvedTarget,
    host::{self, ExplicitResolver, HostError, prepare_destination},
    native_runtime_notebook_list as leaf, parse_args,
};
use fabric_cli_output::OutputStream;
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeFuture, NativeMethod,
        NativeRequest, NativeResponse,
    },
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor, native_runtime::NativeFabricExecutor};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    future::pending,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const PATH: &str = "/Team.Workspace/Analysis.Notebook/sessions";
const ROUTE: &str = "https://selected.pbidedicated.windows.net/webapi/capacities/cap/workloads/Notebook/Data/Automatic/api/workspaces/ws/artifacts/nb/jupyterApi/versions/1/api/sessions";
const CHILD_MODE: &str = "FAB_NOTEBOOK_LIST_TEST_CHILD";
const CHILD_ROOT: &str = "FAB_NOTEBOOK_LIST_TEST_ROOT";

fn input(facade: bool, output: &str) -> ParsedInvocation {
    let mut args = vec!["fab-cli-v2"];
    if facade {
        args.extend(["artifact", "notebook"]);
    }
    args.extend(["list", PATH, "--output", output]);
    parse_args(args).unwrap()
}

fn resolver(input: &ParsedInvocation, origin: &str) -> ExplicitResolver {
    let runtime = NotebookRuntime::new(
        NativeEndpoint::new(Url::parse(origin).unwrap()).unwrap(),
        "cap",
        "ws",
        "nb",
        ContentLimits::default(),
        4 * 1024 * 1024,
        1000,
    )
    .unwrap();
    ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Notebook(runtime)).unwrap()
}

struct Script {
    status: u16,
    body: Vec<u8>,
    calls: AtomicUsize,
    wait: bool,
    dropped: AtomicUsize,
}

impl Script {
    fn new(rows: &Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(rows).unwrap(),
            calls: AtomicUsize::new(0),
            wait: false,
            dropped: AtomicUsize::new(0),
        }
    }
}

struct Dropped<'a>(&'a AtomicUsize);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl NativeExecutor for Script {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        _: usize,
    ) -> NativeFuture<'a, Result<NativeResponse, NativeError>> {
        Box::pin(async move {
            assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0, "HTTP replay");
            assert_eq!(request.method(), NativeMethod::Get);
            assert_eq!(request.effect(), NativeEffect::Read);
            assert_eq!(request.url().as_str(), ROUTE);
            if self.wait {
                let _guard = Dropped(&self.dropped);
                pending::<()>().await;
            }
            Ok(NativeResponse {
                status: self.status,
                body: self.body.clone(),
                content_type: Some("application/json".into()),
                headers: Default::default(),
            })
        })
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

async fn acquire_to_file(
    input: &ParsedInvocation,
    executor: &dyn NativeExecutor,
    root: &Path,
    end: Instant,
) -> Result<ReadReceipt, HostError> {
    let binding = resolver(input, "https://selected.pbidedicated.windows.net");
    let cancelled = pending::<()>();
    tokio::pin!(cancelled);
    let destination =
        prepare_destination(input.output.clone(), root.into(), end, &mut cancelled).await?;
    let observation = leaf::acquire(
        input,
        executor,
        &binding,
        ReadOptions::default(),
        &destination,
        end,
        &mut cancelled,
    )
    .await?;
    observation.deliver(destination, end, &mut cancelled).await
}

#[tokio::test]
async fn shared_host_canonical_facade_preserve_collection_and_identical_receipts() {
    let rows = json!([
        {"id":"failed","state":"Failed","kernel":{"id":"kernel","execution_state":"future"}},
        {"id":"unknown","state":false,"future":{"keep":[null,[]]}}
    ]);
    let mut delivered = Vec::new();
    for facade in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let executor = Script::new(&rows);
        let receipt = acquire_to_file(
            &input(facade, "result.json"),
            &executor,
            root.path(),
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(receipt.data, rows);
        assert_eq!(receipt.target.native_url, ROUTE);
        assert_eq!(receipt.meta.workload_observation, None);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        let bytes = std::fs::read(root.path().join("result.json")).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<ReadReceipt>(&bytes).unwrap(),
            receipt
        );
        delivered.push(bytes);
    }
    assert_eq!(delivered[0], delivered[1]);
}

#[tokio::test]
async fn existing_destination_and_atomic_race_never_clobber_or_replay() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    let invocation = input(false, "result.json");
    let executor = Script::new(&json!([]));
    std::fs::write(&path, "original").unwrap();
    assert_eq!(
        acquire_to_file(&invocation, &executor, root.path(), deadline())
            .await
            .unwrap_err()
            .kind,
        ReadErrorKind::OutputUnavailable
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    std::fs::remove_file(&path).unwrap();
    let end = deadline();
    let destination = prepare_destination(
        invocation.output.clone(),
        root.path().into(),
        end,
        pending(),
    )
    .await
    .unwrap();
    let binding = resolver(&invocation, "https://selected.pbidedicated.windows.net");
    let observation = leaf::acquire(
        &invocation,
        &executor,
        &binding,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .unwrap();
    std::fs::write(&path, "racing owner").unwrap();
    let error = observation
        .deliver(destination, end, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    assert!(error.read_completed);
    assert!(!error.file_created);
    assert_eq!(error.bytes_written, 0);
    let receipt = error.receipt.as_ref().unwrap();
    assert_eq!(receipt.data, json!([]));
    assert_eq!(receipt.target.native_url, ROUTE);
    assert_eq!(receipt.meta.http_status, 200);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "racing owner");
}

#[tokio::test]
async fn already_cancelled_delivery_retains_collection_without_creating_file_or_replay() {
    let root = tempfile::tempdir().unwrap();
    let invocation = input(false, "result.json");
    let rows = json!([{"id":"failed","state":"Failed","private":"retained-provider-field"}]);
    let executor = Script::new(&rows);
    let binding = resolver(&invocation, "https://selected.pbidedicated.windows.net");
    let end = deadline();
    let destination = prepare_destination(
        invocation.output.clone(),
        root.path().into(),
        end,
        pending(),
    )
    .await
    .unwrap();
    let observation = leaf::acquire(
        &invocation,
        &executor,
        &binding,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .unwrap();
    let error = observation
        .deliver(destination, end, async {})
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Cancelled);
    assert!(error.read_completed);
    assert!(!error.file_created);
    assert_eq!(error.bytes_written, 0);
    let receipt = error.receipt.as_ref().unwrap();
    assert_eq!(receipt.data, rows);
    assert_eq!(receipt.target.native_url, ROUTE);
    assert!(!format!("{error:?} {error}").contains("retained-provider-field"));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!root.path().join("result.json").exists());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn original_deadline_and_cancellation_stop_pending_native_read_without_remote_replay() {
    let root = tempfile::tempdir().unwrap();
    let invocation = input(false, "result.json");
    let binding = resolver(&invocation, "https://selected.pbidedicated.windows.net");
    for cancel in [false, true] {
        let mut executor = Script::new(&json!([]));
        executor.wait = true;
        let end = Instant::now()
            + if cancel {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(100)
            };
        let destination = prepare_destination(
            invocation.output.clone(),
            root.path().into(),
            end,
            pending(),
        )
        .await
        .unwrap();
        let cancellation = async {
            if cancel {
                tokio::time::sleep(Duration::from_millis(10)).await;
            } else {
                pending::<()>().await;
            }
        };
        tokio::pin!(cancellation);
        let error = leaf::acquire(
            &invocation,
            &executor,
            &binding,
            ReadOptions::default(),
            &destination,
            end,
            &mut cancellation,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.kind,
            if cancel {
                ReadErrorKind::Cancelled
            } else {
                ReadErrorKind::Timeout
            }
        );
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
        assert!(!root.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn foreign_invocation_expired_deadline_and_latched_cancel_are_zero_send() {
    let root = tempfile::tempdir().unwrap();
    let invocation = input(false, "result.json");
    let binding = resolver(&invocation, "https://selected.pbidedicated.windows.net");
    let executor = Script::new(&json!([]));
    let end = deadline();
    let destination = prepare_destination(
        invocation.output.clone(),
        root.path().into(),
        end,
        pending(),
    )
    .await
    .unwrap();
    let foreign = parse_args([
        "fab-cli-v2",
        "get",
        "/Team.Workspace/Analysis.Notebook/src/content",
    ])
    .unwrap();
    assert_eq!(
        leaf::acquire(
            &foreign,
            &executor,
            &binding,
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .err()
        .unwrap()
        .kind,
        ReadErrorKind::UnsupportedCapability
    );
    assert_eq!(
        leaf::acquire(
            &invocation,
            &executor,
            &binding,
            ReadOptions::default(),
            &destination,
            Instant::now() - Duration::from_secs(1),
            pending(),
        )
        .await
        .err()
        .unwrap()
        .kind,
        ReadErrorKind::Timeout
    );
    assert_eq!(
        leaf::acquire(
            &invocation,
            &executor,
            &binding,
            ReadOptions::default(),
            &destination,
            end,
            async {},
        )
        .await
        .err()
        .unwrap()
        .kind,
        ReadErrorKind::Cancelled
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert!(!root.path().join("result.json").exists());
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("Notebook must not request a public Fabric token")
    }
}

#[derive(Debug, Default)]
struct Wire(AtomicUsize);
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, _: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("Unapproved Notebook binding must be zero-send")
    }
}

#[tokio::test]
async fn public_fabric_adapter_cannot_authorize_notebook_origin_route_or_wrong_audience() {
    let root = tempfile::tempdir().unwrap();
    let invocation = input(false, "result.json");
    for (origin, audience) in [
        (
            "https://selected.pbidedicated.windows.net",
            Audience::Fabric,
        ),
        ("https://api.fabric.microsoft.com", Audience::Fabric),
        (
            "https://selected.pbidedicated.windows.net",
            Audience::OneLake,
        ),
    ] {
        let credential = Arc::new(Credential::default());
        let wire = Arc::new(Wire::default());
        let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
        let context = RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("selected-account", "selected-tenant", audience).unwrap(),
            endpoint.clone(),
        );
        let http = HttpExecutor::new(
            wire.clone(),
            Arc::new(TokioTimer),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(endpoint);
        let executor = NativeFabricExecutor::new(context, http);
        if audience == Audience::Fabric {
            let executor = executor.unwrap();
            let binding = resolver(&invocation, origin);
            let end = deadline();
            let destination = prepare_destination(
                invocation.output.clone(),
                root.path().into(),
                end,
                pending(),
            )
            .await
            .unwrap();
            assert!(
                leaf::acquire(
                    &invocation,
                    &executor,
                    &binding,
                    ReadOptions::default(),
                    &destination,
                    end,
                    pending(),
                )
                .await
                .is_err()
            );
        } else {
            assert!(executor.is_err());
        }
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert_eq!(wire.0.load(Ordering::SeqCst), 0);
        assert!(!root.path().join("result.json").exists());
    }
}

async fn blocked_child(mode: &str, root: &Path) -> u8 {
    let mut script = Script::new(&json!([{"id":"one","unknown":"x".repeat(512 * 1024)}]));
    let invocation = input(false, "-");
    if mode == "stderr" {
        script.status = 403;
    }
    let end = Instant::now() + Duration::from_secs(2);
    let binding = resolver(&invocation, "https://selected.pbidedicated.windows.net");
    let (send_cancel, receive_cancel) = tokio::sync::oneshot::channel();
    let mut send_cancel = Some(send_cancel);
    let cancelled = async {
        receive_cancel.await.unwrap();
    };
    tokio::pin!(cancelled);
    let destination = prepare_destination("-".into(), root.into(), end, &mut cancelled)
        .await
        .unwrap();
    let result = match leaf::acquire(
        &invocation,
        &script,
        &binding,
        ReadOptions::default(),
        &destination,
        end,
        &mut cancelled,
    )
    .await
    {
        Ok(read) => {
            if mode == "stdout_cancel" {
                let send = send_cancel.take().unwrap();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    send.send(()).unwrap();
                });
            }
            read.deliver(destination, end, &mut cancelled).await
        }
        Err(error) => Err(error),
    };
    let error = result.unwrap_err();
    if mode == "stderr" {
        // Fill an unread pipe through the shared owned-handle policy, then prove
        // diagnostics cannot fall back to a synchronous write after its deadline.
        let mut blocked = false;
        for _ in 0..128 {
            if host::write_stream(OutputStream::Stderr, vec![b'x'; 16 * 1024], end)
                .await
                .is_err()
            {
                blocked = true;
                break;
            }
        }
        assert!(
            blocked,
            "Unread stderr pipe accepted more than the bounded fixture budget"
        );
    }
    let diagnostic = host::diagnostic(&error, end).await;
    if mode == "stdout_cancel" {
        assert!(diagnostic.is_ok());
    } else {
        assert!(diagnostic.is_err());
    }
    std::fs::write(
        root.join("evidence.json"),
        serde_json::to_vec(&json!({
            "calls":script.calls.load(Ordering::SeqCst), "kind":error.kind,
            "readCompleted":error.read_completed, "replayed":false,
        }))
        .unwrap(),
    )
    .unwrap();
    1
}

#[test]
fn blocked_stdout_and_stderr_child_pipes_exit_bounded_nonzero_without_replay() {
    if let Some(mode) = std::env::var_os(CHILD_MODE) {
        let root = std::path::PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .unwrap();
        let exit = runtime.block_on(blocked_child(mode.to_str().unwrap(), &root));
        runtime.shutdown_timeout(Duration::from_millis(100));
        std::process::exit(i32::from(exit));
    }
    for mode in ["stdout", "stderr", "stdout_cancel"] {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "blocked_stdout_and_stderr_child_pipes_exit_bounded_nonzero_without_replay",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD_MODE, mode)
            .env(CHILD_ROOT, root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let started = Instant::now();
        let mut child = command.spawn().unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Unread {mode} pipe exceeded bounded child shutdown");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(1));
        let evidence: Value =
            serde_json::from_slice(&std::fs::read(root.path().join("evidence.json")).unwrap())
                .unwrap();
        assert_eq!(evidence["calls"], 1);
        assert_eq!(evidence["replayed"], false);
        assert_eq!(
            evidence["kind"],
            match mode {
                "stdout" => "timeout",
                "stdout_cancel" => "cancelled",
                _ => "http",
            }
        );
        assert_eq!(evidence["readCompleted"], mode != "stderr");
    }
}
