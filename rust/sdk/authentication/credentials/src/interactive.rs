// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit public-cloud device authorization. No browser, ambient credential,
//! access-token inspection, transport, runtime, or automatic interactive fallback.
//!
//! ID-token metadata is accepted only from the selected HTTPS token endpoint.
//! This uses authenticated token-endpoint provenance, not local JWT signature
//! verification. Caller transports must validate TLS and never follow redirects,
//! retry grants, or log request/response secrets (OIDC Core 3.1.3.7).

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, Secret, TokenCredential, TokenRequestOptions},
    error::{Error, ErrorKind},
    http::{HttpClient, Method, Request, Url},
    time::OffsetDateTime,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};
use tokio::time::Instant;

#[path = "interactive_json.rs"]
mod json;
#[path = "interactive_store.rs"]
mod store;
pub use store::{OsSessionStore, SavedSelection};

/// Explicitly trusted macOS protected-store helper configuration and protocol.
#[path = "interactive_macos.rs"]
pub mod macos;

const BODY_LIMIT: usize = 128 * 1024;
const MAX_LIFETIME: u64 = 86_400;
/// The three explicitly supported public-cloud resource scopes.
pub const SCOPES: [&str; 3] = [
    "https://api.fabric.microsoft.com/.default",
    "https://storage.azure.com/.default",
    "https://management.azure.com/.default",
];

/// Classified failures with no provider bodies, secret values or source chains.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InteractiveError {
    /// Tenant/client must be explicit GUIDs and hints must be bounded.
    #[error(
        "Device-code login requires explicit tenant and client GUIDs; account is an optional UPN hint."
    )]
    Configuration,
    /// No selected persistent session exists.
    #[error(
        "No device-code session is selected. Run auth login --method device-code --tenant <TENANT_GUID> --client-id <CLIENT_GUID>."
    )]
    NotConfigured,
    /// The tenant, application or immutable user changed.
    #[error("The token endpoint returned a different identity than the selected session.")]
    IdentityMismatch,
    /// Explicit login is needed; silent callers never prompt.
    #[error("Sign-in is required. Run auth login --method device-code explicitly.")]
    InteractionRequired,
    /// User declined or caller cancelled the operation.
    #[error("Device-code authentication was cancelled.")]
    Cancelled,
    /// The server-issued code expired.
    #[error("The device code expired. Start a new explicit login.")]
    Expired,
    /// Provider rejected this specific grant.
    #[error("The selected tenant rejected the authentication grant.")]
    Rejected,
    /// The injected transport or provider is unavailable.
    #[error("The authentication provider is unavailable; no fallback was attempted.")]
    Unavailable,
    /// Token response metadata is malformed, missing or expired.
    #[error("The authentication provider returned invalid token metadata.")]
    Response,
    /// OS credential storage is unavailable or locked; never use plaintext.
    #[error(
        "Cannot access the OS-protected credential store. Unlock Windows Credential Manager, macOS Keychain, or Linux Secret Service and retry. No file credential fallback is available."
    )]
    Store,
    /// Another process changed the session; no token is published.
    #[error("The device-code session changed or was signed out. Retry with the current session.")]
    SessionChanged,
    /// Unsupported target; do not use keyring's mock fallback.
    #[error("Device-code persistence requires Windows, macOS or Linux Secret Service.")]
    Unsupported,
    /// Only one known resource scope is accepted.
    #[error("Device-code credentials require exactly one Fabric, OneLake or ARM scope.")]
    Scope,
    /// The caller could not present the safe challenge.
    #[error("Cannot display the device-code sign-in instructions.")]
    Presentation,
}

/// Explicit directory/application selection; account is only a comparison hint.
#[derive(Clone)]
pub struct InteractiveOptions {
    tenant: String,
    client: String,
    account_hint: Option<String>,
}

