// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source GET orchestration with explicitly borrowed UI, files and operation policy.
//!
//! This additive entry deliberately does not change the legacy metadata-only host.

#[path = "get_output.rs"]
pub mod output;

#[cfg(feature = "native-host")]
#[path = "get_resources.rs"]
pub mod resources;

use super::{ReadSession, get_options, snapshot::Snapshot};
use crate::{
    PythonQuery, ReadHost, errors,
    parser::ReadCommand,
    reads::{required_id, unsupported_get},
};
use fabric_cli_context::{Element, ItemType, NodeKind, PathInput, ResolveOptions, Resolver};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use fabric_cli_transfer::{TransferEvent, TransferFiles, TransferUi};
use fabric_deploy::definitions::{
    self, DefinitionError, DefinitionErrorKind, DefinitionLimits, DefinitionTarget,
    GetDefinitionPreprocess, GetDefinitionValueError, GetOutputNewline,
};
use fabric_sdk::{
    Client,
    items::{ItemCollection, ItemError, ItemGetOptions},
    workspaces::{
        WorkspaceEnrichmentOutcome, WorkspaceListLimits, WorkspaceMetadata, WorkspaceMetadataError,
        WorkspaceMetadataErrorKind,
    },
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use futures_util::{FutureExt, future::FusedFuture};
use serde_json::{Value, json};
use std::{
    fmt,
    future::{Future, poll_fn},
    io::{self, Write},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::Duration,
};

/// Exact source prompt; only the caller's UI may perform interaction.
pub const DEFINITION_CONFIRMATION: &str =
    "Item definition is retrieved without its sensitivity label. Are you sure?";

/// Caller-owned capacity settings; no configuration or account discovery occurs.
#[derive(Clone, Debug, Default)]
pub struct GetResourceConfig {
    /// Python `default_az_subscription_id`, if explicitly configured.
    pub subscription_id: Option<String>,
    /// Python `default_az_resource_group`, if explicitly configured.
    pub resource_group: Option<String>,
}

/// Every additional capability needed by source item GET and projected output.
///
/// The caller supplies its existing executor, console `TransferUi`, deferred
/// local/OneLake `TransferFiles`, local timestamp and native newline policy.
/// No hidden environment, file roots, clock, prompt or runtime is consulted.
pub struct GetHost<'a> {
    /// Existing shared HTTP/time policy; not another transport or operation loop.
    pub operations: &'a dyn OperationExecutor,
    /// Existing console or scripted UI, including immediate progress failures.
    pub ui: &'a dyn TransferUi,
    /// Selected filesystem/storage policy; needed only for a nonempty `--output`.
    pub files: Option<&'a mut dyn TransferFiles>,
    /// Caller-owned local wall-clock timestamp for output naming.
    pub local_timestamp: output::LocalTimestamp,
    /// Explicit local text-mode newline policy.
    pub newline: GetOutputNewline,
    /// Finite definition, projected-output and portable-path bounds.
    pub definition_limits: DefinitionLimits,
    /// Existing request/poll/response bounds for each finite source operation.
    pub operation_limits: OperationLimits,
}

/// GET capabilities with a deferred caller-owned local output clock.
///
/// Unlike the fixed-timestamp entry, construction does not sample a clock.
/// The clock is used only after projection and classified output resolution.
pub struct ClockedGetHost<'a> {
    /// Existing shared request and polling executor.
    pub operations: &'a dyn OperationExecutor,
    /// Explicit confirmation and immediate progress policy.
    pub ui: &'a dyn TransferUi,
    /// Explicit file authority, needed only for nonempty output.
    pub files: Option<&'a mut dyn TransferFiles>,
    /// Caller-owned asynchronous local output clock.
    pub clock: &'a dyn output::GetOutputClock,
    /// Explicit local text newline policy.
    pub newline: GetOutputNewline,
    /// Existing finite definition and output limits.
    pub definition_limits: DefinitionLimits,
    /// Existing finite shared operation limits.
    pub operation_limits: OperationLimits,
}

pub(super) enum OutputClock<'a> {
    Fixed(&'a output::LocalTimestamp),
    Deferred(&'a dyn output::GetOutputClock),
}

pub(super) struct RuntimeGetHost<'a> {
    pub(super) operations: &'a dyn OperationExecutor,
    pub(super) ui: &'a dyn TransferUi,
    pub(super) files: Option<&'a mut dyn TransferFiles>,
    pub(super) clock: OutputClock<'a>,
    pub(super) newline: GetOutputNewline,
    pub(super) definition_limits: DefinitionLimits,
    pub(super) operation_limits: OperationLimits,
}

impl GetHost<'_> {
    fn borrowed(&mut self) -> RuntimeGetHost<'_> {
        RuntimeGetHost {
            operations: self.operations,
            ui: self.ui,
            files: self
                .files
                .as_mut()
                .map(|files| &mut **files as &mut dyn TransferFiles),
            clock: OutputClock::Fixed(&self.local_timestamp),
            newline: self.newline,
            definition_limits: self.definition_limits,
            operation_limits: self.operation_limits,
        }
    }
}

