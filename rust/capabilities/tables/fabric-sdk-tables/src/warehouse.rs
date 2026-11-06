// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-owned Warehouse SQL operations over an injected native executor.
//!
//! This module does not include a TDS driver. The caller supplies its async
//! runtime, clock, cancellation and encrypted, certificate-validated TDS
//! transport (TCP 1433). No HTTP, process, ambient credential or driver fallback
//! exists. Connection metadata must be resolved by the Warehouse owner.
//!
//! Query read-only enforcement is an explicit executor/server contract, not a
//! SQL-prefix filter or `ApplicationIntent=ReadOnly` hint. The resolved table is
//! query context, **not** a single-table authorization sandbox. SQL permissions,
//! RLS/CLS and any stronger isolation remain server responsibilities.
//!
//! COPY supports only native CSV and Parquet from existing GUID-addressed
//! OneLake Lakehouse `Files` paths. JSONL and external-storage credentials are
//! deliberately not advertised. There is no upload, DDL, overwrite, truncate,
//! rejected-row allowance, error-file output, detached execution or replay.
//! Source identity is separate from the caller's SQL login. Callers must not
//! modify source files during execution and must arrange the documented source
//! and target grants; this client never grants permissions.
//!
//! Success requires complete result sets and terminal SQL protocol evidence.
//! Loss after dispatch, local cancellation or deadline expiry is not rollback
//! and must never trigger automatic append retry. An executor must drain the
//! protocol after attention or discard the connection, never return an
//! undrained connection to a pool. Streaming limits must be enforced by the
//! executor before allocation; the client also checks materialized outcomes.

use azure_core::{
    credentials::{AccessToken, TokenCredential},
    http::Url,
};
use std::{
    collections::BTreeSet,
    fmt,
    future::{Future, poll_fn, ready},
    num::{NonZeroU32, NonZeroUsize},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};

/// Public-cloud SQL login scope, never a Fabric REST or storage audience.
pub const SQL_SCOPE: &str = "https://database.windows.net/.default";

/// Future running on the caller's runtime.
pub type SqlFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Dispatch evidence, independent of success and transaction rollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlDispatch {
    /// No SQL statement was sent.
    NotDispatched,
    /// Dispatch may have occurred, but terminal server evidence is missing.
    Unknown,
    /// The server's final protocol completion was observed.
    Terminal,
}

/// Structured SQL failure classification. Diagnostic content is never Display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarehouseErrorKind {
    /// Invalid binding, identifier, source or structured options.
    InvalidInput,
    /// A finite request, result or event budget was exceeded.
    Limit,
    /// The injected credential could not provide a SQL token.
    Credential,
    /// The adapter cannot enforce read-only SQL on the server.
    ReadOnlyUnavailable,
    /// Native transport failed.
    Transport,
    /// One or more SQL server errors or a failed terminal status were received.
    Server,
    /// Missing completion, malformed rows or inconsistent protocol evidence.
    Protocol,
    /// Caller cancellation, not rollback.
    Cancelled,
    /// Caller deadline, not rollback.
    Deadline,
}

/// Failure with lossless bounded protocol evidence when it was received.
///
/// Debug and Display intentionally omit SQL, source locations and server text.
#[derive(Clone, PartialEq)]
pub struct WarehouseError {
    /// Failure classification.
    pub kind: WarehouseErrorKind,
    /// Statement dispatch/completion evidence.
    pub dispatch: SqlDispatch,
    /// Whether this was COPY and it may already have appended data.
    pub mutation_may_have_occurred: bool,
    /// Ordered partial or failed protocol data; explicit diagnostic access only.
    pub outcome: Option<Box<SqlOutcome>>,
}

impl WarehouseError {
    /// Construct a failure before a statement has been dispatched.
    pub fn before_dispatch(kind: WarehouseErrorKind) -> Self {
        Self {
            kind,
            dispatch: SqlDispatch::NotDispatched,
            mutation_may_have_occurred: false,
            outcome: None,
        }
    }

    fn after_dispatch(kind: WarehouseErrorKind, mutation: bool) -> Self {
        Self {
            kind,
            dispatch: SqlDispatch::Unknown,
            mutation_may_have_occurred: mutation,
            outcome: None,
        }
    }
}

impl fmt::Debug for WarehouseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarehouseError")
            .field("kind", &self.kind)
            .field("dispatch", &self.dispatch)
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .field("outcome", &self.outcome.as_ref().map(|_| "<SQL data>"))
            .finish()
    }
}

impl fmt::Display for WarehouseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Warehouse SQL operation failed or is incomplete; inspect structured evidence")
    }
}

impl std::error::Error for WarehouseError {}

