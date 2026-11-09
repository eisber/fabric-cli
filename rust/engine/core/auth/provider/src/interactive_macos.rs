// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-pinned one-shot helper for the user-default legacy macOS Keychain.
//! Native operations and process-global UI policy changes belong only in the
//! separately installed protected-store helper, never in the SDK host.

use super::InteractiveError;
#[cfg(target_os = "macos")]
use crate::macos_helper::Helper;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, time::Duration};

/// Existing service identity, shared by every protected-store operation.
#[doc(hidden)]
pub const PROTECTED_SERVICE: &str = "Microsoft.Fabric.Cli.Rust.DeviceCode.v1";
/// Complete encoded stdin/stdout limit, including base64 and schema overhead.
#[doc(hidden)]
pub const PROTECTED_WIRE_LIMIT: usize = 352 * 1024;
const SECRET_LIMIT: usize = 256 * 1024;
const SCHEMA: &str = "fabric-protected-store-v1";

/// Trusted, root-custodied helper installation captured by the SDK caller.
///
/// Construction is inert. The digest must come from an independent trusted
/// installer/administrator policy, not from hashing a discovered executable.
/// There is no environment, PATH, home-directory or certificate-feature fallback.
///
/// Attach this configuration using [`super::OsSessionStore::with_macos_helper`].
/// Every read, upsert and deletion then runs in a separate one-shot process,
/// preserving the user-default legacy Keychain service/account identity. Only
/// that child changes Security.framework's process-global interaction policy.
/// The executable and every ancestor must be root-owned, nonsymlinked and not
/// group/world writable; each execution verifies the supplied digest and custody.
///
/// Operations use the caller's Tokio runtime and blocking pool, with two bounded
/// protected-store slots. Cancellation/deadline stop waiting, not native rollback:
/// the blocking owner still kills/reaps its exact child and joins pipe workers.
/// Unconfirmed cleanup quarantines its slot. Trust, protocol, native, capacity,
/// deadline and cleanup failures are sanitized as [`InteractiveError::Store`].
#[derive(Clone)]
pub struct MacProtectedStoreHelper {
    #[cfg(target_os = "macos")]
    pub(crate) inner: Helper,
}

impl MacProtectedStoreHelper {
    /// Captures an absolute helper path, independent SHA-256 and finite deadline.
    ///
    /// # Errors
    /// Returns [`InteractiveError::Configuration`] for nonabsolute/parent-traversing paths,
    /// control characters or deadlines outside (0, 120s] on macOS.
    /// Returns `Unsupported` off macOS without accessing the path or any store.
    pub fn new(
        executable: PathBuf,
        sha256: [u8; 32],
        deadline: Duration,
    ) -> Result<Self, InteractiveError> {
        #[cfg(target_os = "macos")]
        return Ok(Self {
            inner: Helper::new(executable, sha256, deadline)
                .map_err(|_| InteractiveError::Configuration)?,
        });
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (executable, sha256, deadline);
            Err(InteractiveError::Unsupported)
        }
    }
}

impl fmt::Debug for MacProtectedStoreHelper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacProtectedStoreHelper")
            .finish_non_exhaustive()
    }
}

/// Closed helper request. Secrets are permitted only in bounded stdio pipes.
#[doc(hidden)]
#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtectedRequest {
    /// Read the exact service/account from the user-default legacy store.
    Read {
        /// Protocol family and revision, distinct from certificate signing.
        schema: String,
        /// Lowercase SHA-256 configuration-root locator.
        locator: String,
    },
    /// Preserve the existing legacy upsert behavior.
    Upsert {
        /// Protocol family and revision.
        schema: String,
        /// Exact account locator.
        locator: String,
        /// Canonical unpadded base64url protected bytes.
        secret: String,
    },
    /// Delete the exact service/account, preserving native error status.
    Delete {
        /// Protocol family and revision.
        schema: String,
        /// Exact account locator.
        locator: String,
    },
}

