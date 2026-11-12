// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fail-closed public Entra RS256 access-token verification. Selection metadata
//! and decoded claims never constitute a principal proof.

use crate::{AccessToken, Audience, CredentialSelection, TokenCredential};
use azure_core::{http::Url, time::OffsetDateTime};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

const TOKEN_LIMIT: usize = 32 * 1024;
/// Maximum decoded JWKS response size, also enforced while streaming by adapters.
pub const JWKS_LIMIT: usize = 128 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(3600);
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const MAX_LIFETIME: i64 = 24 * 3600;

/// Sanitized identity-proof failures; no provider data or error chains are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PrincipalError {
    /// The account, tenant or requested capability is not explicitly supported.
    #[error("unsupported explicit principal selection")]
    Selection,
    /// The selected provider could not acquire the requested token.
    #[error("selected principal token acquisition failed")]
    Credential,
    /// A token's format, signature, claims or lifetime could not be verified.
    #[error("access token principal verification failed")]
    Token,
    /// The issuer-bound key document is invalid, unavailable or exceeds its bound.
    #[error("issuer signing keys could not be verified")]
    Keys,
    /// Verified tokens do not belong to the same explicitly selected principal.
    #[error("verified access token principal changed")]
    Continuity,
}

/// An exact known public-cloud key endpoint, constructed only by the verifier.
#[derive(Clone, Debug)]
pub struct EntraJwksEndpoint(Url);

impl EntraJwksEndpoint {
    /// Borrow the fixed issuer/tenant-bound HTTPS key endpoint.
    pub fn url(&self) -> &Url {
        &self.0
    }
}

/// Caller-owned no-redirect, no-retry HTTP hook for the known Entra key endpoint.
///
/// Implementations must bound the stream to [`JWKS_LIMIT`], discard provider
/// diagnostics and cancel local I/O when dropped. The operation owner supplies
/// its original absolute deadline and cancellation around the entire verifier.
pub trait EntraJwksSource: Send + Sync {
    /// Fetch one key document, without credentials, redirects or retries.
    fn fetch<'a>(
        &'a self,
        endpoint: &'a EntraJwksEndpoint,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PrincipalError>> + Send + 'a>>;
}

/// Canonical non-nil lowercase GUID, without parsing or normalization ambiguity.
pub fn canonical_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && value
            .bytes()
            .any(|byte| matches!(byte, b'1'..=b'9' | b'a'..=b'f'))
}

fn login(value: &str) -> bool {
    value.len() <= 320
        && value.split('@').count() == 2
        && !value.starts_with('@')
        && !value.ends_with('@')
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Checks that metadata can be verified without acquiring a credential.
///
/// User selectors are explicit ASCII logins; application selectors are canonical
/// client GUIDs. Opaque labels and sovereign or unselected tenants are unsupported.
///
/// # Errors
/// Rejects unsupported selection before token or HTTP access.
pub fn validate_selection(selection: &CredentialSelection) -> Result<(), PrincipalError> {
    if selection.audience() != Audience::Fabric
        || !canonical_guid(selection.tenant())
        || !(canonical_guid(selection.account()) || login(selection.account()))
    {
        return Err(PrincipalError::Selection);
    }
    Ok(())
}

/// A signature-verified principal. Values cannot be constructed from caller claims.
#[derive(Clone, Eq, PartialEq)]
pub struct VerifiedPrincipal {
    tenant: String,
    object: String,
    account: String,
    application: bool,
}

impl VerifiedPrincipal {
    /// Verified directory object ID, not a UPN or application/client ID.
    pub fn object_id(&self) -> &str {
        &self.object
    }
    /// Verified resource tenant.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    /// Verified selected login or application/client ID.
    pub fn account(&self) -> &str {
        &self.account
    }
    /// Whether this proof belongs to an application rather than a user.
    pub const fn is_application(&self) -> bool {
        self.application
    }
}

impl fmt::Debug for VerifiedPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedPrincipal(<verified>)")
    }
}

/// Memory-only PBI token bound to both verified Fabric and PBI principal proofs.
pub struct VerifiedPbiToken {
    token: AccessToken,
    principal: VerifiedPrincipal,
    expires: OffsetDateTime,
}

impl VerifiedPbiToken {
    /// The exact PBI access token; never persist, format or log its secret.
    pub fn token(&self) -> &AccessToken {
        &self.token
    }
    /// Principal verified independently on both access tokens.
    pub fn principal(&self) -> &VerifiedPrincipal {
        &self.principal
    }
    /// Earliest verified token/provider expiry across the selected token pair.
    pub const fn expires_on(&self) -> OffsetDateTime {
        self.expires
    }
}

