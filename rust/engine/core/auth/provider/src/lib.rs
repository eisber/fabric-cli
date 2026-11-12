// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Current-context Azure CLI authentication. No fallback, context mutation, or disk tokens.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use azure_core::credentials::TokenCredential as AzureTokenCredential;
use azure_identity::{AzureCliCredential, AzureCliCredentialOptions, process::Executor};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fabric_core::{AccessToken, Clock, CoreError, ErrorKind, OperationContext, TokenCredential};
use process_wrap::tokio::{KillOnDrop, TokioChildWrapper, TokioCommandWrap};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
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
        context: &OperationContext,
    ) -> Result<(Binding, AccessToken), CoreError> {
        // Restrict the SDK's shell-bound scope to the fixed product audiences.
        validate_scope(scope)?;
        let value = bounded(context, self.credential.get_token(&[scope], None))
            .await?
            .map_err(|_| unauthorized())?;
        let binding = Binding::from_token(value.token.secret())?;
        let seconds =
            u64::try_from(value.expires_on.unix_timestamp()).map_err(|_| unauthorized())?;
        let expires_on = UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or_else(unauthorized)?;
        if expires_on <= self.clock.wall_now() {
            return Err(unauthorized());
        }
        Ok((
            binding,
            AccessToken {
                token: value.token.secret().to_owned(),
                expires_on,
            },
        ))
    }

    /// Validate the current Azure CLI identity for explicit Rust login, without selecting a subscription.
    pub async fn login(&self, context: &OperationContext) -> Result<Binding, CoreError> {
        self.acquire(fabric_core::Audience::Fabric.scope(), context)
            .await
            .map(|(binding, _)| binding)
    }
}

type ScopeSlot = Arc<Mutex<Option<AccessToken>>>;

/// Caller-owned per-scope single-flight token reuse. Tokens exist only in this object.
///
/// Detected identity drift or invalid identity metadata disables all cached scopes.
pub struct BoundCredential {
    session: Arc<AzureCliSession>,
    binding: Binding,
    slots: Mutex<BTreeMap<String, ScopeSlot>>,
    invalidated: AtomicBool,
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
    async fn token(
        &self,
        scope: &str,
        context: &OperationContext,
    ) -> Result<AccessToken, CoreError> {
        context.remaining()?;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(unauthorized());
        }
        validate_scope(scope)?;
        let slot = {
            let mut slots = bounded(context, self.slots.lock()).await?;
            slots
                .entry(scope.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut cached = bounded(context, slot.lock()).await?;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(unauthorized());
        }
        if let Some(value) = cached.as_ref()
            && value
                .expires_on
                .duration_since(self.session.clock.wall_now())
                .is_ok_and(|remaining| remaining > Duration::from_secs(30))
        {
            return Ok(AccessToken {
                token: value.token.clone(),
                expires_on: value.expires_on,
            });
        }
        let (binding, token) = match self.session.acquire(scope, context).await {
            Ok(value) => value,
            Err(error) => {
                if error.kind == ErrorKind::Unauthorized {
                    self.invalidated.store(true, Ordering::Release);
                }
                return Err(error);
            }
        };
        if binding != self.binding {
            self.invalidated.store(true, Ordering::Release);
            *cached = None;
            return Err(unauthorized());
        }
        if self.invalidated.load(Ordering::Acquire) {
            return Err(unauthorized());
        }
        let result = AccessToken {
            token: token.token.clone(),
            expires_on: token.expires_on,
        };
        *cached = Some(token);
        Ok(result)
    }
}
