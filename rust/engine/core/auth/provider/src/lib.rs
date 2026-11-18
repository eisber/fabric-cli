// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Current-context Azure CLI authentication. No fallback, context mutation, or disk tokens.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use azure_identity::{AzureCliCredential, AzureCliCredentialOptions, Executor};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub use fabric_core::Binding;
use fabric_core::{
    AZURE_CLI_LOGIN_REQUIRED as LOGIN_REQUIRED, Clock, CoreError, ErrorKind, OperationContext,
    bounded_with_clock,
};
use process_wrap::tokio::{KillOnDrop, TokioChildWrapper, TokioCommandWrap};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt, io,
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::Mutex,
};

const MAX_PROVIDER_BYTES: usize = 256 * 1024;
const CLI_UNAVAILABLE: &str = "Azure CLI executable is unavailable. Install Azure CLI 2.54+ or make az available on PATH, then run az login and fab-rs auth login";
const CLI_SIGNED_OUT: &str =
    "Azure CLI reports no signed-in account. Run az login, then fab-rs auth login";
const LOGIN_FAILED: &str = "Could not validate the current Azure CLI identity. Check Azure CLI 2.54+ and its current account/session, then retry fab-rs auth login";

#[derive(Debug)]
enum LoginDiagnostic {
    Unavailable,
    SignedOut,
}

impl fmt::Display for LoginDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => CLI_UNAVAILABLE,
            Self::SignedOut => CLI_SIGNED_OUT,
        })
    }
}

impl std::error::Error for LoginDiagnostic {}

#[derive(Debug)]
struct DiagnosticExecutor(Arc<dyn Executor>);

fn process_diagnostic(program: &OsStr, output: &Output) -> Option<LoginDiagnostic> {
    if output.status.success() || !output.stdout.is_empty() {
        return None;
    }
    // Exact known diagnostics only: exit 127 alone is not proof of a missing az.
    let stderr = std::str::from_utf8(&output.stderr)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    if output.status.code() == Some(1) && stderr == "ERROR: Please run 'az login' to setup account."
    {
        return Some(LoginDiagnostic::SignedOut);
    }
    if program == OsStr::new("/bin/sh")
        && output.status.code() == Some(127)
        && [
            "/bin/sh: 1: az: not found",
            "/bin/sh: az: not found",
            "/bin/sh: az: command not found",
            "/bin/sh: line 1: az: command not found",
            "/bin/sh: az: No such file or directory",
        ]
        .contains(&stderr)
    {
        return Some(LoginDiagnostic::Unavailable);
    }
    if program == OsStr::new("cmd")
        && matches!(output.status.code(), Some(1 | 9009))
        && stderr
            == "'az' is not recognized as an internal or external command,\r\noperable program or batch file."
    {
        return Some(LoginDiagnostic::Unavailable);
    }
    None
}

#[async_trait]
impl Executor for DiagnosticExecutor {
    async fn run(&self, program: &OsStr, args: &[&OsStr]) -> io::Result<Output> {
        let output = self.0.run(program, args).await?;
        if let Some(diagnostic) = process_diagnostic(program, &output) {
            return Err(io::Error::other(diagnostic));
        }
        Ok(output)
    }
}

fn login_error(error: azure_core::Error) -> CoreError {
    let mut cause: &(dyn std::error::Error + 'static) = &error;
    loop {
        if let Some(error) = cause.downcast_ref::<io::Error>()
            && let Some(diagnostic) = error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<LoginDiagnostic>())
        {
            return CoreError::new(ErrorKind::Unauthorized, diagnostic.to_string());
        }
        let Some(source) = cause.source() else {
            return CoreError::new(ErrorKind::Unauthorized, LOGIN_FAILED);
        };
        cause = source;
    }
}

fn unauthorized() -> CoreError {
    CoreError::new(ErrorKind::Unauthorized, LOGIN_REQUIRED)
}

fn sdk_unauthorized() -> azure_core::Error {
    azure_core::Error::with_message(azure_core::error::ErrorKind::Credential, LOGIN_REQUIRED)
}

fn token_expiry(token: &AccessToken) -> azure_core::Result<std::time::SystemTime> {
    let seconds =
        u64::try_from(token.expires_on.unix_timestamp()).map_err(|_| sdk_unauthorized())?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(sdk_unauthorized)
}

fn validate_scope(scope: &str) -> Result<(), CoreError> {
    if [
        fabric_core::Audience::Fabric.scope(),
        fabric_core::Audience::Storage.scope(),
        fabric_core::Audience::Azure.scope(),
    ]
    .contains(&scope)
    {
        Ok(())
    } else {
        Err(CoreError::new(
            ErrorKind::InvalidInput,
            "Unsupported credential scope",
        ))
    }
}

fn binding_from_token(token: &str) -> Result<Binding, CoreError> {
    if token.len() > MAX_PROVIDER_BYTES {
        return Err(unauthorized());
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(unauthorized());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| unauthorized())?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unauthorized())?;
    let binding = Binding {
        tenant: claims["tid"]
            .as_str()
            .ok_or_else(unauthorized)?
            .to_ascii_lowercase(),
        principal: claims["oid"]
            .as_str()
            .ok_or_else(unauthorized)?
            .to_ascii_lowercase(),
    };
    binding.validate()?;
    Ok(binding)
}

/// SDK process executor with bounded output and group/job termination on acquisition drop.
///
/// The Azure SDK supplies its own command. This adapter does not implement a token protocol.
/// Drop requests termination of contained descendants without awaiting a cleanup barrier.
#[derive(Debug)]
pub struct ProcessExecutor;