impl InteractiveOptions {
    /// Validates explicit public-cloud GUIDs without inspecting ambient inputs.
    ///
    /// # Errors
    /// Returns a sanitized configuration failure for invalid selectors.
    pub fn new(
        tenant: impl Into<String>,
        client: impl Into<String>,
        account_hint: Option<String>,
    ) -> Result<Self, InteractiveError> {
        let tenant = tenant.into().to_ascii_lowercase();
        let client = client.into().to_ascii_lowercase();
        if !is_guid(&tenant)
            || !is_guid(&client)
            || account_hint.as_ref().is_some_and(|hint| {
                hint.len() > 320
                    || !hint.contains('@')
                    || !hint.bytes().all(|b| b.is_ascii_graphic())
            })
        {
            return Err(InteractiveError::Configuration);
        }
        Ok(Self {
            tenant,
            client,
            account_hint,
        })
    }

    /// Explicit directory GUID.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    /// Explicit public application GUID.
    pub fn client(&self) -> &str {
        &self.client
    }
}

impl fmt::Debug for InteractiveOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InteractiveOptions").finish_non_exhaustive()
    }
}

pub(super) fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// Non-secret identity metadata from the authenticated token-endpoint response.
/// `object_id` plus `tenant` is the identity key; UPN is mutable display metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    /// Explicit tenant, checked against returned `tid` and `iss`.
    pub tenant: String,
    /// Directory object GUID from the endpoint's ID token.
    pub object_id: String,
    /// App-specific immutable subject, checked on every refresh.
    pub subject: String,
    /// Mutable preferred username, never an authorization key.
    pub account: String,
}

impl SessionIdentity {
    pub(super) fn validate(&self) -> Result<(), InteractiveError> {
        if !is_guid(&self.tenant)
            || !is_guid(&self.object_id)
            || self.subject.is_empty()
            || self.subject.len() > 512
            || self.account.is_empty()
            || self.account.len() > 320
            || !self.subject.bytes().all(|b| b.is_ascii_graphic())
            || !self.account.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(InteractiveError::Response);
        }
        Ok(())
    }
}

/// Refresh material for an explicitly injected secure store. Never log/debug it.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshSession {
    /// Stable login generation; renewal cannot reopen another generation.
    pub session_id: u64,
    /// Exact application GUID.
    pub client: String,
    /// Bound immutable identity and display metadata.
    pub identity: SessionIdentity,
    /// OAuth refresh secret. Production stores must protect this with the OS.
    #[serde(with = "secret_serde")]
    pub refresh_token: Secret,
}

mod secret_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Secret,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.secret())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Secret, D::Error> {
        String::deserialize(deserializer).map(Secret::new)
    }
}

/// Snapshot revision used for cross-process compare-and-exchange.
#[derive(Clone)]
pub struct SessionSnapshot {
    /// Monotonic revision retained even after logout.
    pub revision: u64,
    /// None means explicitly unselected; no ambient fallback is permitted.
    pub session: Option<RefreshSession>,
}

/// Caller-injected durable protected store. Implementations must serialize writes,
/// enforce revision checks and commit secrets before publishing new selections.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Read one coherent snapshot without initiating UI.
    ///
    /// # Errors
    /// Returns sanitized storage errors, never secret/native error text.
    async fn load(&self) -> Result<SessionSnapshot, InteractiveError>;
    /// Replace a revision atomically; None invalidates prior credentials.
    ///
    /// # Errors
    /// Fails on stale revision or protected-store failure; never reports success
    /// before the rotated refresh token is durably committed.
    async fn compare_exchange(
        &self,
        revision: u64,
        session: Option<RefreshSession>,
    ) -> Result<u64, InteractiveError>;
}

/// Safe challenge projection: the device code and provider message are absent.
#[derive(Clone, Debug)]
pub struct DeviceChallenge {
    /// Allowlisted HTTPS URL without query/fragment/userinfo.
    pub verification_uri: String,
    /// Bounded human-entered user code, not the device credential.
    pub user_code: String,
    /// Server-defined remaining validity in seconds.
    pub expires_in: u64,
}

/// Explicit caller-owned presentation. Libraries never discover terminals.
pub trait DeviceCodeUi: Send + Sync {
    /// Display only safe challenge fields, generally on CLI stderr.
    ///
    /// # Errors
    /// Returns presentation failure without leaking provider text.
    fn present(&self, challenge: &DeviceChallenge) -> Result<(), InteractiveError>;
}

