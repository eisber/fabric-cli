// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Notebook read authorization, separate from Fabric REST execution.
//!
//! Every selected origin and resource must be supplied before credential access.
//! Bootstrap responses confirm that selection; they never choose configuration.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor, native_runtime::response_metadata};
use azure_core::{
    http::{Method, Request, Url},
    time::OffsetDateTime,
};
use fabric_core::{
    CredentialSelection, RequestContext, ServiceOrigin, cancel_with,
    principal::{
        EntraJwksEndpoint, EntraJwksSource, EntraJwtVerifier, JWKS_LIMIT, PrincipalError,
        VerifiedPbiToken, canonical_guid, validate_selection,
    },
};
use fabric_sdk::{
    runtime::{
        NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod,
        NativeRequest, NativeResponse,
    },
    workspaces::WorkspaceResponse,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    fmt,
    future::Future,
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
};

const BOOTSTRAP_LIMIT: usize = 128 * 1024;
const MAX_GRANT_SECONDS: i64 = 3600;

/// Explicit supported catalog; EDog and sovereign PBI audiences are unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookEnvironment {
    /// Public production.
    Production,
    /// Explicit MSIT selection, subject to verified non-production account policy.
    Msit,
    /// Explicit daily selection, subject to verified non-production account policy.
    Daily,
    /// Explicit DXT selection, subject to verified non-production account policy.
    Dxt,
}

impl NotebookEnvironment {
    fn catalog(self) -> &'static str {
        match self {
            Self::Production => "https://api.fabric.microsoft.com",
            Self::Msit => "https://msitapi.fabric.microsoft.com",
            Self::Daily => "https://dailyapi.fabric.microsoft.com",
            Self::Dxt => "https://dxtapi.fabric.microsoft.com",
        }
    }
}

/// Exact ASCII DNS HTTPS origin. No IP, credentials, path, query or fragment.
#[derive(Clone, Eq, PartialEq)]
pub struct NotebookOrigin(Url);

impl fmt::Debug for NotebookOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NotebookOrigin(<selected>)")
    }
}

impl NotebookOrigin {
    /// Validate raw syntax before a URL parser can normalize an authority or path.
    ///
    /// # Errors
    /// Rejects anything except lowercase HTTPS DNS with optional literal `:443`
    /// and an optional root slash.
    pub fn new(value: &str) -> Result<Self, NativeError> {
        Self::parse(value, false)
    }

    fn parse(value: &str, response_host: bool) -> Result<Self, NativeError> {
        let authority = value
            .strip_prefix("https://")
            .ok_or(NativeError::InvalidInput)?;
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        let host = authority.strip_suffix(":443").unwrap_or(authority);
        if host.is_empty()
            || host.len() > 253
            || !host.contains('.')
            || !host.bytes().any(|byte| byte.is_ascii_alphabetic())
            || host.parse::<std::net::IpAddr>().is_ok()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || byte == b'-'
                            || response_host && byte.is_ascii_uppercase()
                    })
            })
        {
            return Err(NativeError::InvalidInput);
        }
        let url = Url::parse(value).map_err(|_| NativeError::InvalidInput)?;
        if url.scheme() != "https"
            || url.host_str() != Some(host.to_ascii_lowercase().as_str())
            || url.port_or_known_default() != Some(443)
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(url))
    }

    fn response(value: &str) -> Result<Self, NativeError> {
        let value = if value.contains("://") {
            value.to_owned()
        } else {
            format!("https://{value}")
        };
        Self::parse(&value, true).map_err(|_| NativeError::InvalidResponse)
    }

    fn at(&self, path: &str) -> Result<Url, NativeError> {
        let url = self.0.join(path).map_err(|_| NativeError::InvalidInput)?;
        if url.origin() != self.0.origin() || url.path() != path {
            return Err(NativeError::InvalidInput);
        }
        Ok(url)
    }
}

/// Exact resource scope; this foundation never resolves or selects any IDs.
#[derive(Clone, Eq, PartialEq)]
pub struct NotebookReadScope {
    capacity: String,
    workspace: String,
    notebook: String,
}

impl fmt::Debug for NotebookReadScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NotebookReadScope(<selected>)")
    }
}

impl NotebookReadScope {
    /// Bind three canonical non-nil GUIDs without I/O.
    ///
    /// # Errors
    /// Rejects opaque labels, normalization and route-changing syntax.
    pub fn new(capacity: &str, workspace: &str, notebook: &str) -> Result<Self, NativeError> {
        if ![capacity, workspace, notebook]
            .into_iter()
            .all(canonical_guid)
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self {
            capacity: capacity.into(),
            workspace: workspace.into(),
            notebook: notebook.into(),
        })
    }
    /// Caller-selected capacity GUID.
    pub fn capacity(&self) -> &str {
        &self.capacity
    }
    /// Caller-selected workspace GUID.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    /// Caller-selected Notebook artifact GUID.
    pub fn notebook(&self) -> &str {
        &self.notebook
    }

    fn path(&self) -> String {
        format!(
            "/webapi/capacities/{}/workloads/Notebook/Data/Automatic/api/workspaces/{}/artifacts/{}",
            self.capacity, self.workspace, self.notebook
        )
    }
}