struct ProcessGuard {
    child: Box<dyn TokioChildWrapper>,
    finished: bool,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if !self.finished {
            // Request group/job termination, not just termination of the SDK shell.
            let _ = self.child.start_kill();
        }
    }
}

async fn read_pipe(pipe: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take((MAX_PROVIDER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_PROVIDER_BYTES {
        return Err(io::Error::other(
            "Credential process output exceeds allocation bound",
        ));
    }
    Ok(bytes)
}

#[async_trait]
impl Executor for ProcessExecutor {
    async fn run(&self, program: &OsStr, args: &[&OsStr]) -> io::Result<Output> {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut wrapped = TokioCommandWrap::from(command);
        wrapped.wrap(KillOnDrop);
        #[cfg(unix)]
        wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        wrapped.wrap(process_wrap::tokio::JobObject);
        let mut guard = ProcessGuard {
            child: wrapped.spawn()?,
            finished: false,
        };
        let stdout = guard
            .child
            .inner_mut()
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Missing process pipe"))?;
        let stderr = guard
            .child
            .inner_mut()
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("Missing process pipe"))?;
        let (stdout, stderr) = tokio::try_join!(read_pipe(stdout), read_pipe(stderr))?;
        let status = Box::into_pin(guard.child.wait()).await?;
        guard.finished = true;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

/// Sole credential source: SDK AzureCliCredential with overrides intentionally unset.
pub struct AzureCliSession {
    credential: Arc<AzureCliCredential>,
    clock: Arc<dyn Clock>,
}

impl AzureCliSession {
    /// Compose an explicitly injected SDK process executor and wall clock.
    pub fn new(executor: Arc<dyn Executor>, clock: Arc<dyn Clock>) -> Result<Self, CoreError> {
        let credential = AzureCliCredential::new(Some(AzureCliCredentialOptions {
            executor: Some(Arc::new(DiagnosticExecutor(executor))),
            ..Default::default()
        }))
        .map_err(|_| unauthorized())?;
        Ok(Self { credential, clock })
    }

    async fn acquire(
        &self,
        scope: &str,
        options: Option<TokenRequestOptions<'_>>,
    ) -> Result<(Binding, AccessToken), CoreError> {
        // Restrict the SDK's shell-bound scope to the fixed product audiences.
        validate_scope(scope)?;
        let value = self
            .credential
            .get_token(&[scope], options)
            .await
            .map_err(login_error)?;
        let binding = binding_from_token(value.token.secret())
            .map_err(|_| CoreError::new(ErrorKind::Unauthorized, LOGIN_FAILED))?;
        if token_expiry(&value)
            .map_err(|_| CoreError::new(ErrorKind::Unauthorized, LOGIN_FAILED))?
            <= self.clock.wall_now()
        {
            return Err(CoreError::new(ErrorKind::Unauthorized, LOGIN_FAILED));
        }
        Ok((binding, value))
    }

    /// Validate the current Azure CLI identity for explicit Rust login, without selecting a subscription.
    pub async fn login(&self, context: &OperationContext) -> Result<Binding, CoreError> {
        bounded_with_clock(
            self.clock.as_ref(),
            context,
            self.acquire(
                fabric_core::Audience::Fabric.scope(),
                Some(TokenRequestOptions::default()),
            ),
        )
        .await?
        .map(|(binding, _)| binding)
    }
}

type ScopeSlot = Arc<Mutex<Option<AccessToken>>>;

/// Caller-owned per-scope single-flight token reuse. Tokens exist only in this object.
///
/// Detected identity drift or invalid identity metadata disables all cached scopes.
/// The caller must bound SDK acquisition using its operation cancellation/deadline;
/// dropping that future releases cache locks and requests owned group/job termination.
pub struct BoundCredential {
    session: Arc<AzureCliSession>,
    binding: Binding,
    slots: Mutex<BTreeMap<String, ScopeSlot>>,
    invalidated: AtomicBool,
}

impl fmt::Debug for BoundCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundCredential")
            .finish_non_exhaustive()
    }
}

impl BoundCredential {
    /// Bind an explicit existing selection. No acquisition or ambient discovery occurs here.
    pub fn new(session: Arc<AzureCliSession>, binding: Binding) -> Result<Self, CoreError> {
        binding.validate()?;
        Ok(Self {
            session,
            binding,
            slots: Mutex::new(BTreeMap::new()),
            invalidated: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl TokenCredential for BoundCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if self.invalidated.load(Ordering::Acquire) {
            return Err(sdk_unauthorized());
        }
        let [scope] = scopes else {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::DataConversion,
                "Exactly one supported credential scope is required",
            ));
        };
        validate_scope(scope).map_err(|_| {
            azure_core::Error::with_message(
                azure_core::error::ErrorKind::DataConversion,
                "Unsupported credential scope",
            )
        })?;
        let slot = {
            let mut slots = self.slots.lock().await;
            slots
                .entry((*scope).to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut cached = slot.lock().await;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(sdk_unauthorized());
        }
        if let Some(value) = cached.as_ref()
            && token_expiry(value)?
                .duration_since(self.session.clock.wall_now())
                .is_ok_and(|remaining| remaining > Duration::from_secs(30))
        {
            return Ok(value.clone());
        }
        let (binding, token) = match self.session.acquire(scope, options).await {
            Ok(value) => value,
            Err(_) => {
                self.invalidated.store(true, Ordering::Release);
                *cached = None;
                return Err(sdk_unauthorized());
            }
        };
        if binding != self.binding {
            self.invalidated.store(true, Ordering::Release);
            *cached = None;
            return Err(sdk_unauthorized());
        }
        if self.invalidated.load(Ordering::Acquire) {
            return Err(sdk_unauthorized());
        }
        let result = token.clone();
        *cached = Some(token);
        Ok(result)
    }
}