impl ClockedGetHost<'_> {
    fn borrowed(&mut self) -> RuntimeGetHost<'_> {
        RuntimeGetHost {
            operations: self.operations,
            ui: self.ui,
            files: self
                .files
                .as_mut()
                .map(|files| &mut **files as &mut dyn TransferFiles),
            clock: OutputClock::Deferred(self.clock),
            newline: self.newline,
            definition_limits: self.definition_limits,
            operation_limits: self.operation_limits,
        }
    }
}

/// Evidence for one shared-operation request, including successful reconciliation.
#[derive(Clone, Debug)]
pub struct GetOperationReceipt {
    /// Source acquisition/enrichment phase, never definition content.
    pub phase: &'static str,
    /// Accepted operation identifier, if observed.
    pub operation_id: Option<String>,
    /// A POST may have reached the service; cancellation never implies rollback.
    pub mutation_may_have_occurred: bool,
}

/// A source best-effort enrichment failure, retained rather than silently lost.
#[derive(Debug)]
pub struct GetEnrichmentFailure {
    /// Source connection/schedule/type-specific enrichment phase.
    pub phase: &'static str,
    /// Original typed error or parse failure.
    pub kind: GetErrorKind,
}

/// Command-local evidence, also retained when output or rendering fails.
#[derive(Debug, Default)]
pub struct GetEvidence {
    /// Shared operation receipts in source order.
    pub operations: Vec<GetOperationReceipt>,
    /// Source-swallowed enrichment errors, in request order.
    pub enrichment_failures: Vec<GetEnrichmentFailure>,
    /// Explicit source metadata-only fallback on definition 401/403.
    pub definition_access_denied: Option<u16>,
    /// File effects, including possibly partial writes.
    pub files: output::GetFileReceipt,
    /// A negative confirmation is a quiet no-op, not cancellation or EOF.
    pub declined: bool,
    /// Existing workspace enrichment observations, without a second pager.
    pub workspace_enrichments: Vec<WorkspaceEnrichmentOutcome>,
}

/// Typed failures; no success-shaped fallback for unavailable codecs or domains.
#[derive(Debug)]
pub enum GetErrorKind {
    /// Context, query, UI, format or host-capability failure.
    Command(CommandError),
    /// Original specialized SDK metadata failure.
    Item(ItemError),
    /// Existing primary/optional workspace SDK failure.
    Workspace(WorkspaceMetadataError),
    /// Original hidden-resource or ARM API failure.
    Api(Box<fabric_sdk_api::ApiError>),
    /// Original collection failure, including complete pages before failure.
    Collection(Box<fabric_sdk_api::CollectionError>),
    /// Original OneLake metadata failure.
    OneLake(fabric_sdk_onelake::OneLakeError),
    /// Original shortcut resource failure, including acceptance and service data.
    Resource(Box<fabric_sdk_workspaces::mutations::ResourceError>),
    /// Shared definition/file failure with accepted-operation or partial-file data.
    Definition(Box<DefinitionError>),
    /// Original decoded-value error and logical definition location.
    Codec(Box<GetDefinitionValueError>),
    /// Shared request/acceptance/polling failure.
    Operation(Box<OperationError>),
    /// Rendering failed after acquisition or file writes.
    Render {
        /// Original rendering error, possibly carrying partial text.
        error: Box<text::TextError>,
        /// Earlier command failure, never replaced by a failed error renderer.
        prior: Option<Box<GetErrorKind>>,
    },
}

impl From<CommandError> for GetErrorKind {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

impl From<DefinitionError> for GetErrorKind {
    fn from(error: DefinitionError) -> Self {
        Self::Definition(Box::new(error))
    }
}

impl From<GetDefinitionValueError> for GetErrorKind {
    fn from(error: GetDefinitionValueError) -> Self {
        Self::Codec(Box::new(error))
    }
}

impl From<OperationError> for GetErrorKind {
    fn from(error: OperationError) -> Self {
        Self::Operation(Box::new(error))
    }
}

impl From<fabric_sdk_api::ApiError> for GetErrorKind {
    fn from(error: fabric_sdk_api::ApiError) -> Self {
        match error {
            fabric_sdk_api::ApiError::Operation(error) => Self::Operation(error),
            error => Self::Api(Box::new(error)),
        }
    }
}

impl From<fabric_sdk_api::CollectionError> for GetErrorKind {
    fn from(error: fabric_sdk_api::CollectionError) -> Self {
        Self::Collection(Box::new(error))
    }
}

impl From<fabric_sdk_onelake::OneLakeError> for GetErrorKind {
    fn from(error: fabric_sdk_onelake::OneLakeError) -> Self {
        Self::OneLake(error)
    }
}

/// Failed GET retaining source and durable-effect evidence.
#[derive(Debug)]
pub struct GetFailure {
    /// Original typed cause.
    pub kind: GetErrorKind,
    /// Everything completed or possibly dispatched before this failure.
    pub evidence: GetEvidence,
}

/// GET result with a bounded-size error carrying complete owned receipts.
pub type GetResult = Result<GetOutcome, Box<GetFailure>>;

impl fmt::Display for GetFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GET failed or is incomplete; inspect acquisition and file receipts")
    }
}