/// Owner-supplied catalog, cluster, workload and resource binding.
#[derive(Clone)]
pub struct NotebookWorkloadSelection {
    owner: CredentialSelection,
    environment: NotebookEnvironment,
    catalog: NotebookOrigin,
    cluster: NotebookOrigin,
    workload: NotebookOrigin,
    scope: NotebookReadScope,
}

impl fmt::Debug for NotebookWorkloadSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NotebookWorkloadSelection(<explicit>)")
    }
}

impl NotebookWorkloadSelection {
    /// Freeze all caller selection before token or HTTP access.
    ///
    /// The catalog must match the explicitly chosen environment. Dynamic origins
    /// remain untrusted until guarded responses confirm these exact selections.
    ///
    /// # Errors
    /// Rejects unsupported owner metadata and mismatched catalog/environment.
    pub fn new(
        owner: CredentialSelection,
        environment: NotebookEnvironment,
        catalog: NotebookOrigin,
        cluster: NotebookOrigin,
        workload: NotebookOrigin,
        scope: NotebookReadScope,
    ) -> Result<Self, NativeError> {
        validate_selection(&owner).map_err(|_| NativeError::InvalidInput)?;
        if catalog != NotebookOrigin::new(environment.catalog())? {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self {
            owner,
            environment,
            catalog,
            cluster,
            workload,
            scope,
        })
    }

    /// Explicit selected scope for the existing typed Notebook request builder.
    pub fn scope(&self) -> &NotebookReadScope {
        &self.scope
    }

    /// Origin for the existing SDK's [`fabric_sdk::notebook_runtime::NotebookRuntime`].
    ///
    /// This does not grant authorization or perform bootstrap.
    pub fn workload_endpoint(&self) -> NativeEndpoint {
        NativeEndpoint::new(self.workload.0.clone()).expect("validated HTTPS origin")
    }
}

/// Read-only native adapter over the shared bounded HTTP engine and real verifier.
///
/// Construction is local-only. Callers must finish target/output preflight first,
/// then pass the original operation deadline. Dropping any returned future stops
/// local auth/JWKS/bootstrap/body observation; it never replays requests. No grant,
/// token or selected context is persisted. Each read performs its own exchange.
pub struct NativeNotebookExecutor {
    context: RequestContext,
    http: HttpExecutor,
    selection: NotebookWorkloadSelection,
    verifier: EntraJwtVerifier,
    deadline: NativeDeadline,
}

impl fmt::Debug for NativeNotebookExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeNotebookExecutor(<explicit>)")
    }
}

impl NativeNotebookExecutor {
    /// Compose an explicitly matched provider, endpoint, context and deadline.
    ///
    /// # Errors
    /// Rejects owner, tenant, audience or catalog mismatch before any I/O.
    pub fn new(
        context: RequestContext,
        http: HttpExecutor,
        selection: NotebookWorkloadSelection,
        deadline: NativeDeadline,
    ) -> Result<Self, NativeError> {
        if context.selection() != &selection.owner
            || context.endpoint().origin() != ServiceOrigin::Fabric
            || context.endpoint() != http.endpoint()
            || context.endpoint().base_url() != selection.catalog.0
        {
            return Err(NativeError::InvalidInput);
        }
        deadline.remaining()?;
        let source = DeadlineJwksSource {
            http: http.clone(),
            deadline: deadline.clone(),
        };
        let verifier = EntraJwtVerifier::new(selection.owner.clone(), Arc::new(source))
            .map_err(|_| NativeError::InvalidInput)?;
        Ok(Self {
            context,
            http,
            selection,
            verifier,
            deadline,
        })
    }

