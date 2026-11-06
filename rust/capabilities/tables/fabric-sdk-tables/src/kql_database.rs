// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-owned, runtime-neutral KQL database operations.
//!
//! The caller resolves the exact Fabric item and database, injects credentials,
//! transport, cancellation and a deadline timer. No transport, identity backend,
//! runtime, polling, upload, queued ingestion, overwrite or replay is installed.
//! Query text is not rewritten: table identity is context, not a single-table
//! security boundary. The executor must preserve the read-only request header.
//!
//! # Transport support
//!
//! These APIs implement native Kusto request construction and response handling
//! over an injected [`KustoExecutor`]. This module does **not** provide a built-in
//! native HTTP transport or select one through a default client. Reexporting the
//! module through an SDK facade does not install an executor or enable native
//! CLI execution. Callers must supply their own conforming executor and timer.
//! Offline fake-executor contracts are test sources, not live-service acceptance
//! or evidence of compilation. Asynchronous storage ingestion remains explicitly
//! unsupported; synchronous existing-storage pull never fabricates a job.
//!
//! Protocol references: Microsoft Learn Kusto `api/rest/request`, `response` and
//! `response-v2` (2024-11-22 / 2024-08-20), and the preparation packet's pinned
//! Fabio KQL utilities/intelligence sources. This implements the bounded native
//! subset, not Fabio's URI-derived audience or unbounded response handling.
//! Partial-failure handling additionally follows Microsoft Learn
//! `concepts/partial-query-failures` and Azure/azure-kusto-python fixtures pinned
//! at `3f53425f7ff8d02de8fc0ea250d02dde30c7d258`: v2 completion information uses
//! `Level`, while nondeferred failures embed `OneApiErrors` objects among rows.
//! V1 multi-table responses resolve roles from the final table of contents;
//! opaque wire table names are retained rather than treated as logical roles.

use azure_core::{
    credentials::{AccessToken, TokenCredential},
    http::Url,
};
use fabric_core::cancel_with;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    future::Future,
    io::{self, Write},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Approved public-cloud service audience, independent of the cluster URI.
pub const KUSTO_SCOPE: &str = "https://api.kusto.windows.net/.default";
/// Maximum serialized transport bytes, not a service CSV ingestion guarantee.
pub const MAX_TRANSPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 1024;
const MAX_SOURCES: usize = 64;

/// Caller-runtime asynchronous work.
pub type KustoFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sanitized operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoErrorKind {
    /// Invalid public-cloud cluster or exact parent identity.
    InvalidBinding,
    /// Invalid name, source, query or options.
    InvalidInput,
    /// A finite raw-input or serialized-request limit was exceeded.
    InputLimit,
    /// The bounded response/table/column/row limit was exceeded.
    ResponseLimit,
    /// This native operation is deliberately unsupported.
    Unsupported,
    /// The injected credential failed.
    Credential,
    /// Transport failed; no automatic replay is safe.
    Execution,
    /// The caller stopped local work, not remote execution.
    Cancelled,
    /// The caller's finite deadline elapsed.
    TimedOut,
    /// The service returned a non-200 response, including unobserved acceptance.
    Http {
        /// HTTP response status.
        status: u16,
    },
    /// The response was malformed or used an unsupported protocol shape.
    InvalidResponse,
    /// Native completion evidence explicitly reports failure.
    Service,
    /// Some results/extents were generated, but completion was not successful.
    Partial,
    /// The available evidence does not establish successful completion.
    UnknownOutcome,
}

/// Failure with explicit, bounded reconciliation evidence.
///
/// Debug and Display never include service text, query/data, URLs or tokens.
pub struct KustoError {
    /// Failure classification.
    pub kind: KustoErrorKind,
    /// True once ingestion dispatch starts, even if its response is lost.
    pub mutation_may_have_occurred: bool,
    /// Parsed partial read results, if available.
    pub result: Option<Box<KustoResult>>,
    /// Parsed ingestion receipts, if available; never a fabricated job handle.
    pub load: Option<Box<KustoLoadOutcome>>,
}

impl KustoError {
    fn new(kind: KustoErrorKind) -> Self {
        Self {
            kind,
            mutation_may_have_occurred: false,
            result: None,
            load: None,
        }
    }
}

impl fmt::Debug for KustoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoError")
            .field("kind", &self.kind)
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .finish_non_exhaustive()
    }
}
impl fmt::Display for KustoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Kusto operation failed or is incomplete; inspect typed completion evidence")
    }
}
impl std::error::Error for KustoError {}

/// Exact parent metadata resolved by the caller, with a validated query origin.
#[derive(Clone)]
pub struct KustoBinding {
    workspace_id: String,
    item_id: String,
    database: String,
    query_origin: Url,
}

impl KustoBinding {
    /// Binds an exact workspace/item/database to an HTTPS public Kusto cluster.
    ///
    /// Only a root URI under `*.kusto.fabric.microsoft.com` or
    /// `*.kusto.windows.net` is accepted. Credentials, ports, query/fragment,
    /// encoded authorities, paths and ingestion endpoints are rejected.
    /// This validates coordinates, not metadata provenance or target permission.
    ///
    /// # Errors
    ///
    /// Returns `InvalidBinding` before credential access for invalid coordinates.
    pub fn new(
        workspace_id: impl Into<String>,
        item_id: impl Into<String>,
        database: impl Into<String>,
        query_service_uri: &str,
    ) -> Result<Self, KustoError> {
        let workspace_id = workspace_id.into();
        let item_id = item_id.into();
        let database = database.into();
        let invalid = || KustoError::new(KustoErrorKind::InvalidBinding);
        if !is_guid(&workspace_id)
            || is_zero_guid(&workspace_id)
            || !is_guid(&item_id)
            || is_zero_guid(&item_id)
            || !valid_name(&database)
        {
            return Err(invalid());
        }
        let authority = query_service_uri
            .strip_prefix("https://")
            .ok_or_else(invalid)?
            .strip_suffix('/')
            .unwrap_or_else(|| &query_service_uri["https://".len()..]);
        if authority.is_empty()
            || !authority
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        {
            return Err(invalid());
        }
        let query_origin = Url::parse(query_service_uri).map_err(|_| invalid())?;
        let host = query_origin.host_str().ok_or_else(invalid)?;
        let prefix = host
            .strip_suffix(".kusto.fabric.microsoft.com")
            .or_else(|| host.strip_suffix(".kusto.windows.net"))
            .ok_or_else(invalid)?;
        if !valid_dns_prefix(prefix) || prefix.starts_with("ingest-") {
            return Err(invalid());
        }
        Ok(Self {
            workspace_id,
            item_id,
            database,
            query_origin,
        })
    }