impl std::error::Error for GetFailure {}

/// Completed source GET, including quiet decline and falsey projections.
pub struct GetOutcome {
    /// Already-projected data, never a definition export plan or output envelope.
    pub projected: Option<Value>,
    /// Source stdout input, absent for quiet/declined reads.
    pub output: Option<OutputInput>,
    /// Acquisition and file effects.
    pub evidence: GetEvidence,
}

impl fmt::Debug for GetOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GetOutcome")
            .field(
                "projected",
                &self.projected.as_ref().map(|_| "<GET result>"),
            )
            .field("evidence", &self.evidence)
            .finish_non_exhaustive()
    }
}

/// Rendered command plus original outcome; accepted is never reported as completed.
#[derive(Debug)]
pub struct GetDispatch {
    /// Existing JSON/text presentation plan.
    pub plan: TextPlan,
    /// Typed command outcome with receipts, even on rendering failure.
    pub result: GetResult,
}

/// Delivery failed, possibly after partial stdout/stderr, with no replay.
#[derive(Debug)]
pub struct GetWriteFailure {
    /// Original writer error.
    pub error: io::Error,
    /// Original rendered result and all remote/local receipts.
    pub dispatch: GetDispatch,
}

impl GetDispatch {
    /// Write once to caller-owned streams without flush, retry or rollback.
    ///
    /// # Errors
    /// Returns the complete dispatch if either writer fails.
    pub fn write_to(
        self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<Self, Box<GetWriteFailure>> {
        match self.plan.write_to(stdout, stderr) {
            Ok(()) => Ok(self),
            Err(error) => Err(Box::new(GetWriteFailure {
                error,
                dispatch: self,
            })),
        }
    }
}

pub(super) struct BorrowedOperations<'a>(pub(super) &'a dyn OperationExecutor);

impl OperationExecutor for BorrowedOperations<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.execute(request)
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.0.wait(delay)
    }
}

