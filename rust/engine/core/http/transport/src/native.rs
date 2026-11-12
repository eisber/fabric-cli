// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    ProxyPolicy, RootCertificate, TransportError, TransportOptions, TrustPolicy,
    connector::Connector,
    decode::{decoded, sanitized_kind},
};
use async_trait::async_trait;
use azure_core::{
    Bytes,
    error::ErrorKind,
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, Url, headers::Headers},
};
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{body::Body as _, body::Frame};
use hyper_rustls::HttpsConnector;
use hyper_timeout::TimeoutConnector;
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioTimer as HyperTimer},
};
use std::{error::Error as StdError, fmt, io, sync::Arc};

type RequestBody = UnsyncBoxBody<Bytes, azure_core::Error>;
type NativeClient = Client<TimeoutConnector<HttpsConnector<Connector>>, RequestBody>;

/// One reusable, pooled, explicitly selected Hyper/Rustls HTTP client.
///
/// Construction does not acquire credentials, perform a service request or
/// create a runtime. Execute it on the caller's Tokio runtime with I/O and time
/// enabled. Clones share the same pool. The existing `HttpExecutor` supplies
/// service-origin checks, retries, overall deadlines and response limits.
#[derive(Clone)]
pub struct NativeHttpClient {
    client: NativeClient,
    metadata_endpoint: Option<Url>,
}

impl NativeHttpClient {
    /// Constructs a client with explicit socket, trust and proxy settings.
    ///
    /// # Errors
    /// Returns sanitized typed configuration, trust and client-build failures.
    pub fn new(options: TransportOptions) -> Result<Self, TransportError> {
        let connector = Connector::new(options.proxy.clone())?;
        Self::build(options, connector, None)
    }

    /// Constructs a dedicated GET-only managed-identity metadata client.
    ///
    /// Only the exact selected HTTP origin and path are accepted. The endpoint
    /// must use a literal loopback or link-local IP; DNS, proxies, redirects,
    /// service HTTPS requests, endpoint queries and user information are refused.
    /// Credential adapters must independently validate provider query parameters.
    ///
    /// # Errors
    /// Returns sanitized configuration or client-construction errors.
    pub fn for_managed_identity(
        options: TransportOptions,
        exact_endpoint: Url,
    ) -> Result<Self, TransportError> {
        Self::validate_metadata_endpoint(&options, &exact_endpoint)?;
        let connector = Connector::new(ProxyPolicy::None)?;
        Self::build(options, connector, Some(exact_endpoint))
    }

    fn validate_metadata_endpoint(
        options: &TransportOptions,
        endpoint: &Url,
    ) -> Result<(), TransportError> {
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
            || endpoint.port_or_known_default() == Some(0)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(options.proxy, ProxyPolicy::None)
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(())
    }

    fn build(
        options: TransportOptions,
        connector: Connector,
        metadata_endpoint: Option<Url>,
    ) -> Result<Self, TransportError> {
        options.validate()?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let TrustPolicy::Platform { additional_roots } = options.trust;
        let verifier = platform_verifier(additional_roots, provider.clone())?;
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|_| TransportError::ClientConstruction)?
            .dangerous()
            // This installs the established platform verifier, not a bypass.
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let mut https = HttpsConnector::from((connector, tls));
        if metadata_endpoint.is_none() {
            https.enforce_https();
        }
        let mut timed = TimeoutConnector::new(https);
        timed.set_connect_timeout(Some(options.connect_timeout));
        timed.set_read_timeout(Some(options.read_timeout));
        timed.set_write_timeout(Some(options.write_timeout));
        timed.set_reset_reader_on_write(true);
        let mut builder = Client::builder(TokioExecutor::new());
        builder
            .retry_canceled_requests(false)
            .pool_timer(HyperTimer::new())
            .pool_idle_timeout(options.pool_idle_timeout)
            .pool_max_idle_per_host(options.max_idle_per_host.get())
            .http1_max_buf_size(32 * 1024);
        Ok(Self {
            client: builder.build(timed),
            metadata_endpoint,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        options: TransportOptions,
        address: std::net::SocketAddr,
    ) -> Result<Self, TransportError> {
        let mut connector = Connector::new(options.proxy.clone())?;
        connector.test_address = Some(address);
        Self::build(options, connector, None)
    }

    #[cfg(test)]
    pub(crate) fn for_managed_identity_test(
        options: TransportOptions,
        endpoint: Url,
        address: std::net::SocketAddr,
    ) -> Result<Self, TransportError> {
        Self::validate_metadata_endpoint(&options, &endpoint)?;
        let mut connector = Connector::new(ProxyPolicy::None)?;
        connector.test_address = Some(address);
        Self::build(options, connector, Some(endpoint))
    }
}

impl fmt::Debug for NativeHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeHttpClient(<pooled>)")
    }
}