fn invalid() -> WarehouseError {
    WarehouseError::before_dispatch(WarehouseErrorKind::InvalidInput)
}

/// One exact SQL identifier, without interpreting dots or existing brackets.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SqlIdentifier(String);

impl SqlIdentifier {
    /// Validate an exact SQL Server identifier (1–128 UTF-16 code units).
    pub fn new(value: impl Into<String>) -> Result<Self, WarehouseError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.encode_utf16().count() > 128
            || value.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(Self(value))
    }

    /// Original identifier, not parsed as SQL syntax.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Bracket-quote an exact identifier; embedded closing brackets are doubled.
    pub fn quoted(&self) -> String {
        format!("[{}]", self.0.replace(']', "]]"))
    }
}

/// An exact schema/table pair; no implicit `dbo` or name splitting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarehouseTable {
    /// Exact resolved schema.
    pub schema: SqlIdentifier,
    /// Exact resolved table.
    pub table: SqlIdentifier,
}

impl WarehouseTable {
    /// Construct from two separately resolved names.
    pub fn new(schema: &str, table: &str) -> Result<Self, WarehouseError> {
        Ok(Self {
            schema: SqlIdentifier::new(schema)?,
            table: SqlIdentifier::new(table)?,
        })
    }

    /// Safely quoted two-part SQL name, including literal dots within a name.
    pub fn quoted(&self) -> String {
        format!("{}.{}", self.schema.quoted(), self.table.quoted())
    }
}

/// Validated public-cloud Warehouse TDS endpoint and exact initial catalog.
///
/// The metadata resolver, not this client, establishes that this is the owned
/// Warehouse endpoint/catalog. Display names must not override initial catalog.
#[derive(Clone, Eq, PartialEq)]
pub struct WarehouseConnection {
    server: String,
    catalog: String,
}

impl WarehouseConnection {
    /// Accept a bare Fabric Warehouse DNS host, never a connection string/URL.
    ///
    /// Only the public-cloud Warehouse suffix is supported. Alternate clouds
    /// and arbitrary SQL hosts need a separately reviewed endpoint policy.
    pub fn new(server: &str, initial_catalog: &str) -> Result<Self, WarehouseError> {
        let server = server.to_ascii_lowercase();
        let suffix = ".datawarehouse.fabric.microsoft.com";
        if !server.ends_with(suffix)
            || server.len() > 253
            || server.len() <= suffix.len()
            || server.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
            || initial_catalog.trim().is_empty()
            || initial_catalog.encode_utf16().count() > 128
            || initial_catalog.chars().any(char::is_control)
            || initial_catalog.contains(';')
        {
            return Err(invalid());
        }
        Ok(Self {
            server,
            catalog: initial_catalog.to_owned(),
        })
    }

    /// DNS name to connect to with hostname/certificate validation enabled.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Fixed native encrypted TDS port.
    pub fn port(&self) -> u16 {
        1433
    }

    /// Exact catalog, passed as a driver field, not concatenated into a string.
    pub fn initial_catalog(&self) -> &str {
        &self.catalog
    }
}

impl fmt::Debug for WarehouseConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarehouseConnection")
            .field("server", &"<validated Warehouse host>")
            .field("catalog", &"<exact initial catalog>")
            .finish()
    }
}

/// Resolved source item kind; GUIDs alone do not prove an item's type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeSourceItemKind {
    /// An actual Lakehouse, resolved by the caller's metadata owner.
    Lakehouse,
    /// Not supported as a COPY source.
    Warehouse,
    /// Other item types are not supported as COPY sources.
    Other,
}

/// Existing OneLake Lakehouse Files source, never an arbitrary URL or secret.
#[derive(Clone, Eq, PartialEq)]
pub struct LakehouseFilesSource {
    url: Url,
}

impl LakehouseFilesSource {
    /// Validate GUID identity and an exact `Files/...` relative path.
    ///
    /// The caller must supply metadata-resolved kind, not infer Lakehouse from
    /// GUID syntax. Only individual paths (not globs, SAS, keys or queries) are
    /// accepted. A folder path may select multiple native files of the explicitly
    /// chosen format; this client does not inspect or convert their contents.
    pub fn new(
        workspace_id: &str,
        item_id: &str,
        kind: OneLakeSourceItemKind,
        relative_path: &str,
    ) -> Result<Self, WarehouseError> {
        if kind != OneLakeSourceItemKind::Lakehouse
            || !is_guid(workspace_id)
            || !is_guid(item_id)
            || !relative_path.starts_with("Files/")
            || relative_path.len() > 4096
            || relative_path.split('/').any(|segment| {
                segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || segment
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#' | '%' | '*' | ';'))
            })
        {
            return Err(invalid());
        }
        let mut url =
            Url::parse("https://onelake.dfs.fabric.microsoft.com/").map_err(|_| invalid())?;
        url.path_segments_mut()
            .map_err(|_| invalid())?
            .clear()
            .extend([workspace_id, item_id])
            .extend(relative_path.split('/'));
        Ok(Self { url })
    }

