// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata existence observations, not mutation guards or physical-path probes.
//!
//! Existing metadata clients/pagers and caller-owned HTTP are reused. Inaccessible
//! inventories, malformed responses and failures never become successful absence.

mod catalog;
mod evidence;
mod probe;
mod request;

pub use catalog::{CatalogBackend, CatalogExistsHost};
pub use request::{
    EXISTS_OPERATION_ID, ExistsRequest, ExistsTarget, MetadataSelector, exists_descriptor,
    prepare_exists,
};

use crate::{
    Audience, Client,
    items::{ItemExecutor, ItemListLimits},
    workspaces::{WorkspaceExecutor, WorkspaceListLimits},
};
use fabric_sdk_api::v2_outcome::{
    Coverage as NativeCoverage, CoverageError, CoverageScope, Outcome as NativeOutcome,
    RetentionError, RetentionLimits, State,
};
use fabric_v2_contracts::{ApprovedOperation, ContractError, ReceiptError};
use serde::{Serialize, Serializer};
use std::{
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    task::Poll,
};

/// Caller-owned stopping reason; local interruption never cancels remote work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistsInterruption {
    /// The caller requested cancellation.
    Cancelled,
    /// The caller's whole-operation deadline elapsed.
    DeadlineExceeded,
}

struct Control<F> {
    future: std::pin::Pin<Box<F>>,
    interruption: Option<ExistsInterruption>,
}
impl<F: Future<Output = ExistsInterruption>> Future for Control<F> {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let control = self.get_mut();
        if control.interruption.is_some() {
            return Poll::Ready(());
        }
        match control.future.as_mut().poll(cx) {
            Poll::Ready(reason) => {
                control.interruption = Some(reason);
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Strongest existence conclusion supported by this invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExistsObservation {
    /// Valid exact metadata response established presence.
    Present,
    /// Exact item Get reported ItemNotFound, or an advertised catalog HEAD returned 404.
    Absent,
    /// Provider returned access denied; this does not prove presence.
    Denied,
    /// Multiple distinct scoped identities match the name/type.
    Ambiguous,
    /// Discovery or evidence exceeded a bound or stopped before completion.
    Incomplete,
    /// Available evidence cannot distinguish missing from inaccessible/changed.
    Unknown,
    /// A protocol, authentication or execution failure was observed.
    Failed,
}

/// Finite operation bounds using the existing paging and native retention policies.
#[derive(Clone, Copy, Debug)]
pub struct ExistsLimits {
    /// Workspace name discovery bounds.
    pub workspaces: WorkspaceListLimits,
    /// Item name discovery bounds.
    pub items: ItemListLimits,
    /// Whole-invocation native-response retention/dispatch allowance.
    pub retention: RetentionLimits,
    /// Entire canonical report including its final LF.
    pub max_output_bytes: NonZeroUsize,
}

impl Default for ExistsLimits {
    fn default() -> Self {
        Self {
            workspaces: WorkspaceListLimits::default(),
            items: ItemListLimits::default(),
            retention: RetentionLimits::default(),
            max_output_bytes: WorkspaceListLimits::default().max_response_bytes,
        }
    }
}

/// Bounded typed report shared verbatim by independent heads.
///
/// Raw native bodies/headers and original SDK causes are accessible explicitly,
/// never included automatically in serialization or Debug.
pub struct ExistsReport {
    request: ExistsRequest,
    observation: ExistsObservation,
    reason: &'static str,
    workspace_id: Option<String>,
    item_id: Option<String>,
    item_type: Option<String>,
    native: NativeOutcome,
    attempts: usize,
    cause: Option<probe::ProbeError>,
    retention_failure: Option<RetentionError>,
    json: Vec<u8>,
}

impl ExistsReport {
    /// Actual scoped observation, never inferred from message text.
    pub fn observation(&self) -> ExistsObservation {
        self.observation
    }
    /// True/false only for conclusive scoped evidence; otherwise null.
    pub fn exists(&self) -> Option<bool> {
        match self.observation {
            ExistsObservation::Present => Some(true),
            ExistsObservation::Absent => Some(false),
            _ => None,
        }
    }
    /// Stable sanitized reason, separate from existence.
    pub fn reason(&self) -> &'static str {
        self.reason
    }
    /// Exact native observations retained by the current shared SDK contract.
    pub fn native(&self) -> &NativeOutcome {
        &self.native
    }
    /// Original classified failure for trusted embedding consumers.
    pub fn cause(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|error| error as &dyn std::error::Error)
    }
    /// Native retention failure, without raw provider diagnostics.
    pub fn retention_failure(&self) -> Option<RetentionError> {
        self.retention_failure
    }
    /// Entire canonical JSON report, terminated by one LF.
    pub fn json_bytes(&self) -> &[u8] {
        &self.json
    }
}

impl fmt::Debug for ExistsReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExistsReport")
            .field("observation", &self.observation)
            .field("reason", &self.reason)
            .field("native", &self.native)
            .finish_non_exhaustive()
    }
}

