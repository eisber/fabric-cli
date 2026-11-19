// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private Shared issuer execution over the existing injected Azure HTTP seam.
//! Redirects and retries must be disabled by the caller's transport. No request
//! is replayed here, including GET, throttling or uncertain POST outcomes.

use azure_core::{
    credentials::TokenCredential,
    http::{HttpClient, Method, Request, Url},
    time::OffsetDateTime,
};
use fabric_sdk::maps::{
    MAX_RESPONSE_BYTES, POWER_BI_SCOPE, PreparedRequest, ProcessingResult, shared_issuer,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Explicit sanitized failure evidence, with no raw URLs, bodies or credentials.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    /// Stable error category.
    pub code: &'static str,
    /// Actionable static diagnostic.
    pub message: &'static str,
    /// HTTP status, when actually observed.
    pub http_status: Option<u16>,
    /// Bounded integer Retry-After, if supplied; no automatic retry is performed.
    pub retry_after_seconds: Option<u64>,
    /// A dispatched POST may have completed remotely; never automatically replay.
    pub post_may_have_been_sent: bool,
}

impl Failure {
    /// Construct a sanitized local failure.
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            http_status: None,
            retry_after_seconds: None,
            post_may_have_been_sent: false,
        }
    }
}

fn status_failure(status: u16, retry_after: Option<u64>) -> Failure {
    let (code, message) = match status {
        401 => (
            "CredentialExpiredOrRefused",
            "Maps credentials expired or were refused. Verify native client admission; explicitly retry only after resolving identity/expiry. No refresh or replay was attempted.",
        ),
        403 => (
            "AdmissionDenied",
            "Private Maps contract admission or entitlement was denied. Verify this client, user, tenant and workload agreement; do not switch client identity or bypass feature gates.",
        ),
        404 => (
            "UnsupportedContract",
            "This private Shared issuer or provider operation is unavailable. No GeoIntel endpoint or alternate credential fallback was attempted.",
        ),
        429 => (
            "Throttled",
            "Maps throttled the request. Respect Retry-After and explicitly retry after checking whether processing occurred; no request was replayed.",
        ),
        300..=399 => (
            "UntrustedRedirect",
            "Maps redirects are not followed; no credential was forwarded to the redirect destination.",
        ),
        _ => (
            "ProviderFailed",
            "Maps returned a failing HTTP status. No raw provider text or request replay is exposed.",
        ),
    };
    Failure {
        http_status: Some(status),
        retry_after_seconds: retry_after,
        ..Failure::new(code, message)
    }
}

fn json(bytes: &[u8]) -> Result<Value, Failure> {
    serde_json::from_slice(bytes).map_err(|_| {
        Failure::new(
            "InvalidResponse",
            "Maps returned invalid JSON. No response body is included.",
        )
    })
}

fn safe_secret(secret: &str) -> bool {
    !secret.is_empty() && secret.len() <= 16 * 1024 && secret.bytes().all(|c| c.is_ascii_graphic())
}

async fn exchange(
    http: &dyn HttpClient,
    method: Method,
    url: &Url,
    authorization: String,
    body: &[u8],
    max_bytes: usize,
) -> Result<Vec<u8>, Failure> {
    let mut request = Request::new(url.clone(), method);
    request.insert_header("authorization", authorization);
    request.insert_header("accept", "application/json, application/geo+json");
    if method == Method::Post {
        request.insert_header("content-type", "application/geo+json");
        request.set_body(body.to_vec());
    }
    let response = http.execute_request(&request).await.map_err(|_| {
        Failure::new(
            "TransportFailed",
            "Maps transport failed; remote processing may be uncertain. No request was replayed.",
        )
    })?;
    let (status, headers, mut stream) = response.deconstruct();
    let status = u16::from(status);
    let retry_after = headers
        .iter()
        .find(|(name, _)| name.as_str().eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.as_str().parse::<u64>().ok())
        .filter(|v| *v <= 86400);
    if status != 200 {
        return Err(status_failure(status, retry_after));
    }
    let mut bytes = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            Failure::new(
                "TransportFailed",
                "Maps response streaming failed; no request was replayed.",
            )
        })?;
        if chunk.len() > max_bytes - bytes.len() {
            return Err(Failure::new(
                "ResponseLimit",
                "Maps response exceeded its decoded byte budget.",
            ));
        }
        bytes.extend_from_slice(&chunk);
        chunks += 1;
        if chunks == 64 {
            chunks = 0;
            tokio::task::yield_now().await;
        }
    }
    Ok(bytes)
}

fn contains_sensitive_field(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_sensitive_field),
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "accountsastoken"
                    | "access_token"
                    | "refresh_token"
                    | "authorization"
                    | "subscription-key"
            ) || contains_sensitive_field(value)
        }),
        _ => false,
    }
}

fn contains_credential(value: &Value, secrets: &[&str]) -> bool {
    let contains = |text: &str| secrets.iter().any(|secret| text.contains(secret));
    match value {
        Value::String(text) => contains(text),
        Value::Array(values) => values
            .iter()
            .any(|value| contains_credential(value, secrets)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| contains(key) || contains_credential(value, secrets)),
        _ => false,
    }
}