    /// Validated HTTPS OneLake URL without credentials or query parameters.
    pub fn url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Debug for LakehouseFilesSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LakehouseFilesSource(<validated GUID Lakehouse Files path>)")
    }
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Explicit source authorization, independent of the SQL caller credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopySourceAuthorization {
    /// Native caller-Entra passthrough. No CREDENTIAL clause is emitted.
    CallerIdentity,
    /// Source access with the target workspace identity, not SQL impersonation.
    WorkspaceIdentity,
}

impl std::str::FromStr for CopySourceAuthorization {
    type Err = WarehouseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "caller-identity" => Ok(Self::CallerIdentity),
            "workspace-identity" => Ok(Self::WorkspaceIdentity),
            _ => Err(invalid()),
        }
    }
}

/// Advertised native COPY subset. JSONL is intentionally not supported yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyFileType {
    /// Native CSV parsing, without host-side conversion.
    Csv,
    /// Native Parquet parsing, without host-side conversion.
    Parquet,
}

impl std::str::FromStr for CopyFileType {
    type Err = WarehouseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "csv" | "CSV" => Ok(Self::Csv),
            "parquet" | "PARQUET" => Ok(Self::Parquet),
            _ => Err(invalid()),
        }
    }
}

/// Supported native CSV encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CsvEncoding {
    /// Native UTF8.
    Utf8,
    /// Native UTF16.
    Utf16,
}

/// Structured optional native CSV options; absence uses the service default.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CsvOptions {
    /// One-based first data row; never zero.
    pub first_row: Option<NonZeroU32>,
    /// Native field terminator literal, not a SQL expression.
    pub field_terminator: Option<String>,
    /// Native row terminator literal, not a SQL expression.
    pub row_terminator: Option<String>,
    /// Explicit source encoding.
    pub encoding: Option<CsvEncoding>,
}

/// Existing-table append only. The type has no DDL/destructive/raw-option fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyIntoOptions {
    /// Required, explicit format.
    pub file_type: CopyFileType,
    /// Required, explicit source identity selection.
    pub source_authorization: CopySourceAuthorization,
    /// Exact ordered target columns, or empty to use native target order.
    pub columns: Vec<SqlIdentifier>,
    /// CSV-only settings. Any presence with Parquet is rejected.
    pub csv: Option<CsvOptions>,
}

impl CopyIntoOptions {
    /// Create an explicitly formatted/authorized native append request.
    pub fn new(file_type: CopyFileType, source_authorization: CopySourceAuthorization) -> Self {
        Self {
            file_type,
            source_authorization,
            columns: Vec::new(),
            csv: None,
        }
    }
}

/// Finite budgets applying to credential acquisition and one SQL statement.
#[derive(Clone, Copy, Debug)]
pub struct SqlLimits {
    /// Total SQL batch dispatches allowed. Each operation dispatches exactly one.
    pub max_requests: NonZeroUsize,
    /// Maximum UTF-8 bytes in the constructed SQL batch.
    pub max_statement_bytes: NonZeroUsize,
    /// Maximum total decoded result bytes, including messages and metadata.
    pub max_result_bytes: NonZeroUsize,
    /// Maximum ordered protocol events, including messages and DONE tokens.
    pub max_events: NonZeroUsize,
    /// Maximum result sets over the entire statement.
    pub max_result_sets: NonZeroUsize,
    /// Maximum total rows across all result sets.
    pub max_rows: NonZeroUsize,
    /// Maximum columns in each result set.
    pub max_columns: NonZeroUsize,
    /// Total duration, including credentials; zero/overflow is invalid.
    pub max_duration: Duration,
}

impl Default for SqlLimits {
    fn default() -> Self {
        Self {
            max_requests: NonZeroUsize::new(1).unwrap(),
            max_statement_bytes: NonZeroUsize::new(256 * 1024).unwrap(),
            max_result_bytes: NonZeroUsize::new(8 * 1024 * 1024).unwrap(),
            max_events: NonZeroUsize::new(4096).unwrap(),
            max_result_sets: NonZeroUsize::new(16).unwrap(),
            max_rows: NonZeroUsize::new(100_000).unwrap(),
            max_columns: NonZeroUsize::new(1024).unwrap(),
            max_duration: Duration::from_secs(120),
        }
    }
}