    /// Exact workspace ID; never inferred from a table name.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    /// Exact KQLDatabase item ID.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
    /// Resolved native database name, not necessarily its item ID.
    pub fn database(&self) -> &str {
        &self.database
    }
    /// Validated HTTPS query origin.
    pub fn query_origin(&self) -> &Url {
        &self.query_origin
    }
}

impl fmt::Debug for KustoBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KustoBinding(<validated parent context>)")
    }
}

/// Validated table identity within the exact parent database.
#[derive(Clone)]
pub struct KustoTable(String);

impl KustoTable {
    /// Validates a nonempty, bounded name without control characters.
    ///
    /// Quotes, backslashes and punctuation are retained and safely escaped when
    /// used as Kusto bracketed identifiers, not interpolated as syntax.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for empty, oversized or control-bearing names.
    pub fn new(name: impl Into<String>) -> Result<Self, KustoError> {
        let name = name.into();
        if !valid_name(&name) {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        Ok(Self(name))
    }
    /// Original table name, for explicit caller projection only.
    pub fn name(&self) -> &str {
        &self.0
    }
    fn quoted(&self) -> String {
        format!("[{}]", literal(&self.0))
    }
}
impl fmt::Debug for KustoTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KustoTable(<redacted>)")
    }
}

/// Explicit finite limits, with the shared serialized transport ceiling.
#[derive(Clone, Copy, Debug)]
pub struct KustoLimits {
    max_input_bytes: NonZeroUsize,
    max_request_bytes: NonZeroUsize,
    max_response_bytes: NonZeroUsize,
    max_rows: NonZeroUsize,
    max_tables: NonZeroUsize,
    max_columns: NonZeroUsize,
}

impl KustoLimits {
    /// Sets raw-input, serialized-request, decoded-response and shape limits.
    ///
    /// All byte limits are bytes, not characters; none may exceed 4 MiB. JSON
    /// escaping and management syntax count toward the serialized request cap.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for zero or above-ceiling limits.
    pub fn new(
        max_input_bytes: usize,
        max_request_bytes: usize,
        max_response_bytes: usize,
        max_rows: usize,
        max_tables: usize,
        max_columns: usize,
    ) -> Result<Self, KustoError> {
        let nz = |value| {
            NonZeroUsize::new(value)
                .filter(|n| n.get() <= MAX_TRANSPORT_BYTES)
                .ok_or_else(|| KustoError::new(KustoErrorKind::InvalidInput))
        };
        Ok(Self {
            max_input_bytes: nz(max_input_bytes)?,
            max_request_bytes: nz(max_request_bytes)?,
            max_response_bytes: nz(max_response_bytes)?,
            max_rows: nz(max_rows)?,
            max_tables: nz(max_tables)?,
            max_columns: nz(max_columns)?,
        })
    }
    /// Maximum decoded response bytes; transport must enforce while streaming.
    pub fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Maximum serialized request bytes.
    pub fn max_request_bytes(&self) -> NonZeroUsize {
        self.max_request_bytes
    }
}

impl Default for KustoLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: NonZeroUsize::new(1024 * 1024).expect("positive constant"),
            max_request_bytes: NonZeroUsize::new(MAX_TRANSPORT_BYTES).expect("positive constant"),
            max_response_bytes: NonZeroUsize::new(MAX_TRANSPORT_BYTES).expect("positive constant"),
            max_rows: NonZeroUsize::new(10_000).expect("positive constant"),
            max_tables: NonZeroUsize::new(64).expect("positive constant"),
            max_columns: NonZeroUsize::new(1024).expect("positive constant"),
        }
    }
}

/// One call's caller-owned cancellation, deadline timer and finite limits.
pub struct KustoCall<'a> {
    limits: KustoLimits,
    deadline: Instant,
    cancelled: KustoFuture<'a, ()>,
    expired: KustoFuture<'a, ()>,
}

impl<'a> KustoCall<'a> {
    /// Supplies a deadline at most one hour ahead and its caller-runtime timer.
    ///
    /// `expired` must become ready by `deadline`; it bounds token acquisition as
    /// well as transport. The executor must also enforce the absolute deadline.
    /// Cancellation wins if both cancellation and completion are ready.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` for an elapsed deadline or `InvalidInput` above one hour.
    pub fn new(
        limits: KustoLimits,
        deadline: Instant,
        cancelled: impl Future<Output = ()> + Send + 'a,
        expired: impl Future<Output = ()> + Send + 'a,
    ) -> Result<Self, KustoError> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| KustoError::new(KustoErrorKind::TimedOut))?;
        if remaining > Duration::from_secs(3600) {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        Ok(Self {
            limits,
            deadline,
            cancelled: Box::pin(cancelled),
            expired: Box::pin(expired),
        })
    }
}

impl fmt::Debug for KustoCall<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoCall")
            .field("limits", &self.limits)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// Validated request intent; management is not inferred from arbitrary text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoRequestKind {
    /// Fixed read-only native schema command.
    Schema,
    /// Fixed read-only native table statistics command.
    Statistics,
    /// Exact caller-supplied query using read-only query protocol.
    Query,
    /// Append-only inline CSV.
    InlineIngestion,
    /// Append-only synchronous existing-storage pull.
    StorageIngestion,
}

impl KustoRequestKind {
    fn mutation(self) -> bool {
        matches!(self, Self::InlineIngestion | Self::StorageIngestion)
    }
}