#[async_trait]
impl HttpClient for NativeHttpClient {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let allowed = match &self.metadata_endpoint {
            Some(endpoint) => {
                request.method() == Method::Get
                    && request.url().origin() == endpoint.origin()
                    && request.url().path() == endpoint.path()
            }
            None => request.url().scheme() == "https",
        };
        if !allowed
            || !request.url().username().is_empty()
            || request.url().password().is_some()
            || request.url().fragment().is_some()
        {
            return Err(failure(ErrorKind::Other, "unsupported HTTP request URL"));
        }
        let mut pending = http::Request::builder()
            .method(request.method().as_str())
            .uri(request.url().as_str());
        let mut has_encoding = false;
        let mut has_framing = false;
        for (name, value) in request.headers().iter() {
            has_encoding |= name.as_str() == "accept-encoding";
            has_framing |= matches!(name.as_str(), "content-length" | "transfer-encoding");
            pending = pending.header(name.as_str(), value.as_str());
        }
        if !has_encoding {
            pending = pending.header("accept-encoding", "gzip, deflate");
        }
        let body = match request.body().clone() {
            Body::Bytes(bytes) => {
                if bytes.is_empty()
                    && !has_framing
                    && matches!(
                        request.method(),
                        Method::Post | Method::Put | Method::Patch | Method::Delete
                    )
                {
                    // Hyper omits the length for end-of-stream bodies; Fabric requires it.
                    pending = pending.header(http::header::CONTENT_LENGTH, "0");
                }
                Full::new(bytes)
                    .map_err(|never| match never {})
                    .boxed_unsync()
            }
            Body::SeekableStream(stream) => {
                StreamBody::new(stream.map_ok(Frame::data).map_err(|error| {
                    failure(
                        transport_error_kind(&error),
                        "HTTP request body stream failed",
                    )
                }))
                .boxed_unsync()
            }
        };
        let pending = pending
            .body(body)
            .map_err(|_| failure(ErrorKind::Other, "invalid HTTP request headers or body"))?;
        let response =
            self.client.request(pending).await.map_err(|error| {
                failure(transport_error_kind(&error), "native HTTP request failed")
            })?;
        let status = response.status();
        let headers = response_headers(request, response.headers())?;
        let has_body = request.method() != Method::Head
            && !matches!(status.as_u16(), 204 | 304)
            && !response.body().is_end_stream();
        let stream = Box::pin(
            TryStreamExt::map_err(response.into_body().into_stream(), |error| {
                failure(
                    transport_error_kind(&error),
                    "native HTTP response stream failed",
                )
            })
            .and_then(|frame| async move {
                frame.into_data().map_err(|_| {
                    failure(
                        ErrorKind::DataConversion,
                        "HTTP trailers cannot be represented",
                    )
                })
            }),
        );
        let body = decoded(stream, &headers, has_body)?;
        Ok(AsyncRawResponse::new(status.as_u16().into(), headers, body))
    }
}