impl Serialize for ExistsReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ResponseSummary {
            http_status: u16,
            body_bytes: usize,
            omitted_headers: usize,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct EvidenceSummary {
            state: &'static str,
            effect: &'static str,
            coverage: Option<bool>,
            scope: &'static str,
            attempts: usize,
            responses: Vec<ResponseSummary>,
            mutation_may_have_occurred: bool,
            atomic_precondition: bool,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Projection<'a> {
            schema_version: &'static str,
            operation_id: &'static str,
            request: &'a ExistsRequest,
            observation: ExistsObservation,
            exists: Option<bool>,
            reason: &'static str,
            workspace_id: &'a Option<String>,
            item_id: &'a Option<String>,
            item_type: &'a Option<String>,
            evidence: EvidenceSummary,
        }
        Projection {
            schema_version: "0.1",
            operation_id: EXISTS_OPERATION_ID,
            request: &self.request,
            observation: self.observation,
            exists: self.exists(),
            reason: self.reason,
            workspace_id: &self.workspace_id,
            item_id: &self.item_id,
            item_type: &self.item_type,
            evidence: EvidenceSummary {
                state: match self.native.state() {
                    State::Completed => "completed",
                    State::Partial => "partial",
                    State::Failed => "failed",
                    State::Unknown => "unknown",
                    State::Accepted => "accepted",
                },
                effect: "remoteRead",
                coverage: self.native.coverage().complete(),
                scope: if self.request.target().is_catalog() {
                    "selectedCatalogTarget"
                } else {
                    "selectedMetadataTarget"
                },
                attempts: self.attempts,
                responses: self
                    .native
                    .responses()
                    .iter()
                    .map(|response| ResponseSummary {
                        http_status: response.status(),
                        body_bytes: response.body().len(),
                        omitted_headers: response.omitted_header_count(),
                    })
                    .collect(),
                mutation_may_have_occurred: false,
                atomic_precondition: false,
            },
        }
        .serialize(serializer)
    }
}

/// Validation/receipt failures, never success-shaped defaults.
#[derive(Debug)]
pub enum ExistsError {
    /// Unsupported/malformed target; no credential or HTTP is acquired.
    InvalidTarget,
    /// The explicit client tenant/audience or approval differs from the request.
    ContextMismatch,
    /// Shared registry validation failed.
    Registration(ContractError),
    /// Shared preparation validation failed.
    Preparation(ReceiptError),
    /// Shared native scoped-coverage validation failed.
    Coverage(CoverageError),
    /// Native evidence locking failed; no result is inferred.
    EvidenceLock,
    /// Native evidence classification failed, retaining the original report.
    Evidence {
        /// Original observation and native responses.
        report: Box<ExistsReport>,
        /// Shared native evidence constraint that failed.
        error: RetentionError,
    },
    /// Encoding failed.
    Encoding,
    /// The complete output exceeded its limit; original evidence is retained.
    OutputLimit(Box<ExistsReport>),
}

impl fmt::Display for ExistsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "metadata Exists could not produce a valid bounded report; inspect the typed cause",
        )
    }
}
impl std::error::Error for ExistsError {}
impl From<ContractError> for ExistsError {
    fn from(error: ContractError) -> Self {
        Self::Registration(error)
    }
}
impl From<ReceiptError> for ExistsError {
    fn from(error: ReceiptError) -> Self {
        Self::Preparation(error)
    }
}
impl From<CoverageError> for ExistsError {
    fn from(error: CoverageError) -> Self {
        Self::Coverage(error)
    }
}

/// Caller-injected metadata/credential/transport boundary, with no runtime/cache.
pub struct ExistsHost<'a> {
    /// Same immutable SDK client used by Get/List.
    pub client: &'a Client,
    /// Existing generic item executor.
    pub items: &'a dyn ItemExecutor,
    /// Existing workspace executor.
    pub workspaces: &'a dyn WorkspaceExecutor,
}

