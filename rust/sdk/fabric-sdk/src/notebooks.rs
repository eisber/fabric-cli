// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook service requests and operation reconciliation via caller-owned execution.

use crate::{Client, Page, models::FabricServiceError};
use azure_core::{credentials::AccessToken, http::Url};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core::EndpointBinding;
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::Future,
    num::NonZeroUsize,
    pin::Pin,
    time::Duration,
};

/// One inline, base64-encoded definition file.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionPart {
    path: String,
    payload: String,
    payload_type: &'static str,
}

impl DefinitionPart {
    /// Encodes a relative definition path and its exact content bytes.
    ///
    /// # Errors
    /// Rejects empty, absolute, backslash-delimited, or traversing paths.
    pub fn inline(path: &str, bytes: &[u8]) -> Result<Self, NotebookErrorKind> {
        if path.is_empty()
            || path.contains(['\\', ':', '\0'])
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(NotebookErrorKind::InvalidInput);
        }
        Ok(Self {
            path: path.to_owned(),
            payload: STANDARD.encode(bytes),
            payload_type: "InlineBase64",
        })
    }
}

/// An explicit Notebook definition, not a shell-only item.
#[derive(Clone, Serialize)]
pub struct NotebookDefinition {
    /// `ipynb` for exported notebooks, absent for Fabric Git source format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Ordered inline files; `.platform` precedes content and settings.
    pub parts: Vec<DefinitionPart>,
}

/// Typed item metadata needed for exact name/type selection.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookItem {
    /// Fabric item ID.
    pub id: String,
    /// Service display name, never normalized.
    pub display_name: String,
    /// Service item type, never normalized.
    #[serde(rename = "type")]
    pub item_type: String,
}

/// One validated service request. Debug output never includes token or content.
pub struct NotebookRequest<'a> {
    method: &'static str,
    url: &'a Url,
    body: &'a [u8],
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}

impl NotebookRequest<'_> {
    /// Returns GET or POST.
    pub const fn method(&self) -> &'static str {
        self.method
    }
    /// Returns the fixed-origin, validated Fabric URL.
    pub fn url(&self) -> &Url {
        self.url
    }
    /// Returns serialized JSON, empty for GET.
    pub fn body(&self) -> &[u8] {
        self.body
    }
    /// Returns the explicitly injected bearer token. Never log it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Returns the content type.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
    /// Returns the body limit the executor must enforce while reading.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
}

impl fmt::Debug for NotebookRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotebookRequest")
            .field("method", &self.method)
            .field("origin", &self.url.origin().ascii_serialization())
            .field("body", &"<definition>")
            .field("token", &"<injected>")
            .finish()
    }
}

/// Response data; status and payload must still be checked.
#[derive(Clone, Debug)]
pub struct NotebookResponse {
    /// HTTP status.
    pub status: u16,
    /// Body bounded by the request's byte limit.
    pub body: Vec<u8>,
    /// Headers, read case-insensitively.
    pub headers: BTreeMap<String, String>,
}

/// Executor failures without provider diagnostic or secret text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookExecutionError {
    /// Transport failed; a POST might already have reached the service.
    Failed,
    /// The caller's execution deadline expired.
    TimedOut,
    /// Request execution was cancelled locally.
    Cancelled,
    /// Streaming execution stopped before exceeding the response byte bound.
    ResponseLimit,
    /// A read exhausted its finite retry budget. No mutation was replayed.
    RetriesExhausted,
}

/// Operation-local seam following the workspace executor pattern.
///
/// Implementations provide bearer authentication from `request.token()`, enforce
/// request deadlines and streaming response limits, never follow redirects, and
/// release local resources when futures are dropped. They MUST NOT automatically
/// retry POSTs: a lost response does not prove the mutation failed. No production
/// HTTP stack, runtime, ambient credentials, or retry policy is installed here.
pub trait NotebookExecutor: Send + Sync {
    /// Executes exactly one request, with no automatic mutation replay.
    fn execute<'a>(
        &'a self,
        request: NotebookRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<NotebookResponse, NotebookExecutionError>> + Send + 'a>>;

    /// Asynchronously waits at least the requested service polling interval.
    ///
    /// Use the caller's runtime; never block its async thread or busy-spin.
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotebookExecutionError>> + Send + '_>>;
}

/// Finite bounds across discovery, mutation and polling within one deployment.
#[derive(Clone, Copy, Debug)]
pub struct NotebookLimits {
    /// Maximum service requests, including pages and operation result retrieval.
    pub max_requests: NonZeroUsize,
    /// Maximum accumulated discovery items.
    pub max_items: NonZeroUsize,
    /// Maximum response bytes per request.
    pub max_response_bytes: NonZeroUsize,
    /// Largest accepted service Retry-After interval.
    pub max_poll_delay: Duration,
}

