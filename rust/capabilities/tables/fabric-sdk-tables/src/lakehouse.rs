// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native, parent-owned Lakehouse table operations, separate from v1 compatibility.
//!
//! Load uses existing same-item OneLake `Files` objects, never local uploads.
//! The caller injects [`TableClient`], [`TableExecutor`] and the async runtime;
//! the executor owns shared HTTP and operation reconciliation. Schema-enabled
//! load stays unavailable until its complete beta response profile is verified.
//! Maintenance uses the existing generic jobs SDK and retains its real handle.
//!
//! These are native service-shaped operations, not a built-in HTTP transport.
//! This module supplies no concrete load executor, identity provider or runtime.
//! A consumer must inject an executor composed over the existing shared HTTP/LRO
//! policy and a credential-injected [`JobClient`] for maintenance. Offline
//! executor contracts do not establish live service or production transport support.

use crate::{
    Audience, LoadMode, LoadOutcome, Session, SourceKind, TableClient, TableError, TableErrorKind,
    TableExecutor, TableLimits, TablePath, TableRequest, model,
};
use azure_core::http::Url;
use fabric_sdk_jobs::{JobClient, JobError, JobStatus, JobTarget, JobType, RunHandle, WaitOptions};
use serde_json::{Value, json};
use std::{
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    task::Poll,
};

/// Exact resolved Lakehouse parent, schema and table identity.
///
/// `None` means a legacy, non-schema-enabled Lakehouse, not an implicit `dbo`.
/// Resolution of display names, item type and schema-enabled state is the
/// caller's responsibility. No service lookup or identity acquisition occurs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LakehouseTable {
    workspace: String,
    lakehouse: String,
    schema: Option<String>,
    name: String,
}

impl LakehouseTable {
    /// Validate an exact resolved parent and native table name.
    ///
    /// The native name is 1–256 ASCII alphanumeric/underscore characters and
    /// must contain an alphabetic character or underscore.
    pub fn new(
        workspace: impl Into<String>,
        lakehouse: impl Into<String>,
        schema: Option<String>,
        name: impl Into<String>,
    ) -> Result<Self, LakehouseError> {
        let table = Self {
            workspace: workspace.into(),
            lakehouse: lakehouse.into(),
            schema,
            name: name.into(),
        };
        table.job_target()?;
        if !native_name(&table.name)
            || table
                .schema
                .as_deref()
                .is_some_and(|schema| !model::safe_segment(schema))
        {
            return Err(LakehouseError::input());
        }
        Ok(table)
    }

    /// Exact workspace ID.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Exact parent Lakehouse item ID.
    pub fn lakehouse(&self) -> &str {
        &self.lakehouse
    }