/// Non-secret successful acquisition metadata.
#[derive(Clone, Copy, Debug)]
pub struct TokenMetadata {
    /// Exact requested public-cloud scope.
    pub scope: &'static str,
    /// Server-derived access-token expiry in Unix seconds.
    pub expires_on: i64,
}

struct State {
    revision: u64,
    tokens: BTreeMap<&'static str, AccessToken>,
}

/// Device-code credential. Construction/login is explicit; SDK renewal is silent.
/// Uses the caller's Tokio runtime and injected no-redirect HTTPS client.
pub struct InteractiveCredential {
    options: InteractiveOptions,
    identity: SessionIdentity,
    session_id: u64,
    http: Arc<dyn HttpClient>,
    store: Arc<dyn SessionStore>,
    state: tokio::sync::Mutex<State>,
}

impl fmt::Debug for InteractiveCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InteractiveCredential")
            .finish_non_exhaustive()
    }
}

impl InteractiveCredential {
    /// Initiates device authorization only on explicit user login intent.
    /// Acquires all three resource tokens before publishing a selected session.
    /// Dropping this future cancels polling; it never opens a browser.
    ///
    /// # Errors
    /// Distinguishes decline, expiry, provider, metadata and persistence failures.
    pub async fn login(
        options: InteractiveOptions,
        http: Arc<dyn HttpClient>,
        store: Arc<dyn SessionStore>,
        ui: &dyn DeviceCodeUi,
    ) -> Result<Arc<Self>, InteractiveError> {
        let snapshot = store.load().await?;
        let requested_at = Instant::now();
        let (device, challenge, interval) = begin(&options, &http).await?;
        ui.present(&challenge)?;
        let deadline = requested_at + Duration::from_secs(challenge.expires_in);
        let mut interval = interval;
        let first = loop {
            if Instant::now() + interval >= deadline {
                tokio::time::sleep_until(deadline).await;
                return Err(InteractiveError::Expired);
            }
            tokio::time::sleep(interval).await;
            let result = tokio::time::timeout_at(
                deadline,
                token_request(
                    &options,
                    &http,
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("client_id", options.client()),
                        ("device_code", device.secret()),
                    ],
                ),
            )
            .await
            .map_err(|_| InteractiveError::Expired)??;
            match result {
                TokenResult::Success(value) => break decode_token(&options, value, None)?,
                TokenResult::Pending => {}
                TokenResult::SlowDown => interval = interval.saturating_add(Duration::from_secs(5)),
            }
        };
        let session_id = snapshot
            .revision
            .checked_add(1)
            .ok_or(InteractiveError::Store)?;
        let mut session = RefreshSession {
            session_id,
            client: options.client.clone(),
            identity: first.identity.clone(),
            refresh_token: first.refresh.ok_or(InteractiveError::Response)?,
        };
        let mut tokens = BTreeMap::new();
        tokens.insert(SCOPES[0], first.token);
        for scope in &SCOPES[1..] {
            let acquired = refresh(&options, &http, &session, scope).await?;
            if let Some(rotated) = acquired.refresh {
                session.refresh_token = rotated;
            }
            tokens.insert(*scope, acquired.token);
        }
        if tokens
            .values()
            .any(|token| token.expires_on <= OffsetDateTime::now_utc())
        {
            return Err(InteractiveError::Response);
        }
        let revision = store
            .compare_exchange(snapshot.revision, Some(session.clone()))
            .await?;
        Ok(Arc::new(Self {
            options,
            identity: session.identity,
            session_id,
            http,
            store,
            state: tokio::sync::Mutex::new(State { revision, tokens }),
        }))
    }

    /// Connects only to the explicitly selected stored session; never prompts.
    ///
    /// # Errors
    /// Missing, stale or mismatched selection fails without device authorization.
    pub async fn connect(
        options: InteractiveOptions,
        http: Arc<dyn HttpClient>,
        store: Arc<dyn SessionStore>,
    ) -> Result<Arc<Self>, InteractiveError> {
        let snapshot = store.load().await?;
        let session = snapshot.session.ok_or(InteractiveError::NotConfigured)?;
        check_selection(&options, &session)?;
        Ok(Arc::new(Self {
            options,
            identity: session.identity,
            session_id: session.session_id,
            http,
            store,
            state: tokio::sync::Mutex::new(State {
                revision: snapshot.revision,
                tokens: BTreeMap::new(),
            }),
        }))
    }

    /// Bound endpoint-provenance identity; not a claim of MFA or service access.
    pub fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    /// Silently validates/acquires every supported resource, returning no secrets.
    ///
    /// # Errors
    /// Any resource/rotation/storage failure prevents a successful status result.
    pub async fn status(&self) -> Result<Vec<TokenMetadata>, InteractiveError> {
        let mut result = Vec::new();
        for scope in SCOPES {
            let token = self.acquire(scope).await?;
            result.push(TokenMetadata {
                scope,
                expires_on: token.expires_on.unix_timestamp(),
            });
        }
        Ok(result)
    }

    /// Invalidates this exact selected generation, including cached SDK tokens.
    /// Does not revoke already issued server-side access tokens.
    ///
    /// # Errors
    /// Stale credentials cannot clear a newer login; failures remain explicit.
    pub async fn logout(&self) -> Result<(), InteractiveError> {
        let mut state = self.state.lock().await;
        let snapshot = self.current().await?;
        self.store.compare_exchange(snapshot.revision, None).await?;
        state.tokens.clear();
        Ok(())
    }

    async fn current(&self) -> Result<SessionSnapshot, InteractiveError> {
        let snapshot = self.store.load().await?;
        let session = snapshot
            .session
            .as_ref()
            .ok_or(InteractiveError::SessionChanged)?;
        if session.session_id != self.session_id || session.identity != self.identity {
            return Err(InteractiveError::SessionChanged);
        }
        check_selection(&self.options, session)?;
        Ok(snapshot)
    }

    async fn acquire(&self, scope: &'static str) -> Result<AccessToken, InteractiveError> {
        let mut state = self.state.lock().await;
        let snapshot = self.current().await?;
        if snapshot.revision == state.revision
            && let Some(token) = state.tokens.get(scope)
            && token.expires_on
                > OffsetDateTime::now_utc() + azure_core::time::Duration::seconds(120)
        {
            return Ok(token.clone());
        }
        let mut session = snapshot.session.ok_or(InteractiveError::SessionChanged)?;
        let acquired = refresh(&self.options, &self.http, &session, scope).await?;
        if let Some(rotated) = acquired.refresh {
            session.refresh_token = rotated;
        }
        // Even nonrotating grants CAS to reject a logout concurrent with HTTP.
        let revision = self
            .store
            .compare_exchange(snapshot.revision, Some(session))
            .await?;
        if acquired.token.expires_on <= OffsetDateTime::now_utc() {
            return Err(InteractiveError::Response);
        }
        state.revision = revision;
        state.tokens.insert(scope, acquired.token.clone());
        Ok(acquired.token)
    }
}

