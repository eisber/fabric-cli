// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit public-cloud unattended authentication. No default credential chain,
//! process launcher, interactive UI, token persistence or default HTTP client.

use async_trait::async_trait;
use azure_core::{
    cloud::CloudConfiguration,
    credentials::{AccessToken, Secret, TokenCredential, TokenRequestOptions},
    error::{Error, ErrorKind},
    http::{
        AsyncRawResponse, ClientOptions, HttpClient, Method, Request, RetryOptions, Transport, Url,
    },
    time::OffsetDateTime,
};
use azure_identity::{
    ClientAssertionCredential, ClientAssertionCredentialOptions, ClientSecretCredential,
    ClientSecretCredentialOptions, ManagedIdentityCredential, ManagedIdentityCredentialOptions,
    UserAssignedId,
};
use std::{fmt, sync::Arc};

#[path = "assertion_file.rs"]
mod assertion_file;
pub use assertion_file::AssertionFile;

#[path = "pipelines.rs"]
mod pipelines;
pub use pipelines::PipelinesConfig;

#[cfg(any(
    feature = "certificate-store-windows",
    feature = "certificate-store-pkcs11",
    feature = "certificate-store-macos"
))]
#[path = "certificate.rs"]
mod certificate;
#[cfg(all(windows, feature = "certificate-store-windows"))]
pub use certificate::WindowsStoreSigner;
#[cfg(any(
    feature = "certificate-store-windows",
    feature = "certificate-store-pkcs11",
    feature = "certificate-store-macos"
))]
pub use certificate::{CertificateConfig, CertificateError, CertificateSigner};
#[cfg(all(windows, feature = "certificate-store-windows"))]
pub use fabric_identity_windows::{
    AllowedCertificateProvider, CertificateSelector, CertificateStore,
};
#[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
#[path = "certificate_pkcs11.rs"]
mod certificate_pkcs11;
#[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
pub use certificate_pkcs11::{
    Pkcs11Authentication, Pkcs11ConfigError, Pkcs11Field, Pkcs11PinSource, Pkcs11Selection,
    Pkcs11Signer, Pkcs11Token,
};
/// Native macOS certificate assertions over an explicitly pinned isolated helper.
#[cfg(feature = "certificate-store-macos")]
#[path = "certificate_macos.rs"]
pub mod certificate_macos;

#[cfg(all(
    test,
    any(
        all(target_os = "linux", feature = "certificate-store-pkcs11"),
        feature = "certificate-store-macos"
    )
))]
#[path = "../../windows/src/certificate_vectors.rs"]
mod public_certificate_vectors;

const ASSERTION_LIMIT: u64 = 64 * 1024;
const MI_VARIABLES: [&str; 6] = [
    "IDENTITY_ENDPOINT",
    "IDENTITY_HEADER",
    "IDENTITY_SERVER_THUMBPRINT",
    "IMDS_ENDPOINT",
    "MSI_ENDPOINT",
    "MSI_SECRET",
];

/// Sanitized failures. Provider bodies, paths, secrets and source chains are discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HeadlessError {
    /// Explicit configuration is missing or malformed.
    #[error("invalid explicit headless credential configuration")]
    Configuration,
    /// The Azure host no longer matches the explicitly selected host contract.
    #[error("the selected managed identity host contract does not match")]
    HostContract,
    /// The Azure Pipelines endpoint or running job no longer matches selection.
    #[error("the selected Azure Pipelines run contract does not match")]
    PipelineContract,
    /// An assertion file could not be read safely.
    #[error("the selected assertion file could not be read safely")]
    AssertionFile,
    /// The explicitly selected certificate failed without retaining native text.
    #[cfg(any(
        feature = "certificate-store-windows",
        feature = "certificate-store-pkcs11",
        feature = "certificate-store-macos"
    ))]
    #[error("{0}")]
    Certificate(CertificateError),
    /// The requested scope is not one supported single audience.
    #[error("unsupported headless credential scope")]
    Scope,
    /// The selected identity could not acquire or refresh a token.
    #[error("the selected headless identity could not acquire or refresh a token")]
    Acquisition,
    /// The provider rejected the selected credential or its permissions.
    #[error("the provider rejected the selected headless credential")]
    Rejected,
    /// The selected provider or transport is temporarily unavailable.
    #[error("the selected headless identity provider is unavailable")]
    Unavailable,
    /// The provider returned an empty or expired token.
    #[error("the selected headless identity returned invalid token metadata")]
    TokenMetadata,
}