/// Caller-owned cancellation and monotonic deadline scheduling.
///
/// All returned cancellation futures must observe the same persistent signal.
/// `sleep_until` must wake at the given deadline even if SQL makes no progress.
/// The SDK never creates a timer, runtime or background worker.
pub trait SqlControl: Send + Sync {
    /// Current monotonic time from the caller's clock.
    fn now(&self) -> Instant;
    /// Wait for persistent caller cancellation.
    fn cancelled(&self) -> SqlFuture<'_, ()>;
    /// Wait until the absolute operation deadline.
    fn sleep_until(&self, deadline: Instant) -> SqlFuture<'_, ()>;
}

/// Trusted adapter capability, not a SQL string classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlReadOnlySupport {
    /// This adapter cannot guarantee server-enforced read-only execution.
    Unsupported,
    /// The adapter ensures server-side permissions/session policy reject writes.
    ///
    /// This must apply to the entire submitted batch, procedures and dynamic
    /// SQL, not just lexical SELECT/CTE detection or an application-intent hint.
    ServerEnforced,
}

/// Typed intent available to the native executor alongside the exact SQL text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarehouseOperation {
    /// Exact table metadata, not DDL export.
    Metadata(WarehouseTable),
    /// Exact table columns ordered by native ordinal.
    Schema(WarehouseTable),
    /// Exact table's statistics and ordered statistic-column membership.
    Statistics(WarehouseTable),
    /// Exact table/statistic DBCC header, density vector and histogram.
    ShowStatistics {
        /// Exact table.
        table: WarehouseTable,
        /// Exact table-scoped statistic name.
        statistic: SqlIdentifier,
    },
    /// Read-only batch; table is context, not a table-isolation guarantee.
    Query(WarehouseTable),
    /// One native existing-table append.
    CopyInto {
        /// Exact destination table.
        table: WarehouseTable,
        /// Validated Lakehouse Files source.
        source: LakehouseFilesSource,
        /// Explicit native structured options.
        options: CopyIntoOptions,
    },
}

impl WarehouseOperation {
    fn is_mutation(&self) -> bool {
        matches!(self, Self::CopyInto { .. })
    }
}

/// Validated typed SQL request. Debug never emits SQL, source or token.
pub struct SqlRequest<'a> {
    connection: &'a WarehouseConnection,
    token: &'a AccessToken,
    operation: WarehouseOperation,
    statement: String,
    limits: SqlLimits,
    deadline: Instant,
}

impl SqlRequest<'_> {
    /// Validated endpoint/catalog; require encrypted TDS and certificate checks.
    pub fn connection(&self) -> &WarehouseConnection {
        self.connection
    }
    /// Injected SQL token. Never log or persist it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Typed operation and exact identity/source options.
    pub fn operation(&self) -> &WarehouseOperation {
        &self.operation
    }
    /// Exact SQL batch; diagnostic/driver access only, never automatic logging.
    pub fn statement(&self) -> &str {
        &self.statement
    }
    /// Streaming/request limits that must be enforced before materialization.
    pub fn limits(&self) -> SqlLimits {
        self.limits
    }
    /// Absolute deadline including credential acquisition.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    /// Whether server-enforced read-only execution is mandatory.
    pub fn read_only_required(&self) -> bool {
        !self.operation.is_mutation()
    }
}

impl fmt::Debug for SqlRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlRequest")
            .field("connection", &self.connection)
            .field("read_only_required", &self.read_only_required())
            .field("statement", &"<SQL data>")
            .field("token", &"<injected>")
            .field("limits", &self.limits)
            .finish()
    }
}

/// Native transport failure with explicit dispatch evidence and partial data.
///
/// Adapters must use Unknown after any possible dispatch; Terminal is valid
/// only with a fully drained terminal outcome. No failure may be replayed.
#[derive(Clone)]
pub struct SqlExecutionError {
    /// Failure classification (usually Transport, Limit, Deadline or Cancelled).
    pub kind: WarehouseErrorKind,
    /// Proven dispatch state.
    pub dispatch: SqlDispatch,
    /// Bounded ordered evidence received before failure.
    pub outcome: Option<SqlOutcome>,
}

impl fmt::Debug for SqlExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlExecutionError")
            .field("kind", &self.kind)
            .field("dispatch", &self.dispatch)
            .field("outcome", &self.outcome.as_ref().map(|_| "<SQL data>"))
            .finish()
    }
}

/// Caller-provided native encrypted TDS execution; no driver is bundled here.
///
/// Execute exactly one batch, never retry after possible dispatch, preserve all
/// ordered sets/messages/errors/DONE counts, and consume the final SQL protocol
/// completion. Enforce limits while streaming and honor control/deadline. Native
/// cancellation/attention and connection cleanup belong to this adapter.
pub trait SqlExecutor: Send + Sync {
    /// Explicitly advertise a real server-enforced read-only execution policy.
    fn read_only_support(&self) -> SqlReadOnlySupport {
        SqlReadOnlySupport::Unsupported
    }