    /// Explicit schema identity, without inference from dots or path depth.
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Exact native table name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Load an existing same-item OneLake file/folder using one native mutation.
    ///
    /// Validation and the schema-beta gate precede credentials and all I/O.
    /// A storage HEAD checks existence and the declared kind; it is not proof
    /// that the remote load identity has source permission, nor a snapshot lock.
    /// The executor receives the remaining request budget and owns shared LRO
    /// reconciliation. Dropping this future does not cancel accepted remote work.
    /// An uncertain result must never be automatically replayed.
    pub async fn load(
        &self,
        client: &TableClient,
        executor: &dyn TableExecutor,
        source: &ExistingOneLakeSource,
        options: &LakehouseLoadOptions,
        limits: LakehouseLoadLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<LoadOutcome, LakehouseError> {
        if self.schema.is_some() {
            return Err(LakehouseError::new(
                LakehouseErrorKind::SchemaLoadUnavailable,
            ));
        }
        if self.workspace != source.path.workspace() || self.lakehouse != source.path.item() {
            return Err(LakehouseError::input());
        }
        let body = fabric_core_json::json_dumps(&options.payload(source)?).into_bytes();
        if limits.table.max_requests.get() < 2 || body.len() > limits.max_request_bytes.get() {
            return Err(LakehouseError::new(LakehouseErrorKind::Limit));
        }
        let mut cancelled = pin!(cancelled);
        let actual = client
            .source_kind(executor, &source.path, limits.table, cancelled.as_mut())
            .await
            .map_err(LakehouseError::table)?;
        if actual != source.kind {
            return Err(LakehouseError::new(
                LakehouseErrorKind::SourceKindMismatch {
                    expected: source.kind,
                    actual,
                },
            ));
        }
        let remaining = TableLimits {
            max_requests: NonZeroUsize::new(limits.table.max_requests.get() - 1)
                .expect("preflight and submission budget validated"),
            ..limits.table
        };
        let mut session = Session::new(client, executor, remaining, cancelled.as_mut());
        session.check().await.map_err(LakehouseError::table)?;
        let scopes = [Audience::Fabric.scope()];
        let token = session
            .run(client.credential.get_token(&scopes, None))
            .await
            .map_err(LakehouseError::table)?
            .map_err(|_| LakehouseError::table(TableError::new(TableErrorKind::Credential)))?;
        session.check().await.map_err(LakehouseError::table)?;
        let mut url = Url::parse("https://api.fabric.microsoft.com/v1/")
            .map_err(|_| LakehouseError::input())?;
        url.path_segments_mut()
            .map_err(|_| LakehouseError::input())?
            .pop_if_empty()
            .extend([
                "workspaces",
                self.workspace(),
                "lakehouses",
                self.lakehouse(),
                "tables",
                self.name(),
                "load",
            ]);
        executor
            .load(
                TableRequest {
                    method: "POST",
                    url,
                    token: &token,
                    credential: client.credential.as_ref(),
                    selection: &client.selection,
                    body,
                    max_response_bytes: remaining.max_response_bytes,
                    max_requests: remaining.max_requests,
                },
                session.cancelled,
            )
            .await
            .map_err(LakehouseError::table)
    }

    /// Read the explicitly named Delta JSON-log compatibility representation.
    ///
    /// This delegates unchanged v1 extraction, not a catalog/schema/statistics
    /// API or a checkpoint-aware Delta reader. No alternative engine is tried.
    /// Names affected by v1 shortcut-suffix rewriting are rejected rather than
    /// reading a different schema.
    pub async fn delta_log_schema_compatibility(
        &self,
        client: &TableClient,
        executor: &dyn TableExecutor,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, LakehouseError> {
        let local = match &self.schema {
            Some(schema) => format!("Tables/{schema}/{}", self.name),
            None => format!("Tables/{}", self.name),
        };
        if local.contains(".Shortcut") || local.contains(".shortcut") {
            return Err(LakehouseError::input());
        }
        let path = TablePath::new(self.workspace(), self.lakehouse(), local)
            .map_err(LakehouseError::table)?;
        client
            .schema(executor, &path, limits, cancelled)
            .await
            .map_err(LakehouseError::table)
    }

    /// Submit one exact-table maintenance job and return acceptance, not completion.
    ///
    /// The existing generic `TableMaintenance` job submission and status routes
    /// own transport, credentials, response bounds and lifecycle. No temporary
    /// job file, workspace setting change or implicit wait/cancel is performed.
    pub async fn submit_maintenance(
        &self,
        jobs: &JobClient,
        options: &LakehouseMaintenanceOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<LakehouseMaintenanceReceipt, LakehouseError> {
        let data = options.execution_data(self)?;
        let target = self.job_target()?;
        let mut cancelled = pin!(cancelled);
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return Err(LakehouseError::table(TableError::new(
                TableErrorKind::Cancelled,
            )));
        }
        let handle = jobs
            .submit(&target, Some(data), cancelled.as_mut())
            .await
            .map_err(|error| LakehouseError::job(error, None, true))?;
        Ok(LakehouseMaintenanceReceipt {
            table: self.clone(),
            handle,
        })
    }

    fn job_target(&self) -> Result<JobTarget, LakehouseError> {
        JobTarget::new(
            self.workspace(),
            self.lakehouse(),
            JobType::TableMaintenance,
        )
        .map_err(|_| LakehouseError::input())
    }
}

fn native_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && name.bytes().any(|c| c.is_ascii_alphabetic() || c == b'_')
}

/// A declared existing remote `Files/...` object, never local file contents.
///
/// Native load verifies the declaration with the existing OneLake properties
/// boundary, then requires this exact workspace/item to equal the target parent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingOneLakeSource {
    path: TablePath,
    kind: SourceKind,
}

