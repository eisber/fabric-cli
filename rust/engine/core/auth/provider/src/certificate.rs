// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Certificate assertions over an explicitly injected non-exporting signer.

use super::{
    ClientAssertionCredential, ClientAssertionCredentialOptions, ClientOptions, HeadlessError,
    ServiceIdentity, TokenCredential, credential_error,
};
use async_trait::async_trait;
use azure_core::{
    base64,
    http::ClientMethodOptions,
    time::{Duration, OffsetDateTime},
};
use azure_identity::ClientAssertion;
use std::{fmt, sync::Arc};

const ASSERTION_SECONDS: i64 = 300;
const SIGNING_INPUT_LIMIT: usize = 8192;

/// Sanitized certificate-specific failures, without keys, assertions or providers' text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CertificateError {
    /// The immutable public certificate selection changed or could not be resolved.
    #[error("the explicit certificate selection could not be established")]
    Selection,
    /// The native search could not establish a complete bounded result.
    #[error("the certificate search exceeded its bounds")]
    SearchLimit,
    /// The acquired key's associated provider or non-exportability policy was rejected.
    #[error("the selected certificate key does not satisfy its provider policy")]
    Policy,
    /// The associated key does not support the required native signing algorithm.
    #[error("the selected certificate key does not support the required signing contract")]
    UnsupportedKey,
    /// The key requires UI, which unattended acquisition never permits.
    #[error("the selected certificate key requires user interaction")]
    InteractionRequired,
    /// Independent certificate-only native capacity is occupied.
    #[error("certificate signing capacity is exhausted")]
    Capacity,
    /// Local certificate waiting was cancelled, not native rollback.
    #[error("certificate signing was cancelled")]
    Cancelled,
    /// Waiting ended or native teardown failed; worker capacity remains retained
    /// until actual cleanup, or is quarantined if cleanup cannot be confirmed.
    #[error("certificate cleanup is pending or could not be confirmed")]
    CleanupPending,
    /// The explicitly pinned direct-provider module could not be established.
    #[error("the pinned certificate provider module could not be established")]
    Module,
    /// An explicitly injected PIN could not be supplied.
    #[error("the explicit certificate PIN source is unavailable")]
    PinUnavailable,
    /// The selected token rejected the explicit authentication policy.
    #[error("the selected certificate authentication failed")]
    Authentication,
    /// A selected provider call failed without preserving its diagnostic text.
    #[error("the selected certificate provider operation failed")]
    Provider,
    /// Clock values cannot establish a fresh bounded assertion.
    #[error("the clock cannot establish a valid certificate assertion lifetime")]
    Clock,
    /// Signature metadata or native verification is invalid.
    #[error("the certificate assertion signature is invalid")]
    Signature,
    /// A native call failed, retaining only its numeric code.
    #[error("the selected certificate operation failed (code {code:#010x})")]
    Native {
        /// Numeric native status, never provider diagnostics.
        code: i32,
    },
}

fn failure(error: CertificateError) -> HeadlessError {
    HeadlessError::Certificate(error)
}

/// Caller-injected certificate signer bound to one immutable public certificate.
///
/// Implementations must verify the selected certificate and current key policy
/// on every signing operation without exporting a private key or permitting UI.
/// `Debug` must not expose key identifiers, signing input or signatures. The
/// provided platform adapters use independently bounded native certificate
/// facades; arbitrary caller implementations remain the caller's responsibility.
#[async_trait]
pub trait CertificateSigner: Send + Sync + fmt::Debug {
    /// Returns the explicitly selected certificate's SHA-256 DER fingerprint.
    fn certificate_sha256(&self) -> [u8; 32];

    /// Obtains fresh 128-bit system randomness for one assertion identifier.
    ///
    /// # Errors
    /// Fails closed if cryptographic randomness is unavailable.
    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError>;