impl<'a> ExistsHost<'a> {
    /// Inject an explicit Storage catalog backend without changing metadata reads.
    pub fn with_catalog(self, catalog: &'a CatalogBackend) -> CatalogExistsHost<'a> {
        CatalogExistsHost {
            metadata: self,
            catalog,
        }
    }

    /// Execute exactly the immutable approved metadata request.
    ///
    /// Service/discovery errors return null with retained native evidence. Only
    /// an exact item-ID Get's documented ItemNotFound establishes absence.
    /// Workspace visible-list misses and bare 404s remain unknown. The caller's
    /// one cancellation/deadline future covers all resolution and probe reads.
    pub async fn execute(
        &self,
        approved: &ApprovedOperation<'_>,
        request: &ExistsRequest,
        limits: ExistsLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ExistsReport, ExistsError> {
        self.execute_until(approved, request, limits, async {
            cancelled.await;
            ExistsInterruption::Cancelled
        })
        .await
    }

    /// Observe with the caller's explicit cancellation/deadline classification.
    ///
    /// The stopping future is latched: completion cannot be repolled while the
    /// existing SDK propagates interruption through several borrowed readers.
    pub async fn execute_until(
        &self,
        approved: &ApprovedOperation<'_>,
        request: &ExistsRequest,
        limits: ExistsLimits,
        interrupted: impl Future<Output = ExistsInterruption> + Send,
    ) -> Result<ExistsReport, ExistsError> {
        self.execute_inner(approved, request, limits, interrupted, None)
            .await
    }

    async fn execute_inner(
        &self,
        approved: &ApprovedOperation<'_>,
        request: &ExistsRequest,
        mut limits: ExistsLimits,
        interrupted: impl Future<Output = ExistsInterruption> + Send,
        catalog: Option<&CatalogBackend>,
    ) -> Result<ExistsReport, ExistsError> {
        let expected = prepare_exists(request)?;
        let prepared = approved.prepared();
        if self.client.selection().audience() != Audience::Fabric
            || !self
                .client
                .selection()
                .tenant()
                .eq_ignore_ascii_case(request.tenant_id())
            || prepared.operation_id() != expected.operation_id()
            || prepared.target() != expected.target()
            || prepared.input() != expected.input()
            || prepared.effects() != expected.effects()
            || prepared.availability() != expected.availability()
            || !prepared.preconditions().is_empty()
            || request.target().is_catalog()
                && catalog.is_some_and(|backend| !backend.matches(self))
        {
            return Err(ExistsError::ContextMismatch);
        }
        let response = NonZeroUsize::new(limits.retention.max_response_bytes())
            .expect("validated positive native retention limit");
        limits.items.max_response_bytes = limits.items.max_response_bytes.min(response);
        limits.workspaces.max_response_bytes = limits.workspaces.max_response_bytes.min(response);
        let evidence = evidence::Evidence::new(self.items, self.workspaces, limits.retention)?;
        let mut cancelled = pin!(Control {
            future: Box::pin(interrupted),
            interruption: None,
        });
        let result = if request.target().is_catalog() {
            if let Some(catalog) = catalog {
                let parent = catalog::parent_request(request)?;
                match probe::probe(self.client, &evidence, &parent, limits, cancelled.as_mut())
                    .await
                {
                    Ok(parent) if parent.state == ExistsObservation::Present => {
                        catalog
                            .probe(
                                &evidence,
                                request.target(),
                                parent,
                                response,
                                cancelled.as_mut(),
                            )
                            .await
                    }
                    result => result,
                }
            } else {
                Ok(catalog::unavailable())
            }
        } else {
            probe::probe(self.client, &evidence, request, limits, cancelled.as_mut()).await
        };
        let interrupted = poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await;
        let (native, attempts, retention_failure) = evidence.finish()?;
        let result = if interrupted {
            Err(
                match cancelled
                    .interruption
                    .expect("ready control has a stopping reason")
                {
                    ExistsInterruption::Cancelled => probe::ProbeError::Cancelled,
                    ExistsInterruption::DeadlineExceeded => probe::ProbeError::TimedOut,
                },
            )
        } else {
            result
        };
        let mut report = probe::report(request.clone(), result, native, attempts);
        report.retention_failure = retention_failure;
        if report.exists().is_some()
            && report
                .native
                .responses()
                .iter()
                .enumerate()
                .any(|(index, response)| {
                    response.status() != 200
                        && !(request.target().is_catalog()
                            && report.observation == ExistsObservation::Present
                            && index + 1 == report.native.responses().len()
                            && response.status() == 204)
                        && !(report.observation == ExistsObservation::Absent
                            && index + 1 == report.native.responses().len()
                            && response.status() == 404)
                })
        {
            report.observation = ExistsObservation::Failed;
            report.reason = "UnsupportedStatus";
        }
        if retention_failure.is_some() {
            report.observation = ExistsObservation::Incomplete;
            report.reason = "EvidenceRetentionFailed";
        }
        let conclusive = report.exists().is_some();
        report.native.set_coverage(NativeCoverage::new(
            CoverageScope::Response,
            if conclusive { Some(true) } else { None },
            0,
        )?);
        if let Err(error) = report.native.set_state(if conclusive {
            State::Completed
        } else if report.observation == ExistsObservation::Incomplete {
            State::Partial
        } else if matches!(
            report.observation,
            ExistsObservation::Denied | ExistsObservation::Failed
        ) {
            State::Failed
        } else {
            State::Unknown
        }) {
            return Err(ExistsError::Evidence {
                report: Box::new(report),
                error,
            });
        }
        let mut json = serde_json::to_vec(&report).map_err(|_| ExistsError::Encoding)?;
        json.push(b'\n');
        if json.len() > limits.max_output_bytes.get() {
            return Err(ExistsError::OutputLimit(Box::new(report)));
        }
        report.json = json;
        Ok(report)
    }
}
