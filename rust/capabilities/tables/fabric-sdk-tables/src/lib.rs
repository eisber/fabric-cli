// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Delta-log schema extraction and Lakehouse table operations.
//!
//! Credentials, service execution, generic Fabric operation reconciliation and
//! generic job lifecycle are injected. This crate never creates a runtime,
//! transport, identity backend, temporary file or ambient configuration.

mod jobs;
mod model;

pub use fabric_sdk_jobs::JobType as MaintenanceJobType;
pub use jobs::{JobMaintenance, MaintenanceEvent};
pub use model::{LoadMode, LoadOptions, Maintenance, SourceKind, TablePath};

use azure_core::{
    credentials::{AccessToken, TokenCredential},
    http::Url,
};
pub use fabric_core::{Audience, CredentialSelection};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeLimits, OneLakePath,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
};

/// A future on the consumer's runtime.
pub type TableFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Failure classification; service text is retained as data, never in Display.
#[derive(Clone, Debug, PartialEq)]
pub enum TableErrorKind {
    /// Invalid resolved path, unsafe segment, options or audience selection.
    InvalidInput,
    /// No JSON commit supplied a nonempty schema string.
    InvalidDeltaTable,
    /// Source JSON or the response shape is malformed.
    InvalidResponse,
    /// Explicit injected provider failed.
    Credential,
    /// Shared execution failed without an HTTP response.
    Execution,
    /// Service rejected a request.
    Http {
        /// HTTP status.
        status: u16,
        /// Service code when present.
        code: Option<String>,
        /// Structured service message, retained only for explicit CLI projection.
        message: Option<String>,
    },
    /// Shared Fabric operation failed.
    OperationFailed,
    /// Service reported a cancelled operation, not local rollback.
    OperationCancelled,
    /// Request, response, log or polling bound was reached.
    Limit,
    /// Caller deadline elapsed.
    TimedOut,
    /// Local cancellation; accepted remote work may still be running.
    Cancelled,
    /// Lossless shared Fabric operation failure, including raw service data and evidence.
    SharedOperation(Box<fabric_sdk_operations::OperationError>),
    /// Lossless generic OneLake read/list/properties classification.
    OneLake(Box<OneLakeError>),
    /// Generic jobs failure with source-facing fields retained for CLI projection.
    Maintenance {
        /// Python-compatible error classification.
        code: String,
        /// Source-facing message, never included in this error's Display.
        message: String,
        /// Service row for a shared table/text renderer, when supplied.
        content: Option<Box<Value>>,
    },
}

/// Failed or incomplete TABLE work with explicit reconciliation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct TableError {
    /// Failure origin.
    pub kind: TableErrorKind,
    /// Observed accepted operation/job ID, when available.
    pub operation_id: Option<String>,
    /// True if a mutation may have reached the service. Never automatically replay.
    pub mutation_may_have_occurred: bool,
}

impl TableError {
    /// Construct a pre-execution failure with no mutation evidence.
    pub fn new(kind: TableErrorKind) -> Self {
        Self {
            kind,
            operation_id: None,
            mutation_may_have_occurred: false,
        }
    }

    pub(crate) fn invalid_path() -> Self {
        Self::new(TableErrorKind::InvalidInput)
    }
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("table operation failed or is incomplete; inspect classification and reconciliation evidence")
    }
}
impl std::error::Error for TableError {}

/// Bounded wire response; acceptance is not evidence of operation completion.
#[derive(Clone, Debug)]
pub struct TableResponse {
    /// HTTP status.
    pub status: u16,
    /// Decoded bytes bounded during streaming by shared execution.
    pub body: Vec<u8>,
    /// Case-insensitive header names; ambiguous duplicates are rejected.
    pub headers: BTreeMap<String, String>,
}

/// Request constructed only from explicit, validated table/source addresses.
///
/// Debug omits the URL, content, and injected token.
pub struct TableRequest<'a> {
    method: &'static str,
    url: Url,
    token: &'a AccessToken,
    credential: &'a dyn TokenCredential,
    selection: &'a CredentialSelection,
    body: Vec<u8>,
    max_response_bytes: NonZeroUsize,
    max_requests: NonZeroUsize,
}