    /// Signs exact JWS bytes using RSA/PSS, SHA-256/MGF1-SHA-256 and 32-byte salt.
    ///
    /// Native implementations must retain the same matched certificate context
    /// through associated-key acquisition, signing and public-key verification.
    /// Dropping the future invalidates publication without claiming that a
    /// synchronous native call has stopped or released its capacity.
    ///
    /// # Errors
    /// Returns sanitized selection, policy, capacity, cancellation or native errors.
    async fn sign_ps256(&self, signing_input: &[u8]) -> Result<Vec<u8>, HeadlessError>;
}

/// An explicit signer and immutable public-certificate binding.
#[derive(Clone)]
pub struct CertificateConfig {
    signer: Arc<dyn CertificateSigner>,
    sha256: [u8; 32],
}

impl CertificateConfig {
    /// Captures the caller's public selection without key access or token acquisition.
    ///
    /// The signer constructor and fingerprint accessor must be inert. There is
    /// no certificate-file, password, default store or fallback-provider input.
    pub fn new(signer: Arc<dyn CertificateSigner>) -> Self {
        let sha256 = signer.certificate_sha256();
        Self { signer, sha256 }
    }

    /// Returns the immutable selected public-certificate fingerprint.
    pub fn certificate_sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub(super) fn provider(
        &self,
        identity: ServiceIdentity,
        mut options: ClientOptions,
    ) -> Result<Arc<dyn TokenCredential>, HeadlessError> {
        options.cloud = Some(Arc::new(azure_core::cloud::CloudConfiguration::AzurePublic));
        options.retry = azure_core::http::RetryOptions::none();
        let assertion = CertificateAssertion {
            identity,
            config: self.clone(),
            clock: Arc::new(SystemClock),
        };
        ClientAssertionCredential::new(
            assertion.identity.tenant().into(),
            assertion.identity.client().into(),
            assertion,
            Some(ClientAssertionCredentialOptions {
                client_options: options,
            }),
        )
        .map(|provider| provider as Arc<dyn TokenCredential>)
        .map_err(|_| HeadlessError::Configuration)
    }
}

impl fmt::Debug for CertificateConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateConfig").finish_non_exhaustive()
    }
}

trait AssertionClock: Send + Sync {
    fn now(&self) -> i64;
}

struct SystemClock;

impl AssertionClock for SystemClock {
    fn now(&self) -> i64 {
        OffsetDateTime::now_utc().unix_timestamp()
    }
}

struct CertificateAssertion {
    identity: ServiceIdentity,
    config: CertificateConfig,
    clock: Arc<dyn AssertionClock>,
}

impl fmt::Debug for CertificateAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateAssertion")
            .finish_non_exhaustive()
    }
}

impl CertificateAssertion {
    fn validate_selection(&self) -> Result<(), HeadlessError> {
        if self.config.signer.certificate_sha256() != self.config.sha256 {
            return Err(failure(CertificateError::Selection));
        }
        Ok(())
    }

    fn lifetime(&self) -> Result<(i64, i64), HeadlessError> {
        let now = self.clock.now();
        let issued = OffsetDateTime::from_unix_timestamp(now)
            .map_err(|_| failure(CertificateError::Clock))?;
        if now < 0 {
            return Err(failure(CertificateError::Clock));
        }
        let expires = issued
            .checked_add(Duration::seconds(ASSERTION_SECONDS))
            .ok_or_else(|| failure(CertificateError::Clock))?;
        Ok((now, expires.unix_timestamp()))
    }