impl Default for NotebookLimits {
    fn default() -> Self {
        Self {
            max_requests: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        }
    }
}

/// Classification of a failed or incomplete native service operation.
#[derive(Clone, Debug, PartialEq)]
pub enum NotebookErrorKind {
    /// Invalid input, including unsafe service IDs or empty definitions.
    InvalidInput,
    /// Credential selection was not explicitly for Fabric.
    InvalidAudience,
    /// The injected credential could not acquire the required token.
    Credential,
    /// The executor reported an explicit failure.
    Execution(NotebookExecutionError),
    /// Fabric rejected the request; valid structured details are preserved as data.
    Http {
        /// HTTP status.
        status: u16,
        /// Service error code, retained even if other error fields are malformed.
        code: Option<String>,
        /// Structured error, when the response contains a valid one.
        error: Option<Box<FabricServiceError>>,
    },
    /// Service returned malformed content, headers, or an unknown operation state.
    InvalidResponse,
    /// A continuation or operation link was not the expected Fabric resource.
    InvalidLink,
    /// Requests, items, response bytes, or polling delay exceeded a caller bound.
    Limit,
    /// Discovery repeated a continuation token.
    RepeatedContinuation,
    /// The service reported operation failure or cancellation.
    OperationFailed {
        /// Reported terminal status (Failed or Cancelled).
        status: String,
        /// Structured service failure when supplied.
        error: Option<Box<FabricServiceError>>,
    },
    /// Local cancellation; never evidence of server rollback.
    Cancelled,
}

/// Failed/incomplete operation with reconciliation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct NotebookError {
    /// Explicit failure classification.
    pub kind: NotebookErrorKind,
    /// Accepted operation ID, when observed. Poll it before attempting replay.
    pub operation_id: Option<String>,
    /// True once mutation execution may have started, even if no response arrived.
    pub mutation_may_have_occurred: bool,
}

impl fmt::Display for NotebookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("native Notebook operation failed or is incomplete; inspect its classification and reconciliation evidence")
    }
}

impl std::error::Error for NotebookError {}

/// Shared bounded state for a single caller-driven Notebook deployment.
pub struct NotebookSession<'a> {
    limits: NotebookLimits,
    endpoint: EndpointBinding,
    operations: OperationSession<'a, NotebookOperationAdapter<'a>>,
}

impl Client {
    /// Starts a bounded native item session; performs no I/O until first awaited call.
    ///
    /// Supply a deadline/cancellation future, or `std::future::pending()` to rely
    /// on executor deadlines and dropping the returned operation future.
    pub fn notebook_session<'a>(
        &'a self,
        executor: &'a dyn NotebookExecutor,
        limits: NotebookLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> NotebookSession<'a> {
        NotebookSession {
            limits,
            endpoint: self.context().endpoint().clone(),
            operations: OperationSession::with_context(
                self.context(),
                NotebookOperationAdapter(executor),
                OperationLimits {
                    max_requests: limits.max_requests,
                    max_response_bytes: limits.max_response_bytes,
                    max_poll_delay: limits.max_poll_delay,
                },
                cancelled,
            ),
        }
    }
}