impl TableRequest<'_> {
    /// GET, HEAD or POST.
    pub fn method(&self) -> &'static str {
        self.method
    }
    /// Validated public-cloud Fabric or OneLake URL.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Explicit caller-injected access token; never log it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Same explicit provider for the shared operation engine's Fabric poll requests.
    pub fn credential(&self) -> &dyn TokenCredential {
        self.credential
    }
    /// Explicit Fabric account/tenant selection bound to the original provider.
    pub fn selection(&self) -> &CredentialSelection {
        self.selection
    }
    /// Exact serialized request body; empty for GET/HEAD.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Maximum decoded response bytes; execution must enforce it while streaming.
    pub fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Remaining request budget including this submission and subsequent LRO requests.
    pub fn max_requests(&self) -> NonZeroUsize {
        self.max_requests
    }
}

impl fmt::Debug for TableRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableRequest")
            .field("method", &self.method)
            .field("url", &"<validated>")
            .field("token", &"<injected>")
            .field("body", &"<table data>")
            .finish()
    }
}

/// Proven load state returned by the shared operation engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadOutcome {
    /// An immediate or reconciled 200/201 response.
    Completed,
    /// Source accepts 204 but emits no table success message.
    NoOutput,
    /// Accepted but not proven complete; Python prints its started message.
    Accepted {
        /// Operation ID if the service supplied one.
        operation_id: Option<String>,
    },
}

/// Table execution seam, implemented by an adapter over shared HTTP/LRO policy.
///
/// Implementations must enforce deadlines/streaming bounds, disable redirects
/// and mutation replay, and drop local resources when the future is dropped.
/// `load` owns generic operation reconciliation, not another table polling loop.
pub trait TableExecutor: Send + Sync {
    /// Select the existing generic OneLake SDK for complete listing/read/properties.
    ///
    /// `None` preserves the original caller-supplied raw table read boundary.
    /// [`TableWithOneLake`] supplies this explicit generic SDK composition.
    fn onelake(&self) -> Option<&dyn OneLakeExecutor> {
        None
    }
    /// Perform bounded storage reading or source-properties inspection.
    ///
    /// Filesystem listings must accumulate all continuation pages through the
    /// generic OneLake pager. Partial results must fail, never appear complete.
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>>;
    /// Submit one Lakehouse load and use the shared Fabric operation engine.
    ///
    /// A lost response is an uncertain mutation, not permission to retry.
    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>>;
}

/// Compose TABLE load execution with the separately owned generic OneLake executor.
///
/// This adapter does not create transport, credentials or another paging engine.
/// Existing raw read consumers remain supported; TABLE SDK methods prefer the
/// selected generic OneLake client and acquire exactly one token per page/request.
pub struct TableWithOneLake<'a> {
    /// Existing table load/raw execution, typically the shared HTTP CLI adapter.
    pub tables: &'a dyn TableExecutor,
    /// Generic OneLake transport adapter supplied by its owner.
    pub storage: &'a dyn OneLakeExecutor,
}
impl TableExecutor for TableWithOneLake<'_> {
    fn onelake(&self) -> Option<&dyn OneLakeExecutor> {
        Some(self.storage)
    }
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        self.tables.read(request)
    }
    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        self.tables.load(request, cancelled)
    }
}

/// Complete generic-job result, including source terminal states without output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaintenanceOutcome {
    /// A job reached Completed.
    Completed {
        /// Observed instance ID.
        instance_id: String,
    },
    /// Source treats Deduped/Cancelled as terminal but emits no success envelope.
    TerminalWithoutOutput {
        /// Observed instance ID.
        instance_id: String,
        /// Cancelled or Deduped.
        status: String,
    },
    /// Cancellation request accepted; not evidence that cancellation completed.
    CancellationAccepted {
        /// Observed instance ID.
        instance_id: String,
    },
    /// Source timeout with configured no-cancellation; the remote job remains running.
    TimedOutWithoutCancellation {
        /// Accepted run that can still be inspected or explicitly cancelled.
        instance_id: String,
        /// Source-accounted elapsed seconds.
        elapsed_seconds: u64,
    },
}