    /// Execute on the caller's runtime using the supplied SQL login token.
    fn execute<'a>(
        &'a self,
        request: SqlRequest<'a>,
        control: &'a dyn SqlControl,
    ) -> SqlFuture<'a, Result<SqlOutcome, SqlExecutionError>>;
}

/// Exact SQL scalar data without JSON-number coercion or named-row maps.
///
/// Native temporal/decimal/other driver values can use `Native` with a stable
/// driver-documented encoding. They must never be replaced with null, debug
/// text or rounded floating-point values. The column retains SQL type metadata.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    /// SQL NULL, distinct from empty text/binary or an unsupported value.
    Null,
    /// Native bit.
    Bit(bool),
    /// Exact signed SQL integer (including bigint).
    Integer(i64),
    /// Exact decimal/numeric or money textual representation, without rounding.
    Decimal(String),
    /// Exact IEEE-754 real bits, including non-finite values.
    Float32Bits(u32),
    /// Exact IEEE-754 float bits, including non-finite values.
    Float64Bits(u64),
    /// Text without parsing dates, XML, JSON or GUIDs.
    Text(String),
    /// Exact binary bytes.
    Binary(Vec<u8>),
    /// Exact native value with a documented, lossless encoding.
    Native {
        /// Encoding/type tag, e.g. `tds:datetime:days-i32-ticks-u32-le`.
        encoding: String,
        /// Exact encoded native scalar bytes.
        bytes: Vec<u8>,
    },
}

/// One ordered SQL column; duplicate names are intentionally permitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlColumn {
    /// Native name, possibly empty or duplicated.
    pub name: String,
    /// Native SQL type description, including precision/scale where relevant.
    pub native_type: String,
    /// Unknown nullability remains None.
    pub nullable: Option<bool>,
}

/// One ordered result set with row/column ordinals preserved.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlResultSet {
    /// Ordered native columns.
    pub columns: Vec<SqlColumn>,
    /// Ordered rows; each row must match column count.
    pub rows: Vec<Vec<SqlValue>>,
    /// Native completion for this set, not just end of a received buffer.
    pub complete: bool,
}

/// Native structured SQL server diagnostic, retained only as explicit data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlDiagnostic {
    /// Native error/information number.
    pub number: i32,
    /// Native error state.
    pub state: u8,
    /// Native error class/severity.
    pub class: u8,
    /// Native diagnostic text; may contain sensitive SQL/source data.
    pub message: String,
    /// Procedure name when supplied.
    pub procedure: Option<String>,
    /// Native line number when supplied.
    pub line: Option<u32>,
}

/// Protocol events in exact arrival order, never flattened by column name.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlEvent {
    /// One complete or partial native result set.
    ResultSet(SqlResultSet),
    /// Native DONE-family statement completion count. Absence is not zero.
    Done {
        /// Native affected row count when the COUNT-valid bit was present.
        rows_affected: Option<u64>,
    },
    /// Native informational message, not a server error.
    Information(SqlDiagnostic),
    /// Native error; an HTTP-style success status must not hide this.
    Error(SqlDiagnostic),
}

/// Final protocol completion evidence for the entire batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlTerminal {
    /// Final DONE observed without error/cancel flags.
    Completed,
    /// Final completion with a server error.
    Failed,
    /// Server acknowledged cancellation; no rollback claim is made.
    Cancelled,
    /// Final completion was not observed, including truncated/lost streams.
    Incomplete,
}

/// Bounded, ordered SQL outcome. Empty result sets are not synthesized summaries.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlOutcome {
    /// Every native result set, message, error and completion count, in order.
    pub events: Vec<SqlEvent>,
    /// Final SQL protocol evidence, required even for statements with no rows.
    pub terminal: SqlTerminal,
    /// Total decoded protocol bytes, including metadata/messages, tracked while
    /// streaming by the executor. The SDK also counts materialized scalar bytes.
    pub decoded_bytes: usize,
}

/// Reusable async Warehouse client with an explicit caller-owned SQL credential.
pub struct WarehouseClient {
    credential: Arc<dyn TokenCredential>,
    connection: WarehouseConnection,
}

impl fmt::Debug for WarehouseClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarehouseClient")
            .field("connection", &self.connection)
            .field("credential", &"<injected>")
            .finish()
    }
}

