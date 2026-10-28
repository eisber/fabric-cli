// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    RootCertificate, TransportError, TransportOptions, TrustPolicy, connector::Connector,
    decode::decoded,
};
use async_trait::async_trait;
use azure_core::{
    Bytes,
    error::ErrorKind,
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, headers::Headers},
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
use std::{fmt, sync::Arc};

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
}

impl NativeHttpClient {
    /// Constructs a client with explicit socket, trust and proxy settings.
    ///
    /// # Errors
    /// Returns sanitized typed configuration, trust and client-build failures.
    pub fn new(options: TransportOptions) -> Result<Self, TransportError> {
        let connector = Connector::new(options.proxy.clone())?;
        Self::build(options, connector)
    }

    fn build(options: TransportOptions, connector: Connector) -> Result<Self, TransportError> {
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
        https.enforce_https();
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
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        options: TransportOptions,
        address: std::net::SocketAddr,
    ) -> Result<Self, TransportError> {
        let mut connector = Connector::new(options.proxy.clone())?;
        connector.test_address = Some(address);
        Self::build(options, connector)
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
        if request.url().scheme() != "https"
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
        for (name, value) in request.headers().iter() {
            has_encoding |= name.as_str() == "accept-encoding";
            pending = pending.header(name.as_str(), value.as_str());
        }
        if !has_encoding {
            pending = pending.header("accept-encoding", "gzip, deflate");
        }
        let body = match request.body().clone() {
            Body::Bytes(bytes) => Full::new(bytes)
                .map_err(|never| match never {})
                .boxed_unsync(),
            Body::SeekableStream(stream) => StreamBody::new(
                stream
                    .map_ok(Frame::data)
                    .map_err(|_| failure(ErrorKind::Io, "HTTP request body stream failed")),
            )
            .boxed_unsync(),
        };
        let pending = pending
            .body(body)
            .map_err(|_| failure(ErrorKind::Other, "invalid HTTP request headers or body"))?;
        let response = self
            .client
            .request(pending)
            .await
            .map_err(|_| failure(ErrorKind::Io, "native HTTP request failed"))?;
        let status = response.status();
        let mut headers = Headers::new();
        for name in response.headers().keys() {
            let mut combined = String::new();
            for (index, value) in response.headers().get_all(name).iter().enumerate() {
                if index > 0 {
                    combined.push_str(", ");
                }
                // Like Requests, retain repeated fields and HTTP's Latin-1 octets.
                combined.extend(value.as_bytes().iter().copied().map(char::from));
            }
            headers.insert(name.as_str().to_owned(), combined);
        }
        let has_body = request.method() != Method::Head
            && !matches!(status.as_u16(), 204 | 304)
            && !response.body().is_end_stream();
        let stream = Box::pin(
            TryStreamExt::map_err(response.into_body().into_stream(), |_| {
                failure(ErrorKind::Io, "native HTTP response stream failed")
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