impl ReadSession<'_> {
    /// Resolve and execute source definition GET on the caller's existing runtime.
    ///
    /// Legacy `dispatch` remains metadata-only. Native callers opt into this
    /// complete item-definition/projection/file contract explicitly.
    pub async fn execute_get(
        &self,
        command: &ReadCommand,
        host: &mut GetHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetResult {
        let mut evidence = GetEvidence::default();
        self.execute_get_runtime(command, &mut host.borrowed(), &mut evidence, cancelled)
            .await
    }

    async fn execute_get_runtime(
        &self,
        command: &ReadCommand,
        host: &mut RuntimeGetHost<'_>,
        evidence: &mut GetEvidence,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetResult {
        let mut cancelled = pin!(cancelled.fuse());
        let result = async {
            get_options::source_preflight(command)?;
            if self.tenant.id() != Some(self.sdk.selection().tenant())
                || self.tenant.kind() != NodeKind::Tenant
                || self.sdk.selection().audience() != fabric_sdk::Audience::Fabric
                || !self.current_path.starts_with('/')
            {
                return Err(errors::error("InvalidInput", "Invalid selected read context").into());
            }
            let mut snapshot = Snapshot::default();
            let path = command.path.join(" ");
            let path = path.trim_matches(['\'', '"']);
            let current = if path.starts_with('/') || path == "~" || path.starts_with("~/") {
                Arc::clone(&self.tenant)
            } else {
                snapshot
                    .resolve(self, &self.tenant, self.current_path, cancelled.as_mut())
                    .await?
            };
            let target = snapshot
                .resolve(self, &current, path, cancelled.as_mut())
                .await?;
            let reads = ReadHost {
                sdk: self.sdk,
                workspaces: self.executor,
                items: self.executor,
                capacities: self.executor,
                lookup: &snapshot,
                current,
                home: self.home.clone(),
                timestamp: self.timestamp.clone(),
                config: self.config,
                projection: &PythonQuery,
            };
            reads
                .source_get_resolved(command, &target, host, cancelled.as_mut(), evidence)
                .await
        }
        .await;
        finish(result, std::mem::take(evidence))
    }

    /// Execute and render while preserving receipts if rendering fails.
    pub async fn dispatch_get(
        &self,
        command: &ReadCommand,
        host: &mut GetHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetDispatch {
        render(
            self.execute_get(command, host, cancelled).await,
            command,
            self.timestamp.clone(),
        )
    }

    /// Execute clocked GET while retaining live effects if its future is dropped.
    ///
    /// Completed dispatch takes the evidence into its owned result. An abandoned
    /// future leaves observed operations and started file effects in `evidence`.
    /// No clock or file capability activates for absent/empty output.
    pub async fn dispatch_get_clocked(
        &self,
        command: &ReadCommand,
        host: &mut ClockedGetHost<'_>,
        evidence: &mut GetEvidence,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetDispatch {
        render(
            self.execute_get_runtime(command, &mut host.borrowed(), evidence, cancelled)
                .await,
            command,
            self.timestamp.clone(),
        )
    }

    /// Compose clocked GET with the existing classified local/Lakehouse adapter.
    ///
    /// Borrows the caller's deferred local capability; remote classification
    /// never activates that local candidate. Uses the same session and executor.
    #[cfg(feature = "native-host")]
    pub async fn dispatch_get_with_storage(
        &self,
        command: &ReadCommand,
        http: &fabric_sdk_http::HttpExecutor,
        host: &mut ClockedGetHost<'_>,
        evidence: &mut GetEvidence,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetDispatch {
        self.dispatch_get_with_resource_storage(
            command,
            http,
            host,
            evidence,
            &resources::GetResourceConfig::default(),
            cancelled,
        )
        .await
    }

    /// Compose all source GET resources, explicit capacity settings and storage.
    ///
    /// Uses the same local/Lakehouse classifier as the original storage entry.
    #[cfg(feature = "native-host")]
    pub async fn dispatch_get_with_resource_storage(
        &self,
        command: &ReadCommand,
        http: &fabric_sdk_http::HttpExecutor,
        host: &mut ClockedGetHost<'_>,
        evidence: &mut GetEvidence,
        config: &resources::GetResourceConfig,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetDispatch {
        if host.files.is_none() {
            return self
                .dispatch_get_with_resources(command, http, host, evidence, config, cancelled)
                .await;
        }
        let local = host.files.as_mut().expect("checked file capability");
        let mut files = crate::families::export_files::ExportFiles::new(&mut **local, self, http);
        let mut selected = ClockedGetHost {
            operations: host.operations,
            ui: host.ui,
            files: Some(&mut files),
            clock: host.clock,
            newline: host.newline,
            definition_limits: host.definition_limits,
            operation_limits: host.operation_limits,
        };
        self.dispatch_get_with_resources(command, http, &mut selected, evidence, config, cancelled)
            .await
    }
}

impl ReadHost<'_> {
    /// Execute source GET against the caller's already-loaded immutable snapshot.
    ///
    /// Uses the same engine as `ReadSession::execute_get`; lookup never performs
    /// synchronous I/O and definitions never replace specialized metadata.
    pub async fn execute_get(
        &self,
        command: &ReadCommand,
        host: &mut GetHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetResult {
        let mut evidence = GetEvidence::default();
        let mut host = host.borrowed();
        let mut cancelled = pin!(cancelled.fuse());
        let result = async {
            get_options::source_preflight(command)?;
            let resolver = Resolver::new(self.lookup, self.current.clone(), self.home.clone());
            let tokens: Vec<_> = command.path.iter().map(String::as_str).collect();
            let target = resolver
                .resolve(PathInput::Tokens(&tokens), ResolveOptions::default())
                .map_err(|error| errors::error(&error.code, error.message))?;
            self.source_get_resolved(
                command,
                &target,
                &mut host,
                cancelled.as_mut(),
                &mut evidence,
            )
            .await
        }
        .await;
        finish(result, evidence)
    }

    async fn source_get_resolved(
        &self,
        command: &ReadCommand,
        target: &Arc<Element>,
        host: &mut RuntimeGetHost<'_>,
        mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
        evidence: &mut GetEvidence,
    ) -> Result<(Option<Value>, Option<OutputInput>), GetErrorKind> {
        checkpoint(cancelled.as_mut()).await?;
        let data = match target.kind() {
            NodeKind::Item(kind) => {
                if unsupported_get(kind) {
                    return Err(errors::error(
                        "UnsupportedCommand",
                        "not supported for command 'get'",
                    )
                    .into());
                }
                let obtain_definition = !command
                    .query
                    .as_deref()
                    .is_some_and(get_options::is_metadata_property_query)
                    && supports_definition(kind);
                if obtain_definition {
                    if command.force {
                        host.ui.emit(TransferEvent::Warning(
                            "Item definition is retrieved without its sensitivity label and its data"
                                .into(),
                        ))?;
                    } else {
                        let confirmed =
                            ui_phase(host.ui.confirm(DEFINITION_CONFIRMATION), cancelled.as_mut())
                                .await?;
                        if !confirmed {
                            evidence.declined = true;
                            return Ok((None, None));
                        }
                    }
                }
                checkpoint(cancelled.as_mut()).await?;
                let workspace = target.workspace().expect("validated item parentage");
                let workspace_id = required_id(&workspace)?;
                let item_id = required_id(target)?;
                let collection =
                    ItemCollection::from_item_type(kind.as_str()).ok_or_else(|| {
                        errors::error("UnsupportedCommand", "not supported for command 'get'")
                    })?;
                let metadata = self
                    .sdk
                    .get_item_metadata_from_collection(
                        self.items,
                        workspace_id,
                        collection,
                        item_id,
                        ItemGetOptions::default(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(GetErrorKind::Item)?
                    .into_value();
                let definition = if obtain_definition {
                    let mut observed = TrackedOperation {
                        session: host.operation_session(self.sdk, cancelled.as_mut()),
                        evidence: &mut *evidence,
                        phase: "definition",
                        post: true,
                    };
                    let result = definitions::read_definition(
                        &mut observed.session,
                        DefinitionTarget {
                            workspace_id,
                            item_id,
                            item_type: kind.as_str(),
                        },
                        None,
                    )
                    .await;
                    drop(observed);
                    match result {
                        Ok(value) => Some(value),
                        Err(error)
                            if matches!(
                                &error.kind,
                                DefinitionErrorKind::Operation(operation)
                                    if matches!(operation.kind,
                                        OperationErrorKind::Http { status: 401 | 403, .. })
                            ) =>
                        {
                            if let DefinitionErrorKind::Operation(operation) = &error.kind
                                && let OperationErrorKind::Http { status, .. } = operation.kind
                            {
                                evidence.definition_access_denied = Some(status);
                            }
                            evidence.enrichment_failures.push(GetEnrichmentFailure {
                                phase: "definition access",
                                kind: GetErrorKind::Definition(Box::new(error)),
                            });
                            None
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    None
                };
                let decoded = definitions::decode_get_definition_value(
                    &metadata,
                    definition.as_ref(),
                    kind.as_str(),
                    if obtain_definition {
                        GetDefinitionPreprocess::Decode
                    } else {
                        GetDefinitionPreprocess::Preserve
                    },
                    host.definition_limits,
                )?;
                // The existing JMESPath provider has a finite serde JSON domain.
                // Never round, stringify binary, replace NaN, or drop surrogates.
                let mut data = decoded.try_to_json()?;
                enrich(
                    self.sdk,
                    host,
                    workspace_id,
                    item_id,
                    kind,
                    &mut data,
                    evidence,
                    cancelled.as_mut(),
                )
                .await?;
                data
            }
            NodeKind::Workspace(_) => {
                let id = required_id(target)?;
                let response = operation(
                    self.sdk,
                    host,
                    "workspace",
                    ResourceMethod::Get,
                    &format!("/v1/workspaces/{id}"),
                    evidence,
                    cancelled.as_mut(),
                )
                .await?;
                if response.status != 200 {
                    return Ok((None, None));
                }
                let metadata: WorkspaceMetadata =
                    serde_json::from_slice(&response.body).map_err(|_| {
                        errors::error("InvalidResponse", "Invalid workspace GET metadata")
                    })?;
                let result = self
                    .sdk
                    .enrich_workspace_metadata(
                        self.workspaces,
                        id,
                        metadata,
                        WorkspaceListLimits::default(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(GetErrorKind::Workspace)?;
                evidence.workspace_enrichments = result.enrichments;
                result.metadata.into_value()
            }
            NodeKind::Folder => {
                let workspace = target.workspace().ok_or_else(|| {
                    errors::error("InvalidInput", "Folder workspace is unavailable")
                })?;
                let path = format!(
                    "/v1/workspaces/{}/folders/{}",
                    required_id(&workspace)?,
                    required_id(target)?
                );
                let response = operation(
                    self.sdk,
                    host,
                    "folder",
                    ResourceMethod::Get,
                    &path,
                    evidence,
                    cancelled.as_mut(),
                )
                .await?;
                if response.status != 200 {
                    return Ok((None, None));
                }
                parse_object(&response)?
            }
            _ => {
                return Err(
                    errors::error("UnsupportedCommand", "not supported for command 'get'").into(),
                );
            }
        };
        project_and_output(
            self.sdk,
            self.projection,
            command,
            &target.name(),
            data,
            host,
            evidence,
            cancelled,
        )
        .await
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared GET projection and effect boundary."
)]
async fn project_and_output(
    sdk: &Client,
    projection: &dyn crate::Projection,
    command: &ReadCommand,
    output_name: &str,
    data: Value,
    host: &mut RuntimeGetHost<'_>,
    evidence: &mut GetEvidence,
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<(Option<Value>, Option<OutputInput>), GetErrorKind> {
    checkpoint(cancelled.as_mut()).await?;
    let projected = projection.project(
        data,
        command.query.as_deref().filter(|query| !query.is_empty()),
        false,
    )?;
    checkpoint(cancelled.as_mut()).await?;
    let output = if let Some(destination) = command.output.as_deref().filter(|s| !s.is_empty()) {
        output::write_projected(
            sdk,
            host,
            destination,
            output_name,
            &projected,
            evidence,
            cancelled.as_mut(),
        )
        .await?;
        Some(OutputInput {
            command: Some(command.name.clone()),
            message: Some("Export completed".into()),
            ..OutputInput::default()
        })
    } else {
        truthy(&projected).then(|| OutputInput {
            command: Some(command.name.clone()),
            data: Some(projected.clone()),
            ..OutputInput::default()
        })
    };
    Ok((Some(projected), output))
}

impl RuntimeGetHost<'_> {
    pub(super) fn operation_session<'a>(
        &'a self,
        sdk: &'a Client,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> OperationSession<'a, BorrowedOperations<'a>> {
        OperationSession::with_context(
            sdk.context(),
            BorrowedOperations(self.operations),
            self.operation_limits,
            cancelled,
        )
    }
}

fn finish(
    result: Result<(Option<Value>, Option<OutputInput>), GetErrorKind>,
    evidence: GetEvidence,
) -> GetResult {
    match result {
        Ok((projected, output)) => Ok(GetOutcome {
            projected,
            output,
            evidence,
        }),
        Err(kind) => Err(Box::new(GetFailure { kind, evidence })),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

/// Source `fs.export.supported_items`; ML types deliberately remain metadata-only.
fn supports_definition(kind: ItemType) -> bool {
    matches!(
        kind.as_str(),
        "Report"
            | "SparkJobDefinition"
            | "SemanticModel"
            | "Notebook"
            | "KQLDashboard"
            | "DataPipeline"
            | "KQLQueryset"
            | "Eventhouse"
            | "KQLDatabase"
            | "MirroredDatabase"
            | "Reflex"
            | "Eventstream"
            | "MountedDataFactory"
            | "CopyJob"
            | "VariableLibrary"
            | "GraphQLApi"
            | "Dataflow"
            | "SQLDatabase"
            | "CosmosDBDatabase"
            | "DigitalTwinBuilder"
            | "DigitalTwinBuilderFlow"
            | "UserDataFunction"
            | "GraphQuerySet"
            | "Map"
            | "Lakehouse"
            | "Environment"
            | "GraphModel"
            | "Ontology"
    )
}

pub(super) async fn checkpoint(
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<(), GetErrorKind> {
    if cancelled.is_terminated()
        || poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await
    {
        Err(errors::error("LongRunningOperationCancelled", "Operation cancelled").into())
    } else {
        Ok(())
    }
}

async fn ui_phase<T>(
    work: impl Future<Output = Result<T, CommandError>>,
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<T, GetErrorKind> {
    let mut work = pin!(work);
    poll_fn(|cx| {
        if cancelled.is_terminated() || cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(errors::error(
                "LongRunningOperationCancelled",
                "Operation cancelled",
            )
            .into()));
        }
        work.as_mut()
            .poll(cx)
            .map(|result| result.map_err(Into::into))
    })
    .await
}

fn receipt<E: OperationExecutor>(
    evidence: &mut GetEvidence,
    phase: &'static str,
    session: &OperationSession<'_, E>,
    post: bool,
) {
    evidence.operations.push(GetOperationReceipt {
        phase,
        operation_id: session.operation_id().map(str::to_owned),
        mutation_may_have_occurred: post
            && session
                .evidence(OperationErrorKind::InvalidResponse)
                .mutation_may_have_occurred,
    });
}

struct TrackedOperation<'evidence, 'session, E: OperationExecutor> {
    session: OperationSession<'session, E>,
    evidence: &'evidence mut GetEvidence,
    phase: &'static str,
    post: bool,
}

impl<E: OperationExecutor> Drop for TrackedOperation<'_, '_, E> {
    fn drop(&mut self) {
        receipt(self.evidence, self.phase, &self.session, self.post);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit source operation boundaries."
)]
async fn operation(
    sdk: &Client,
    host: &RuntimeGetHost<'_>,
    phase: &'static str,
    method: ResourceMethod,
    path: &str,
    evidence: &mut GetEvidence,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<OperationResponse, GetErrorKind> {
    let session = host.operation_session(sdk, cancelled);
    let operation = FabricOperation::new(
        method,
        path,
        Vec::new(),
        CompletionPolicy::PythonSynchronous,
    )
    .map_err(|kind| session.evidence(kind))?;
    let operation = if method == ResourceMethod::Get {
        operation
            .with_source_read_operation_ids()
            .map_err(|kind| session.evidence(kind))?
    } else {
        operation
    };
    let mut observed = TrackedOperation {
        session,
        evidence,
        phase,
        post: method != ResourceMethod::Get,
    };
    let result = observed.session.execute(operation).await;
    let response = result?;
    if response.status == 202 {
        return Err(observed
            .session
            .evidence(OperationErrorKind::InvalidResponse)
            .into());
    }
    Ok(response)
}

fn parse_object(response: &OperationResponse) -> Result<Value, GetErrorKind> {
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|_| errors::error("InvalidResponse", "GET enrichment returned invalid JSON"))?;
    if !value.is_object() {
        return Err(errors::error("InvalidResponse", "GET enrichment returned no object").into());
    }
    Ok(value)
}

fn terminal_failure(kind: &GetErrorKind) -> bool {
    matches!(
        kind,
        GetErrorKind::Operation(error) if matches!(&error.kind,
            OperationErrorKind::Cancelled
                | OperationErrorKind::Limit
                | OperationErrorKind::Execution(
                    OperationExecutionError::Cancelled
                        | OperationExecutionError::ResponseLimit
                        | OperationExecutionError::TimedOut
                )
        )
    ) || matches!(
        kind,
        GetErrorKind::Operation(error)
            if matches!(&error.kind, OperationErrorKind::OperationFailed { status, .. }
                if status == "Cancelled")
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit source enrichment boundaries."
)]
async fn enrich(
    sdk: &Client,
    host: &RuntimeGetHost<'_>,
    workspace: &str,
    item: &str,
    kind: ItemType,
    data: &mut Value,
    evidence: &mut GetEvidence,
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<(), GetErrorKind> {
    let base = format!("/v1/workspaces/{workspace}");
    let mut steps = vec![(
        "connections",
        ResourceMethod::Get,
        format!("{base}/items/{item}/connections"),
        "connections",
        Some("value"),
        false,
    )];
    let job = match kind.as_str() {
        "SparkJobDefinition" => Some("sparkjob"),
        "Notebook" => Some("RunNotebook"),
        "DataPipeline" => Some("Pipeline"),
        "Lakehouse" => Some("TableMaintenance"),
        "GraphModel" => Some("refreshGraph"),
        _ => None,
    };
    if let Some(job) = job {
        steps.push((
            "schedules",
            ResourceMethod::Get,
            format!("{base}/items/{item}/jobs/{job}/schedules"),
            "schedules",
            Some("value"),
            true,
        ));
    }
    if kind.as_str() == "Environment" {
        for (phase, suffix, destination) in [
            ("published libraries", "libraries", "published.libraries"),
            (
                "staging libraries",
                "staging/libraries",
                "staging.libraries",
            ),
            (
                "published compute",
                "sparkcompute",
                "published.sparkComputeSettings",
            ),
            (
                "staging compute",
                "staging/sparkcompute",
                "staging.sparkComputeSettings",
            ),
        ] {
            steps.push((
                phase,
                ResourceMethod::Get,
                format!("{base}/environments/{item}/{suffix}"),
                destination,
                None,
                true,
            ));
        }
    }
    if kind.as_str() == "MirroredDatabase" {
        steps.extend([
            (
                "mirroring status",
                ResourceMethod::Post,
                format!("{base}/mirroredDatabases/{item}/getMirroringStatus"),
                "status",
                None,
                true,
            ),
            (
                "mirroring tables",
                ResourceMethod::Post,
                format!("{base}/mirroredDatabases/{item}/getTablesMirroringStatus"),
                "tablesStatus",
                Some("data"),
                true,
            ),
        ]);
    }
    for (phase, method, path, destination, member, only_200) in steps {
        checkpoint(cancelled.as_mut()).await?;
        let result = async {
            let response = operation(
                sdk,
                host,
                phase,
                method,
                &path,
                evidence,
                cancelled.as_mut(),
            )
            .await?;
            if only_200 && response.status != 200 {
                return Ok(None);
            }
            let value = parse_object(&response)?;
            Ok(Some(if let Some(member) = member {
                value.get(member).cloned().ok_or_else(|| {
                    errors::error("InvalidResponse", "GET enrichment member is missing")
                })?
            } else {
                value
            }))
        }
        .await;
        match result {
            Ok(Some(value)) => {
                // A malformed source published/staging container makes that
                // type-specific helper fail; do not replace it with an object.
                if !assign(data, destination, value) {
                    evidence.enrichment_failures.push(GetEnrichmentFailure {
                        phase,
                        kind: errors::error(
                            "InvalidResponse",
                            "GET enrichment container is invalid",
                        )
                        .into(),
                    });
                    break;
                }
            }
            Ok(None) => {}
            Err(kind) if terminal_failure(&kind) => return Err(kind),
            Err(kind) => {
                evidence
                    .enrichment_failures
                    .push(GetEnrichmentFailure { phase, kind });
                if (matches!(destination, "status" | "tablesStatus")
                    || destination.starts_with("published.")
                    || destination.starts_with("staging."))
                    && !assign(data, destination, json!([]))
                {
                    break;
                }
            }
        }
    }
    checkpoint(cancelled).await
}

fn assign(data: &mut Value, destination: &str, value: Value) -> bool {
    let Some(object) = data.as_object_mut() else {
        return false;
    };
    if let Some((parent, key)) = destination.split_once('.') {
        let Some(parent) = object
            .entry(parent)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        else {
            return false;
        };
        parent.insert(key.into(), value);
    } else {
        object.insert(destination.into(), value);
    }
    true
}

fn error_output(kind: &GetErrorKind) -> OutputInput {
    match kind {
        GetErrorKind::Operation(error) => fabric_cli_mutations::operation_error_output(error),
        GetErrorKind::Definition(error)
            if matches!(&error.kind, DefinitionErrorKind::Operation(_)) =>
        {
            let DefinitionErrorKind::Operation(operation) = &error.kind else {
                unreachable!()
            };
            fabric_cli_mutations::operation_error_output(operation)
        }
        GetErrorKind::Command(error) => command_output(error),
        GetErrorKind::Item(error) => command_output(&errors::item(&error.kind)),
        GetErrorKind::Workspace(error) => command_output(&match &error.kind {
            WorkspaceMetadataErrorKind::Read(kind) => errors::workspace(kind),
            WorkspaceMetadataErrorKind::InvalidIdentifier => {
                errors::error("InvalidInput", "Invalid workspace request identifier")
            }
        }),
        GetErrorKind::Api(error) => api_error_output(error),
        GetErrorKind::Collection(error) => api_error_output(&error.error),
        GetErrorKind::OneLake(error) => command_output(&match error {
            fabric_sdk_onelake::OneLakeError::Cancelled => {
                errors::error("LongRunningOperationCancelled", "Operation cancelled")
            }
            fabric_sdk_onelake::OneLakeError::Credential => {
                errors::error("Unauthorized", "Access is unauthorized")
            }
            error => errors::error("OneLakeError", error.to_string()),
        }),
        GetErrorKind::Resource(error) => {
            let error = fabric_cli_mutations::MutationError::from((**error).clone());
            error.output()
        }
        GetErrorKind::Codec(_) => command_output(&errors::unsupported(
            "Decoded GET value is outside the configured projection domain or limits",
        )),
        GetErrorKind::Definition(_) => command_output(&errors::error(
            "GetOutputFailed",
            "GET definition or result-file output did not complete",
        )),
        GetErrorKind::Render { .. } => {
            command_output(&errors::error("OutputError", "Rendering GET output failed"))
        }
    }
}

fn api_error_output(error: &fabric_sdk_api::ApiError) -> OutputInput {
    if let fabric_sdk_api::ApiError::Operation(operation) = error {
        return fabric_cli_mutations::operation_error_output(operation);
    }
    if matches!(
        error,
        fabric_sdk_api::ApiError::Cancelled
            | fabric_sdk_api::ApiError::Execution(fabric_sdk_api::ExecutionError::Cancelled)
    ) {
        return command_output(&errors::error(
            "LongRunningOperationCancelled",
            "Operation cancelled",
        ));
    }
    let error = fabric_cli_resources::CommandError::from(error.clone());
    command_output(&errors::error(&error.code, error.message))
}

fn command_output(error: &CommandError) -> OutputInput {
    // CommandError's existing envelope owns the product exit/message contract.
    let envelope = error.envelope(Timestamp::parse("1970-01-01T00:00:00Z").unwrap(), None);
    OutputInput {
        status: fabric_cli_output::OutputStatus::Failure,
        message: envelope.result().message().map(str::to_owned),
        error_code: envelope.result().error_code().map(str::to_owned),
        ..OutputInput::default()
    }
}

fn render(mut result: GetResult, command: &ReadCommand, timestamp: Timestamp) -> GetDispatch {
    let input = match &result {
        Ok(outcome) => outcome.output.clone(),
        Err(error) => Some(error_output(&error.kind)),
    };
    let Some(mut input) = input else {
        return GetDispatch {
            plan: TextPlan {
                records: Vec::new(),
                exit_code: 0,
            },
            result,
        };
    };
    input.command = Some(command.name.clone());
    // Invalid format is already a preflight command failure. Render that failure
    // as JSON without replacing it with a secondary formatting error.
    let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))
        .unwrap_or(OutputFormat::Json);
    let plan = fabric_cli_output::production_unicode::width_provider().and_then(|widths| {
        text::render(
            &OutputEnvelope::new(timestamp.clone(), input),
            format,
            &TextOptions {
                subcommand: Some(&command.name),
                ..TextOptions::default()
            },
            widths,
            Presentation::default(),
        )
    });
    match plan {
        Ok(plan) => GetDispatch { plan, result },
        Err(error) => {
            let mut partial = error.partial_output().cloned().unwrap_or(TextPlan {
                records: Vec::new(),
                exit_code: 1,
            });
            let (evidence, prior) = match result {
                Ok(outcome) => (outcome.evidence, None),
                Err(failure) => (failure.evidence, Some(Box::new(failure.kind))),
            };
            result = Err(Box::new(GetFailure {
                kind: GetErrorKind::Render {
                    error: Box::new(error),
                    prior,
                },
                evidence,
            }));
            let diagnostic =
                OutputEnvelope::new(timestamp, error_output(&result.as_ref().unwrap_err().kind));
            if let Ok(plan) = text::render(
                &diagnostic,
                OutputFormat::Json,
                &TextOptions::default(),
                &text::ascii_width,
                Presentation::default(),
            ) {
                partial.records.extend(plan.records);
            }
            partial.exit_code = 1;
            GetDispatch {
                plan: partial,
                result,
            }
        }
    }
}