impl WarehouseClient {
    /// Bind an explicit credential to metadata-resolved Warehouse connection.
    ///
    /// The provider owns account/tenant selection. No ambient lookup occurs;
    /// awaiting a service method requests only `SQL_SCOPE`.
    pub fn new(credential: Arc<dyn TokenCredential>, connection: WarehouseConnection) -> Self {
        Self {
            credential,
            connection,
        }
    }

    /// Exact table metadata visible to the SQL caller, not full DDL.
    pub async fn metadata(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        let sql = format!(
            "SELECT TABLE_CATALOG, TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE \
             FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = {} \
             AND TABLE_NAME = {};",
            unicode_literal(table.schema.as_str()),
            unicode_literal(table.table.as_str())
        );
        self.run(
            executor,
            WarehouseOperation::Metadata(table.clone()),
            sql,
            limits,
            control,
        )
        .await
    }

    /// Exact table's columns in native ordinal order with SQL type information.
    pub async fn schema(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        let sql = format!(
            "SELECT TABLE_CATALOG, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
             ORDINAL_POSITION, DATA_TYPE, IS_NULLABLE, CHARACTER_MAXIMUM_LENGTH, \
             NUMERIC_PRECISION, NUMERIC_SCALE, DATETIME_PRECISION, COLUMN_DEFAULT \
             FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = {} \
             AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION;",
            unicode_literal(table.schema.as_str()),
            unicode_literal(table.table.as_str())
        );
        self.run(
            executor,
            WarehouseOperation::Schema(table.clone()),
            sql,
            limits,
            control,
        )
        .await
    }

    /// List only this exact table's statistics and ordered column membership.
    ///
    /// Native freshness, missing dates and metadata visibility are preserved;
    /// no exact logical row count or permission inference is made.
    pub async fn statistics(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        let sql = format!(
            "SELECT s.stats_id, s.name AS statistic_name, \
             STATS_DATE(s.object_id, s.stats_id) AS last_updated, \
             sc.stats_column_id, c.name AS column_name \
             FROM sys.stats AS s \
             JOIN sys.tables AS t ON t.object_id = s.object_id \
             JOIN sys.schemas AS sch ON sch.schema_id = t.schema_id \
             JOIN sys.stats_columns AS sc ON sc.object_id = s.object_id \
             AND sc.stats_id = s.stats_id \
             JOIN sys.columns AS c ON c.object_id = sc.object_id \
             AND c.column_id = sc.column_id \
             WHERE sch.name = {} AND t.name = {} \
             ORDER BY s.stats_id, sc.stats_column_id;",
            unicode_literal(table.schema.as_str()),
            unicode_literal(table.table.as_str())
        );
        self.run(
            executor,
            WarehouseOperation::Statistics(table.clone()),
            sql,
            limits,
            control,
        )
        .await
    }

    /// DBCC for the exact table/statistic, preserving all three ordered sets.
    ///
    /// This never resolves a statistic by its unscoped name or creates/updates
    /// statistics. Header, density vector and histogram are not flattened.
    pub async fn show_statistics(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        statistic: &SqlIdentifier,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        let sql = format!(
            "DBCC SHOW_STATISTICS ({}, {});",
            unicode_literal(&table.quoted()),
            unicode_literal(statistic.as_str())
        );
        self.run(
            executor,
            WarehouseOperation::ShowStatistics {
                table: table.clone(),
                statistic: statistic.clone(),
            },
            sql,
            limits,
            control,
        )
        .await
    }

    /// Execute caller SQL only with the explicit server-enforced read contract.
    ///
    /// No lexical prefix heuristic, textual table substitution, single-table
    /// isolation promise, backend fallback or arbitrary write API is supplied.
    pub async fn query(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        sql: &str,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        if sql.trim().is_empty() || sql.contains('\0') {
            return Err(invalid());
        }
        if sql.len() > limits.max_statement_bytes.get() {
            return Err(WarehouseError::before_dispatch(WarehouseErrorKind::Limit));
        }
        self.run(
            executor,
            WarehouseOperation::Query(table.clone()),
            sql.to_owned(),
            limits,
            control,
        )
        .await
    }

    /// Append from existing Lakehouse Files with explicit native format/auth.
    ///
    /// CSV explicitly uses `MAXERRORS = 0`; Parquet forbids that native option.
    /// No ERRORFILE is permitted. A completed batch is not evidence that every
    /// source object had any rows; native sets/counts are returned without
    /// inventing a loaded-row summary.
    pub async fn copy_into(
        &self,
        executor: &dyn SqlExecutor,
        table: &WarehouseTable,
        source: &LakehouseFilesSource,
        options: &CopyIntoOptions,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        let sql = copy_statement(table, source, options)?;
        self.run(
            executor,
            WarehouseOperation::CopyInto {
                table: table.clone(),
                source: source.clone(),
                options: options.clone(),
            },
            sql,
            limits,
            control,
        )
        .await
    }