/// Validated authenticated request, constructed only by the parent client.
///
/// Implementors must not log tokens, body, table context or source URLs.
pub struct KustoRequest<'a> {
    kind: KustoRequestKind,
    binding: &'a KustoBinding,
    table: &'a KustoTable,
    url: Url,
    token: &'a AccessToken,
    body: Vec<u8>,
    deadline: Instant,
    limits: KustoLimits,
}

impl KustoRequest<'_> {
    /// Validated native request intent.
    pub fn kind(&self) -> KustoRequestKind {
        self.kind
    }
    /// Exact parent context, for explicit access checks.
    pub fn binding(&self) -> &KustoBinding {
        self.binding
    }
    /// Table context; query execution is not isolated to this table.
    pub fn table(&self) -> &KustoTable {
        self.table
    }
    /// Always POST.
    pub fn method(&self) -> &'static str {
        "POST"
    }
    /// Validated cluster and fixed management/query route.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Caller-injected credential's token for the fixed Kusto scope.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Already serialized, bounded JSON; never rewrite or log it.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Headers which MUST be preserved, especially `x-ms-readonly`.
    pub fn headers(&self) -> Vec<(&'static str, &'static str)> {
        let mut headers = vec![
            ("accept", "application/json"),
            ("content-type", "application/json; charset=utf-8"),
        ];
        if !self.kind.mutation() {
            headers.push(("x-ms-readonly", "true"));
        }
        headers
    }
    /// Finite absolute deadline; no transport retry may extend it.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    /// Explicit serialized/decoded byte limits to enforce during I/O.
    pub fn limits(&self) -> KustoLimits {
        self.limits
    }
}

impl fmt::Debug for KustoRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoRequest")
            .field("kind", &self.kind)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Raw response after transport-side streaming and decoded-byte enforcement.
pub struct KustoResponse {
    /// Actual HTTP response status; 200 alone never establishes completion.
    pub status: u16,
    /// Bounded decoded JSON bytes.
    pub body: Vec<u8>,
}

impl fmt::Debug for KustoResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoResponse")
            .field("status", &self.status)
            .field("bytes", &self.body.len())
            .finish()
    }
}

/// Transport classification without arbitrary provider diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoExecutionError {
    /// The executor can prove the request was not sent.
    NotDispatched,
    /// Dispatch occurred or is uncertain and no complete response was received.
    ResponseLost,
    /// Decoded response exceeded its explicit streaming bound.
    ResponseLimit,
    /// The executor's absolute deadline elapsed.
    TimedOut,
}

/// Consumer-owned one-shot native execution; no implementation is installed here.
pub trait KustoExecutor: Send + Sync {
    /// Execute once on the caller's runtime, preserving URL, body and headers.
    ///
    /// Implementors enforce streaming byte limits and the absolute deadline,
    /// never follow redirects, never replay ingestion, and never turn this into
    /// queued ingestion or an upload. Dropping this future only stops local work.
    fn execute<'a>(
        &'a self,
        request: KustoRequest<'a>,
    ) -> KustoFuture<'a, Result<KustoResponse, KustoExecutionError>>;
}

/// Native completion evidence, independent of HTTP success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoCompletion {
    /// Complete native response with no failure evidence.
    Succeeded,
    /// Explicit failure with no successful result/extent.
    Failed,
    /// Some result/extent exists but not all work is established successful.
    /// Partial query data must be discarded, not consumed as correct results.
    Partial,
    /// Missing or inconclusive native completion evidence.
    Unknown,
}

/// One rectangular native result table, including diagnostic tables.
///
/// Debug omits column names and data.
pub struct KustoResultTable {
    /// Native result table name, not necessarily the addressed table's name.
    pub name: String,
    /// Native role: v2 table kind or validated v1 table-of-contents role.
    /// Absent on simple v1 results without explicit role metadata.
    pub kind: Option<String>,
    /// Column names, in native order.
    pub columns: Vec<String>,
    /// Native scalar types, or v1 .NET types when scalar types are absent.
    /// This vector has exactly one entry per column.
    pub column_types: Vec<String>,
    /// Native rectangular rows; values remain data, never diagnostic text.
    /// Rows retained after query failure are inspection evidence only: the
    /// service may have flushed incorrect data, so discard the entire query.
    pub rows: Vec<Vec<Value>>,
    /// Native `OneApiErrors` objects interleaved among rows, kept separately
    /// from rectangular data. These are bounded by response bytes and row limits
    /// and never appear in Debug or Display.
    pub errors: Vec<Value>,
}

impl fmt::Debug for KustoResultTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoResultTable")
            .field("rows", &self.rows.len())
            .field("columns", &self.columns.len())
            .field("native_errors", &self.errors.len())
            .finish_non_exhaustive()
    }
}

/// Bounded native results; failures retain these in [`KustoError::result`].
///
/// Only `Succeeded` establishes a usable query result. Partial-failure evidence
/// is not a usable subset: Kusto may flush incorrect data on failure, so callers
/// must discard the entire query result rather than trust the retained rows.
#[derive(Debug)]
pub struct KustoResult {
    /// Every returned table, not just the first primary result.
    pub tables: Vec<KustoResultTable>,
    /// Explicit completion state.
    pub completion: KustoCompletion,
}

/// One observed generated extent receipt, never a Fabric operation/job.
pub struct KustoExtent {
    /// Validated native extent ID, including zero GUID if the service returned it.
    pub extent_id: String,
    /// Native ingestion operation ID, if explicitly returned.
    pub operation_id: Option<String>,
    /// Native storage-pull error indication, when supplied.
    pub has_errors: Option<bool>,
    /// Native source locator, retained only for explicit caller projection.
    pub item_loaded: Option<String>,
    /// Truthful per-extent completion.
    pub completion: KustoCompletion,
}

impl fmt::Debug for KustoExtent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoExtent")
            .field("has_errors", &self.has_errors)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

/// Complete bounded ingestion evidence, including multiple extents.
#[derive(Debug)]
pub struct KustoLoadOutcome {
    /// Authoritative aggregate state; zero extents is never success.
    pub completion: KustoCompletion,
    /// All native extent receipts.
    pub extents: Vec<KustoExtent>,
}