#[async_trait]
impl TokenCredential for InteractiveCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let scope = match scopes {
            [scope] => SCOPES.into_iter().find(|known| known == scope),
            _ => None,
        }
        .ok_or_else(|| Error::new(ErrorKind::Credential, InteractiveError::Scope))?;
        self.acquire(scope)
            .await
            .map_err(|error| Error::new(ErrorKind::Credential, error))
    }
}

fn check_selection(
    options: &InteractiveOptions,
    session: &RefreshSession,
) -> Result<(), InteractiveError> {
    session.identity.validate()?;
    if session.session_id == 0
        || !session
            .identity
            .tenant
            .eq_ignore_ascii_case(options.tenant())
        || !session.client.eq_ignore_ascii_case(options.client())
        || options
            .account_hint
            .as_ref()
            .is_some_and(|hint| !hint.eq_ignore_ascii_case(&session.identity.account))
    {
        return Err(InteractiveError::IdentityMismatch);
    }
    Ok(())
}

async fn begin(
    options: &InteractiveOptions,
    http: &Arc<dyn HttpClient>,
) -> Result<(Secret, DeviceChallenge, Duration), InteractiveError> {
    let scope = format!("{} openid profile offline_access", SCOPES[0]);
    let (status, value) = post(
        options,
        http,
        "devicecode",
        &[("client_id", options.client()), ("scope", &scope)],
    )
    .await?;
    if status != 200 {
        return Err(classify(&value));
    }
    let device = field(&value, "device_code", 32_768)?;
    let user_code = field(&value, "user_code", 32)?;
    if !user_code
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(InteractiveError::Response);
    }
    let uri = field(&value, "verification_uri", 256)?;
    if !matches!(
        uri.as_str(),
        "https://microsoft.com/devicelogin"
            | "https://www.microsoft.com/devicelogin"
            | "https://login.microsoftonline.com/common/oauth2/deviceauth"
    ) {
        return Err(InteractiveError::Response);
    }
    let expires_in = lifetime(&value, "expires_in")?;
    let interval = match value.get("interval") {
        None => 5,
        Some(value) => value
            .as_u64()
            .filter(|v| *v > 0 && *v <= 600)
            .ok_or(InteractiveError::Response)?,
    };
    Ok((
        Secret::new(device),
        DeviceChallenge {
            verification_uri: uri,
            user_code,
            expires_in,
        },
        Duration::from_secs(interval.max(5)),
    ))
}