impl fmt::Debug for VerifiedPbiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedPbiToken(<redacted>)")
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
    typ: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct Claims {
    aud: String,
    iss: String,
    tid: String,
    oid: String,
    exp: i64,
    nbf: i64,
    iat: i64,
    ver: String,
    idtyp: Option<String>,
    appid: Option<String>,
    azp: Option<String>,
    upn: Option<String>,
    preferred_username: Option<String>,
    unique_name: Option<String>,
    xms_mirid: Option<String>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    n: String,
    e: String,
    issuer: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    key_ops: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

struct Document {
    keys: Vec<Jwk>,
    fetched: Instant,
    refreshed: Option<Instant>,
}

/// Production RS256 verifier over known, tenant-bound public Entra JWKS.
///
/// The two issuer versions have separate bounded caches. A key miss can refresh
/// once per five minutes; signature failure never fetches a replacement key.
/// Concurrent verification shares the asynchronous cache lock, not a network-
/// blocking setup worker. Unsupported opaque/encrypted tokens fail honestly.
pub struct EntraJwtVerifier {
    selection: CredentialSelection,
    source: Arc<dyn EntraJwksSource>,
    documents: tokio::sync::Mutex<[Option<Document>; 2]>,
}

impl fmt::Debug for EntraJwtVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntraJwtVerifier(<explicit>)")
    }
}

impl EntraJwtVerifier {
    /// Bind one supported account and tenant without I/O.
    ///
    /// # Errors
    /// Rejects unsupported metadata before provider or HTTP access.
    pub fn new(
        selection: CredentialSelection,
        source: Arc<dyn EntraJwksSource>,
    ) -> Result<Self, PrincipalError> {
        validate_selection(&selection)?;
        Ok(Self {
            selection,
            source,
            documents: tokio::sync::Mutex::new([None, None]),
        })
    }

    /// Acquire exactly one Fabric and one PBI token from the same selected provider.
    ///
    /// All claims, including account and tenant, are cryptographically checked.
    /// No fallback, identity discovery, opaque-token acceptance or replay occurs.
    ///
    /// # Errors
    /// Returns only sanitized acquisition, verification and continuity failures.
    pub async fn acquire_pbi(
        &self,
        credential: &dyn TokenCredential,
    ) -> Result<VerifiedPbiToken, PrincipalError> {
        let fabric = credential
            .get_token(&[Audience::Fabric.scope()], None)
            .await
            .map_err(|_| PrincipalError::Credential)?;
        let (first, fabric_expiry) = self.verify(&fabric, Audience::Fabric).await?;
        let pbi = credential
            .get_token(&[Audience::PowerBiBootstrap.scope()], None)
            .await
            .map_err(|_| PrincipalError::Credential)?;
        let (second, pbi_expiry) = self.verify(&pbi, Audience::PowerBiBootstrap).await?;
        if first != second {
            return Err(PrincipalError::Continuity);
        }
        let expires = fabric_expiry.min(pbi_expiry);
        if expires.unix_timestamp() <= OffsetDateTime::now_utc().unix_timestamp() + 60 {
            return Err(PrincipalError::Token);
        }
        Ok(VerifiedPbiToken {
            token: pbi,
            principal: second,
            expires,
        })
    }