    /// Observe one typed read with explicit cancellation, sharing the same deadline.
    ///
    /// # Errors
    /// Cancellation wins before polling authentication or HTTP.
    pub async fn execute_cancelled(
        &self,
        request: &NativeRequest,
        max_response_bytes: usize,
        cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<NativeResponse, NativeError> {
        cancel_with(self.execute(request, max_response_bytes), cancelled)
            .await
            .map_err(|_| NativeError::Cancelled)?
    }

    fn validate_read(
        &self,
        request: &NativeRequest,
        limit: usize,
    ) -> Result<NonZeroUsize, NativeError> {
        let limit = NonZeroUsize::new(limit)
            .filter(|limit| limit.get() <= 16 * 1024 * 1024)
            .ok_or(NativeError::InvalidInput)?;
        let url = request.url();
        if request.method() != NativeMethod::Get
            || request.effect() != NativeEffect::Read
            || request.body().is_some()
            || request.bytes().is_some()
            || !request.headers().is_empty()
            || request.accept() != "application/json"
            || request.content_type() != "application/json"
            || url.origin() != self.selection.workload.0.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(NativeError::InvalidInput);
        }
        let prefix = self.selection.scope.path();
        let suffix = url
            .path()
            .strip_prefix(&prefix)
            .ok_or(NativeError::InvalidInput)?;
        let sessions = "/jupyterApi/versions/1/api/sessions";
        let exact_session = suffix
            .strip_prefix(&format!("{sessions}/"))
            .is_some_and(|id| {
                id != "active"
                    && !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            });
        if !(suffix == "/content"
            || suffix == sessions
            || suffix == format!("{sessions}/active")
            || exact_session)
            || url
                .query()
                .is_some_and(|query| !(exact_session && query == "detailed=true"))
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(limit)
    }

    async fn read(
        &self,
        request: &NativeRequest,
        limit: NonZeroUsize,
    ) -> Result<NativeResponse, NativeError> {
        let pbi = self
            .verifier
            .acquire_pbi(self.context.credential().as_ref())
            .await
            .map_err(|_| NativeError::InvalidResponse)?;
        self.validate_environment(&pbi)?;
        self.check_expiry(pbi.expires_on())?;
        let url = self.selection.catalog.at("/metadata/cluster")?;
        let response = self.bearer(Method::Get, &url, &pbi, &[]).await?;
        let cluster: ClusterResponse = decode(&response)?;
        if NotebookOrigin::response(&cluster.backend_url)? != self.selection.cluster {
            return Err(NativeError::InvalidResponse);
        }
        self.check_expiry(pbi.expires_on())?;
        let scope = &self.selection.scope;
        let body = serde_json::to_vec(&json!({
            "capacityObjectId": scope.capacity,
            "workspaceObjectId": scope.workspace,
            "workloadType": "Notebook",
            "artifactObjectIds": [scope.notebook],
        }))
        .map_err(|_| NativeError::InvalidInput)?;
        let url = self
            .selection
            .cluster
            .at("/metadata/v201606/generatemwctoken")?;
        let response = self.bearer(Method::Post, &url, &pbi, &body).await?;
        let grant = self.grant(decode(&response)?, &pbi)?;
        self.check_expiry(grant.expires)?;
        let mut wire = Request::new(request.url().clone(), Method::Get);
        wire.insert_header("authorization", format!("MwcToken {}", grant.token));
        wire.insert_header("accept", "application/json");
        let response = self.once(wire, limit).await?;
        if (300..400).contains(&response.status) {
            return Err(NativeError::Http(response.status));
        }
        let (headers, content_type) = response_metadata(
            response
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )?;
        Ok(NativeResponse {
            status: response.status,
            body: response.body,
            content_type,
            headers,
        })
    }

    fn validate_environment(&self, token: &VerifiedPbiToken) -> Result<(), NativeError> {
        if self.selection.environment == NotebookEnvironment::Production {
            return Ok(());
        }
        let principal = token.principal();
        let domain = principal
            .account()
            .split_once('@')
            .map(|(_, domain)| domain);
        let allowed = !principal.is_application()
            && domain.is_some_and(|domain| {
                domain == "microsoft.com"
                    || domain
                        .strip_suffix(".onmicrosoft.com")
                        .is_some_and(|label| {
                            !label.is_empty()
                                && !label.contains('.')
                                && !label.starts_with('-')
                                && !label.ends_with('-')
                                && label
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        })
            });
        if !allowed {
            return Err(NativeError::UnsupportedCapability);
        }
        Ok(())
    }

    fn check_expiry(&self, expires: OffsetDateTime) -> Result<(), NativeError> {
        self.deadline.remaining()?;
        if expires.unix_timestamp() <= OffsetDateTime::now_utc().unix_timestamp() + 1 {
            return Err(NativeError::InvalidResponse);
        }
        Ok(())
    }

    async fn bearer(
        &self,
        method: Method,
        url: &Url,
        token: &VerifiedPbiToken,
        body: &[u8],
    ) -> Result<WorkspaceResponse, NativeError> {
        self.check_expiry(token.expires_on())?;
        let mut request = Request::new(url.clone(), method);
        request.insert_header(
            "authorization",
            format!("Bearer {}", token.token().token.secret()),
        );
        request.insert_header("content-type", "application/json");
        request.set_body(body.to_vec());
        let response = self
            .once(
                request,
                NonZeroUsize::new(BOOTSTRAP_LIMIT).expect("positive"),
            )
            .await?;
        if response.status != 200 {
            return Err(NativeError::Http(response.status));
        }
        Ok(response)
    }

