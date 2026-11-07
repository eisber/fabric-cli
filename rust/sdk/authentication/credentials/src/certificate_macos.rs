// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit macOS Keychain signing through a pinned, one-shot helper process.
//!
//! This library never changes Security.framework's process-global UI policy.
//! Only the separately installed helper accesses the Keychain. Its executable
//! and every ancestor must be root-owned, nonsymlinked and not group/world
//! writable. The caller supplies its trusted installation digest; there is no
//! PATH, home-directory, developer-build or environment fallback.

use super::HeadlessError;
use super::certificate::{CertificateError, CertificateSigner};
use crate::macos_helper::{
    Helper, HelperError, OwnedHelperChild, run_owned_exchange, validate_helper,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
    signature::{RSA_PSS_2048_8192_SHA256, RsaPublicKeyComponents},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
    time::{Instant, sleep_until},
};
use x509_parser::{
    prelude::{FromDer, X509Certificate},
    public_key::{PublicKey, RSAPublicKey},
};

/// Maximum complete request or response on the helper's stdio protocol.
pub const MAC_HELPER_WIRE_LIMIT: usize = 64 * 1024;
/// Maximum decoded public certificate DER, additionally bounded by wire size.
pub const MAC_CERTIFICATE_DER_LIMIT: usize = 64 * 1024;
const SIGNING_INPUT_LIMIT: usize = 64 * 1024;
const WORKER_COUNT: usize = 2;

impl From<HelperError> for MacCertificateError {
    fn from(error: HelperError) -> Self {
        match error {
            HelperError::InvalidInput => Self::InvalidInput,
            #[cfg(target_os = "macos")]
            HelperError::HelperTrust => Self::HelperTrust,
            HelperError::Helper => Self::Helper,
            HelperError::Cancelled => Self::Cancelled,
            HelperError::Deadline => Self::Deadline,
            HelperError::CleanupPending => Self::CleanupPending,
            #[cfg(not(target_os = "macos"))]
            HelperError::UnsupportedPlatform => Self::UnsupportedPlatform,
        }
    }
}

/// Sanitized helper and native failures; never contains OS diagnostics or paths.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum MacCertificateError {
    /// Invalid explicit metadata or a malformed closed-schema request.
    #[error("invalid macOS certificate configuration or protocol")]
    InvalidInput,
    /// No exact, unique certificate identity was established.
    #[error("the explicit macOS certificate selection was not established")]
    Selection,
    /// A complete search cannot be established within the identity limit.
    #[error("the macOS identity search exceeded its bounds")]
    SearchLimit,
    /// A required native policy attribute is missing or has the wrong type.
    #[error("macOS key policy could not be established")]
    PolicyUnavailable,
    /// The native key explicitly permits private-key extraction.
    #[error("the macOS key is exportable")]
    ExportableKey,
    /// The identity does not support the required RSA/PSS contract.
    #[error("the macOS key does not support the signing contract")]
    UnsupportedKey,
    /// The helper cannot access the identity without interaction.
    #[error("the macOS identity requires interaction or is inaccessible")]
    InteractionRequired,
    /// The caller's pinned helper installation could not be established.
    #[error("the trusted macOS helper installation was not established")]
    HelperTrust,
    /// The finite protocol or process failed.
    #[error("the macOS certificate helper failed")]
    Helper,
    /// The supplied certificate cannot verify the exact signature.
    #[error("the macOS certificate signature is invalid")]
    Signature,
    /// Both independently bounded helper slots are occupied.
    #[error("macOS certificate signing capacity is exhausted")]
    Capacity,
    /// The caller stopped waiting; this does not assert native rollback.
    #[error("macOS certificate signing was cancelled")]
    Cancelled,
    /// The operation deadline expired.
    #[error("macOS certificate signing exceeded its deadline")]
    Deadline,
    /// Cleanup could not be confirmed; unconfirmed resources quarantine capacity.
    #[error("macOS certificate helper cleanup could not be confirmed")]
    CleanupPending,
    /// This native backend is unavailable on the current operating system.
    #[error("macOS certificate stores are unavailable on this platform")]
    UnsupportedPlatform,
}