    async fn verify(
        &self,
        token: &AccessToken,
        audience: Audience,
    ) -> Result<(VerifiedPrincipal, OffsetDateTime), PrincipalError> {
        let text = token.token.secret();
        if text.len() > TOKEN_LIMIT || !text.is_ascii() {
            return Err(PrincipalError::Token);
        }
        let parts: Vec<_> = text.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
            return Err(PrincipalError::Token);
        }
        let header: Header = decode_json(parts[0])?;
        if header.alg != "RS256"
            || header.kid.is_empty()
            || header.kid.len() > 128
            || !header.kid.bytes().all(|byte| byte.is_ascii_graphic())
            || header.typ.as_deref().is_some_and(|typ| typ != "JWT")
            || header
                .extra
                .keys()
                .any(|name| matches!(name.as_str(), "crit" | "jku" | "jwk" | "x5u" | "b64"))
        {
            return Err(PrincipalError::Token);
        }
        let claims: Claims = decode_json(parts[1])?;
        let version = match claims.ver.as_str() {
            "1.0" => 0,
            "2.0" => 1,
            _ => return Err(PrincipalError::Token),
        };
        let issuer = if version == 0 {
            format!("https://sts.windows.net/{}/", self.selection.tenant())
        } else {
            format!(
                "https://login.microsoftonline.com/{}/v2.0",
                self.selection.tenant()
            )
        };
        let expected_audience = match audience {
            Audience::Fabric => "https://api.fabric.microsoft.com",
            Audience::PowerBiBootstrap => "https://analysis.windows.net/powerbi/api",
            _ => return Err(PrincipalError::Selection),
        };
        if claims.iss != issuer
            || claims.tid != self.selection.tenant()
            || claims.aud != expected_audience
            || !canonical_guid(&claims.oid)
        {
            return Err(PrincipalError::Token);
        }
        check_lifetime(&claims, token.expires_on)?;
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| PrincipalError::Token)?;
        let mut documents = self.documents.lock().await;
        if documents[version]
            .as_ref()
            .is_none_or(|doc| doc.fetched.elapsed() >= CACHE_TTL)
        {
            documents[version] = Some(self.fetch(version, None).await?);
        }
        let document = documents[version].as_mut().ok_or(PrincipalError::Keys)?;
        if !document.keys.iter().any(|key| key.kid == header.kid)
            && document
                .refreshed
                .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL)
        {
            let attempted = Instant::now();
            document.refreshed = Some(attempted);
            *document = self.fetch(version, Some(attempted)).await?;
        }
        let key = document
            .keys
            .iter()
            .find(|key| key.kid == header.kid)
            .ok_or(PrincipalError::Keys)?;
        let key_issuer = key.issuer.replace("{tenantid}", self.selection.tenant());
        if key_issuer != issuer
            || key.kty != "RSA"
            || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
            || key.usage.as_deref().is_some_and(|usage| usage != "sig")
            || key
                .key_ops
                .as_ref()
                .is_some_and(|ops| ops.as_slice() != ["verify"])
        {
            return Err(PrincipalError::Keys);
        }
        let n = URL_SAFE_NO_PAD
            .decode(&key.n)
            .map_err(|_| PrincipalError::Keys)?;
        let e = URL_SAFE_NO_PAD
            .decode(&key.e)
            .map_err(|_| PrincipalError::Keys)?;
        if !(256..=512).contains(&n.len()) || n[0] < 128 || e != [1, 0, 1] {
            return Err(PrincipalError::Keys);
        }
        let signing_input = &text[..parts[0].len() + 1 + parts[1].len()];
        RsaPublicKeyComponents { n: &n, e: &e }
            .verify(
                &RSA_PKCS1_2048_8192_SHA256,
                signing_input.as_bytes(),
                &signature,
            )
            .map_err(|_| PrincipalError::Token)?;
        drop(documents);
        check_lifetime(&claims, token.expires_on)?;
        let application = canonical_guid(self.selection.account());
        let account = if application {
            if claims.idtyp.as_deref() != Some("app") {
                return Err(PrincipalError::Token);
            }
            let client = claims
                .azp
                .as_deref()
                .or(claims.appid.as_deref())
                .ok_or(PrincipalError::Token)?;
            if !canonical_guid(client)
                || claims
                    .appid
                    .as_ref()
                    .zip(claims.azp.as_ref())
                    .is_some_and(|(first, second)| first != second)
            {
                return Err(PrincipalError::Token);
            }
            client
        } else {
            if claims.idtyp.as_deref() == Some("app") || claims.xms_mirid.is_some() {
                return Err(PrincipalError::Token);
            }
            claims
                .upn
                .as_deref()
                .or(claims.preferred_username.as_deref())
                .or(claims.unique_name.as_deref())
                .filter(|value| login(value))
                .ok_or(PrincipalError::Token)?
        };
        if !account.eq_ignore_ascii_case(self.selection.account()) {
            return Err(PrincipalError::Continuity);
        }
        let expires = OffsetDateTime::from_unix_timestamp(claims.exp)
            .map_err(|_| PrincipalError::Token)?
            .min(token.expires_on);
        Ok((
            VerifiedPrincipal {
                tenant: claims.tid,
                object: claims.oid,
                account: account.to_ascii_lowercase(),
                application,
            },
            expires,
        ))
    }

    async fn fetch(
        &self,
        version: usize,
        refreshed: Option<Instant>,
    ) -> Result<Document, PrincipalError> {
        let suffix = if version == 0 {
            "discovery/keys"
        } else {
            "discovery/v2.0/keys"
        };
        let endpoint = EntraJwksEndpoint(
            Url::parse(&format!(
                "https://login.microsoftonline.com/{}/{suffix}",
                self.selection.tenant()
            ))
            .map_err(|_| PrincipalError::Selection)?,
        );
        let bytes = self.source.fetch(&endpoint).await?;
        if bytes.len() > JWKS_LIMIT {
            return Err(PrincipalError::Keys);
        }
        let document: Jwks = serde_json::from_slice(&bytes).map_err(|_| PrincipalError::Keys)?;
        if document.keys.is_empty() || document.keys.len() > 32 {
            return Err(PrincipalError::Keys);
        }
        let mut ids = std::collections::BTreeSet::new();
        for key in &document.keys {
            if key.kid.is_empty() || key.kid.len() > 128 || !ids.insert(&key.kid) {
                return Err(PrincipalError::Keys);
            }
        }
        Ok(Document {
            keys: document.keys,
            fetched: Instant::now(),
            refreshed,
        })
    }
}

fn decode_json<T: serde::de::DeserializeOwned>(part: &str) -> Result<T, PrincipalError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| PrincipalError::Token)?;
    serde_json::from_slice(&bytes).map_err(|_| PrincipalError::Token)
}

fn check_lifetime(claims: &Claims, provider_expiry: OffsetDateTime) -> Result<(), PrincipalError> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if claims.exp <= now.saturating_add(60)
        || claims.exp > now.saturating_add(MAX_LIFETIME)
        || claims.nbf > now
        || claims.iat > now
        || claims.nbf > claims.exp
        || claims.iat > claims.exp
        || claims.exp.saturating_sub(claims.nbf) > MAX_LIFETIME
        || provider_expiry.unix_timestamp() <= now.saturating_add(60)
    {
        return Err(PrincipalError::Token);
    }
    Ok(())
}