/// Generic jobs boundary, implemented by the separately owned job client.
pub trait MaintenanceExecutor: Send + Sync {
    /// Run `TableMaintenance` synchronously using the supplied execution data.
    ///
    /// The adapter owns generic job submission, status, timeout and explicit
    /// cancellation policy. No temporary file is necessary in the native port.
    fn run<'a>(
        &'a self,
        table: &'a TablePath,
        execution_data: Value,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>>;

    /// Execute with typed lifecycle observations on the same caller runtime.
    ///
    /// Existing custom executors may use the result-only default. The shared
    /// JobClient adapter overrides this to expose creation/status/timeout events.
    fn run_observed<'a>(
        &'a self,
        table: &'a TablePath,
        execution_data: Value,
        _observe: &'a mut (dyn FnMut(MaintenanceEvent) -> Result<(), TableError> + Send),
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        self.run(table, execution_data, cancelled)
    }

    /// Preserve the owning item's source-selected generic job type.
    ///
    /// Python's nested `job run` overwrites the initial TableMaintenance flag
    /// with the item's job type. Existing custom TABLE-only adapters fail
    /// explicitly for other types; [`JobMaintenance`] handles every generic type.
    fn run_observed_for_job<'a>(
        &'a self,
        table: &'a TablePath,
        execution_data: Value,
        job_type: MaintenanceJobType,
        observe: &'a mut (dyn FnMut(MaintenanceEvent) -> Result<(), TableError> + Send),
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        if job_type == MaintenanceJobType::TableMaintenance {
            self.run_observed(table, execution_data, observe, cancelled)
        } else {
            Box::pin(async {
                Err(TableError::new(TableErrorKind::Maintenance {
                code:"NotSupported".into(),message:"The selected maintenance executor does not support the owning item's job type".into(),content:None,
            }))
            })
        }
    }
}

/// Finite bounds for one schema or load operation.
#[derive(Clone, Copy, Debug)]
pub struct TableLimits {
    /// Maximum schema read/properties requests before failing explicitly.
    pub max_requests: NonZeroUsize,
    /// Maximum log entries accumulated by the OneLake listing adapter.
    pub max_logs: NonZeroUsize,
    /// Per-response decoded byte limit, enforced during streaming.
    pub max_response_bytes: NonZeroUsize,
}
impl Default for TableLimits {
    fn default() -> Self {
        Self {
            max_requests: NonZeroUsize::new(100).unwrap(),
            max_logs: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// Runtime-neutral SDK client with explicitly approved Fabric and OneLake scopes.
pub struct TableClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
    onelake: OneLakeClient,
}

impl fmt::Debug for TableClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableClient")
            .field("credential", &"<injected>")
            .finish()
    }
}