/// Native existing-storage format, not a raw management-language fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoStorageFormat {
    /// CSV.
    Csv,
    /// Tab-separated values.
    Tsv,
    /// JSON.
    Json,
    /// Multiline JSON.
    MultiJson,
    /// Parquet.
    Parquet,
}

impl KustoStorageFormat {
    fn token(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::MultiJson => "multijson",
            Self::Parquet => "parquet",
        }
    }
}

/// Validated existing storage object with explicit source authority.
///
/// SAS values and source data are never exposed by Debug or Display.
pub struct KustoStorageSource {
    locator: String,
}

impl KustoStorageSource {
    /// Uses the caller's native source impersonation, not a Fabric/storage token.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for non-HTTPS/non-storage endpoints, container-only
    /// locations, credentials, query/fragment, controls or delimiter injection.
    pub fn impersonated(url: &str) -> Result<Self, KustoError> {
        let parsed = storage_url(url)?;
        if parsed.query().is_some() {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        Ok(Self {
            locator: format!("{};impersonate", parsed.as_str()),
        })
    }

    /// Uses an explicitly supplied existing-object SAS URL.
    ///
    /// This never obtains, refreshes or persists a SAS.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an unsafe URL or missing/ambiguous SAS signature.
    pub fn sas(url: &str) -> Result<Self, KustoError> {
        let parsed = storage_url(url)?;
        let signatures: Vec<_> = parsed
            .query_pairs()
            .filter(|(name, _)| name == "sig")
            .collect();
        if signatures.len() != 1 || signatures[0].1.is_empty() {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        Ok(Self {
            locator: parsed.into(),
        })
    }
}

impl fmt::Debug for KustoStorageSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KustoStorageSource(<redacted>)")
    }
}

/// Typed append-only synchronous pull from already existing storage objects.
pub struct KustoStorageLoad {
    sources: Vec<KustoStorageSource>,
    format: KustoStorageFormat,
    ignore_first_record: bool,
    mapping_reference: Option<String>,
}