fn headless(error: MacCertificateError) -> HeadlessError {
    let error = match error {
        MacCertificateError::InvalidInput => return HeadlessError::Configuration,
        MacCertificateError::Selection => CertificateError::Selection,
        MacCertificateError::SearchLimit => CertificateError::SearchLimit,
        MacCertificateError::PolicyUnavailable | MacCertificateError::ExportableKey => {
            CertificateError::Policy
        }
        MacCertificateError::UnsupportedKey => CertificateError::UnsupportedKey,
        MacCertificateError::InteractionRequired => CertificateError::InteractionRequired,
        MacCertificateError::HelperTrust => CertificateError::Module,
        MacCertificateError::Signature => CertificateError::Signature,
        MacCertificateError::Capacity => CertificateError::Capacity,
        MacCertificateError::Cancelled => CertificateError::Cancelled,
        MacCertificateError::Deadline | MacCertificateError::CleanupPending => {
            CertificateError::CleanupPending
        }
        MacCertificateError::Helper => CertificateError::Provider,
        MacCertificateError::UnsupportedPlatform => return HeadlessError::Unavailable,
    };
    HeadlessError::Certificate(error)
}

/// An exact absolute macOS Keychain path and SHA-256 of the entire certificate DER.
///
/// The path is stored as macOS syntax, including in cross-platform protocol tests.
/// Construction is inert and never opens a keychain or resolves a default identity.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MacCertificateSelector {
    keychain_path: String,
    certificate_sha256: [u8; 32],
}

impl MacCertificateSelector {
    /// Captures an explicit selection without any filesystem or native access.
    ///
    /// # Errors
    /// Rejects relative, control-containing, overlong and noncanonical lexical paths.
    pub fn new(
        keychain_path: String,
        certificate_sha256: [u8; 32],
    ) -> Result<Self, MacCertificateError> {
        let selection = Self {
            keychain_path,
            certificate_sha256,
        };
        selection.validate()?;
        Ok(selection)
    }

    fn validate(&self) -> Result<(), MacCertificateError> {
        let path = &self.keychain_path;
        if path.len() > 4096
            || !path.starts_with('/')
            || path.chars().any(char::is_control)
            || path[1..]
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(MacCertificateError::InvalidInput);
        }
        Ok(())
    }

    /// Returns the explicitly selected macOS path, never a discovered default.
    pub fn keychain_path(&self) -> &str {
        &self.keychain_path
    }

    /// Returns the immutable SHA-256 DER fingerprint.
    pub fn certificate_sha256(&self) -> [u8; 32] {
        self.certificate_sha256
    }
}

impl fmt::Debug for MacCertificateSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacCertificateSelector")
            .finish_non_exhaustive()
    }
}

/// Caller-pinned, root-custodied helper installation and local operation deadline.
#[derive(Clone)]
pub struct MacKeychainHelper {
    inner: Helper,
}

impl MacKeychainHelper {
    /// Captures trusted installation metadata without reading or executing a file.
    ///
    /// The digest must come from the caller's trusted administrator/installer
    /// policy, not from hashing an arbitrary discovered executable at runtime.
    /// Root modification of the executable or loader dependencies is outside
    /// this contract. No caller-supplied environment is inherited by the helper.
    ///
    /// # Errors
    /// Rejects nonabsolute/noncanonical paths and deadlines outside (0, 120] seconds.
    pub fn new(
        executable: PathBuf,
        sha256: [u8; 32],
        deadline: Duration,
    ) -> Result<Self, MacCertificateError> {
        Ok(Self {
            inner: Helper::new(executable, sha256, deadline)?,
        })
    }
}

impl fmt::Debug for MacKeychainHelper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacKeychainHelper").finish_non_exhaustive()
    }
}

/// Native-store signer with no SDK-process Security.framework calls or UI mutation.
#[derive(Clone)]
pub struct MacStoreSigner {
    selector: MacCertificateSelector,
    helper: MacKeychainHelper,
}

impl MacStoreSigner {
    /// Binds an inert explicit selector to an explicitly trusted helper installation.
    pub fn new(selector: MacCertificateSelector, helper: MacKeychainHelper) -> Self {
        Self { selector, helper }
    }

    /// Returns a public immutable batch binding without revealing installation paths.
    pub fn binding(&self) -> String {
        let mut bytes = self.selector.keychain_path.as_bytes().to_vec();
        bytes.push(0);
        bytes.extend_from_slice(&self.selector.certificate_sha256);
        bytes.extend_from_slice(self.helper.inner.executable.as_os_str().as_encoded_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&self.helper.inner.sha256);
        format!(
            "macos-keychain:{}",
            URL_SAFE_NO_PAD.encode(digest(&SHA256, &bytes))
        )
    }
}