impl TableClient {
    /// Bind one caller-owned provider to explicit same-account, same-tenant scopes.
    ///
    /// This validates selection metadata, not the provider's token claims. No
    /// credential is acquired until a service method is awaited.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        fabric: &CredentialSelection,
        storage: &CredentialSelection,
    ) -> Result<Self, TableError> {
        if fabric.audience() != Audience::Fabric
            || storage.audience() != Audience::OneLake
            || fabric.account() != storage.account()
            || fabric.tenant() != storage.tenant()
        {
            return Err(TableError::invalid_path());
        }
        let onelake = OneLakeClient::new(Arc::clone(&credential), storage.clone());
        Ok(Self {
            credential,
            selection: fabric.clone(),
            onelake,
        })
    }

    /// Read the first metadata action in reverse-lexicographic JSON commit order.
    ///
    /// The OneLake executor supplies a complete bounded listing, matching Python's
    /// shared continuation handling. Checkpoints do not supply schema. A malformed
    /// newer log is an error, not a skipped file.
    pub async fn schema(
        &self,
        executor: &dyn TableExecutor,
        table: &TablePath,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, TableError> {
        let schema = self
            .schema_string(executor, table, limits, cancelled)
            .await?;
        schema_fields(&schema)
    }

    /// Extract the raw metadata schema string before the CLI success diagnostic.
    ///
    /// Python prints its extraction diagnostic before decoding the inner schema.
    /// Keeping these phases separate preserves that ordering even on parse error.
    pub async fn schema_string(
        &self,
        executor: &dyn TableExecutor,
        table: &TablePath,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<String, TableError> {
        let mut session = Session::new(self, executor, limits, cancelled);
        let local = format!("{}/_delta_log", table.local_path())
            .replace(".Shortcut", "")
            .replace(".shortcut", "");
        let mut url = storage_url(&[table.workspace(), ""])?;
        url.query_pairs_mut()
            .append_pair("recursive", "false")
            .append_pair("resource", "filesystem")
            .append_pair("directory", &format!("{}/{local}", table.item()))
            .append_pair("getShortcutMetadata", "true");
        let directory = TablePath::new(table.workspace(), table.item(), &local)?;
        let response = session.read("GET", url, &directory, true).await?;
        if matches!(response.status, 202 | 204) {
            return Err(TableError::new(TableErrorKind::InvalidDeltaTable));
        }
        ensure_status(&response, &[200, 201])?;
        let listing: Value = serde_json::from_slice(&response.body)
            .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?;
        let empty = Vec::new();
        let paths = match listing.get("paths") {
            None => &empty,
            Some(value) => value
                .as_array()
                .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?,
        };
        if paths.len() > limits.max_logs.get() {
            return Err(TableError::new(TableErrorKind::Limit));
        }
        let mut logs = Vec::new();
        for entry in paths {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?;
            if name.ends_with(".json") {
                let (_, local) = name
                    .split_once('/')
                    .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?;
                if local.split('/').any(|part| !model::safe_segment(part)) {
                    return Err(TableError::new(TableErrorKind::InvalidResponse));
                }
                logs.push(format!("{}/{}/{}", table.workspace(), table.item(), local));
            }
        }
        logs.sort_by(|a, b| b.cmp(a));
        for log in logs {
            let relative = log
                .splitn(3, '/')
                .nth(2)
                .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?;
            let file = TablePath::new(table.workspace(), table.item(), relative)?;
            let response = session
                .read(
                    "GET",
                    storage_url(&log.split('/').collect::<Vec<_>>())?,
                    &file,
                    false,
                )
                .await?;
            if matches!(response.status, 202 | 204) {
                continue;
            }
            ensure_status(&response, &[200, 201])?;
            let text = std::str::from_utf8(&response.body)
                .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?;
            for line in text.trim().split('\n') {
                let action: Value = serde_json::from_str(line)
                    .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?;
                match &action {
                    Value::Object(_) => {}
                    Value::Array(values)
                        if !values.iter().any(|v| v.as_str() == Some("metaData")) =>
                    {
                        continue;
                    }
                    Value::String(value) if !value.contains("metaData") => continue,
                    _ => return Err(TableError::new(TableErrorKind::InvalidResponse)),
                }
                if let Some(metadata) = action.get("metaData") {
                    let schema = metadata
                        .get("schemaString")
                        .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?;
                    if match schema {
                        Value::Null => true,
                        Value::Bool(value) => !value,
                        Value::Number(value) => value.as_f64() == Some(0.0),
                        Value::String(value) => value.is_empty(),
                        Value::Array(value) => value.is_empty(),
                        Value::Object(value) => value.is_empty(),
                    } {
                        return Err(TableError::new(TableErrorKind::InvalidDeltaTable));
                    }
                    let schema = schema
                        .as_str()
                        .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))?;
                    return Ok(schema.to_owned());
                }
            }
        }
        Err(TableError::new(TableErrorKind::InvalidDeltaTable))
    }

    /// HEAD the source then submit the exact Lakehouse load request.
    ///
    /// Schema-qualified paths still address the final name only, as in Python.
    /// Cross-item source addresses are not silently rewritten: only their local
    /// path enters the load payload, preserving that source quirk.
    pub async fn load(
        &self,
        executor: &dyn TableExecutor,
        table: &TablePath,
        source: &TablePath,
        options: &LoadOptions,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<LoadOutcome, TableError> {
        let mut cancelled = pin!(cancelled);
        let kind = self
            .source_kind(executor, source, limits, cancelled.as_mut())
            .await?;
        if limits.max_requests.get() < 2 {
            return Err(TableError::new(TableErrorKind::Limit));
        }
        self.load_with_kind(
            executor,
            table,
            source,
            kind,
            options,
            TableLimits {
                max_requests: NonZeroUsize::new(limits.max_requests.get() - 1).unwrap(),
                ..limits
            },
            cancelled.as_mut(),
        )
        .await
    }

    /// Inspect the source before parsing format options, preserving Python I/O order.
    pub async fn source_kind(
        &self,
        executor: &dyn TableExecutor,
        source: &TablePath,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<SourceKind, TableError> {
        let mut session = Session::new(self, executor, limits, cancelled);
        let response = session
            .read(
                "HEAD",
                storage_url(&source.storage_segments())?,
                source,
                false,
            )
            .await?;
        ensure_status(&response, &[200, 201])?;
        match header(&response, "x-ms-resource-type")? {
            Some("file") => Ok(SourceKind::File),
            Some("directory") => Ok(SourceKind::Folder),
            Some(_) => Err(TableError::invalid_path()),
            None => Err(TableError::new(TableErrorKind::InvalidResponse)),
        }
    }

    /// Submit using a caller-observed source kind, without a second HEAD.
    ///
    /// This composes source-ordered CLI validation with the reusable typed API.
    #[allow(clippy::too_many_arguments)]
    pub async fn load_with_kind(
        &self,
        executor: &dyn TableExecutor,
        table: &TablePath,
        source: &TablePath,
        kind: SourceKind,
        options: &LoadOptions,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<LoadOutcome, TableError> {
        let mut session = Session::new(self, executor, limits, cancelled);
        let mut url = Url::parse("https://api.fabric.microsoft.com/v1/")
            .map_err(|_| TableError::invalid_path())?;
        url.path_segments_mut()
            .map_err(|_| TableError::invalid_path())?
            .pop_if_empty()
            .extend([
                "workspaces",
                table.workspace(),
                "lakehouses",
                table.item(),
                "tables",
                table.table_name(),
                "load",
            ]);
        let body = fabric_core_json::json_dumps(&options.payload(source, kind)).into_bytes();
        session.check().await?;
        if session.requests >= limits.max_requests.get() {
            return Err(TableError::new(TableErrorKind::Limit));
        }
        let scopes = [Audience::Fabric.scope()];
        let token = session
            .run(self.credential.get_token(&scopes, None))
            .await?
            .map_err(|_| TableError::new(TableErrorKind::Credential))?;
        session.check().await?;
        let request = TableRequest {
            method: "POST",
            url,
            token: &token,
            credential: self.credential.as_ref(),
            selection: &self.selection,
            body,
            max_response_bytes: limits.max_response_bytes,
            max_requests: limits.max_requests,
        };
        executor.load(request, session.cancelled).await
    }
}

/// Decode the inner schema and select its exact `fields` value.
pub fn schema_fields(schema: &str) -> Result<Value, TableError> {
    let schema: Value = serde_json::from_str(schema)
        .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?;
    schema_fields_value(&schema)
}

/// Select fields from a caller-decoded schema without parsing or numeric coercion.
///
/// CLI consumers use their existing strict JSON decoder before this SDK selector.
pub fn schema_fields_value(schema: &Value) -> Result<Value, TableError> {
    schema
        .get("fields")
        .cloned()
        .ok_or_else(|| TableError::new(TableErrorKind::InvalidResponse))
}

struct Session<'a> {
    client: &'a TableClient,
    executor: &'a dyn TableExecutor,
    limits: TableLimits,
    cancelled: TableFuture<'a, ()>,
    requests: usize,
}
impl<'a> Session<'a> {
    fn new(
        client: &'a TableClient,
        executor: &'a dyn TableExecutor,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Self {
        Self {
            client,
            executor,
            limits,
            cancelled: Box::pin(cancelled),
            requests: 0,
        }
    }
    async fn run<T>(&mut self, future: impl Future<Output = T>) -> Result<T, TableError> {
        let mut future = pin!(future);
        poll_fn(|cx| {
            if self.cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(TableError::new(TableErrorKind::Cancelled)));
            }
            future.as_mut().poll(cx).map(Ok)
        })
        .await
    }
    async fn check(&mut self) -> Result<(), TableError> {
        self.run(std::future::ready(())).await
    }
    async fn read(
        &mut self,
        method: &'static str,
        url: Url,
        resource: &TablePath,
        listing: bool,
    ) -> Result<TableResponse, TableError> {
        self.check().await?;
        if self.requests >= self.limits.max_requests.get() {
            return Err(TableError::new(TableErrorKind::Limit));
        }
        if let Some(executor) = self.executor.onelake() {
            let path =
                OneLakePath::new(resource.workspace(), resource.item(), resource.local_path())
                    .map_err(one_lake_error)?;
            let limits = OneLakeLimits {
                max_pages: NonZeroUsize::new(self.limits.max_requests.get() - self.requests)
                    .unwrap(),
                max_paths: self.limits.max_logs,
                max_response_bytes: self.limits.max_response_bytes,
            };
            let client = &self.client.onelake;
            let response = if listing {
                let result = self
                    .run(client.list(executor, &path, false, limits, std::future::pending()))
                    .await?
                    .map_err(one_lake_error)?;
                self.requests += result.pages_received;
                TableResponse {
                    status: 200,
                    body: serde_json::to_vec(&serde_json::json!({"paths":result.paths}))
                        .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?,
                    headers: BTreeMap::new(),
                }
            } else if method == "HEAD" {
                let headers = self
                    .run(client.properties(executor, &path, limits, std::future::pending()))
                    .await?
                    .map_err(one_lake_error)?;
                self.requests += 1;
                TableResponse {
                    status: 200,
                    body: Vec::new(),
                    headers,
                }
            } else {
                let body = self
                    .run(client.read(executor, &path, limits, std::future::pending()))
                    .await?
                    .map_err(one_lake_error)?;
                self.requests += 1;
                TableResponse {
                    status: 200,
                    body,
                    headers: BTreeMap::new(),
                }
            };
            if response.body.len() > self.limits.max_response_bytes.get() {
                return Err(TableError::new(TableErrorKind::Limit));
            }
            return Ok(response);
        }
        let scopes = [Audience::OneLake.scope()];
        let token = self
            .run(self.client.credential.get_token(&scopes, None))
            .await?
            .map_err(|_| TableError::new(TableErrorKind::Credential))?;
        self.check().await?;
        self.requests += 1;
        let request = TableRequest {
            method,
            url,
            token: &token,
            credential: self.client.credential.as_ref(),
            selection: &self.client.selection,
            body: Vec::new(),
            max_response_bytes: self.limits.max_response_bytes,
            max_requests: NonZeroUsize::new(self.limits.max_requests.get() - self.requests + 1)
                .unwrap(),
        };
        let response = self.run(self.executor.read(request)).await??;
        if response.body.len() > self.limits.max_response_bytes.get() {
            return Err(TableError::new(TableErrorKind::Limit));
        }
        Ok(response)
    }
}

