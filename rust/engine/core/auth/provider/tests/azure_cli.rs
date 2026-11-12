// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests exercising the actual Azure SDK credential with injected process results.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_identity::process::Executor;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fabric_core::{
    Audience, CancellationToken, Clock, CoreError, ErrorKind, OperationContext, TokenCredential,
};
use fabric_identity::{AzureCliSession, BoundCredential};
use serde_json::json;
use std::{
    collections::VecDeque,
    ffi::OsStr,
    io,
    process::{ExitStatus, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TENANT: &str = "00000000-0000-0000-0000-000000000001";
const PRINCIPAL: &str = "00000000-0000-0000-0000-000000000002";
const OTHER: &str = "00000000-0000-0000-0000-000000000003";

fn token(tenant: &str, principal: &str) -> String {
    format!(
        "e30.{}.c2ln",
        URL_SAFE_NO_PAD.encode(json!({"tid":tenant,"oid":principal}).to_string())
    )
}

fn response(tenant: &str, principal: &str, subscription: &str, expiry: u64) -> String {
    json!({"accessToken":token(tenant,principal),"tokenType":"Bearer","expires_on":expiry,"subscription":subscription}).to_string()
}

#[cfg(unix)]
fn success() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}
#[cfg(windows)]
fn success() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[derive(Debug)]
struct FakeProcess {
    outputs: Mutex<VecDeque<String>>,
    commands: Mutex<Vec<String>>,
    hang: bool,
    dropped: Arc<AtomicBool>,
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Executor for FakeProcess {
    async fn run(&self, _: &OsStr, args: &[&OsStr]) -> io::Result<Output> {
        self.commands.lock().unwrap().push(
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
        );
        let _drop = DropFlag(self.dropped.clone());
        if self.hang {
            std::future::pending::<()>().await;
        }
        let stdout = self
            .outputs
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
            .into_bytes();
        Ok(Output {
            status: success(),
            stdout,
            stderr: Vec::new(),
        })
    }
}

struct FakeClock(AtomicU64);
#[async_trait]
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall_now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
    async fn sleep(&self, _: Duration, _: &OperationContext) -> Result<(), CoreError> {
        panic!("no sleeps in credential tests")
    }
}

fn setup(
    outputs: Vec<String>,
    hang: bool,
) -> (Arc<AzureCliSession>, Arc<FakeProcess>, Arc<FakeClock>) {
    let process = Arc::new(FakeProcess {
        outputs: Mutex::new(outputs.into()),
        commands: Mutex::new(Vec::new()),
        hang,
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let clock = Arc::new(FakeClock(AtomicU64::new(1000)));
    let session = Arc::new(AzureCliSession::new(process.clone(), clock.clone()).unwrap());
    (session, process, clock)
}
fn context() -> OperationContext {
    OperationContext::new(Duration::from_secs(5), CancellationToken::new())
}

#[tokio::test]
async fn sdk_uses_current_context_and_single_flight_per_scope_with_expiry_refresh() {
    let (session, process, clock) = setup(
        vec![
            response(TENANT, PRINCIPAL, "first", 5000),
            response(TENANT, PRINCIPAL, "second", 5000),
            response(TENANT, PRINCIPAL, "third", 5000),
            response(TENANT, PRINCIPAL, "changed-only-subscription", 9000),
        ],
        false,
    );
    let binding = session.login(&context()).await.unwrap();
    let credential = BoundCredential::new(session, binding).unwrap();
    let ctx = context();
    let (first, second) = tokio::join!(
        credential.token(Audience::Fabric.scope(), &ctx),
        credential.token(Audience::Fabric.scope(), &ctx),
    );
    assert_eq!(first.unwrap().token, second.unwrap().token);
    assert_eq!(process.commands.lock().unwrap().len(), 2);
    credential
        .token(Audience::Storage.scope(), &context())
        .await
        .unwrap();
    assert_eq!(process.commands.lock().unwrap().len(), 3);
    clock.0.store(4990, Ordering::SeqCst);
    credential
        .token(Audience::Fabric.scope(), &context())
        .await
        .unwrap();
    assert_eq!(process.commands.lock().unwrap().len(), 4);
    for command in process.commands.lock().unwrap().iter() {
        assert!(command.contains("az account get-access-token -o json --scope"));
        assert!(!command.contains("--tenant"));
        assert!(!command.contains("--subscription"));
        assert!(!command.contains("az login"));
        assert!(!command.contains("az logout"));
        assert!(!command.contains("account set"));
    }
}

#[tokio::test]
async fn missing_az_and_malformed_signed_out_output_are_safe_unauthorized() {
    for outputs in [
        vec![],
        vec!["not-json TOKEN-CANARY".into()],
        vec!["{}".into()],
    ] {
        let (session, _, _) = setup(outputs, false);
        let error = session.login(&context()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unauthorized);
        assert!(!error.to_string().contains("TOKEN-CANARY"));
        assert!(error.to_string().contains("az login"));
    }
}

#[derive(Debug)]
struct SignedOutProcess;

#[async_trait]
impl Executor for SignedOutProcess {
    async fn run(&self, _: &OsStr, _: &[&OsStr]) -> io::Result<Output> {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(256)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(1)
        };
        Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: b"Please run az login. STDERR-CANARY".to_vec(),
        })
    }
}

