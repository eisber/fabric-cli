// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native pooled HTTPS, with platform trust and bounded wire/decoded bodies.
#![forbid(unsafe_code)]
use async_compression::tokio::bufread::{GzipDecoder, ZlibDecoder};
use async_trait::async_trait;
use fabric_core::{
    ApiRequest, ApiResponse, Clock, CoreError, ErrorKind, HttpTransport, MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES, OperationContext, ResponseHeadCapture, bounded_with_clock,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_rustls::HttpsConnector;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioTimer},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;

/// Explicit native transport. No redirect, proxy, auth discovery, or runtime setup.
pub struct NativeHttpClient {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}
impl NativeHttpClient {
    /// Create a TLS transport using the operating system trust policy.
    pub fn new() -> Result<Self, CoreError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls_platform_verifier::Verifier::new(provider.clone())
            .map_err(|_| error("Unable to initialize platform certificate verification"))?;
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| error("Unable to configure TLS"))?
            .dangerous()
            // Established OS verifier, not an insecure verification bypass.
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .build();
        let mut builder = Client::builder(TokioExecutor::new());
        builder
            .retry_canceled_requests(false)
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(4)
            .http1_max_buf_size(32 * 1024);
        Ok(Self {
            client: builder.build(connector),
        })
    }
    async fn send_once(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        clock: &dyn Clock,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        if request.url.scheme() != "https" {
            return Err(error("Native transport requires an HTTPS URL"));
        }
        if request.body.len() > MAX_REQUEST_BYTES {
            return Err(error(&format!(
                "Request body is {} bytes, exceeding the {MAX_REQUEST_BYTES}-byte limit",
                request.body.len()
            )));
        }
        let mut builder = http::Request::builder()
            .method(request.method.as_str())
            .uri(request.url.as_str());
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(Full::new(Bytes::from(request.body)))
            .map_err(|_| error("Invalid HTTP request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|failure| send_failure(&failure))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(CoreError::new(
                ErrorKind::Protocol,
                "HTTP redirects are not followed",
            ));
        }
        let headers = ingest_headers(status, response.headers(), head)?;
        receive(status, headers, response.into_body(), context, clock, head).await
    }
}

// On rejection inspect the complete raw head, not just the prefix preceding the
// duplicate. A later second operation-ID field must invalidate ID-only evidence.
fn ingest_headers(
    status: u16,
    raw: &http::HeaderMap,
    head: &ResponseHeadCapture,
) -> Result<BTreeMap<String, String>, CoreError> {
    let rejected = |error| {
        head.reject_headers(
            status,
            raw.iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
            error,
        )
    };
    let mut headers = BTreeMap::new();
    let mut header_bytes = 0usize;
    for (name, value) in raw {
        header_bytes = header_bytes
            .saturating_add(name.as_str().len())
            .saturating_add(value.len());
        if header_bytes > 64 * 1024 {
            return Err(rejected(protocol(
                "Response headers exceed the 65536-byte total limit",
            )));
        }
        if headers.contains_key(name.as_str())
            && [
                "location",
                "azure-asyncoperation",
                "x-ms-operation-id",
                "retry-after",
                "content-encoding",
            ]
            .contains(&name.as_str())
        {
            return Err(rejected(protocol("Duplicate operation control header")));
        }
        // Python Requests exposes HTTP header octets as Latin-1, joining
        // repeated fields. Control headers above remain unambiguous.
        let value: String = value.as_bytes().iter().copied().map(char::from).collect();
        headers
            .entry(name.as_str().to_owned())
            .and_modify(|previous: &mut String| {
                previous.push_str(", ");
                previous.push_str(&value);
            })
            .or_insert(value);
    }
    Ok(headers)
}