fn one_lake_error(error: OneLakeError) -> TableError {
    TableError::new(TableErrorKind::OneLake(Box::new(error)))
}

fn storage_url(segments: &[&str]) -> Result<Url, TableError> {
    let mut url = Url::parse("https://onelake.dfs.fabric.microsoft.com/")
        .map_err(|_| TableError::invalid_path())?;
    url.path_segments_mut()
        .map_err(|_| TableError::invalid_path())?
        .clear()
        .extend(segments.iter().copied());
    Ok(url)
}

fn header<'a>(response: &'a TableResponse, name: &str) -> Result<Option<&'a str>, TableError> {
    let mut entries = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let result = entries.next().map(|(_, value)| value.as_str());
    if entries.next().is_some() {
        return Err(TableError::new(TableErrorKind::InvalidResponse));
    }
    Ok(result)
}

fn ensure_status(response: &TableResponse, allowed: &[u16]) -> Result<(), TableError> {
    if allowed.contains(&response.status) {
        return Ok(());
    }
    let body: Option<Value> = serde_json::from_slice(&response.body).ok();
    let code = header(response, "x-ms-public-api-error-code")?
        .or(header(response, "x-ms-error-code")?)
        .map(str::to_owned)
        .or_else(|| body.as_ref()?.get("errorCode")?.as_str().map(str::to_owned));
    let message = body
        .as_ref()
        .and_then(|body| body.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Err(TableError::new(TableErrorKind::Http {
        status: response.status,
        code,
        message,
    }))
}