impl ExistingOneLakeSource {
    /// Validate a resolved OneLake Files source and its expected file/folder kind.
    ///
    /// Encoded path separators, dot segments, URLs and OS paths are not accepted.
    pub fn new(
        workspace: impl Into<String>,
        lakehouse: impl Into<String>,
        relative_path: impl Into<String>,
        kind: SourceKind,
    ) -> Result<Self, LakehouseError> {
        let path =
            TablePath::new(workspace, lakehouse, relative_path).map_err(LakehouseError::table)?;
        JobTarget::new(path.workspace(), path.item(), JobType::TableMaintenance)
            .map_err(|_| LakehouseError::input())?;
        if !path.local_path().starts_with("Files/")
            || path.local_path().contains(['%', '?', '#', ':'])
        {
            return Err(LakehouseError::input());
        }
        Ok(Self { path, kind })
    }

    /// Exact resolved remote address.
    pub fn path(&self) -> &TablePath {
        &self.path
    }

    /// Declared kind; checked before mutation rather than inferred from extension.
    pub fn kind(&self) -> SourceKind {
        self.kind
    }
}

/// Native format discriminator; CSV-only options cannot be attached to Parquet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LakehouseLoadFormat {
    /// Native CSV format options; absence preserves service defaults.
    Csv {
        /// Explicit first-row header selection.
        header: Option<bool>,
        /// Exact native delimiter string.
        delimiter: Option<String>,
    },
    /// Native Parquet format, with no CSV coercion or conversion fallback.
    Parquet,
}

/// Native options requiring an explicit load mode, without the v1 overwrite default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LakehouseLoadOptions {
    /// Explicit append or overwrite intent.
    pub mode: LoadMode,
    /// Exactly CSV or Parquet.
    pub format: LakehouseLoadFormat,
    /// Native `fileExtension`, preserved without adding/removing dots or changing case.
    pub file_extension: Option<String>,
    /// Recurse into a folder; rejected for file sources.
    pub recursive: bool,
}

impl LakehouseLoadOptions {
    /// Construct explicit native mode and format with recursion off and no filter.
    pub fn new(mode: LoadMode, format: LakehouseLoadFormat) -> Self {
        Self {
            mode,
            format,
            file_extension: None,
            recursive: false,
        }
    }

    /// Produce the service-native body without acquiring credentials or doing I/O.
    pub fn payload(&self, source: &ExistingOneLakeSource) -> Result<Value, LakehouseError> {
        if self.recursive && source.kind == SourceKind::File {
            return Err(LakehouseError::input());
        }
        let format = match &self.format {
            LakehouseLoadFormat::Csv { header, delimiter } => {
                let mut format = json!({"format": "Csv"});
                if let Some(header) = header {
                    format["header"] = (*header).into();
                }
                if let Some(delimiter) = delimiter {
                    if delimiter.is_empty() || delimiter.contains(['\0', '\r', '\n']) {
                        return Err(LakehouseError::input());
                    }
                    format["delimiter"] = delimiter.as_str().into();
                }
                format
            }
            LakehouseLoadFormat::Parquet => json!({"format": "Parquet"}),
        };
        let mut body = json!({
            "relativePath": source.path.local_path(),
            "pathType": match source.kind {
                SourceKind::File => "File",
                SourceKind::Folder => "Folder",
            },
            "mode": match self.mode {
                LoadMode::Append => "Append",
                LoadMode::Overwrite => "Overwrite",
            },
            "formatOptions": format,
        });
        if source.kind == SourceKind::Folder {
            body["recursive"] = self.recursive.into();
        }
        if let Some(extension) = &self.file_extension {
            if !model::safe_segment(extension) || extension.contains(['%', '?', '#', ':']) {
                return Err(LakehouseError::input());
            }
            body["fileExtension"] = extension.as_str().into();
        }
        Ok(body)
    }
}

/// Finite request/response bounds for native Lakehouse load.
#[derive(Clone, Copy, Debug)]
pub struct LakehouseLoadLimits {
    /// Shared properties, submission and operation request/response limits.
    pub table: TableLimits,
    /// SDK-local serialized recipe bound, not a claimed service data-size limit.
    pub max_request_bytes: NonZeroUsize,
}

impl Default for LakehouseLoadLimits {
    fn default() -> Self {
        Self {
            table: TableLimits::default(),
            max_request_bytes: NonZeroUsize::new(1024 * 1024).expect("positive bound"),
        }
    }
}

