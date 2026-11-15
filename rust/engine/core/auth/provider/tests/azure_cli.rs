// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests exercising the actual Azure SDK credential with injected process results.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::{TokenCredential, TokenRequestOptions};
use azure_identity::Executor;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fabric_core::{Audience, CancellationToken, Clock, CoreError, ErrorKind, OperationContext};
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
        // Force concurrent cache callers to contend while SDK acquisition owns
        // the scope slot, rather than accidentally testing sequential reuse.
        tokio::task::yield_now().await;
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
    let scopes = [Audience::Fabric.scope()];
    let (first, second) = tokio::join!(
        credential.get_token(&scopes, Some(TokenRequestOptions::default())),
        credential.get_token(&scopes, Some(TokenRequestOptions::default())),
    );
    assert_eq!(first.unwrap().token, second.unwrap().token);
    assert_eq!(process.commands.lock().unwrap().len(), 2);
    credential
        .get_token(&[Audience::Storage.scope()], None)
        .await
        .unwrap();
    assert_eq!(process.commands.lock().unwrap().len(), 3);
    clock.0.store(4990, Ordering::SeqCst);
    credential
        .get_token(&[Audience::Fabric.scope()], None)
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
async fn process_spawn_failure_and_malformed_output_do_not_claim_signed_out() {
    for outputs in [
        vec![],
        vec!["not-json TOKEN-CANARY".into()],
        vec!["{}".into()],
    ] {
        let (session, _, _) = setup(outputs, false);
        let error = session.login(&context()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unauthorized);
        assert!(!error.to_string().contains("TOKEN-CANARY"));
        assert!(error.to_string().contains("Could not validate"));
        assert!(!error.to_string().contains("no signed-in account"));
        assert!(!error.to_string().contains("executable is unavailable"));
    }
}

#[derive(Debug)]
struct FailureProcess {
    code: i32,
    stdout: &'static str,
    stderr: &'static str,
}

#[async_trait]
impl Executor for FailureProcess {
    async fn run(&self, _: &OsStr, _: &[&OsStr]) -> io::Result<Output> {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(self.code << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(self.code as u32)
        };
        Ok(Output {
            status,
            stdout: self.stdout.as_bytes().to_vec(),
            stderr: self.stderr.as_bytes().to_vec(),
        })
    }
}

async fn failed_login(code: i32, stdout: &'static str, stderr: &'static str) -> CoreError {
    let session = AzureCliSession::new(
        Arc::new(FailureProcess {
            code,
            stdout,
            stderr,
        }),
        Arc::new(FakeClock(AtomicU64::new(1000))),
    )
    .unwrap();
    let error = session.login(&context()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unauthorized);
    assert!(!format!("{error:?}").contains("CANARY"));
    error
}

#[tokio::test]
async fn recognized_missing_executable_is_distinct_from_signed_out() {
    #[cfg(unix)]
    let missing_cases = [
        (127, "/bin/sh: az: command not found\n"),
        (127, "/bin/sh: 1: az: not found\n"),
        (127, "/bin/sh: az: not found\n"),
        (127, "/bin/sh: line 1: az: command not found\n"),
        (127, "/bin/sh: az: No such file or directory\n"),
    ];
    #[cfg(windows)]
    let missing_cases = [
        (
            1,
            "'az' is not recognized as an internal or external command,\r\noperable program or batch file.\r\n",
        ),
        (
            9009,
            "'az' is not recognized as an internal or external command,\r\noperable program or batch file.\r\n",
        ),
    ];
    for (code, stderr) in missing_cases {
        let missing = failed_login(code, "", stderr).await;
        assert!(
            missing
                .message
                .contains("Azure CLI executable is unavailable")
        );
        assert!(missing.message.contains("Install Azure CLI 2.54+"));
        assert!(missing.message.contains("PATH"));
        assert!(!missing.message.contains("no signed-in account"));
    }

    let signed_out =
        failed_login(1, "", "ERROR: Please run 'az login' to setup account.\r\n").await;
    assert_eq!(
        signed_out.message,
        "Azure CLI reports no signed-in account. Run az login, then fab-rs auth login"
    );
}

#[tokio::test]
async fn uncertain_failures_stay_generic_without_leaking_provider_output() {
    for (code, stdout, stderr) in [
        (
            1,
            "",
            "ERROR: AADSTS700082: expired refresh token TOKEN-CANARY",
        ),
        (1, "", "ERROR: scope denied STDERR-CANARY"),
        (127, "", "internal CLI process failure STDERR-CANARY"),
        (126, "", "/bin/sh: az: Permission denied STDERR-CANARY"),
        (1, "", "Please run az login. STDERR-CANARY"),
        (
            1,
            "",
            "ERROR: Please run 'az login' to setup account. STDERR-CANARY",
        ),
        (
            1,
            "TOKEN-CANARY",
            "ERROR: Please run 'az login' to setup account.",
        ),
        (127, "TOKEN-CANARY", "/bin/sh: az: command not found"),
    ] {
        let error = failed_login(code, stdout, stderr).await;
        assert!(error.message.contains("Could not validate"));
        assert!(!error.message.contains("no signed-in account"));
        assert!(!error.message.contains("executable is unavailable"));
    }
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
                .get_token(&[Audience::Fabric.scope()], None)
                .await
                .unwrap_err()
                .kind(),
            &azure_core::error::ErrorKind::Credential
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
        .get_token(&[Audience::Fabric.scope()], None)
        .await
        .unwrap();
    credential
        .get_token(&[Audience::Storage.scope()], None)
        .await
        .unwrap();
    clock.0.store(4990, Ordering::SeqCst);
    for scope in [Audience::Fabric.scope(), Audience::Storage.scope()] {
        assert_eq!(
            credential
                .get_token(&[scope], None)
                .await
                .unwrap_err()
                .kind(),
            &azure_core::error::ErrorKind::Credential
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

#[tokio::test(start_paused = true)]
async fn deadline_bounds_acquisition_and_expired_context_never_starts_process() {
    let (session, process, _) = setup(vec![], true);
    let ctx = OperationContext::new(Duration::from_secs(5), CancellationToken::new());
    assert_eq!(
        session.login(&ctx).await.unwrap_err().kind,
        ErrorKind::Timeout
    );
    assert!(process.dropped.load(Ordering::SeqCst));
    let ctx = OperationContext {
        deadline: Some(Instant::now() - Duration::from_secs(1)),
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
            .get_token(&["https://evil.invalid/.default"], None)
            .await
            .unwrap_err()
            .kind(),
        &azure_core::error::ErrorKind::DataConversion
    );
    assert_eq!(process.commands.lock().unwrap().len(), 1);
    for scopes in [
        vec![],
        vec![Audience::Fabric.scope(), Audience::Storage.scope()],
    ] {
        assert_eq!(
            credential
                .get_token(&scopes, Some(TokenRequestOptions::default()))
                .await
                .unwrap_err()
                .kind(),
            &azure_core::error::ErrorKind::DataConversion,
        );
    }
    assert_eq!(process.commands.lock().unwrap().len(), 1);
}