#[tokio::test]
async fn nonzero_signed_out_process_output_is_sanitized() {
    let session = AzureCliSession::new(
        Arc::new(SignedOutProcess),
        Arc::new(FakeClock(AtomicU64::new(1000))),
    )
    .unwrap();
    let error = session.login(&context()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unauthorized);
    assert!(!error.to_string().contains("STDERR-CANARY"));
}

#[tokio::test]
async fn missing_claims_expired_tokens_and_tenant_or_principal_drift_fail_closed() {
    for (tenant, principal) in [(OTHER, PRINCIPAL), (TENANT, OTHER), ("", PRINCIPAL)] {
        let (session, _, _) = setup(
            vec![
                response(TENANT, PRINCIPAL, "same", 5000),
                response(tenant, principal, "same", 5000),
            ],
            false,
        );
        let binding = session.login(&context()).await.unwrap();
        let credential = BoundCredential::new(session, binding).unwrap();
        assert_eq!(
            credential
                .token(Audience::Fabric.scope(), &context())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unauthorized
        );
    }
    let (session, _, _) = setup(vec![response(TENANT, PRINCIPAL, "same", 999)], false);
    assert_eq!(
        session.login(&context()).await.unwrap_err().kind,
        ErrorKind::Unauthorized
    );
}

#[tokio::test]
async fn detected_identity_drift_also_blocks_previously_cached_other_scopes() {
    let (session, process, clock) = setup(
        vec![
            response(TENANT, PRINCIPAL, "same", 5000),
            response(TENANT, PRINCIPAL, "same", 5000),
            response(TENANT, PRINCIPAL, "same", 9000),
            response(OTHER, PRINCIPAL, "same", 9000),
        ],
        false,
    );
    let binding = session.login(&context()).await.unwrap();
    let credential = BoundCredential::new(session, binding).unwrap();
    credential
        .token(Audience::Fabric.scope(), &context())
        .await
        .unwrap();
    credential
        .token(Audience::Storage.scope(), &context())
        .await
        .unwrap();
    clock.0.store(4990, Ordering::SeqCst);
    for scope in [Audience::Fabric.scope(), Audience::Storage.scope()] {
        assert_eq!(
            credential.token(scope, &context()).await.unwrap_err().kind,
            ErrorKind::Unauthorized
        );
    }
    assert_eq!(process.commands.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn inflight_credential_cancellation_drops_the_injected_process_future() {
    let (session, process, _) = setup(vec![], true);
    let ctx = context();
    let cancellation = ctx.cancellation.clone();
    let cancel = async {
        tokio::task::yield_now().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(session.login(&ctx), cancel);
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(process.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn deadline_bounds_acquisition_and_expired_context_never_starts_process() {
    let (session, process, _) = setup(vec![], true);
    let ctx = OperationContext::new(Duration::from_millis(5), CancellationToken::new());
    assert_eq!(
        session.login(&ctx).await.unwrap_err().kind,
        ErrorKind::Timeout
    );
    assert!(process.dropped.load(Ordering::SeqCst));
    let ctx = OperationContext {
        deadline: Instant::now() - Duration::from_secs(1),
        cancellation: CancellationToken::new(),
    };
    assert_eq!(
        session.login(&ctx).await.unwrap_err().kind,
        ErrorKind::Timeout
    );
    assert_eq!(process.commands.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unsupported_scope_is_rejected_without_process_execution() {
    let (session, process, _) = setup(vec![response(TENANT, PRINCIPAL, "same", 5000)], false);
    let binding = session.login(&context()).await.unwrap();
    let credential = BoundCredential::new(session, binding).unwrap();
    assert_eq!(
        credential
            .token("https://evil.invalid/.default", &context())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(process.commands.lock().unwrap().len(), 1);
}
