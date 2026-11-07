// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::TransportError;
use azure_core::http::Url;
use rustls::pki_types::CertificateDer;
use std::{fmt, num::NonZeroUsize, time::Duration};

/// One validated, explicitly supplied DER certificate added to platform trust.
#[derive(Clone)]
pub struct RootCertificate(pub(crate) CertificateDer<'static>);

impl RootCertificate {
    /// Parses a DER certificate without modifying the operating system store.
    ///
    /// # Errors
    /// Returns a sanitized error for a malformed or oversized certificate.
    pub fn from_der(der: &[u8]) -> Result<Self, TransportError> {
        if der.is_empty() || der.len() > 64 * 1024 {
            return Err(TransportError::InvalidCertificate);
        }
        let certificate = CertificateDer::from(der.to_vec());
        rustls::RootCertStore::empty()
            .add(certificate.clone())
            .map_err(|_| TransportError::InvalidCertificate)?;
        Ok(Self(certificate))
    }
}

impl fmt::Debug for RootCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RootCertificate(<explicit>)")
    }
}

/// Explicit certificate source. No bundled public-root fallback is installed.
#[derive(Clone, Debug)]
pub enum TrustPolicy {
    /// Use the platform verifier, augmented with these explicitly supplied roots.
    ///
    /// On Unix other than Apple, its native-root loader honors `SSL_CERT_FILE`
    /// and `SSL_CERT_DIR`. Selecting this policy explicitly selects that source
    /// behavior. Windows and Apple use native certificate verification.
    Platform {
        /// Extra roots, used only by this client, never installed in the OS.
        additional_roots: Vec<RootCertificate>,
    },
}

/// An explicitly configured unauthenticated static HTTP proxy.
///
/// PAC, integrated authentication, credentials, and ambient proxy discovery are
/// not supported. Its endpoint is excluded from `Debug` and all error messages.
#[derive(Clone)]
pub struct StaticHttpProxy(pub(crate) Url);

impl StaticHttpProxy {
    /// Validates an HTTP proxy origin with no user information, path or query.
    ///
    /// # Errors
    /// Rejects non-HTTP schemes, credentials, paths, fragments and query strings.
    pub fn new(endpoint: &str) -> Result<Self, TransportError> {
        let url = Url::parse(endpoint).map_err(|_| TransportError::InvalidProxy)?;
        if url.scheme() != "http"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() == Some(0)
        {
            return Err(TransportError::InvalidProxy);
        }
        Ok(Self(url))
    }
}

impl fmt::Debug for StaticHttpProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticHttpProxy(<explicit>)")
    }
}

/// Explicit proxy selection; there is no ambient or system-discovery default.
#[derive(Clone, Debug)]
pub enum ProxyPolicy {
    /// Connect directly, ignoring all proxy environment and OS settings.
    None,
    /// Use only this static HTTP proxy, including CONNECT for HTTPS.
    StaticHttp(StaticHttpProxy),
}

/// Caller-selected finite connection, socket-idle and pool settings.
///
/// There is deliberately no `Default`. The existing `HttpExecutor` owns the
/// overall request deadline and decoded-response cap. These socket settings
/// neither restart nor replace that deadline.
#[derive(Clone, Debug)]
pub struct TransportOptions {
    /// Deadline for establishing a connection, including TLS/proxy negotiation.
    pub connect_timeout: Duration,
    /// Maximum inactivity while awaiting response headers or body data.
    pub read_timeout: Duration,
    /// Maximum inactivity while writing request data.
    pub write_timeout: Duration,
    /// Maximum time an unused connection remains in the pool.
    pub pool_idle_timeout: Duration,
    /// Maximum idle pooled connections per origin.
    pub max_idle_per_host: NonZeroUsize,
    /// Explicit trust source and additional certificates.
    pub trust: TrustPolicy,
    /// Explicit direct or static-proxy selection.
    pub proxy: ProxyPolicy,
}

impl TransportOptions {
    pub(crate) fn validate(&self) -> Result<(), TransportError> {
        for duration in [
            self.connect_timeout,
            self.read_timeout,
            self.write_timeout,
            self.pool_idle_timeout,
        ] {
            if duration.is_zero() || duration > Duration::from_secs(24 * 60 * 60) {
                return Err(TransportError::InvalidTimeout);
            }
        }
        let TrustPolicy::Platform { additional_roots } = &self.trust;
        if additional_roots.len() > 128 || self.max_idle_per_host.get() > 1024 {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(())
    }
}