fn credential_error(error: HeadlessError) -> Error {
    Error::new(ErrorKind::Credential, error)
}

fn sanitize_provider(error: Error) -> Error {
    let mut current: &(dyn std::error::Error + 'static) = &error;
    loop {
        if let Some(error) = current.downcast_ref::<HeadlessError>() {
            return credential_error(*error);
        }
        match current.source() {
            Some(source) => current = source,
            None => return credential_error(HeadlessError::Acquisition),
        }
    }
}

/// Explicit GUID-bound service identity. Labels do not prove token claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceIdentity {
    tenant: String,
    client: String,
}

impl ServiceIdentity {
    /// Selects one tenant and application, without reading ambient inputs.
    ///
    /// # Errors
    /// Rejects non-GUID selectors without echoing their values.
    pub fn new(
        tenant: impl Into<String>,
        client: impl Into<String>,
    ) -> Result<Self, HeadlessError> {
        let tenant = tenant.into();
        let client = client.into();
        if !is_guid(&tenant) || !is_guid(&client) {
            return Err(HeadlessError::Configuration);
        }
        Ok(Self { tenant, client })
    }

    /// Explicit tenant selector, not independently verified token metadata.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Explicit application/client selector, not a UPN.
    pub fn client(&self) -> &str {
        &self.client
    }
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Explicit managed identity host. No source probing or fallback is authorized.
#[derive(Clone)]
pub enum ManagedHost {
    /// Azure VM IMDS at its fixed link-local HTTP endpoint.
    VirtualMachine,
    /// Azure App Service's exact local endpoint and explicit platform header source.
    AppService {
        /// Explicit platform-provided endpoint; no query, fragment or user info.
        endpoint: Url,
    },
}

impl fmt::Debug for ManagedHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedHost").finish_non_exhaustive()
    }
}

impl ManagedHost {
    /// Validates an explicit App Service endpoint using the `IDENTITY_HEADER` source.
    ///
    /// # Errors
    /// Only literal IPv4/IPv6 loopback or link-local HTTP endpoints with a bounded header are supported.
    pub fn app_service(endpoint: &str) -> Result<Self, HeadlessError> {
        let endpoint = Url::parse(endpoint).map_err(|_| HeadlessError::Configuration)?;
        let local = endpoint.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| match address {
                    std::net::IpAddr::V4(address) => {
                        address.is_loopback() || address.is_link_local()
                    }
                    std::net::IpAddr::V6(address) => {
                        address.is_loopback() || address.is_unicast_link_local()
                    }
                })
        });
        if !local
            || endpoint.scheme() != "http"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.port_or_known_default() == Some(0)
        {
            return Err(HeadlessError::Configuration);
        }
        Ok(Self::AppService { endpoint })
    }

    /// Returns the exact non-secret metadata endpoint selected by the caller.
    pub fn endpoint(&self) -> Url {
        match self {
            Self::VirtualMachine => {
                Url::parse("http://169.254.169.254/metadata/identity/oauth2/token")
                    .expect("constant endpoint")
            }
            Self::AppService { endpoint, .. } => endpoint.clone(),
        }
    }

    fn validate_environment(&self) -> Result<(), HeadlessError> {
        if let Self::AppService { endpoint } = self {
            Self::app_service(endpoint.as_str())?;
        }
        self.validate_with(|name| {
            std::env::var_os(name)
                .map(|value| value.into_string().map_err(|_| HeadlessError::HostContract))
                .transpose()
        })
    }
}