    async fn assertion(&self) -> Result<String, HeadlessError> {
        self.validate_selection()?;
        let (issued, expires) = self.lifetime()?;
        let nonce = self.config.signer.assertion_nonce()?;
        let endpoint = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.identity.tenant()
        );
        let header = serde_json::json!({
            "alg": "PS256",
            "typ": "JWT",
            "x5t#S256": base64::encode_url_safe(self.config.sha256),
        });
        let payload = serde_json::json!({
            "aud": endpoint,
            "iss": self.identity.client(),
            "sub": self.identity.client(),
            "iat": issued,
            "nbf": issued,
            "exp": expires,
            "jti": base64::encode_url_safe(nonce),
        });
        let header = serde_json::to_vec(&header).map_err(|_| HeadlessError::Configuration)?;
        let payload = serde_json::to_vec(&payload).map_err(|_| HeadlessError::Configuration)?;
        let signing_input = format!(
            "{}.{}",
            base64::encode_url_safe(header),
            base64::encode_url_safe(payload)
        );
        if signing_input.len() > SIGNING_INPUT_LIMIT {
            return Err(HeadlessError::Configuration);
        }
        let signature = self
            .config
            .signer
            .sign_ps256(signing_input.as_bytes())
            .await?;
        self.validate_selection()?;
        let finished = self.clock.now();
        if finished < issued || finished >= expires {
            return Err(failure(CertificateError::Clock));
        }
        if !(256..=512).contains(&signature.len()) {
            return Err(failure(CertificateError::Signature));
        }
        Ok(format!(
            "{signing_input}.{}",
            base64::encode_url_safe(signature)
        ))
    }
}

#[async_trait]
impl ClientAssertion for CertificateAssertion {
    async fn secret(&self, _: Option<ClientMethodOptions<'_>>) -> azure_core::Result<String> {
        self.assertion().await.map_err(credential_error)
    }
}

/// Inert explicit Windows store selection over the isolated native signer facade.
#[cfg(all(windows, feature = "certificate-store-windows"))]
pub struct WindowsStoreSigner {
    selector: fabric_identity_windows::CertificateSelector,
}

#[cfg(all(windows, feature = "certificate-store-windows"))]
impl WindowsStoreSigner {
    /// Captures selection metadata without opening a store or accessing a key.
    pub fn new(selector: fabric_identity_windows::CertificateSelector) -> Self {
        Self { selector }
    }

    /// Returns non-secret store/fingerprint/provider-policy batch binding metadata.
    pub fn binding(&self) -> String {
        self.selector.binding()
    }
}

#[cfg(all(windows, feature = "certificate-store-windows"))]
impl fmt::Debug for WindowsStoreSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsStoreSigner").finish_non_exhaustive()
    }
}

#[cfg(all(windows, feature = "certificate-store-windows"))]
fn native_error(error: fabric_identity_windows::CertificateError) -> HeadlessError {
    use fabric_identity_windows::CertificateError as Native;

    failure(match error {
        Native::InvalidInput => return HeadlessError::Configuration,
        Native::CapacityExhausted { .. } => CertificateError::Capacity,
        Native::Cancelled => CertificateError::Cancelled,
        Native::NotFound | Native::AmbiguousMatch | Native::CertificateMetadata => {
            CertificateError::Selection
        }
        Native::SearchLimit => CertificateError::SearchLimit,
        Native::ProviderNotAllowed | Native::KeyPolicyUnavailable | Native::ExportableKey => {
            CertificateError::Policy
        }
        Native::UnsupportedKey => CertificateError::UnsupportedKey,
        Native::InteractionRequired => CertificateError::InteractionRequired,
        Native::SignatureInvalid => CertificateError::Signature,
        Native::WorkerStart | Native::WorkerFailed => return HeadlessError::Unavailable,
        Native::CleanupUnconfirmed { code, .. } | Native::Native { code, .. } => {
            CertificateError::Native { code }
        }
    })
}

#[cfg(all(windows, feature = "certificate-store-windows"))]
#[async_trait]
impl CertificateSigner for WindowsStoreSigner {
    fn certificate_sha256(&self) -> [u8; 32] {
        self.selector.certificate_sha256()
    }

    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError> {
        fabric_identity_windows::assertion_nonce().map_err(native_error)
    }

    async fn sign_ps256(&self, signing_input: &[u8]) -> Result<Vec<u8>, HeadlessError> {
        fabric_identity_windows::sign_ps256(self.selector.clone(), signing_input.to_vec())
            .map_err(native_error)?
            .await
            .map_err(native_error)
    }
}

#[cfg(test)]
#[path = "certificate_tests.rs"]
mod tests;