impl fmt::Debug for MacStoreSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacStoreSigner").finish_non_exhaustive()
    }
}

#[async_trait]
impl CertificateSigner for MacStoreSigner {
    fn certificate_sha256(&self) -> [u8; 32] {
        self.selector.certificate_sha256
    }

    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError> {
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| headless(MacCertificateError::Helper))?;
        Ok(nonce)
    }

    async fn sign_ps256(&self, signing_input: &[u8]) -> Result<Vec<u8>, HeadlessError> {
        if !cfg!(target_os = "macos") {
            return Err(headless(MacCertificateError::UnsupportedPlatform));
        }
        let request = MacHelperRequest::new(self.selector.clone(), signing_input)
            .and_then(|request| request.encode())
            .map_err(headless)?;
        tokio::runtime::Handle::try_current().map_err(|_| HeadlessError::Unavailable)?;
        let permit = capacity()
            .try_acquire_owned()
            .map_err(|_| headless(MacCertificateError::Capacity))?;
        let (publication, result) = oneshot::channel();
        let selector = self.selector.clone();
        let helper = self.helper.clone();
        let input = signing_input.to_vec();
        let deadline = Instant::now() + helper.inner.deadline;
        // This owned task is deliberately not aborted when the caller drops its
        // future. It observes receiver closure and retains capacity through reap.
        tokio::spawn(async move {
            let mut publication = publication;
            let outcome = run_worker(
                &selector,
                helper,
                input,
                request,
                deadline,
                &mut publication,
                permit,
            )
            .await;
            let _ = publication.send(outcome);
        });
        result
            .await
            .map_err(|_| headless(MacCertificateError::CleanupPending))?
            .map_err(headless)
    }
}

fn capacity() -> Arc<Semaphore> {
    static CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CAPACITY
        .get_or_init(|| Arc::new(Semaphore::new(WORKER_COUNT)))
        .clone()
}

/// Closed schema for one finite request; never transported through argv.
#[doc(hidden)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MacHelperRequest {
    schema: u8,
    selector: MacCertificateSelector,
    signing_input: String,
}

impl MacHelperRequest {
    /// Creates a finite request without native access.
    ///
    /// # Errors
    /// Rejects empty/overlong message bytes or invalid explicit selection.
    pub fn new(
        selector: MacCertificateSelector,
        input: &[u8],
    ) -> Result<Self, MacCertificateError> {
        selector.validate()?;
        if input.is_empty() || input.len() > SIGNING_INPUT_LIMIT {
            return Err(MacCertificateError::InvalidInput);
        }
        Ok(Self {
            schema: 1,
            selector,
            signing_input: URL_SAFE_NO_PAD.encode(input),
        })
    }

    /// Parses and validates exactly one request before any Keychain access.
    ///
    /// # Errors
    /// Rejects unknown/duplicate keys, trailing data, invalid schema and bounds.
    pub fn decode(bytes: &[u8]) -> Result<(MacCertificateSelector, Vec<u8>), MacCertificateError> {
        if bytes.is_empty() || bytes.len() > MAC_HELPER_WIRE_LIMIT {
            return Err(MacCertificateError::InvalidInput);
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| MacCertificateError::InvalidInput)?;
        request.selector.validate()?;
        if request.schema != 1 {
            return Err(MacCertificateError::InvalidInput);
        }
        let input = decode_base64(&request.signing_input, SIGNING_INPUT_LIMIT)
            .map_err(|_| MacCertificateError::InvalidInput)?;
        Ok((request.selector, input))
    }

    fn encode(&self) -> Result<Vec<u8>, MacCertificateError> {
        let bytes = serde_json::to_vec(self).map_err(|_| MacCertificateError::InvalidInput)?;
        if bytes.len() > MAC_HELPER_WIRE_LIMIT {
            return Err(MacCertificateError::InvalidInput);
        }
        Ok(bytes)
    }
}

/// Closed, sanitized helper reply; public certificate/signature bytes only.
#[doc(hidden)]
#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MacHelperReply {
    /// A certificate-verified signature over the exact request message.
    Ok {
        /// Protocol revision.
        schema: u8,
        /// Complete public certificate DER, unpadded base64url.
        certificate: String,
        /// RSA/PSS signature, unpadded base64url.
        signature: String,
    },
    /// A static error code without native descriptions or supplied values.
    Error {
        /// Protocol revision.
        schema: u8,
        /// Sanitized enumerated failure.
        code: MacCertificateError,
    },
}