/// Validate explicitly synthetic offline data without identity or networking.
///
/// Credential-bearing fields are rejected even in a local fixture, so this
/// diagnostic path cannot publish an issuer response as processing output.
pub fn offline_fixture(
    request: &PreparedRequest,
    bytes: &[u8],
) -> Result<ProcessingResult, Failure> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(Failure::new(
            "ResponseLimit",
            "Offline fixture exceeded its decoded byte budget.",
        ));
    }
    let payload = json(bytes)?;
    if contains_sensitive_field(&payload) {
        return Err(Failure::new(
            "CredentialInResponse",
            "Credential-bearing fixture data was withheld from output and exports.",
        ));
    }
    ProcessingResult::parse(request, bytes).map_err(|_| {
        Failure::new(
            "InvalidFixture",
            "The synthetic offline fixture does not match this processing operation.",
        )
    })
}

/// Execute exactly one admitted processing operation with memory-only SAS.
///
/// The caller must inject its existing native user credential, not an Azure CLI
/// cache, static token, subscription key or impersonated client. One total
/// timeout bounds token acquisition, discovery, issuance and body streaming.
pub async fn execute(
    request: &PreparedRequest,
    experimental_private_contract: bool,
    timeout: Duration,
    credential: Arc<dyn TokenCredential>,
    http: Arc<dyn HttpClient>,
) -> Result<ProcessingResult, Failure> {
    if !experimental_private_contract {
        return Err(Failure::new(
            "PrivateContractOptInRequired",
            "Maps uses a private report-oriented Shared SAS contract. Supply --experimental-private-contract only with applicable admission and entitlement; this does not qualify public Fabric v1 support.",
        ));
    }
    if timeout.is_zero() || timeout > Duration::from_secs(120) {
        return Err(Failure::new(
            "InvalidInput",
            "Maps deadline must be between 1 and 120 seconds.",
        ));
    }
    // PreparedRequest is immutable and only built by fixed-origin validation.
    let post_sent = AtomicBool::new(false);
    let work = async {
        let discovery = Url::parse("https://api.powerbi.com/metadata/cluster").map_err(|_| {
            Failure::new("InvalidConfiguration", "Invalid compiled discovery origin.")
        })?;
        let token = credential.get_token(&[POWER_BI_SCOPE], None).await
            .map_err(|_| Failure::new("IdentityUnavailable", "The selected native credential could not acquire the private Power BI issuer scope silently. Check client admission and existing user sign-in; app-only and alternate-client fallback are unsupported."))?;
        let bearer = token.token.secret();
        if !safe_secret(bearer) || token.expires_on <= OffsetDateTime::now_utc() {
            return Err(Failure::new(
                "CredentialExpired",
                "The native credential is empty, invalid or expired; no service request was sent.",
            ));
        }
        let discovery_body = exchange(
            http.as_ref(),
            Method::Get,
            &discovery,
            format!("Bearer {bearer}"),
            &[],
            64 * 1024,
        )
        .await?;
        let discovery_json = json(&discovery_body)?;
        let backend = discovery_json["backendUrl"].as_str().ok_or_else(|| {
            Failure::new("InvalidDiscovery", "Shared discovery omitted backendUrl.")
        })?;
        let issuer = shared_issuer(backend).map_err(|_| Failure::new("UntrustedOrigin", "Shared discovery returned an untrusted HTTPS origin. No credential was forwarded to it."))?;
        let issuer_body = exchange(
            http.as_ref(),
            Method::Get,
            &issuer,
            format!("Bearer {bearer}"),
            &[],
            64 * 1024,
        )
        .await?;
        let issuer_json = json(&issuer_body)?;
        let sas = issuer_json["accountSasToken"].as_str().filter(|v| safe_secret(v))
            .ok_or_else(|| Failure::new("InvalidIssuerResponse", "Shared did not return a valid memory-only accountSasToken; no provider request was sent."))?;
        post_sent.store(request.method() == Method::Post, Ordering::Relaxed);
        let bytes = exchange(
            http.as_ref(),
            request.method(),
            request.url(),
            format!("jwt-sas {sas}"),
            request.body(),
            MAX_RESPONSE_BYTES,
        )
        .await?;
        let payload = json(&bytes)?;
        if contains_credential(&payload, &[bearer, sas]) || contains_sensitive_field(&payload) {
            return Err(Failure::new(
                "CredentialInResponse",
                "Maps response contained credential material and was withheld from output and exports.",
            ));
        }
        ProcessingResult::parse(request, &bytes).map_err(|_| {
            Failure::new(
                "InvalidResponse",
                "Maps returned malformed or incomplete processing data; no body is included.",
            )
        })
    };
    let result = tokio::time::timeout(timeout, work).await
        .unwrap_or_else(|_| Err(Failure::new("TimedOut", "Maps total deadline elapsed. Local waiting stopped; an in-flight POST may still complete. No replay or remote cancellation was attempted.")));
    result.map_err(|mut failure| {
        failure.post_may_have_been_sent = post_sent.load(Ordering::Relaxed);
        failure
    })
}