// Separate from sockets so exactly the production body/decode/recovery path can
// be exercised with offline failing or pending HTTP bodies.
async fn receive<B>(
    status: u16,
    headers: BTreeMap<String, String>,
    incoming: B,
    context: &OperationContext,
    clock: &dyn Clock,
    head: &ResponseHeadCapture,
) -> Result<ApiResponse, CoreError>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin,
    B::Error: Send,
{
    head.record(status, &headers)?;
    let encoding = headers.get("content-encoding").cloned();
    bounded_with_clock(clock, context, receive_body(status, encoding, incoming))
        .await
        .map_err(|error| head.attach(error))?
        .map(|body| ApiResponse {
            status,
            headers,
            body,
        })
        .map_err(|error| head.attach(error))
}

async fn receive_body<B>(
    status: u16,
    encoding: Option<String>,
    mut incoming: B,
) -> Result<Vec<u8>, CoreError>
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin,
    B::Error: Send,
{
    let mut wire = Vec::new();
    while let Some(frame) = incoming.frame().await {
        let frame = frame.map_err(|_| error("HTTP response stream failed"))?;
        if let Ok(bytes) = frame.into_data() {
            if wire.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
                return Err(protocol(&format!(
                    "Response body exceeds the {MAX_RESPONSE_BYTES}-byte size limit"
                )));
            }
            wire.extend_from_slice(&bytes);
        }
        // An always-ready body must still yield to cancellation/deadlines.
        tokio::task::yield_now().await;
    }
    let body = if status == 204 {
        Vec::new()
    } else {
        decode(wire, encoding.as_deref()).await?
    };
    Ok(body)
}
fn error(message: &str) -> CoreError {
    CoreError::new(ErrorKind::Transport, message)
}

// Only fixed labels are reported: lower-level error text can echo hostnames,
// addresses, or peer-supplied data, so it is never copied into the message.
fn send_failure(failure: &hyper_util::client::legacy::Error) -> CoreError {
    let reason = failure_reason(failure);
    let message = if failure.is_connect() {
        match reason {
            Some(reason) => {
                format!("Unable to connect to the service ({reason}); request was not sent")
            }
            None => "Unable to connect to the service; request was not sent".to_owned(),
        }
    } else {
        match reason {
            Some(reason) => {
                format!("HTTP send failed ({reason}); request outcome may be uncertain")
            }
            None => "HTTP send failed; request outcome may be uncertain".to_owned(),
        }
    };
    error(&message)
}

fn failure_reason(failure: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    let mut current = Some(failure);
    let mut depth = 0;
    while let Some(cause) = current {
        if let Some(reason) = cause_reason(cause) {
            return Some(reason);
        }
        depth += 1;
        if depth > 16 {
            break;
        }
        current = cause.source();
    }
    None
}

fn cause_reason(cause: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    const TLS: &str = "TLS handshake or certificate verification failed";
    if cause.downcast_ref::<rustls::Error>().is_some() {
        return Some(TLS);
    }
    // hyper-util's DNS ConnectError type is private; its fixed display text is stable.
    if cause.to_string() == "dns error" {
        return Some("DNS resolution failed");
    }
    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
        // io::Error::source skips the wrapped error, so inspect it explicitly.
        if io
            .get_ref()
            .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
        {
            return Some(TLS);
        }
        use std::io::ErrorKind as Io;
        return match io.kind() {
            Io::ConnectionRefused => Some("connection refused"),
            Io::TimedOut => Some("connection timed out"),
            Io::ConnectionReset | Io::ConnectionAborted | Io::BrokenPipe | Io::UnexpectedEof => {
                Some("connection reset or closed by peer")
            }
            Io::NetworkUnreachable | Io::HostUnreachable | Io::NetworkDown => {
                Some("network unreachable")
            }
            Io::AddrNotAvailable => Some("local address not available"),
            _ => None,
        };
    }
    if let Some(hyper) = cause.downcast_ref::<hyper::Error>() {
        if hyper.is_incomplete_message() {
            return Some("connection closed before the response completed");
        }
        if hyper.is_timeout() {
            return Some("timed out");
        }
        if hyper.is_parse() {
            return Some("malformed HTTP response");
        }
    }
    None
}
fn protocol(message: &str) -> CoreError {
    CoreError::new(ErrorKind::Protocol, message)
}