/// Checked vacuum retention, with no unsafe workspace-setting bypass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VacuumRetention(u64);

impl VacuumRetention {
    /// Require at least 168 hours and a duration representable in whole seconds.
    pub fn from_hours(hours: i64) -> Result<Self, LakehouseError> {
        let hours = u64::try_from(hours).map_err(|_| LakehouseError::input())?;
        if hours < 168 || hours.checked_mul(3600).is_none() {
            return Err(LakehouseError::input());
        }
        Ok(Self(hours))
    }

    /// Exact validated retention hours.
    pub fn hours(self) -> u64 {
        self.0
    }

    /// Native `days:hh:mm:ss` representation, without rounding away hours.
    pub fn retention_period(self) -> String {
        format!("{}:{:02}:00:00", self.0 / 24, self.0 % 24)
    }
}

impl Default for VacuumRetention {
    fn default() -> Self {
        Self(168)
    }
}

/// One native maintenance operation against one exact table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LakehouseMaintenanceOptions {
    /// Compaction; V-Order and Z-Order are explicit opt-ins.
    Optimize {
        /// Omitted from the payload unless true.
        v_order: bool,
        /// Exact native column names; an empty vector omits Z-Order.
        z_order_by: Vec<String>,
    },
    /// Historical-file removal with checked retention.
    Vacuum {
        /// Default/minimum 168 hours.
        retention: VacuumRetention,
    },
}

impl LakehouseMaintenanceOptions {
    /// Compaction without V-Order, Z-Order or any workspace-setting change.
    pub fn optimize() -> Self {
        Self::Optimize {
            v_order: false,
            z_order_by: Vec::new(),
        }
    }

    /// Native default-safe vacuum with seven days' retention.
    pub fn vacuum() -> Self {
        Self::Vacuum {
            retention: VacuumRetention::default(),
        }
    }

    /// Exact generic-job execution data, retaining the resolved schema and table.
    pub fn execution_data(&self, table: &LakehouseTable) -> Result<Value, LakehouseError> {
        let mut data = json!({"tableName": table.name()});
        if let Some(schema) = table.schema() {
            data["schemaName"] = schema.into();
        }
        match self {
            Self::Optimize {
                v_order,
                z_order_by,
            } => {
                if z_order_by
                    .iter()
                    .any(|column| column.is_empty() || column.chars().any(char::is_control))
                {
                    return Err(LakehouseError::input());
                }
                let mut settings = json!({});
                if *v_order {
                    settings["vOrder"] = true.into();
                }
                if !z_order_by.is_empty() {
                    settings["zOrderBy"] = json!(z_order_by);
                }
                data["optimizeSettings"] = settings;
            }
            Self::Vacuum { retention } => {
                data["vacuumSettings"] = json!({"retentionPeriod": retention.retention_period()});
            }
        }
        Ok(data)
    }
}

/// Service acceptance retaining the exact table and existing generic-job handle.
///
/// Timeout, cancellation, failure, deduplication and unknown states do not turn
/// this receipt into successful maintenance or authorize replay.
#[derive(Clone, Debug)]
pub struct LakehouseMaintenanceReceipt {
    table: LakehouseTable,
    handle: RunHandle,
}

impl LakehouseMaintenanceReceipt {
    /// Exact parent/schema/table addressed by the accepted request.
    pub fn table(&self) -> &LakehouseTable {
        &self.table
    }

    /// Real generic-job handle, including the original accepted response.
    pub fn handle(&self) -> &RunHandle {
        &self.handle
    }