impl NotebookSession<'_> {
    /// Lists all workspace items for safe exact-name/type create/update selection.
    ///
    /// # Errors
    /// Incomplete discovery is an error, never permission to create an absent item.
    pub async fn list_items(
        &mut self,
        workspace: &str,
    ) -> Result<Vec<NotebookItem>, NotebookError> {
        self.list_inner(workspace)
            .await
            .map_err(|kind| self.error(kind))
    }

    async fn list_inner(
        &mut self,
        workspace: &str,
    ) -> Result<Vec<NotebookItem>, NotebookErrorKind> {
        let path = items_path(workspace)?;
        let mut token: Option<String> = None;
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        loop {
            let mut url = service_url(&self.endpoint, &path)?;
            if let Some(token) = &token {
                url.query_pairs_mut()
                    .append_pair("continuationToken", token);
            }
            let response = self.request("GET", url, Vec::new()).await?;
            ensure_status(&response, &[200])?;
            let page: Page<NotebookItem> = decode(&response.body)?;
            if page.value.len() > self.limits.max_items.get() - items.len() {
                return Err(NotebookErrorKind::Limit);
            }
            items.extend(page.value);
            token = header(&response, "x-ms-continuation")?
                .map(str::to_owned)
                .or(page.continuation_token)
                .filter(|value| !value.is_empty());
            let Some(next) = &token else {
                return Ok(items);
            };
            if let Some(uri) = page.continuation_uri {
                validate_link(&self.endpoint, &uri, &path)?;
            }
            if !seen.insert(next.clone()) {
                return Err(NotebookErrorKind::RepeatedContinuation);
            }
        }
    }

    /// Creates one Notebook and resolves asynchronous completion to actual item data.
    ///
    /// # Errors
    /// Accepted, malformed, failed or cancelled operations never return success.
    /// Lost POST responses are not automatically replayed.
    pub async fn create(
        &mut self,
        workspace: &str,
        name: &str,
        description: &str,
        definition: &NotebookDefinition,
    ) -> Result<NotebookItem, NotebookError> {
        let result = async {
            let path = items_path(workspace)?;
            validate_definition(definition)?;
            if name.trim().is_empty() {
                return Err(NotebookErrorKind::InvalidInput);
            }
            let body = json!({
                "displayName": name, "description": description,
                "type": "Notebook", "definition": definition
            });
            let response = self
                .mutate(service_url(&self.endpoint, &path)?, body, true)
                .await?;
            let item: NotebookItem = decode(&response.body)?;
            if !valid_id(&item.id) || item.item_type != "Notebook" || item.display_name != name {
                return Err(NotebookErrorKind::InvalidResponse);
            }
            Ok(item)
        }
        .await;
        result.map_err(|kind| self.error(kind))
    }

    /// Updates definition and `.platform` metadata, waiting for terminal success.
    ///
    /// # Errors
    /// Returns explicit incomplete/failure evidence without rollback or replay.
    pub async fn update(
        &mut self,
        workspace: &str,
        item: &str,
        definition: &NotebookDefinition,
    ) -> Result<(), NotebookError> {
        let result = async {
            let path = items_path(workspace)?;
            if !valid_id(item) {
                return Err(NotebookErrorKind::InvalidInput);
            }
            validate_definition(definition)?;
            let url = service_url(
                &self.endpoint,
                &format!("{path}/{item}/updateDefinition?updateMetadata=True"),
            )?;
            self.mutate(url, json!({"definition": definition}), false)
                .await?;
            Ok(())
        }
        .await;
        result.map_err(|kind| self.error(kind))
    }

    fn error(&self, kind: NotebookErrorKind) -> NotebookError {
        let evidence = self.operations.evidence(OperationErrorKind::InvalidInput);
        NotebookError {
            kind,
            operation_id: evidence.operation_id,
            mutation_may_have_occurred: evidence.mutation_may_have_occurred,
        }
    }

    async fn request(
        &mut self,
        method: &'static str,
        url: Url,
        body: Vec<u8>,
    ) -> Result<NotebookResponse, NotebookErrorKind> {
        let method = match method {
            "GET" => ResourceMethod::Get,
            "POST" => ResourceMethod::Post,
            _ => return Err(NotebookErrorKind::InvalidInput),
        };
        let operation = FabricOperation::new(
            method,
            &match url.query() {
                Some(query) => format!("{}?{query}", url.path()),
                None => url.path().to_owned(),
            },
            body,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(operation_kind)?;
        self.operations
            .execute(operation)
            .await
            .map(notebook_response)
            .map_err(|error| operation_kind(error.kind))
    }

    async fn mutate(
        &mut self,
        url: Url,
        body: serde_json::Value,
        create: bool,
    ) -> Result<NotebookResponse, NotebookErrorKind> {
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &path,
            serde_json::to_vec(&body).map_err(|_| NotebookErrorKind::InvalidInput)?,
            if create {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::WaitForCompletion
            },
        )
        .map_err(operation_kind)?;
        let response = self
            .operations
            .execute(operation)
            .await
            .map(notebook_response)
            .map_err(|error| operation_kind(error.kind))?;
        ensure_status(
            &response,
            if create && self.operations.operation_id().is_none() {
                &[201]
            } else {
                &[200]
            },
        )?;
        Ok(response)
    }
}

struct NotebookOperationAdapter<'a>(&'a dyn NotebookExecutor);

impl OperationExecutor for NotebookOperationAdapter<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => "GET",
                ResourceMethod::Post => "POST",
                ResourceMethod::Patch | ResourceMethod::Delete => {
                    return Err(OperationExecutionError::Failed);
                }
            };
            self.0
                .execute(NotebookRequest {
                    method,
                    url: request.url(),
                    body: request.body(),
                    token: request.token(),
                    max_response_bytes: request.max_response_bytes(),
                })
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(|error| match error {
                    NotebookExecutionError::Failed => OperationExecutionError::Failed,
                    NotebookExecutionError::TimedOut => OperationExecutionError::TimedOut,
                    NotebookExecutionError::Cancelled => OperationExecutionError::Cancelled,
                    NotebookExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
                    NotebookExecutionError::RetriesExhausted => {
                        OperationExecutionError::RetriesExhausted
                    }
                })
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            self.0.wait(delay).await.map_err(|error| match error {
                NotebookExecutionError::Failed => OperationExecutionError::Failed,
                NotebookExecutionError::TimedOut => OperationExecutionError::TimedOut,
                NotebookExecutionError::Cancelled => OperationExecutionError::Cancelled,
                NotebookExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
                NotebookExecutionError::RetriesExhausted => {
                    OperationExecutionError::RetriesExhausted
                }
            })
        })
    }
}