enum TokenResult {
    Success(serde_json::Value),
    Pending,
    SlowDown,
}

async fn token_request(
    options: &InteractiveOptions,
    http: &Arc<dyn HttpClient>,
    form: &[(&str, &str)],
) -> Result<TokenResult, InteractiveError> {
    let (status, value) = post(options, http, "token", form).await?;
    if status == 200 {
        return Ok(TokenResult::Success(value));
    }
    if status == 400 {
        match value.get("error").and_then(serde_json::Value::as_str) {
            Some("authorization_pending") => return Ok(TokenResult::Pending),
            Some("slow_down") => return Ok(TokenResult::SlowDown),
            _ => {}
        }
    }
    Err(classify(&value))
}

async fn post(
    options: &InteractiveOptions,
    http: &Arc<dyn HttpClient>,
    leaf: &str,
    form: &[(&str, &str)],
) -> Result<(u16, serde_json::Value), InteractiveError> {
    let endpoint = Url::parse(&format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/{leaf}",
        options.tenant()
    ))
    .map_err(|_| InteractiveError::Configuration)?;
    let mut encoded =
        Url::parse("https://form.invalid/").map_err(|_| InteractiveError::Configuration)?;
    encoded.query_pairs_mut().extend_pairs(form.iter().copied());
    let mut request = Request::new(endpoint, Method::Post);
    request.insert_header("content-type", "application/x-www-form-urlencoded");
    request.insert_header("accept", "application/json");
    request.set_body(encoded.query().unwrap_or_default().to_owned());
    let response = http
        .execute_request(&request)
        .await
        .map_err(|_| InteractiveError::Unavailable)?;
    let status: u16 = response.status().into();
    if status == 429 || status >= 500 {
        return Err(InteractiveError::Unavailable);
    }
    if !(status == 200 || status == 400 || status == 401 || status == 403) {
        return Err(InteractiveError::Rejected);
    }
    let (_, headers, body) = response.deconstruct();
    if headers
        .get_str(&"content-type".into())
        .ok()
        .is_none_or(|value| {
            !value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(InteractiveError::Response);
    }
    let mut bytes = vec![0; BODY_LIMIT];
    let length = body
        .collect_into(&mut bytes)
        .await
        .map_err(|_| InteractiveError::Response)?;
    bytes.truncate(length);
    Ok((status, json::parse(&bytes)?))
}