pub(crate) fn transport_error_kind(error: &(dyn StdError + 'static)) -> ErrorKind {
    let mut source = Some(error);
    let mut io_failure = false;
    let mut conversion = false;
    let mut user_failure = false;
    // Error::source is caller-controlled; do not loop forever on a cyclic chain.
    for _ in 0..32 {
        let Some(error) = source else { break };
        if let Some(error) = error.downcast_ref::<azure_core::Error>() {
            return sanitized_kind(error.kind());
        }
        if error.is::<rustls::Error>() {
            return ErrorKind::Other;
        }
        if let Some(error) = error.downcast_ref::<hyper::Error>() {
            if error.is_parse() {
                return ErrorKind::DataConversion;
            }
            user_failure |= error.is_user();
            io_failure |= error.is_incomplete_message() || error.is_timeout();
        }
        if let Some(error) = error.downcast_ref::<io::Error>() {
            conversion |= matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
            );
            io_failure |= matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::AddrInUse
                    | io::ErrorKind::AddrNotAvailable
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::Interrupted
            );
        }
        source = if let Some(error) = error.downcast_ref::<io::Error>() {
            error
                .get_ref()
                .map(|source| source as &(dyn StdError + 'static))
                .or_else(|| error.source())
        } else {
            error.source()
        };
    }
    if source.is_some() {
        ErrorKind::Other
    } else if conversion {
        ErrorKind::DataConversion
    } else if io_failure && !user_failure {
        ErrorKind::Io
    } else {
        ErrorKind::Other
    }
}

pub(crate) fn response_headers(
    request: &Request,
    raw: &http::HeaderMap,
) -> azure_core::Result<Headers> {
    let livy = request.url().path().contains("/livyApi/versions/")
        || request
            .headers()
            .get_optional_str(&"accept".into())
            .is_some_and(|accept| {
                accept.split(',').any(|value| {
                    let media = value.split(';').next().unwrap_or("").trim();
                    media.eq_ignore_ascii_case("application/vnd.livy.arrow-chunks")
                        || media.eq_ignore_ascii_case("application/vnd.apache.arrow.stream")
                })
            });
    if livy {
        for name in [
            "content-type",
            "content-encoding",
            "content-length",
            "retry-after",
            "location",
            "etag",
            "statementid",
            "id",
        ] {
            if raw.get_all(name).iter().nth(1).is_some() {
                return Err(failure(
                    ErrorKind::DataConversion,
                    "ambiguous native Livy response metadata",
                ));
            }
        }
    }
    let mut headers = Headers::new();
    for name in raw.keys() {
        let mut combined = String::new();
        for (index, value) in raw.get_all(name).iter().enumerate() {
            if index > 0 {
                combined.push_str(", ");
            }
            // Like Requests, retain repeated non-singleton fields and Latin-1 octets.
            combined.extend(value.as_bytes().iter().copied().map(char::from));
        }
        headers.insert(name.as_str().to_owned(), combined);
    }
    Ok(headers)
}

fn failure(kind: ErrorKind, message: &'static str) -> azure_core::Error {
    azure_core::Error::with_message(kind, message)
}

fn platform_verifier(
    additional_roots: Vec<RootCertificate>,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>, TransportError> {
    #[cfg(target_os = "linux")]
    {
        let native = rustls_native_certs::load_native_certs();
        if !native.errors.is_empty() || native.certs.is_empty() {
            return Err(TransportError::PlatformTrust);
        }
        let mut store = rustls::RootCertStore::empty();
        for certificate in native
            .certs
            .into_iter()
            .chain(additional_roots.into_iter().map(|root| root.0))
        {
            store
                .add(certificate)
                .map_err(|_| TransportError::PlatformTrust)?;
        }
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider)
            .build()
            .map(|verifier| verifier as Arc<dyn rustls::client::danger::ServerCertVerifier>)
            .map_err(|_| TransportError::PlatformTrust)
    }
    #[cfg(not(target_os = "linux"))]
    {
        rustls_platform_verifier::Verifier::new_with_extra_roots(
            additional_roots.into_iter().map(|root| root.0),
            provider,
        )
        .map(|verifier| Arc::new(verifier) as Arc<dyn rustls::client::danger::ServerCertVerifier>)
        .map_err(|_| TransportError::PlatformTrust)
    }
}