    async fn run(
        &self,
        executor: &dyn SqlExecutor,
        operation: WarehouseOperation,
        statement: String,
        limits: SqlLimits,
        control: &dyn SqlControl,
    ) -> Result<SqlOutcome, WarehouseError> {
        if statement.len() > limits.max_statement_bytes.get() {
            return Err(WarehouseError::before_dispatch(WarehouseErrorKind::Limit));
        }
        if limits.max_duration.is_zero() {
            return Err(invalid());
        }
        let deadline = control
            .now()
            .checked_add(limits.max_duration)
            .ok_or_else(invalid)?;
        let mutation = operation.is_mutation();
        if !mutation && executor.read_only_support() != SqlReadOnlySupport::ServerEnforced {
            return Err(WarehouseError::before_dispatch(
                WarehouseErrorKind::ReadOnlyUnavailable,
            ));
        }
        let token = interrupt(
            self.credential.get_token(&[SQL_SCOPE], None),
            control,
            deadline,
        )
        .await
        .map_err(WarehouseError::before_dispatch)?
        .map_err(|_| WarehouseError::before_dispatch(WarehouseErrorKind::Credential))?;
        interrupt(ready(()), control, deadline)
            .await
            .map_err(WarehouseError::before_dispatch)?;
        let request = SqlRequest {
            connection: &self.connection,
            token: &token,
            operation,
            statement,
            limits,
            deadline,
        };
        let outcome = interrupt(executor.execute(request, control), control, deadline)
            .await
            .map_err(|kind| WarehouseError::after_dispatch(kind, mutation))?;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let mut dispatch = error.dispatch;
                let mut kind = error.kind;
                if let Some(outcome) = &error.outcome {
                    if let Err(failure) = validate_contents(outcome, limits) {
                        kind = failure;
                    }
                    // A declared pre-dispatch failure cannot contain server data.
                    if dispatch == SqlDispatch::NotDispatched
                        || (dispatch == SqlDispatch::Terminal && !has_terminal_evidence(outcome))
                    {
                        dispatch = SqlDispatch::Unknown;
                    }
                } else if dispatch == SqlDispatch::Terminal {
                    dispatch = SqlDispatch::Unknown;
                }
                return Err(WarehouseError {
                    kind,
                    dispatch,
                    mutation_may_have_occurred: mutation && dispatch != SqlDispatch::NotDispatched,
                    outcome: error.outcome.map(Box::new),
                });
            }
        };
        if let Err(kind) = validate_outcome(&outcome, limits) {
            return Err(WarehouseError {
                kind,
                dispatch: if has_terminal_evidence(&outcome) {
                    SqlDispatch::Terminal
                } else {
                    SqlDispatch::Unknown
                },
                mutation_may_have_occurred: mutation,
                outcome: Some(Box::new(outcome)),
            });
        }
        Ok(outcome)
    }
}

async fn interrupt<F: Future>(
    work: F,
    control: &dyn SqlControl,
    deadline: Instant,
) -> Result<F::Output, WarehouseErrorKind> {
    let mut work = pin!(work);
    let mut cancelled = control.cancelled();
    let mut elapsed = control.sleep_until(deadline);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(WarehouseErrorKind::Cancelled));
        }
        if control.now() >= deadline || elapsed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(WarehouseErrorKind::Deadline));
        }
        work.as_mut().poll(cx).map(Ok)
    })
    .await
}

fn unicode_literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn copy_statement(
    table: &WarehouseTable,
    source: &LakehouseFilesSource,
    options: &CopyIntoOptions,
) -> Result<String, WarehouseError> {
    if options.columns.len() > 1024
        || options.columns.iter().collect::<BTreeSet<_>>().len() != options.columns.len()
        || (options.file_type == CopyFileType::Parquet && options.csv.is_some())
    {
        return Err(invalid());
    }
    let columns = if options.columns.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            options
                .columns
                .iter()
                .map(SqlIdentifier::quoted)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let format = match options.file_type {
        CopyFileType::Csv => "CSV",
        CopyFileType::Parquet => "PARQUET",
    };
    let mut native = vec![format!("FILE_TYPE = '{format}'")];
    if options.file_type == CopyFileType::Csv {
        native.push("MAXERRORS = 0".to_owned());
    }
    if options.source_authorization == CopySourceAuthorization::WorkspaceIdentity {
        native.push("CREDENTIAL = (IDENTITY = 'Workspace Identity')".to_owned());
    }
    if let Some(csv) = &options.csv {
        if let Some(first_row) = csv.first_row {
            native.push(format!("FIRSTROW = {first_row}"));
        }
        for (name, value) in [
            ("FIELDTERMINATOR", &csv.field_terminator),
            ("ROWTERMINATOR", &csv.row_terminator),
        ] {
            if let Some(value) = value {
                if value.is_empty() || value.len() > 16 || value.contains('\0') {
                    return Err(invalid());
                }
                native.push(format!("{name} = {}", literal(value)));
            }
        }
        if let Some(encoding) = csv.encoding {
            native.push(format!(
                "ENCODING = '{}'",
                match encoding {
                    CsvEncoding::Utf8 => "UTF8",
                    CsvEncoding::Utf16 => "UTF16",
                }
            ));
        }
    }
    Ok(format!(
        "COPY INTO {}{columns}\nFROM {}\nWITH ({});",
        table.quoted(),
        literal(source.url.as_str()),
        native.join(", ")
    ))
}