impl ProtectedRequest {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn read(locator: &str) -> Self {
        Self::Read {
            schema: SCHEMA.into(),
            locator: locator.into(),
        }
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn upsert(locator: &str, bytes: &[u8]) -> Self {
        Self::Upsert {
            schema: SCHEMA.into(),
            locator: locator.into(),
            secret: URL_SAFE_NO_PAD.encode(bytes),
        }
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn delete(locator: &str) -> Self {
        Self::Delete {
            schema: SCHEMA.into(),
            locator: locator.into(),
        }
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn encode(&self) -> Result<Vec<u8>, InteractiveError> {
        self.validate().map_err(|_| InteractiveError::Store)?;
        let bytes = serde_json::to_vec(self).map_err(|_| InteractiveError::Store)?;
        if bytes.len() > PROTECTED_WIRE_LIMIT {
            return Err(InteractiveError::Store);
        }
        Ok(bytes)
    }

    /// Validates the complete closed request before any native API is called.
    ///
    /// # Errors
    /// Rejects duplicates, unknown fields, other protocols, aliases and bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtectedError> {
        if bytes.is_empty() || bytes.len() > PROTECTED_WIRE_LIMIT {
            return Err(ProtectedError::InvalidInput);
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| ProtectedError::InvalidInput)?;
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), ProtectedError> {
        let (schema, locator) = match self {
            Self::Read { schema, locator }
            | Self::Delete { schema, locator }
            | Self::Upsert {
                schema, locator, ..
            } => (schema, locator),
        };
        if schema != SCHEMA
            || locator.len() != 64
            || !locator
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ProtectedError::InvalidInput);
        }
        if let Self::Upsert { secret, .. } = self {
            decode_secret(secret)?;
        }
        Ok(())
    }

    /// Executes one previously validated operation against an explicit backend.
    /// The helper establishes no-UI process policy before constructing a backend.
    pub fn execute(&self, backend: &impl ProtectedBackend) -> ProtectedReply {
        if let Err(code) = self.validate() {
            return ProtectedReply::error(code);
        }
        let result = match self {
            Self::Read { locator, .. } => {
                backend
                    .read(locator)
                    .map_err(native_error)
                    .and_then(|bytes| {
                        if bytes.is_empty() || bytes.len() > SECRET_LIMIT {
                            Err(ProtectedError::Unavailable)
                        } else {
                            Ok(ProtectedReply::Read {
                                schema: SCHEMA.into(),
                                secret: URL_SAFE_NO_PAD.encode(bytes),
                            })
                        }
                    })
            }
            Self::Upsert {
                locator, secret, ..
            } => decode_secret(secret)
                .and_then(|bytes| backend.upsert(locator, &bytes).map_err(native_error))
                .map(|()| ProtectedReply::Done {
                    schema: SCHEMA.into(),
                }),
            Self::Delete { locator, .. } => match backend.delete(locator) {
                Ok(()) | Err(-25300) => Ok(ProtectedReply::Done {
                    schema: SCHEMA.into(),
                }),
                Err(code) => Err(native_error(code)),
            },
        };
        result.unwrap_or_else(ProtectedReply::error)
    }
}

fn decode_secret(secret: &str) -> Result<Vec<u8>, ProtectedError> {
    if secret.is_empty() || secret.len() > SECRET_LIMIT.div_ceil(3) * 4 {
        return Err(ProtectedError::InvalidInput);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| ProtectedError::InvalidInput)?;
    if bytes.is_empty() || bytes.len() > SECRET_LIMIT || URL_SAFE_NO_PAD.encode(&bytes) != secret {
        return Err(ProtectedError::InvalidInput);
    }
    Ok(bytes)
}

fn native_error(code: i32) -> ProtectedError {
    if code == -25300 {
        ProtectedError::NotFound
    } else {
        ProtectedError::Unavailable
    }
}