impl MacHelperReply {
    /// Encodes exactly one finite reply.
    ///
    /// # Errors
    /// Rejects replies larger than the aggregate protocol bound.
    pub fn encode(&self) -> Result<Vec<u8>, MacCertificateError> {
        let bytes = serde_json::to_vec(self).map_err(|_| MacCertificateError::Helper)?;
        if bytes.len() > MAC_HELPER_WIRE_LIMIT {
            return Err(MacCertificateError::Helper);
        }
        Ok(bytes)
    }

    fn verify(
        bytes: &[u8],
        selector: &MacCertificateSelector,
        input: &[u8],
    ) -> Result<Vec<u8>, MacCertificateError> {
        if bytes.is_empty() || bytes.len() > MAC_HELPER_WIRE_LIMIT {
            return Err(MacCertificateError::Helper);
        }
        match serde_json::from_slice(bytes).map_err(|_| MacCertificateError::Helper)? {
            Self::Ok {
                schema: 1,
                certificate,
                signature,
            } => {
                let der = decode_base64(&certificate, MAC_CERTIFICATE_DER_LIMIT)?;
                let signature = decode_base64(&signature, 512)?;
                verify_mac_public_signature(&der, &selector.certificate_sha256, input, &signature)?;
                Ok(signature)
            }
            Self::Error { schema: 1, code } => Err(code),
            _ => Err(MacCertificateError::Helper),
        }
    }
}

fn decode_base64(value: &str, limit: usize) -> Result<Vec<u8>, MacCertificateError> {
    if value.is_empty() || value.len() > limit.div_ceil(3) * 4 {
        return Err(MacCertificateError::Helper);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| MacCertificateError::Helper)?;
    if bytes.is_empty() || bytes.len() > limit || URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(MacCertificateError::Helper);
    }
    Ok(bytes)
}

/// Fails closed unless both native policy facts are explicitly known booleans.
///
/// `None` means unavailable/wrong-typed, never false or a guessed permission.
///
/// # Errors
/// Rejects unavailable attributes, exportable keys and keys unable to sign.
#[doc(hidden)]
pub fn validate_mac_key_policy(
    extractable: Option<bool>,
    can_sign: Option<bool>,
) -> Result<(), MacCertificateError> {
    match (extractable, can_sign) {
        (None, _) | (_, None) => Err(MacCertificateError::PolicyUnavailable),
        (Some(true), _) => Err(MacCertificateError::ExportableKey),
        (Some(false), Some(false)) => Err(MacCertificateError::UnsupportedKey),
        (Some(false), Some(true)) => Ok(()),
    }
}

struct CertificatePublicKey {
    modulus: Vec<u8>,
    exponent: Vec<u8>,
    bits: usize,
}

impl CertificatePublicKey {
    fn parse(der: &[u8], expected: &[u8; 32]) -> Result<Self, MacCertificateError> {
        if der.is_empty()
            || der.len() > MAC_CERTIFICATE_DER_LIMIT
            || digest(&SHA256, der).as_ref() != expected
        {
            return Err(MacCertificateError::Selection);
        }
        let (remaining, certificate) =
            X509Certificate::from_der(der).map_err(|_| MacCertificateError::Selection)?;
        if !remaining.is_empty() || !certificate.validity().is_valid() {
            return Err(MacCertificateError::Selection);
        }
        let usage = certificate
            .key_usage()
            .map_err(|_| MacCertificateError::UnsupportedKey)?;
        if usage.is_some_and(|usage| !usage.value.digital_signature()) {
            return Err(MacCertificateError::UnsupportedKey);
        }
        let PublicKey::RSA(key) = certificate
            .public_key()
            .parsed()
            .map_err(|_| MacCertificateError::UnsupportedKey)?
        else {
            return Err(MacCertificateError::UnsupportedKey);
        };
        let public_bits = &certificate.public_key().subject_public_key;
        let (remaining, _) = RSAPublicKey::from_der(public_bits.data.as_ref())
            .map_err(|_| MacCertificateError::UnsupportedKey)?;
        if !remaining.is_empty() || public_bits.unused_bits != 0 {
            return Err(MacCertificateError::UnsupportedKey);
        }
        let modulus = unsigned_integer(key.modulus)?;
        let exponent = unsigned_integer(key.exponent)?;
        let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
        if !(2048..=4096).contains(&bits) || exponent.len() > 8 {
            return Err(MacCertificateError::UnsupportedKey);
        }
        Ok(Self {
            modulus: modulus.to_vec(),
            exponent: exponent.to_vec(),
            bits,
        })
    }