fn notebook_response(response: OperationResponse) -> NotebookResponse {
    NotebookResponse {
        status: response.status,
        body: response.body,
        headers: response.headers,
    }
}

fn operation_kind(kind: OperationErrorKind) -> NotebookErrorKind {
    match kind {
        OperationErrorKind::InvalidInput => NotebookErrorKind::InvalidInput,
        OperationErrorKind::InvalidAudience => NotebookErrorKind::InvalidAudience,
        OperationErrorKind::Credential => NotebookErrorKind::Credential,
        OperationErrorKind::InvalidResponse => NotebookErrorKind::InvalidResponse,
        OperationErrorKind::InvalidLink => NotebookErrorKind::InvalidLink,
        OperationErrorKind::Limit => NotebookErrorKind::Limit,
        OperationErrorKind::Cancelled => NotebookErrorKind::Cancelled,
        OperationErrorKind::Http {
            status,
            code,
            error,
        } => NotebookErrorKind::Http {
            status,
            code,
            error,
        },
        OperationErrorKind::OperationFailed { status, error } => {
            NotebookErrorKind::OperationFailed { status, error }
        }
        OperationErrorKind::Execution(error) => NotebookErrorKind::Execution(match error {
            OperationExecutionError::Failed => NotebookExecutionError::Failed,
            OperationExecutionError::TimedOut => NotebookExecutionError::TimedOut,
            OperationExecutionError::Cancelled => NotebookExecutionError::Cancelled,
            OperationExecutionError::ResponseLimit => NotebookExecutionError::ResponseLimit,
            OperationExecutionError::RetriesExhausted => NotebookExecutionError::RetriesExhausted,
        }),
    }
}

fn items_path(workspace: &str) -> Result<String, NotebookErrorKind> {
    if !valid_id(workspace) {
        return Err(NotebookErrorKind::InvalidInput);
    }
    Ok(format!("/v1/workspaces/{workspace}/items"))
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn service_url(endpoint: &EndpointBinding, path: &str) -> Result<Url, NotebookErrorKind> {
    endpoint
        .url(path)
        .map_err(|_| NotebookErrorKind::InvalidInput)
}

fn validate_link(
    endpoint: &EndpointBinding,
    uri: &str,
    path: &str,
) -> Result<(), NotebookErrorKind> {
    let url = Url::parse(uri).map_err(|_| NotebookErrorKind::InvalidLink)?;
    endpoint
        .validate_url(&url)
        .map_err(|_| NotebookErrorKind::InvalidLink)?;
    if url.path() != path || (!path.ends_with("/items") && url.query().is_some()) {
        return Err(NotebookErrorKind::InvalidLink);
    }
    Ok(())
}

fn header<'a>(
    response: &'a NotebookResponse,
    name: &str,
) -> Result<Option<&'a str>, NotebookErrorKind> {
    let mut matches = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = matches.next().map(|(_, value)| value.as_str());
    if matches.next().is_some() {
        return Err(NotebookErrorKind::InvalidResponse);
    }
    Ok(value)
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, NotebookErrorKind> {
    serde_json::from_slice(body).map_err(|_| NotebookErrorKind::InvalidResponse)
}

fn ensure_status(response: &NotebookResponse, allowed: &[u16]) -> Result<(), NotebookErrorKind> {
    if allowed.contains(&response.status) {
        Ok(())
    } else {
        Err(NotebookErrorKind::Http {
            status: response.status,
            code: header(response, "x-ms-public-api-error-code")?
                .or(header(response, "x-ms-error-code")?)
                .filter(|code| !code.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    serde_json::from_slice::<serde_json::Value>(&response.body)
                        .ok()
                        .and_then(|body| {
                            body.get("errorCode")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                }),
            error: serde_json::from_slice(&response.body).ok().map(Box::new),
        })
    }
}

fn validate_definition(definition: &NotebookDefinition) -> Result<(), NotebookErrorKind> {
    let mut paths = HashSet::new();
    if definition.parts.is_empty()
        || definition
            .parts
            .iter()
            .any(|part| !paths.insert(&part.path))
        || definition
            .format
            .as_deref()
            .is_some_and(|format| format != "ipynb")
    {
        return Err(NotebookErrorKind::InvalidInput);
    }
    Ok(())
}
