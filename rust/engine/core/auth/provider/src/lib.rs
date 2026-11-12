// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Current-context Azure CLI authentication. No fallback, context mutation, or disk tokens.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use azure_identity::{AzureCliCredential, AzureCliCredentialOptions, process::Executor};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fabric_core::{Clock, CoreError, ErrorKind, OperationContext};
use process_wrap::tokio::{KillOnDrop, TokioChildWrapper, TokioCommandWrap};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt,
    future::Future,
    io,
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
const LOGIN_REQUIRED: &str = "Azure CLI identity unavailable or changed. Install Azure CLI 2.54+ and run az login, then fab-rs auth login";

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

/// Non-secret identity consistency metadata, not cryptographic token verification.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Entra tenant object ID.
    pub tenant: String,
    /// Entra principal object ID. Subscription is deliberately not selected.
    pub principal: String,
}

impl Binding {
    /// Validate persisted metadata without accepting incomplete bindings.
    pub fn validate(&self) -> Result<(), CoreError> {
        if valid_id(&self.tenant) && valid_id(&self.principal) {
            Ok(())
        } else {
            Err(unauthorized())
        }
    }

    fn from_token(token: &str) -> Result<Self, CoreError> {
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
        let claims: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| unauthorized())?;
        let binding = Self {
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
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

async fn bounded<T>(
    context: &OperationContext,
    future: impl Future<Output = T>,
) -> Result<T, CoreError> {
    let remaining = context.remaining()?;
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => Err(CoreError::new(ErrorKind::Cancelled, "Credential acquisition cancelled locally")),
        () = tokio::time::sleep(remaining) => Err(CoreError::new(ErrorKind::Timeout, "Credential acquisition deadline reached")),
        result = future => { context.remaining()?; Ok(result) }
    }
}

/// SDK process executor. Dropping acquisition terminates the owned child; output is bounded.
///
/// The Azure SDK supplies its own command. This adapter does not implement a token protocol.
#[derive(Debug)]
pub struct ProcessExecutor;

struct ProcessGuard {
    child: Box<dyn TokioChildWrapper>,
    finished: bool,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if !self.finished {
            // Kill the entire owned process group/job, not just the SDK's shell.
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
            executor: Some(executor),
            ..Default::default()
        }))
        .map_err(|_| unauthorized())?;
        Ok(Self { credential, clock })
    }

    async fn acquire(
        &self,
        scope: &str,
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<(Binding, AccessToken)> {
        // Restrict the SDK's shell-bound scope to the fixed product audiences.
        validate_scope(scope).map_err(|_| sdk_unauthorized())?;
        let value = self
            .credential
            .get_token(&[scope], options)
            .await
            .map_err(|_| sdk_unauthorized())?;
        let binding = Binding::from_token(value.token.secret()).map_err(|_| sdk_unauthorized())?;
        if token_expiry(&value)? <= self.clock.wall_now() {
            return Err(sdk_unauthorized());
        }
        Ok((binding, value))
    }

    /// Validate the current Azure CLI identity for explicit Rust login, without selecting a subscription.
    pub async fn login(&self, context: &OperationContext) -> Result<Binding, CoreError> {
        bounded(
            context,
            self.acquire(
                fabric_core::Audience::Fabric.scope(),
                Some(TokenRequestOptions::default()),
            ),
        )
        .await?
        .map(|(binding, _)| binding)
        .map_err(|_| unauthorized())
    }
}

type ScopeSlot = Arc<Mutex<Option<AccessToken>>>;

/// Caller-owned per-scope single-flight token reuse. Tokens exist only in this object.
///
/// Detected identity drift or invalid identity metadata disables all cached scopes.
/// The caller must bound SDK acquisition using its operation cancellation/deadline;
/// dropping that future releases cache locks and terminates the owned process tree.
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
            Err(error) => {
                self.invalidated.store(true, Ordering::Release);
                *cached = None;
                return Err(error);
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