    fn verify(&self, input: &[u8], signature: &[u8]) -> Result<(), MacCertificateError> {
        if input.is_empty()
            || input.len() > SIGNING_INPUT_LIMIT
            || signature.len() != self.bits.div_ceil(8)
        {
            return Err(MacCertificateError::Signature);
        }
        RsaPublicKeyComponents {
            n: self.modulus.as_slice(),
            e: self.exponent.as_slice(),
        }
        .verify(&RSA_PSS_2048_8192_SHA256, input, signature)
        .map_err(|_| MacCertificateError::Signature)
    }
}

fn unsigned_integer(bytes: &[u8]) -> Result<&[u8], MacCertificateError> {
    let Some(&first) = bytes.first() else {
        return Err(MacCertificateError::UnsupportedKey);
    };
    if first & 0x80 != 0 {
        return Err(MacCertificateError::UnsupportedKey);
    }
    if first == 0 {
        if bytes.len() < 2 || bytes[1] & 0x80 == 0 {
            return Err(MacCertificateError::UnsupportedKey);
        }
        Ok(&bytes[1..])
    } else {
        Ok(bytes)
    }
}

/// Validates the pinned public certificate and returns its RSA modulus bit size.
///
/// # Errors
/// Rejects malformed, expired, wrong-fingerprint, non-RSA and out-of-policy keys.
#[doc(hidden)]
pub fn mac_certificate_rsa_bits(
    der: &[u8],
    expected: &[u8; 32],
) -> Result<usize, MacCertificateError> {
    CertificatePublicKey::parse(der, expected).map(|key| key.bits)
}

/// Independently verifies PS256 using only the selected public certificate.
///
/// Verification runs in both processes. Ring enforces SHA-256/MGF1-SHA-256 and
/// the SHA-256-sized PSS salt; a helper's claim of success is not sufficient.
///
/// # Errors
/// Rejects selection, certificate policy, wrong message/key/salt or signature.
#[doc(hidden)]
pub fn verify_mac_public_signature(
    der: &[u8],
    expected: &[u8; 32],
    input: &[u8],
    signature: &[u8],
) -> Result<(), MacCertificateError> {
    CertificatePublicKey::parse(der, expected)?.verify(input, signature)
}

type Publication = oneshot::Sender<Result<Vec<u8>, MacCertificateError>>;

async fn run_worker(
    selector: &MacCertificateSelector,
    helper: MacKeychainHelper,
    input: Vec<u8>,
    request: Vec<u8>,
    deadline: Instant,
    publication: &mut Publication,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, MacCertificateError> {
    let executable = helper.inner.executable.clone();
    // Hashing is bounded and not performed on the caller's async executor.
    // The blocking closure owns the slot even if validation waiting is cancelled.
    let validated = tokio::task::spawn_blocking(move || {
        let result = validate_helper(&helper.inner).map_err(MacCertificateError::from);
        (permit, result)
    });
    let (permit, trust) = tokio::select! {
        biased;
        _ = publication.closed() => return Err(MacCertificateError::Cancelled),
        _ = sleep_until(deadline) => return Err(MacCertificateError::Deadline),
        result = validated => result.map_err(|_| MacCertificateError::Helper)?,
    };
    trust?;
    if publication.is_closed() || Instant::now() >= deadline {
        return Err(MacCertificateError::Cancelled);
    }
    let mut command = Command::new(executable);
    command.env_clear().current_dir(Path::new("/"));
    let output = run_owned_exchange(
        command,
        request,
        deadline,
        publication,
        permit,
        OwnedHelperChild::start_pipes,
    )
    .await?;
    if publication.is_closed() || Instant::now() >= deadline {
        return Err(MacCertificateError::Cancelled);
    }
    MacHelperReply::verify(&output, selector, &input)
}

#[cfg(test)]
#[path = "certificate_macos_tests.rs"]
mod tests;