/// Native status-preserving boundary; no native description or error chain.
#[doc(hidden)]
pub trait ProtectedBackend {
    /// Reads the exact fixed service and supplied account.
    ///
    /// # Errors
    /// Returns only the native numeric status.
    fn read(&self, locator: &str) -> Result<Vec<u8>, i32>;
    /// Upserts the exact fixed service and supplied account.
    ///
    /// # Errors
    /// Returns only the native numeric status.
    fn upsert(&self, locator: &str, bytes: &[u8]) -> Result<(), i32>;
    /// Deletes the exact fixed service and supplied account.
    ///
    /// # Errors
    /// Must retain native failure status; only -25300 is idempotent.
    fn delete(&self, locator: &str) -> Result<(), i32>;
}

/// Sanitized closed protocol failures, never native text or supplied values.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedError {
    /// Invalid or other-family request.
    InvalidInput,
    /// Exact native item-not-found result.
    NotFound,
    /// Access denied, interaction required or another native failure.
    Unavailable,
    /// Native helper operations are not supported on this OS.
    UnsupportedPlatform,
}

/// Operation-specific replies prevent a write acknowledgement passing as a read.
#[doc(hidden)]
#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtectedReply {
    /// Bounded protected bytes.
    Read {
        /// Exact family/revision.
        schema: String,
        /// Canonical unpadded base64url.
        secret: String,
    },
    /// Completed upsert or delete.
    Done {
        /// Exact family/revision.
        schema: String,
    },
    /// Sanitized error; never interpreted as an idempotent deletion by the host.
    Error {
        /// Exact family/revision.
        schema: String,
        /// Closed error code.
        code: ProtectedError,
    },
}

impl ProtectedReply {
    /// Creates a finite, sanitized error response.
    pub fn error(code: ProtectedError) -> Self {
        Self::Error {
            schema: SCHEMA.into(),
            code,
        }
    }

    /// Encodes exactly one finite reply.
    ///
    /// # Errors
    /// Rejects responses exceeding the complete wire limit.
    pub fn encode(&self) -> Result<Vec<u8>, ProtectedError> {
        let schema = match self {
            Self::Read { schema, secret } => {
                decode_secret(secret)?;
                schema
            }
            Self::Done { schema } | Self::Error { schema, .. } => schema,
        };
        if schema != SCHEMA {
            return Err(ProtectedError::InvalidInput);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| ProtectedError::Unavailable)?;
        if bytes.len() > PROTECTED_WIRE_LIMIT {
            return Err(ProtectedError::Unavailable);
        }
        Ok(bytes)
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn verify(
        bytes: &[u8],
        request: &ProtectedRequest,
    ) -> Result<Vec<u8>, InteractiveError> {
        if bytes.is_empty() || bytes.len() > PROTECTED_WIRE_LIMIT {
            return Err(InteractiveError::Store);
        }
        let reply: Self = serde_json::from_slice(bytes).map_err(|_| InteractiveError::Store)?;
        match (reply, request) {
            (Self::Read { schema, secret }, ProtectedRequest::Read { .. }) if schema == SCHEMA => {
                decode_secret(&secret).map_err(|_| InteractiveError::Store)
            }
            (
                Self::Done { schema },
                ProtectedRequest::Upsert { .. } | ProtectedRequest::Delete { .. },
            ) if schema == SCHEMA => Ok(Vec::new()),
            _ => Err(InteractiveError::Store),
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn exchange(
    helper: &MacProtectedStoreHelper,
    operation: &super::store::MacOperation,
    request: ProtectedRequest,
) -> Result<Vec<u8>, InteractiveError> {
    use std::sync::{Arc, OnceLock};
    use tokio::sync::Semaphore;
    static CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = CAPACITY
        .get_or_init(|| Arc::new(Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| InteractiveError::Store)?;
    let output = crate::macos_helper::trusted_blocking_exchange(
        &helper.inner,
        request.encode()?,
        operation.deadline,
        operation.cancelled.clone(),
        permit,
        PROTECTED_WIRE_LIMIT,
    )
    .map_err(|_| InteractiveError::Store)?;
    ProtectedReply::verify(&output, &request)
}

#[cfg(test)]
#[path = "interactive_macos_tests.rs"]
mod tests;