fn has_terminal_evidence(outcome: &SqlOutcome) -> bool {
    outcome.terminal != SqlTerminal::Incomplete
        && outcome
            .events
            .iter()
            .all(|event| !matches!(event, SqlEvent::ResultSet(set) if !set.complete))
}

fn validate_outcome(outcome: &SqlOutcome, limits: SqlLimits) -> Result<(), WarehouseErrorKind> {
    let has_error = validate_contents(outcome, limits)?;
    if !has_terminal_evidence(outcome) {
        return Err(WarehouseErrorKind::Protocol);
    }
    if has_error || outcome.terminal == SqlTerminal::Failed {
        return Err(WarehouseErrorKind::Server);
    }
    if outcome.terminal == SqlTerminal::Cancelled {
        return Err(WarehouseErrorKind::Cancelled);
    }
    Ok(())
}

fn validate_contents(outcome: &SqlOutcome, limits: SqlLimits) -> Result<bool, WarehouseErrorKind> {
    if outcome.decoded_bytes > limits.max_result_bytes.get()
        || outcome.events.len() > limits.max_events.get()
    {
        return Err(WarehouseErrorKind::Limit);
    }
    let mut bytes = 0_usize;
    let mut sets = 0_usize;
    let mut rows = 0_usize;
    let mut has_error = false;
    for event in &outcome.events {
        match event {
            SqlEvent::ResultSet(set) => {
                add_bounded(&mut sets, 1, limits.max_result_sets.get())?;
                add_bounded(&mut rows, set.rows.len(), limits.max_rows.get())?;
                if set.columns.len() > limits.max_columns.get() {
                    return Err(WarehouseErrorKind::Limit);
                }
                for column in &set.columns {
                    add_bounded(&mut bytes, column.name.len(), limits.max_result_bytes.get())?;
                    add_bounded(
                        &mut bytes,
                        column.native_type.len(),
                        limits.max_result_bytes.get(),
                    )?;
                }
                for row in &set.rows {
                    if row.len() != set.columns.len() {
                        return Err(WarehouseErrorKind::Protocol);
                    }
                    for scalar in row {
                        let size = match scalar {
                            SqlValue::Null | SqlValue::Bit(_) => 1,
                            SqlValue::Integer(_) | SqlValue::Float64Bits(_) => 8,
                            SqlValue::Float32Bits(_) => 4,
                            SqlValue::Decimal(value) | SqlValue::Text(value) => value.len(),
                            SqlValue::Binary(value) => value.len(),
                            SqlValue::Native {
                                encoding,
                                bytes: native_bytes,
                            } => {
                                add_bounded(
                                    &mut bytes,
                                    encoding.len(),
                                    limits.max_result_bytes.get(),
                                )?;
                                native_bytes.len()
                            }
                        };
                        add_bounded(&mut bytes, size, limits.max_result_bytes.get())?;
                    }
                }
            }
            SqlEvent::Information(diagnostic) | SqlEvent::Error(diagnostic) => {
                has_error |= matches!(event, SqlEvent::Error(_));
                add_bounded(
                    &mut bytes,
                    diagnostic.message.len(),
                    limits.max_result_bytes.get(),
                )?;
                add_bounded(
                    &mut bytes,
                    diagnostic.procedure.as_ref().map_or(0, String::len),
                    limits.max_result_bytes.get(),
                )?;
            }
            SqlEvent::Done { .. } => {
                add_bounded(&mut bytes, 8, limits.max_result_bytes.get())?;
            }
        }
    }
    Ok(has_error)
}

fn add_bounded(total: &mut usize, count: usize, maximum: usize) -> Result<(), WarehouseErrorKind> {
    *total = total
        .checked_add(count)
        .filter(|value| *value <= maximum)
        .ok_or(WarehouseErrorKind::Limit)?;
    Ok(())
}