async fn decode(wire: Vec<u8>, encoding: Option<&str>) -> Result<Vec<u8>, CoreError> {
    let encoding = encoding.unwrap_or("identity");
    let mut reader: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + '_>> =
        if encoding.eq_ignore_ascii_case("gzip") {
            let mut decoder = GzipDecoder::new(wire.as_slice());
            decoder.multiple_members(true);
            Box::pin(decoder)
        } else if encoding.eq_ignore_ascii_case("deflate") {
            let mut decoder = ZlibDecoder::new(wire.as_slice());
            decoder.multiple_members(true);
            Box::pin(decoder)
        } else if encoding.eq_ignore_ascii_case("identity") {
            return Ok(wire);
        } else {
            return Err(CoreError::new(
                ErrorKind::Protocol,
                "Unsupported response content encoding",
            ));
        };
    let mut decoded = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|_| protocol("Invalid compressed response"))?;
        if count == 0 {
            break;
        }
        if decoded.len().saturating_add(count) > MAX_RESPONSE_BYTES {
            return Err(protocol(&format!(
                "Decompressed response exceeds the {MAX_RESPONSE_BYTES}-byte size limit"
            )));
        }
        decoded.extend_from_slice(&chunk[..count]);
        tokio::task::yield_now().await;
    }
    Ok(decoded)
}
#[async_trait]
impl HttpTransport for NativeHttpClient {
    async fn send(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        clock: &dyn Clock,
    ) -> Result<ApiResponse, CoreError> {
        let head = ResponseHeadCapture::for_url(&request.url);
        self.send_with_observer(request, context, clock, &head)
            .await
    }
    async fn send_with_observer(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        clock: &dyn Clock,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        bounded_with_clock(
            clock,
            context,
            self.send_once(request, context, clock, head),
        )
        .await
        .map_err(|error| head.attach(error))?
        .map_err(|error| head.attach(error))
    }
}

#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_gzip_and_identity_decode_offline() {
        let gzip = vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 19, 203, 72, 205, 201, 201, 7, 0, 134, 166, 16, 54, 5, 0,
            0, 0,
        ];
        assert_eq!(decode(gzip, Some("gzip")).await.unwrap(), b"hello");
        assert_eq!(decode(b"{}".to_vec(), None).await.unwrap(), b"{}");
        assert_eq!(
            decode(vec![0, 1, 2], Some("gzip")).await.unwrap_err().kind,
            ErrorKind::Protocol
        );
        assert_eq!(
            decode(vec![], Some("unsupported")).await.unwrap_err().kind,
            ErrorKind::Protocol
        );
    }

    #[tokio::test]
    async fn compressed_small_wire_cannot_expand_past_bound() {
        // Deterministic gzip member for one MiB of 'x'; 33 members fit in
        // 35 KiB wire bytes but exceed the independent decoded allocation bound.
        let mut member = vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 19, 237, 193, 49, 1, 0, 0, 0, 194, 160, 218, 139, 111, 8,
            95, 160,
        ];
        member.extend(std::iter::repeat_n(0, 1016));
        member.extend([62, 3, 50, 156, 59, 21, 0, 0, 16, 0]);
        let wire = member.repeat(33);
        assert!(wire.len() < 64 * 1024);
        assert_eq!(
            decode(wire, Some("gzip")).await.unwrap_err().kind,
            ErrorKind::Protocol
        );
    }

    #[test]
    fn failure_reasons_use_fixed_labels() {
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "SECRET-host");
        assert_eq!(failure_reason(&refused), Some("connection refused"));
        let tls = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::Expired),
        );
        assert_eq!(
            failure_reason(&tls),
            Some("TLS handshake or certificate verification failed")
        );
        let other = std::io::Error::other("SECRET-detail");
        assert_eq!(failure_reason(&other), None);
    }
}