impl ManagedHost {
    fn validate_with(
        &self,
        read: impl Fn(&str) -> Result<Option<String>, HeadlessError>,
    ) -> Result<(), HeadlessError> {
        for name in MI_VARIABLES {
            let actual = read(name)?;
            if matches!(self, Self::AppService { .. }) && name == "IDENTITY_HEADER" {
                if actual.as_deref().is_none_or(|value| !valid_header(value)) {
                    return Err(HeadlessError::HostContract);
                }
                continue;
            }
            let expected = match (self, name) {
                (Self::AppService { endpoint, .. }, "IDENTITY_ENDPOINT") => Some(endpoint.as_str()),
                _ => None,
            };
            match (actual, expected) {
                (Some(actual), Some(expected)) if actual == expected => {}
                (None, None) => {}
                _ => return Err(HeadlessError::HostContract),
            }
        }
        Ok(())
    }

    fn validate_request_header(
        &self,
        actual: Option<&str>,
        read: impl Fn(&str) -> Result<Option<String>, HeadlessError>,
    ) -> Result<(), HeadlessError> {
        match self {
            Self::VirtualMachine if actual.is_none() => Ok(()),
            Self::AppService { .. } => {
                let expected = read("IDENTITY_HEADER")?.ok_or(HeadlessError::HostContract)?;
                if valid_header(&expected) && actual == Some(expected.as_str()) {
                    Ok(())
                } else {
                    Err(HeadlessError::HostContract)
                }
            }
            _ => Err(HeadlessError::HostContract),
        }
    }
}

fn valid_header(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Explicit input to the official credential adapter.
///
/// Client secrets are snapshots; assertion files renew on each token refresh.
/// Managed identity never interprets absent selectors as permission to choose an
/// account. The caller must explicitly select system-assigned or a client ID.
pub enum HeadlessInput {
    /// Application client secret supplied by the caller, not an access token.
    ClientSecret {
        /// Explicit tenant/application binding.
        identity: ServiceIdentity,
        /// Client secret; never accepted as a command argument or serialized.
        secret: Secret,
    },
    /// Renewable client assertion, including AKS projected service-account tokens.
    AssertionFile {
        /// Explicit tenant/application binding.
        identity: ServiceIdentity,
        /// Bounded file provider; no permanent assertion cache.
        assertion: AssertionFile,
    },
    /// Azure Pipelines service-connection workload federation.
    AzurePipelines {
        /// Explicit tenant/application binding.
        identity: ServiceIdentity,
        /// Explicit connection, running job endpoint and secret system token.
        pipeline: Box<PipelinesConfig>,
    },
    /// Explicit non-exporting native certificate signer.
    #[cfg(any(
        feature = "certificate-store-windows",
        feature = "certificate-store-pkcs11",
        feature = "certificate-store-macos"
    ))]
    Certificate {
        /// Explicit public-cloud tenant/application binding.
        identity: ServiceIdentity,
        /// Immutable certificate binding and caller-injected signer.
        certificate: Box<CertificateConfig>,
    },
    /// Azure VM or App Service managed identity.
    ManagedIdentity {
        /// Explicit host contract.
        host: ManagedHost,
        /// `None` explicitly selects system-assigned; `Some` selects a GUID client ID.
        client_id: Option<String>,
    },
}

impl fmt::Debug for HeadlessInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessInput").finish_non_exhaustive()
    }
}

/// Scope-restricted, redacting wrapper around official Azure credential types.
pub struct HeadlessCredential {
    credential: Arc<dyn TokenCredential>,
    host: Option<ManagedHost>,
    pipeline: Option<PipelinesConfig>,
}

impl fmt::Debug for HeadlessCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessCredential").finish_non_exhaustive()
    }
}