    async fn once(
        &self,
        request: Request,
        limit: NonZeroUsize,
    ) -> Result<WorkspaceResponse, NativeError> {
        let http = self
            .http
            .with_timeout(self.deadline.remaining()?)
            .map_err(execution_error)?;
        http.before_deadline(http.send_once(&request, limit, None))
            .await
            .map_err(execution_error)
    }

    fn grant(
        &self,
        response: GrantResponse,
        pbi: &VerifiedPbiToken,
    ) -> Result<WorkloadGrant, NativeError> {
        let expiry = chrono::DateTime::parse_from_rfc3339(&response.expiry)
            .map_err(|_| NativeError::InvalidResponse)?
            .timestamp();
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if response.token.is_empty()
            || response.token.len() > 32 * 1024
            || !response.token.bytes().all(|byte| byte.is_ascii_graphic())
            || NotebookOrigin::response(&response.target_uri_host)? != self.selection.workload
            || response.capacity_object_id != self.selection.scope.capacity
            || expiry <= now + 1
            || expiry > now + MAX_GRANT_SECONDS
            || expiry > pbi.expires_on().unix_timestamp()
        {
            return Err(NativeError::InvalidResponse);
        }
        Ok(WorkloadGrant {
            token: response.token,
            expires: OffsetDateTime::from_unix_timestamp(expiry)
                .map_err(|_| NativeError::InvalidResponse)?,
        })
    }
}

impl NativeExecutor for NativeNotebookExecutor {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> ExecutionFuture<'a, Result<NativeResponse, NativeError>> {
        Box::pin(async move {
            let limit = self.validate_read(request, max_response_bytes)?;
            let remaining = self.deadline.remaining()?;
            cancel_with(
                self.read(request, limit),
                pin!(self.http.timer.sleep(remaining)),
            )
            .await
            .map_err(|_| NativeError::TimedOut)?
        })
    }
}

impl EntraJwksSource for HttpExecutor {
    fn fetch<'a>(
        &'a self,
        endpoint: &'a EntraJwksEndpoint,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PrincipalError>> + Send + 'a>> {
        Box::pin(async move {
            let request = Request::new(endpoint.url().clone(), Method::Get);
            let limit = NonZeroUsize::new(JWKS_LIMIT).expect("positive");
            let response = self
                .before_deadline(self.send_once(&request, limit, None))
                .await
                .map_err(|_| PrincipalError::Keys)?;
            if response.status != 200 {
                return Err(PrincipalError::Keys);
            }
            Ok(response.body)
        })
    }
}

struct DeadlineJwksSource {
    http: HttpExecutor,
    deadline: NativeDeadline,
}

impl EntraJwksSource for DeadlineJwksSource {
    fn fetch<'a>(
        &'a self,
        endpoint: &'a EntraJwksEndpoint,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PrincipalError>> + Send + 'a>> {
        Box::pin(async move {
            let remaining = self
                .deadline
                .remaining()
                .map_err(|_| PrincipalError::Keys)?;
            let http = self
                .http
                .with_timeout(remaining)
                .map_err(|_| PrincipalError::Keys)?;
            http.fetch(endpoint).await
        })
    }
}

#[derive(Deserialize)]
struct ClusterResponse {
    #[serde(rename = "backendUrl")]
    backend_url: String,
}

#[derive(Deserialize)]
struct GrantResponse {
    #[serde(rename = "Token", alias = "token")]
    token: String,
    #[serde(rename = "TargetUriHost", alias = "targetUriHost")]
    target_uri_host: String,
    #[serde(rename = "CapacityObjectId", alias = "capacityObjectId")]
    capacity_object_id: String,
    #[serde(rename = "Expiry", alias = "expiry")]
    expiry: String,
}

struct WorkloadGrant {
    token: String,
    expires: OffsetDateTime,
}

fn decode<T: serde::de::DeserializeOwned>(response: &WorkspaceResponse) -> Result<T, NativeError> {
    serde_json::from_slice(&response.body).map_err(|_| NativeError::InvalidResponse)
}

fn execution_error(error: ExecutionError) -> NativeError {
    match error {
        ExecutionError::InvalidInput => NativeError::InvalidInput,
        ExecutionError::TimedOut => NativeError::TimedOut,
        ExecutionError::ResponseLimit => NativeError::Limit,
        ExecutionError::Transport => NativeError::Transport,
        ExecutionError::RetriesExhausted | ExecutionError::RetryDelay => {
            NativeError::InvalidResponse
        }
    }
}