impl KustoStorageLoad {
    /// Constructs a synchronous pull with explicit native format.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` unless 1 through 64 existing sources are supplied.
    pub fn new(
        sources: Vec<KustoStorageSource>,
        format: KustoStorageFormat,
    ) -> Result<Self, KustoError> {
        if sources.is_empty() || sources.len() > MAX_SOURCES {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        Ok(Self {
            sources,
            format,
            ignore_first_record: false,
            mapping_reference: None,
        })
    }

    /// Explicitly enables native CSV/TSV first-record skipping.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for non-delimited formats.
    pub fn with_ignore_first_record(mut self) -> Result<Self, KustoError> {
        if !matches!(
            self.format,
            KustoStorageFormat::Csv | KustoStorageFormat::Tsv
        ) {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        self.ignore_first_record = true;
        Ok(self)
    }

    /// Uses an existing native ingestion mapping reference, safely quoted.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an empty, oversized or control-bearing name.
    pub fn with_mapping_reference(mut self, name: impl Into<String>) -> Result<Self, KustoError> {
        let name = name.into();
        if !valid_name(&name) {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        self.mapping_reference = Some(name);
        Ok(self)
    }

    fn command(&self, table: &KustoTable, limits: KustoLimits) -> Result<String, KustoError> {
        let raw_bytes = self.sources.iter().try_fold(0usize, |total, source| {
            total.checked_add(source.locator.len())
        });
        if raw_bytes.is_none_or(|n| n > limits.max_input_bytes.get()) {
            return Err(KustoError::new(KustoErrorKind::InputLimit));
        }
        let sources = self
            .sources
            .iter()
            .map(|source| format!("h{}", literal(&source.locator)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut properties = vec![format!("format={}", literal(self.format.token()))];
        if self.ignore_first_record {
            properties.push("ignoreFirstRecord=true".into());
        }
        if let Some(mapping) = &self.mapping_reference {
            properties.push(format!("ingestionMappingReference={}", literal(mapping)));
        }
        Ok(format!(
            ".ingest into table {} ({sources}) with ({})",
            table.quoted(),
            properties.join(", ")
        ))
    }
}

impl fmt::Debug for KustoStorageLoad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KustoStorageLoad")
            .field("sources", &self.sources.len())
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// Parent-owned reusable async KQLDatabase client with injected credentials.
#[derive(Clone)]
pub struct KqlDatabaseClient {
    credential: Arc<dyn TokenCredential>,
    binding: KustoBinding,
}

impl KqlDatabaseClient {
    /// Stores caller-resolved metadata and an explicit credential; performs no I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, binding: KustoBinding) -> Self {
        Self {
            credential,
            binding,
        }
    }
    /// Exact parent metadata for this client.
    pub fn binding(&self) -> &KustoBinding {
        &self.binding
    }

    /// Retrieves native table schema, not a Fabric item definition or data export.
    ///
    /// # Errors
    ///
    /// Returns typed authentication, transport, bound, cancellation or native
    /// completion failure with no sensitive diagnostic formatting.
    pub async fn schema(
        &self,
        table: &KustoTable,
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoResult, KustoError> {
        self.run(
            table,
            KustoRequestKind::Schema,
            format!(".show table {} schema as json", table.quoted()),
            executor,
            call,
        )
        .await
    }

    /// Retrieves native `.show table ... details` statistics without SQL fallback.
    ///
    /// # Errors
    ///
    /// Returns the same bounded execution/completion failures as [`Self::schema`].
    pub async fn statistics(
        &self,
        table: &KustoTable,
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoResult, KustoError> {
        self.run(
            table,
            KustoRequestKind::Statistics,
            format!(".show table {} details", table.quoted()),
            executor,
            call,
        )
        .await
    }

    /// Executes exact KQL text via v2 query protocol and `x-ms-readonly: true`.
    ///
    /// No text rewriting, management auto-routing, SQL conversion or table
    /// isolation is promised. The selected table is parent context only; native
    /// authorization and the service read-only restriction remain authoritative.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for empty/NUL-bearing text, `InputLimit` for raw or
    /// serialized overflows, and typed execution/completion failures otherwise.
    pub async fn query(
        &self,
        table: &KustoTable,
        query: &str,
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoResult, KustoError> {
        if query.trim().is_empty() || query.contains('\0') {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        if query.len() > call.limits.max_input_bytes.get() {
            return Err(KustoError::new(KustoErrorKind::InputLimit));
        }
        self.run(table, KustoRequestKind::Query, query.into(), executor, call)
            .await
    }

    /// Appends explicit UTF-8 CSV bytes into the existing native table.
    ///
    /// CSV is passed verbatim after the native inline delimiter. This does not
    /// infer schema, upload, replace, batch or fall back to queued ingestion.
    ///
    /// # Errors
    ///
    /// Returns failures before dispatch for invalid/oversized input, or
    /// reconciliation-bearing failures if dispatch or native completion fails.
    pub async fn load_inline_csv(
        &self,
        table: &KustoTable,
        csv: &[u8],
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoLoadOutcome, KustoError> {
        if csv.len() > call.limits.max_input_bytes.get() {
            return Err(KustoError::new(KustoErrorKind::InputLimit));
        }
        let csv =
            std::str::from_utf8(csv).map_err(|_| KustoError::new(KustoErrorKind::InvalidInput))?;
        if csv.is_empty() || csv.contains('\0') {
            return Err(KustoError::new(KustoErrorKind::InvalidInput));
        }
        let result = self
            .run(
                table,
                KustoRequestKind::InlineIngestion,
                format!(".ingest inline into table {} <|\n{csv}", table.quoted()),
                executor,
                call,
            )
            .await;
        finish_ingestion(result, false)
    }

    /// Appends by synchronous native pull from explicitly existing storage.
    ///
    /// No local bytes are uploaded. Source authority is independent of the
    /// Kusto token. There is no overwrite, async job fabrication or replay.
    ///
    /// # Errors
    ///
    /// Returns typed validation/execution/completion failures. Partial/unknown
    /// receipts are retained in [`KustoError::load`], never reported as success.
    pub async fn load_storage(
        &self,
        table: &KustoTable,
        load: &KustoStorageLoad,
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoLoadOutcome, KustoError> {
        let command = load.command(table, call.limits)?;
        let result = self
            .run(
                table,
                KustoRequestKind::StorageIngestion,
                command,
                executor,
                call,
            )
            .await;
        finish_ingestion(result, true)
    }

    /// Explicitly refuses asynchronous storage ingestion without native observation.
    ///
    /// This method performs no credential access or dispatch and never returns a
    /// fabricated Fabric operation/job. Synchronous [`Self::load_storage`] is
    /// available; real async ingestion and `.show operations` remain unsupported.
    ///
    /// # Errors
    ///
    /// Always returns `Unsupported`.
    pub async fn load_storage_async(
        &self,
        _table: &KustoTable,
        _load: &KustoStorageLoad,
    ) -> Result<KustoLoadOutcome, KustoError> {
        Err(KustoError::new(KustoErrorKind::Unsupported))
    }

    async fn run(
        &self,
        table: &KustoTable,
        kind: KustoRequestKind,
        command: String,
        executor: &dyn KustoExecutor,
        call: KustoCall<'_>,
    ) -> Result<KustoResult, KustoError> {
        let KustoCall {
            limits,
            deadline,
            mut cancelled,
            mut expired,
        } = call;
        let authorize = async {
            if Instant::now() >= deadline {
                return Err(KustoError::new(KustoErrorKind::TimedOut));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let server_seconds = remaining.as_secs().clamp(1, 3600);
            let body = request_body(
                &json!({
                    "db": self.binding.database,
                    "csl": command,
                    "properties": {
                        "Options": {
                            "results_progressive_enabled": false,
                            "servertimeout": format!(
                                "{:02}:{:02}:{:02}",
                                server_seconds / 3600,
                                (server_seconds / 60) % 60,
                                server_seconds % 60
                            )
                        }
                    }
                }),
                limits.max_request_bytes.get(),
            )?;
            let token = self
                .credential
                .get_token(&[KUSTO_SCOPE], None)
                .await
                .map_err(|_| KustoError::new(KustoErrorKind::Credential))?;
            if Instant::now() >= deadline {
                return Err(KustoError::new(KustoErrorKind::TimedOut));
            }
            Ok((token, body))
        };
        let (token, body) = bounded(authorize, &mut cancelled, &mut expired).await?;
        let route = if kind == KustoRequestKind::Query {
            "v2/rest/query"
        } else {
            "v1/rest/mgmt"
        };
        let url = self
            .binding
            .query_origin
            .join(route)
            .map_err(|_| KustoError::new(KustoErrorKind::InvalidBinding))?;
        let request = KustoRequest {
            kind,
            binding: &self.binding,
            table,
            url,
            token: &token,
            body,
            deadline,
            limits,
        };
        let dispatched = AtomicBool::new(false);
        let operation = async {
            if Instant::now() >= deadline {
                return Err(KustoError::new(KustoErrorKind::TimedOut));
            }
            dispatched.store(kind.mutation(), Ordering::Relaxed);
            executor.execute(request).await.map_err(|error| {
                let kind = match error {
                    KustoExecutionError::NotDispatched => {
                        dispatched.store(false, Ordering::Relaxed);
                        KustoErrorKind::Execution
                    }
                    KustoExecutionError::ResponseLost => KustoErrorKind::Execution,
                    KustoExecutionError::ResponseLimit => KustoErrorKind::ResponseLimit,
                    KustoExecutionError::TimedOut => KustoErrorKind::TimedOut,
                };
                KustoError::new(kind)
            })
        };
        let response = match bounded(operation, &mut cancelled, &mut expired).await {
            Ok(response) => response,
            Err(mut error) => {
                error.mutation_may_have_occurred |= dispatched.load(Ordering::Relaxed);
                return Err(error);
            }
        };
        let finish = async {
            if Instant::now() >= deadline {
                return Err(KustoError::new(KustoErrorKind::TimedOut));
            }
            if response.body.len() > limits.max_response_bytes.get() {
                return Err(KustoError::new(KustoErrorKind::ResponseLimit));
            }
            if response.status != 200 {
                return Err(KustoError::new(KustoErrorKind::Http {
                    status: response.status,
                }));
            }
            let result = decode(&response.body, kind == KustoRequestKind::Query, limits)?;
            if Instant::now() >= deadline {
                return Err(KustoError::new(KustoErrorKind::TimedOut));
            }
            Ok(result)
        };
        let result = bounded(finish, &mut cancelled, &mut expired).await;
        result.map_err(|mut error| {
            error.mutation_may_have_occurred |= dispatched.load(Ordering::Relaxed);
            error
        })
    }
}

impl fmt::Debug for KqlDatabaseClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KqlDatabaseClient(<injected credential and parent context>)")
    }
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_NAME_BYTES
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn is_zero_guid(value: &str) -> bool {
    value.bytes().all(|b| b == b'0' || b == b'-')
}

fn valid_dns_prefix(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn request_body(value: &Value, limit: usize) -> Result<Vec<u8>, KustoError> {
    struct BoundedBody {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for BoundedBody {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.limit {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut body = BoundedBody {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut body, value)
        .map_err(|_| KustoError::new(KustoErrorKind::InputLimit))?;
    Ok(body.bytes)
}

fn storage_url(value: &str) -> Result<Url, KustoError> {
    let invalid = || KustoError::new(KustoErrorKind::InvalidInput);
    if value.len() > 16 * 1024
        || !value.starts_with("https://")
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || value.contains(['\\', ';', '\'', '"', '#'])
    {
        return Err(invalid());
    }
    let parsed = Url::parse(value).map_err(|_| invalid())?;
    let host = parsed.host_str().ok_or_else(invalid)?;
    let host_valid = matches!(
        host,
        "onelake.blob.fabric.microsoft.com" | "onelake.dfs.fabric.microsoft.com"
    ) || host
        .strip_suffix(".blob.core.windows.net")
        .or_else(|| host.strip_suffix(".dfs.core.windows.net"))
        .is_some_and(|prefix| {
            (3..=24).contains(&prefix.len())
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    let authority = value
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(invalid)?;
    let segments: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
    if !host_valid
        || parsed.as_str() != value
        || authority.contains(['@', ':', '%'])
        || parsed.fragment().is_some()
        || segments.len() < 2
        || segments
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
        || parsed.path().ends_with('/')
    {
        return Err(invalid());
    }
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let encoded = value.get(i + 1..i + 3).ok_or_else(invalid)?;
            let decoded = u8::from_str_radix(encoded, 16).map_err(|_| invalid())?;
            if decoded.is_ascii_control() || matches!(decoded, b';' | b'\\') {
                return Err(invalid());
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(parsed)
}

fn native_error(value: &Value) -> bool {
    ["error", "Error", "Errors", "Exceptions", "OneApiErrors"]
        .iter()
        .any(|name| {
            value.get(name).is_some_and(|v| {
                !v.is_null()
                    && v.as_array().is_none_or(|a| !a.is_empty())
                    && v.as_str().is_none_or(|s| !s.is_empty())
            })
        })
        || value.get("HasErrors").and_then(Value::as_bool) == Some(true)
}

fn decode(bytes: &[u8], v2: bool, limits: KustoLimits) -> Result<KustoResult, KustoError> {
    let invalid = || KustoError::new(KustoErrorKind::InvalidResponse);
    reject_duplicate_keys(bytes)?;
    let root: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let root_failure = native_error(&root);
    if root_failure && (v2 || !root.get("Tables").is_some_and(Value::is_array)) {
        return Err(KustoError::new(KustoErrorKind::Service));
    }
    let mut failure = root_failure;
    let mut unknown = false;
    for flag in ["HasErrors", "Cancelled"] {
        if let Some(value) = root.get(flag) {
            failure |= value.as_bool().ok_or_else(invalid)?;
        }
    }
    let raw_tables: Vec<&Value> = if v2 {
        let frames = root.as_array().ok_or_else(invalid)?;
        if frames.len() > limits.max_tables.get().saturating_add(2) {
            return Err(KustoError::new(KustoErrorKind::ResponseLimit));
        }
        let first = frames.first().ok_or_else(invalid)?;
        let last = frames.last().ok_or_else(invalid)?;
        if frames.len() < 2
            || first.get("FrameType").and_then(Value::as_str) != Some("DataSetHeader")
            || first.get("Version").and_then(Value::as_str) != Some("v2.0")
            || first.get("IsProgressive").and_then(Value::as_bool) != Some(false)
            || last.get("FrameType").and_then(Value::as_str) != Some("DataSetCompletion")
        {
            return Err(invalid());
        }
        failure |= native_error(first) || native_error(last);
        match last.get("HasErrors").and_then(Value::as_bool) {
            Some(value) => failure |= value,
            None => unknown = true,
        }
        match last.get("Cancelled").and_then(Value::as_bool) {
            Some(value) => failure |= value,
            None => unknown = true,
        }
        let mut ids = BTreeSet::new();
        let mut tables = Vec::new();
        for frame in &frames[1..frames.len() - 1] {
            if frame.get("FrameType").and_then(Value::as_str) != Some("DataTable") {
                return Err(invalid());
            }
            let id = frame
                .get("TableId")
                .and_then(Value::as_u64)
                .ok_or_else(invalid)?;
            if !ids.insert(id) {
                return Err(invalid());
            }
            failure |= native_error(frame);
            tables.push(frame);
        }
        tables
    } else {
        root.get("Tables")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?
            .iter()
            .collect()
    };
    if raw_tables.is_empty() {
        unknown = true;
    }
    if raw_tables.len() > limits.max_tables.get() {
        return Err(KustoError::new(KustoErrorKind::ResponseLimit));
    }
    let mut tables = Vec::new();
    let mut total_rows = 0usize;
    for raw in raw_tables {
        failure |= native_error(raw);
        let name = raw
            .get("TableName")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let kind = raw.get("TableKind").and_then(Value::as_str);
        if name.len() > MAX_NAME_BYTES || (v2 && kind.is_none()) {
            return Err(invalid());
        }
        let raw_columns = raw
            .get("Columns")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?;
        let raw_rows = raw
            .get("Rows")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?;
        total_rows = total_rows.checked_add(raw_rows.len()).ok_or_else(invalid)?;
        if raw_columns.len() > limits.max_columns.get() || total_rows > limits.max_rows.get() {
            return Err(KustoError::new(KustoErrorKind::ResponseLimit));
        }
        let mut seen = BTreeSet::new();
        let mut columns = Vec::new();
        let mut column_types = Vec::new();
        for column in raw_columns {
            let name = column
                .get("ColumnName")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            if name.len() > MAX_NAME_BYTES || !seen.insert(name) {
                return Err(invalid());
            }
            let column_type = column
                .get("ColumnType")
                .or_else(|| column.get("DataType"))
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            if column_type.is_empty() || column_type.len() > MAX_NAME_BYTES {
                return Err(invalid());
            }
            columns.push(name.to_owned());
            column_types.push(column_type.to_owned());
        }
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for row in raw_rows {
            if let Some(row) = row.as_array() {
                if row.len() != columns.len() {
                    return Err(invalid());
                }
                rows.push(row.clone());
            } else if native_row_error(row) {
                failure = true;
                errors.push(row.clone());
            } else {
                return Err(invalid());
            }
        }
        tables.push(KustoResultTable {
            name: name.into(),
            kind: kind.map(str::to_owned),
            columns,
            column_types,
            rows,
            errors,
        });
    }
    if !v2 {
        resolve_v1_roles(&mut tables)?;
    }
    for table in &tables {
        if table.kind.as_deref() == Some("QueryCompletionInformation") {
            let column = if v2 { "Level" } else { "Severity" };
            if let Some(index) = table.columns.iter().position(|name| name == column) {
                for row in &table.rows {
                    failure |= row[index].as_i64().ok_or_else(invalid)? <= 2;
                }
            } else {
                unknown = true;
            }
        }
    }
    let has_results = tables.iter().any(|table| {
        table
            .kind
            .as_deref()
            .is_none_or(|kind| kind == "PrimaryResult")
            && table.name != "QueryStatus"
            && !table.rows.is_empty()
    });
    let completion = if failure && has_results {
        KustoCompletion::Partial
    } else if failure {
        KustoCompletion::Failed
    } else if unknown {
        KustoCompletion::Unknown
    } else {
        KustoCompletion::Succeeded
    };
    let result = KustoResult { tables, completion };
    if completion != KustoCompletion::Succeeded {
        let mut error = KustoError::new(completion_error(completion));
        error.result = Some(Box::new(result));
        return Err(error);
    }
    Ok(result)
}

fn resolve_v1_roles(tables: &mut [KustoResultTable]) -> Result<(), KustoError> {
    let invalid = || KustoError::new(KustoErrorKind::InvalidResponse);
    let Some(toc) = tables.last() else {
        return Ok(());
    };
    let toc_columns = ["Ordinal", "Kind", "Name", "Id", "PrettyName"];
    let has_toc_columns = toc.columns.len() == toc_columns.len()
        && toc_columns
            .iter()
            .all(|name| toc.columns.iter().any(|column| column.as_str() == *name));
    if !has_toc_columns && tables.len() <= 2 {
        if toc.name == "TableOfContents" || toc.kind.as_deref() == Some("TableOfContents") {
            return Err(invalid());
        }
        for table in tables {
            let kind = match table.name.as_str() {
                "QueryStatus" => Some("QueryCompletionInformation"),
                "@ExtendedProperties" => Some("QueryProperties"),
                _ => None,
            };
            if let Some(kind) = kind {
                if table
                    .kind
                    .as_deref()
                    .is_some_and(|existing| existing != kind)
                {
                    return Err(invalid());
                }
                table.kind = Some(kind.into());
            }
        }
        return Ok(());
    }
    if !has_toc_columns || !toc.errors.is_empty() || toc.rows.len() != tables.len() - 1 {
        return Err(invalid());
    }
    let ordinal = toc
        .columns
        .iter()
        .position(|column| column == "Ordinal")
        .ok_or_else(invalid)?;
    let kind = toc
        .columns
        .iter()
        .position(|column| column == "Kind")
        .ok_or_else(invalid)?;
    let name = toc
        .columns
        .iter()
        .position(|column| column == "Name")
        .ok_or_else(invalid)?;
    let id = toc
        .columns
        .iter()
        .position(|column| column == "Id")
        .ok_or_else(invalid)?;
    let pretty_name = toc
        .columns
        .iter()
        .position(|column| column == "PrettyName")
        .ok_or_else(invalid)?;
    let mut roles = vec![None; tables.len() - 1];
    for row in &toc.rows {
        let index = row[ordinal]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&value| value < roles.len())
            .ok_or_else(invalid)?;
        let role = match row[kind].as_str() {
            Some("QueryResult") => "PrimaryResult",
            Some("QueryProperties") => "QueryProperties",
            Some("QueryStatus") => "QueryCompletionInformation",
            _ => return Err(invalid()),
        };
        if roles[index].replace(role).is_some()
            || !row[name].as_str().is_some_and(valid_name)
            || !row[id].as_str().is_some_and(is_guid)
            || row[pretty_name].as_str().is_none()
        {
            return Err(invalid());
        }
    }
    for (table, role) in tables.iter_mut().zip(roles) {
        let role = role.ok_or_else(invalid)?;
        if table
            .kind
            .as_deref()
            .is_some_and(|existing| existing != role)
        {
            return Err(invalid());
        }
        table.kind = Some(role.into());
    }
    tables.last_mut().ok_or_else(invalid)?.kind = Some("TableOfContents".into());
    Ok(())
}

fn native_row_error(row: &Value) -> bool {
    row.as_object()
        .and_then(|object| object.get("OneApiErrors"))
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            !errors.is_empty()
                && errors.iter().all(|entry| {
                    entry
                        .get("error")
                        .and_then(Value::as_object)
                        .is_some_and(|error| {
                            error
                                .get("code")
                                .and_then(Value::as_str)
                                .is_some_and(|s| !s.is_empty())
                                && error.get("message").and_then(Value::as_str).is_some()
                        })
                })
        })
}

// Value intentionally overwrites duplicate JSON keys. Reject ambiguity before
// decoding completion flags so a later false flag cannot hide an earlier error.
fn reject_duplicate_keys(bytes: &[u8]) -> Result<(), KustoError> {
    enum Container {
        Object(BTreeSet<String>, bool),
        Array,
    }
    let invalid = || KustoError::new(KustoErrorKind::InvalidResponse);
    let mut stack = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => stack.push(Container::Object(BTreeSet::new(), true)),
            b'[' => stack.push(Container::Array),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(Container::Object(_, key_expected)) = stack.last_mut() {
                    *key_expected = true;
                }
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(invalid());
                }
                if let Some(Container::Object(keys, key_expected)) = stack.last_mut() {
                    if *key_expected {
                        let key: String =
                            serde_json::from_slice(&bytes[start..=i]).map_err(|_| invalid())?;
                        if !keys.insert(key) {
                            return Err(invalid());
                        }
                        *key_expected = false;
                    }
                }
            }
            _ => {}
        }
        if stack.len() > 128 {
            return Err(invalid());
        }
        i += 1;
    }
    Ok(())
}

fn completion_error(completion: KustoCompletion) -> KustoErrorKind {
    match completion {
        KustoCompletion::Succeeded | KustoCompletion::Unknown => KustoErrorKind::UnknownOutcome,
        KustoCompletion::Failed => KustoErrorKind::Service,
        KustoCompletion::Partial => KustoErrorKind::Partial,
    }
}

fn ingestion_outcome(result: &KustoResult, storage: bool) -> Result<KustoLoadOutcome, KustoError> {
    let mut extents = Vec::new();
    for table in &result.tables {
        if table
            .kind
            .as_deref()
            .is_some_and(|kind| kind != "PrimaryResult")
        {
            continue;
        }
        let Some(extent) = table.columns.iter().position(|name| name == "ExtentId") else {
            continue;
        };
        let error_index = table.columns.iter().position(|name| name == "HasErrors");
        let operation = table.columns.iter().position(|name| name == "OperationId");
        let item = table.columns.iter().position(|name| name == "ItemLoaded");
        for row in &table.rows {
            let invalid = || {
                let mut error = KustoError::new(KustoErrorKind::InvalidResponse);
                error.mutation_may_have_occurred = true;
                error
            };
            let extent_id = row[extent]
                .as_str()
                .filter(|s| is_guid(s))
                .ok_or_else(invalid)?;
            let has_errors = error_index
                .map(|index| row[index].as_bool().ok_or_else(invalid))
                .transpose()?;
            let operation_id = operation
                .map(|index| {
                    row[index]
                        .as_str()
                        .filter(|s| is_guid(s) && !is_zero_guid(s))
                        .map(str::to_owned)
                        .ok_or_else(invalid)
                })
                .transpose()?;
            let item_loaded = item
                .map(|index| row[index].as_str().map(str::to_owned).ok_or_else(invalid))
                .transpose()?;
            let generated = !is_zero_guid(extent_id);
            let completion = match (generated, has_errors) {
                (true, Some(true)) => KustoCompletion::Partial,
                (false, Some(true)) => KustoCompletion::Failed,
                (true, Some(false)) => KustoCompletion::Succeeded,
                (true, None) if !storage => KustoCompletion::Succeeded,
                _ => KustoCompletion::Unknown,
            };
            extents.push(KustoExtent {
                extent_id: extent_id.into(),
                operation_id,
                has_errors,
                item_loaded,
                completion,
            });
        }
    }
    let completion = if extents.is_empty() {
        KustoCompletion::Unknown
    } else if extents
        .iter()
        .all(|e| e.completion == KustoCompletion::Succeeded)
    {
        KustoCompletion::Succeeded
    } else if extents.iter().any(|e| !is_zero_guid(&e.extent_id)) {
        KustoCompletion::Partial
    } else if extents
        .iter()
        .all(|e| e.completion == KustoCompletion::Failed)
    {
        KustoCompletion::Failed
    } else {
        KustoCompletion::Unknown
    };
    let outcome = KustoLoadOutcome {
        completion,
        extents,
    };
    if completion == KustoCompletion::Succeeded {
        return Ok(outcome);
    }
    let mut error = KustoError::new(completion_error(completion));
    error.mutation_may_have_occurred = true;
    error.load = Some(Box::new(outcome));
    Err(error)
}

async fn bounded<T>(
    operation: impl Future<Output = Result<T, KustoError>>,
    cancelled: &mut KustoFuture<'_, ()>,
    expired: &mut KustoFuture<'_, ()>,
) -> Result<T, KustoError> {
    let timed = async {
        cancel_with(operation, Pin::new(expired))
            .await
            .map_err(|_| KustoError::new(KustoErrorKind::TimedOut))?
    };
    cancel_with(timed, Pin::new(cancelled))
        .await
        .map_err(|_| KustoError::new(KustoErrorKind::Cancelled))?
}

fn finish_ingestion(
    result: Result<KustoResult, KustoError>,
    storage: bool,
) -> Result<KustoLoadOutcome, KustoError> {
    match result {
        Ok(result) => ingestion_outcome(&result, storage).map_err(|mut error| {
            error.result = Some(Box::new(result));
            error
        }),
        Err(mut original) => {
            if let Some(result) = original.result.take() {
                match ingestion_outcome(&result, storage) {
                    Ok(mut outcome) => {
                        outcome.completion = match original.kind {
                            KustoErrorKind::UnknownOutcome => KustoCompletion::Unknown,
                            _ if !outcome.extents.is_empty() => KustoCompletion::Partial,
                            _ => KustoCompletion::Failed,
                        };
                        original.load = Some(Box::new(outcome));
                    }
                    Err(mut evidence) => {
                        original.load = evidence.load.take();
                    }
                }
                original.result = Some(result);
            }
            Err(original)
        }
    }
}