    /// Observe this accepted job once through the existing generic status API.
    pub async fn status(
        &self,
        jobs: &JobClient,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, LakehouseError> {
        self.check_endpoint(jobs)?;
        jobs.status(self.handle.target(), self.handle.id(), cancelled)
            .await
            .map_err(|error| LakehouseError::job(error, Some(self), false))
    }

    /// Use the shared job wait/deadline/Retry-After lifecycle, without another loop.
    ///
    /// Only [`fabric_sdk_jobs::JobState::Completed`] proves completion. The native
    /// status row and Failed/Cancelled/Deduped/unknown classifications are unmodified.
    /// A local timeout or cancellation never requests remote cancellation.
    pub async fn wait(
        &self,
        jobs: &JobClient,
        options: WaitOptions,
        observe: impl FnMut(&JobStatus) -> Result<(), JobError> + Send,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, LakehouseError> {
        self.check_endpoint(jobs)?;
        jobs.wait(&self.handle, options, observe, cancelled)
            .await
            .map_err(|error| LakehouseError::job(error, Some(self), false))
    }

    /// Explicitly request remote cancellation once; 202 is not cancelled/completed.
    ///
    /// The returned generic handle retains response evidence and indicates if
    /// the caller's shared operation reconciler is needed. It is not a new job.
    pub async fn request_cancellation(
        &self,
        jobs: &JobClient,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RunHandle, LakehouseError> {
        self.check_endpoint(jobs)?;
        jobs.cancel(self.handle.target(), self.handle.id(), cancelled)
            .await
            .map_err(|error| LakehouseError::job(error, Some(self), true))
    }

    fn check_endpoint(&self, jobs: &JobClient) -> Result<(), LakehouseError> {
        if self.handle.endpoint() != jobs.context().endpoint() {
            return Err(LakehouseError::job(
                JobError::InvalidEndpoint,
                Some(self),
                false,
            ));
        }
        Ok(())
    }
}

/// Native Lakehouse failure classification; raw provider/service data is retained.
#[derive(Debug)]
pub enum LakehouseErrorKind {
    /// Invalid exact identity, source or native options.
    InvalidInput,
    /// Schema-beta load is intentionally gated before any credential/I/O.
    SchemaLoadUnavailable,
    /// The remote source kind differs from the explicit declaration.
    SourceKindMismatch {
        /// Caller-declared source kind.
        expected: SourceKind,
        /// Actual OneLake properties classification.
        actual: SourceKind,
    },
    /// Finite local request or recipe bound was reached before mutation.
    Limit,
    /// Lossless existing properties/load/shared-operation failure.
    Table(Box<TableError>),
    /// Lossless existing generic-job failure.
    Job(Box<JobError>),
}

/// Sanitized error with accepted-job receipt and explicit mutation uncertainty.
#[derive(Debug)]
pub struct LakehouseError {
    /// Structured classification and retained service data.
    pub kind: LakehouseErrorKind,
    /// Actual accepted operation/job ID when observed.
    pub operation_id: Option<String>,
    /// Accepted maintenance receipt survives status/wait/output-observer failure.
    pub maintenance_receipt: Option<Box<LakehouseMaintenanceReceipt>>,
    /// Remote work may exist; this is never permission to replay a write.
    pub mutation_may_have_occurred: bool,
}

impl LakehouseError {
    fn new(kind: LakehouseErrorKind) -> Self {
        Self {
            kind,
            operation_id: None,
            maintenance_receipt: None,
            mutation_may_have_occurred: false,
        }
    }

    fn input() -> Self {
        Self::new(LakehouseErrorKind::InvalidInput)
    }

    fn table(error: TableError) -> Self {
        Self {
            operation_id: error.operation_id.clone(),
            mutation_may_have_occurred: error.mutation_may_have_occurred,
            maintenance_receipt: None,
            kind: LakehouseErrorKind::Table(Box::new(error)),
        }
    }

    fn job(
        error: JobError,
        receipt: Option<&LakehouseMaintenanceReceipt>,
        submitted: bool,
    ) -> Self {
        let pre_dispatch = matches!(
            &error,
            JobError::Credential
                | JobError::InvalidAudience
                | JobError::InvalidEndpoint
                | JobError::InvalidIdentifier
                | JobError::InvalidRequestHeaders
                | JobError::RequestHeadersUnsupported
        );
        let operation_id = match &error {
            JobError::Operation(error) => error.operation_id.clone(),
            _ => None,
        }
        .or_else(|| receipt.map(|receipt| receipt.handle.id().to_owned()));
        let shared_uncertainty = matches!(
            &error,
            JobError::Operation(error) if error.mutation_may_have_occurred
        );
        Self {
            kind: LakehouseErrorKind::Job(Box::new(error)),
            operation_id,
            maintenance_receipt: receipt.cloned().map(Box::new),
            mutation_may_have_occurred: receipt.is_some()
                || shared_uncertainty
                || (submitted && !pre_dispatch),
        }
    }
}

impl fmt::Display for LakehouseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Lakehouse table operation failed or is incomplete; inspect classification and retained receipt",
        )
    }
}

impl std::error::Error for LakehouseError {}