impl HeadlessCredential {
    /// Composes an explicit provider and injected HTTP transport, without network I/O.
    ///
    /// Managed identity checks only its selected host's environment contract,
    /// because the official credential's public API obtains host data there.
    /// No environment is mutated. The contract is checked again on every token
    /// call and request, including cache hits. Pipelines similarly checks its
    /// caller-captured run against the environment required by the official SDK.
    /// Secret and assertion-file providers read no environment.
    /// The injected HTTP client must disable redirects and automatic retries.
    ///
    /// # Errors
    /// Returns sanitized configuration/host errors, never provider diagnostics.
    pub fn new(
        input: HeadlessInput,
        http: Arc<dyn HttpClient>,
    ) -> Result<Arc<Self>, HeadlessError> {
        let host = match &input {
            HeadlessInput::ManagedIdentity { host, .. } => {
                host.validate_environment()?;
                Some(host.clone())
            }
            _ => None,
        };
        let pipeline = match &input {
            HeadlessInput::AzurePipelines { pipeline, .. } => {
                pipeline.validate_environment()?;
                Some(pipeline.as_ref().clone())
            }
            _ => None,
        };
        let endpoint = match &input {
            HeadlessInput::ClientSecret { identity, .. }
            | HeadlessInput::AssertionFile { identity, .. }
            | HeadlessInput::AzurePipelines { identity, .. } => Url::parse(&format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                identity.tenant()
            ))
            .map_err(|_| HeadlessError::Configuration)?,
            HeadlessInput::ManagedIdentity { host, .. } => host.endpoint(),
            #[cfg(any(
                feature = "certificate-store-windows",
                feature = "certificate-store-pkcs11",
                feature = "certificate-store-macos"
            ))]
            HeadlessInput::Certificate { identity, .. } => Url::parse(&format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                identity.tenant()
            ))
            .map_err(|_| HeadlessError::Configuration)?,
        };
        let guarded = CredentialHttp {
            inner: http,
            endpoint,
            host: host.clone(),
            managed_client: match &input {
                HeadlessInput::ManagedIdentity { client_id, .. } => client_id.clone(),
                _ => None,
            },
        };
        let guarded: Arc<dyn HttpClient> = match &pipeline {
            Some(config) => Arc::new(pipelines::PipelinesHttp {
                token_http: guarded,
                config: config.clone(),
            }),
            None => Arc::new(guarded),
        };
        let options = ClientOptions {
            transport: Some(Transport::new(guarded)),
            cloud: Some(Arc::new(CloudConfiguration::AzurePublic)),
            retry: RetryOptions::none(),
            ..Default::default()
        };
        let credential: Arc<dyn TokenCredential> = match input {
            HeadlessInput::ClientSecret { identity, secret } => {
                if secret.secret().is_empty() || secret.secret().len() > ASSERTION_LIMIT as usize {
                    return Err(HeadlessError::Configuration);
                }
                ClientSecretCredential::new(
                    identity.tenant(),
                    identity.client().into(),
                    secret,
                    Some(ClientSecretCredentialOptions {
                        client_options: options,
                    }),
                )
                .map_err(|_| HeadlessError::Configuration)?
            }
            HeadlessInput::AssertionFile {
                identity,
                assertion,
            } => ClientAssertionCredential::new(
                identity.tenant().into(),
                identity.client().into(),
                assertion,
                Some(ClientAssertionCredentialOptions {
                    client_options: options,
                }),
            )
            .map_err(|_| HeadlessError::Configuration)?,
            HeadlessInput::AzurePipelines { identity, pipeline } => {
                pipeline.provider(identity, options)?
            }
            #[cfg(any(
                feature = "certificate-store-windows",
                feature = "certificate-store-pkcs11",
                feature = "certificate-store-macos"
            ))]
            HeadlessInput::Certificate {
                identity,
                certificate,
            } => certificate.provider(identity, options)?,
            HeadlessInput::ManagedIdentity { client_id, .. } => {
                if client_id.as_deref().is_some_and(|value| !is_guid(value)) {
                    return Err(HeadlessError::Configuration);
                }
                ManagedIdentityCredential::new(Some(ManagedIdentityCredentialOptions {
                    user_assigned_id: client_id.map(UserAssignedId::ClientId),
                    client_options: options,
                }))
                .map_err(|_| HeadlessError::Configuration)?
            }
        };
        Ok(Arc::new(Self {
            credential,
            host,
            pipeline,
        }))
    }
}