fn classify(value: &serde_json::Value) -> InteractiveError {
    match value.get("error").and_then(serde_json::Value::as_str) {
        Some("authorization_declined" | "access_denied") => InteractiveError::Cancelled,
        Some("expired_token") => InteractiveError::Expired,
        Some("interaction_required" | "login_required" | "consent_required" | "invalid_grant") => {
            InteractiveError::InteractionRequired
        }
        Some("temporarily_unavailable" | "server_error") => InteractiveError::Unavailable,
        _ => InteractiveError::Rejected,
    }
}

fn field(value: &serde_json::Value, name: &str, limit: usize) -> Result<String, InteractiveError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= limit && value.bytes().all(|b| b.is_ascii_graphic())
        })
        .map(str::to_owned)
        .ok_or(InteractiveError::Response)
}

fn lifetime(value: &serde_json::Value, name: &str) -> Result<u64, InteractiveError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0 && *value <= MAX_LIFETIME)
        .ok_or(InteractiveError::Response)
}

struct Acquired {
    identity: SessionIdentity,
    token: AccessToken,
    refresh: Option<Secret>,
}

fn decode_token(
    options: &InteractiveOptions,
    value: serde_json::Value,
    expected: Option<&SessionIdentity>,
) -> Result<Acquired, InteractiveError> {
    if field(&value, "token_type", 16)? != "Bearer" {
        return Err(InteractiveError::Response);
    }
    let token = field(&value, "access_token", 65_536)?;
    let expires = lifetime(&value, "expires_in")?;
    let expires_on = OffsetDateTime::now_utc()
        .checked_add(azure_core::time::Duration::seconds(expires as i64))
        .ok_or(InteractiveError::Response)?;
    let id = field(&value, "id_token", 65_536)?;
    let parts: Vec<_> = id.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(InteractiveError::Response);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| InteractiveError::Response)?;
    let claims = json::parse(&payload)?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if field(&claims, "aud", 36)?.to_ascii_lowercase() != options.client
        || field(&claims, "tid", 36)?.to_ascii_lowercase() != options.tenant
        || field(&claims, "iss", 256)?
            != format!("https://login.microsoftonline.com/{}/v2.0", options.tenant)
        || claims
            .get("exp")
            .and_then(serde_json::Value::as_i64)
            .is_none_or(|exp| exp <= now)
        || claims
            .get("nbf")
            .and_then(serde_json::Value::as_i64)
            .is_none_or(|nbf| nbf > now + 60)
    {
        return Err(InteractiveError::IdentityMismatch);
    }
    let identity = SessionIdentity {
        tenant: options.tenant.clone(),
        object_id: field(&claims, "oid", 36)?.to_ascii_lowercase(),
        subject: field(&claims, "sub", 512)?,
        account: field(&claims, "preferred_username", 320)?,
    };
    identity.validate()?;
    if expected.is_some_and(|expected| {
        expected.tenant != identity.tenant
            || expected.object_id != identity.object_id
            || expected.subject != identity.subject
    }) || options
        .account_hint
        .as_ref()
        .is_some_and(|hint| !hint.eq_ignore_ascii_case(&identity.account))
    {
        return Err(InteractiveError::IdentityMismatch);
    }
    let refresh = if value.get("refresh_token").is_some() {
        Some(Secret::new(field(&value, "refresh_token", 32_768)?))
    } else {
        None
    };
    Ok(Acquired {
        identity,
        token: AccessToken::new(token, expires_on),
        refresh,
    })
}

async fn refresh(
    options: &InteractiveOptions,
    http: &Arc<dyn HttpClient>,
    session: &RefreshSession,
    scope: &str,
) -> Result<Acquired, InteractiveError> {
    let scope = format!("{scope} openid profile offline_access");
    let result = token_request(
        options,
        http,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", options.client()),
            ("refresh_token", session.refresh_token.secret()),
            ("scope", &scope),
        ],
    )
    .await?;
    match result {
        TokenResult::Success(value) => decode_token(options, value, Some(&session.identity)),
        _ => Err(InteractiveError::Response),
    }
}

#[cfg(test)]
#[path = "interactive_tests.rs"]
mod tests;