#[async_trait]
impl TokenCredential for HeadlessCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if !matches!(
            scopes,
            ["https://api.fabric.microsoft.com/.default"]
                | ["https://storage.azure.com/.default"]
                | ["https://management.azure.com/.default"]
                | ["https://database.windows.net/.default"]
        ) {
            return Err(credential_error(HeadlessError::Scope));
        }
        if let Some(host) = &self.host {
            host.validate_environment().map_err(credential_error)?;
        }
        if let Some(pipeline) = &self.pipeline {
            pipeline.validate_environment().map_err(credential_error)?;
        }
        let token = self
            .credential
            .get_token(scopes, options)
            .await
            .map_err(sanitize_provider)?;
        if let Some(pipeline) = &self.pipeline {
            pipeline.validate_environment().map_err(credential_error)?;
        }
        if token.token.secret().is_empty() || token.expires_on <= OffsetDateTime::now_utc() {
            return Err(credential_error(HeadlessError::TokenMetadata));
        }
        Ok(token)
    }
}

struct CredentialHttp {
    inner: Arc<dyn HttpClient>,
    endpoint: Url,
    host: Option<ManagedHost>,
    managed_client: Option<String>,
}

impl fmt::Debug for CredentialHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialHttp").finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for CredentialHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let url = request.url();
        let expected_method = if self.host.is_some() {
            Method::Get
        } else {
            Method::Post
        };
        if url.origin() != self.endpoint.origin()
            || url.path() != self.endpoint.path()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || request.method() != expected_method
            || self.host.is_none() && url.query().is_some()
        {
            return Err(credential_error(HeadlessError::Configuration));
        }
        if let Some(host) = &self.host {
            host.validate_environment().map_err(credential_error)?;
            let actual = request.headers().get_str(&"x-identity-header".into()).ok();
            host.validate_request_header(actual, |name| {
                std::env::var_os(name)
                    .map(|value| value.into_string().map_err(|_| HeadlessError::HostContract))
                    .transpose()
            })
            .map_err(credential_error)?;
            let pairs = url.query_pairs().collect::<Vec<_>>();
            let expected_count = if self.managed_client.is_some() { 3 } else { 2 };
            if pairs.len() != expected_count
                || pairs
                    .iter()
                    .filter(|(name, value)| name == "api-version" && value == "2019-08-01")
                    .count()
                    != 1
                || pairs
                    .iter()
                    .filter(|(name, value)| {
                        name == "resource"
                            && matches!(
                                value.as_ref(),
                                "https://api.fabric.microsoft.com"
                                    | "https://storage.azure.com"
                                    | "https://management.azure.com"
                            )
                    })
                    .count()
                    != 1
                || self.managed_client.as_deref().is_some_and(|client| {
                    pairs
                        .iter()
                        .filter(|(name, value)| name == "client_id" && value == client)
                        .count()
                        != 1
                })
            {
                return Err(credential_error(HeadlessError::Configuration));
            }
        }
        let response = self
            .inner
            .execute_request(request)
            .await
            .map_err(|_| credential_error(HeadlessError::Unavailable))?;
        let status: u16 = response.status().into();
        if !(200..300).contains(&status) {
            // Do not hand provider response bodies or headers to an error chain.
            return Err(credential_error(if status == 429 || status >= 500 {
                HeadlessError::Unavailable
            } else {
                HeadlessError::Rejected
            }));
        }
        let (status, headers, body) = response.deconstruct();
        let mut bytes = vec![0; 256 * 1024];
        let length = body
            .collect_into(&mut bytes)
            .await
            .map_err(|_| credential_error(HeadlessError::TokenMetadata))?;
        bytes.truncate(length);
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| credential_error(HeadlessError::TokenMetadata))?;
        let valid_token = value
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|token| !token.is_empty());
        let valid_expiry = if self.host.is_some() {
            value
                .get("expires_on")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
                .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
                .is_some_and(|expiry| expiry > OffsetDateTime::now_utc())
        } else {
            value
                .get("expires_in")
                .and_then(serde_json::Value::as_i64)
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| {
                    OffsetDateTime::now_utc()
                        .checked_add(azure_core::time::Duration::seconds(seconds))
                })
                .is_some()
        };
        if !valid_token || !valid_expiry {
            return Err(credential_error(HeadlessError::TokenMetadata));
        }
        Ok(AsyncRawResponse::from_bytes(status, headers, bytes))
    }
}

#[cfg(test)]
#[path = "headless_tests.rs"]
mod tests;
