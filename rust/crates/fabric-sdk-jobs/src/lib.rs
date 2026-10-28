// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric job execution, distinct from Notebook definition publication.
//!
//! Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
//! `client/fab_api_jobs.py` and `utils/fab_cmd_job_utils.py`. Credentials,
//! transport, asynchronous time and cancellation belong to the caller.

mod command;
mod headers;
mod numeric;
mod operations;
use azure_core::{
    credentials::AccessToken,
    http::{Method, Url},
};
pub use command::JobCommandRoute;
pub use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_core::{AuthenticatedExecution, RequestError};
pub use fabric_sdk_operations::OperationError;
use fabric_sdk_paging::TokenPager;
pub use fabric_sdk_paging::{PagedValues, PagingError, PagingErrorKind, PagingLimits};
use fabric_sdk_workspaces::models::FabricServiceError;
pub use fabric_sdk_workspaces::workspaces::{
    WorkspaceExecutionError as JobExecutionError, WorkspaceResponse as JobResponse,
};
pub use headers::JobRequestHeaders;
pub use numeric::{source_decimal_digit, source_decimal_text, source_integer_text};
pub use operations::SharedJobOperations;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};

/// Caller-runtime execution future; dropping it cancels local work.
pub type JobFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A validated job request, constructed only by [`JobClient`].
pub struct JobRequest<'a> {
    method: Method,
    url: Url,
    token: &'a AccessToken,
    body: Vec<u8>,
    max_response_bytes: NonZeroUsize,
    headers: JobRequestHeaders,
}

impl JobRequest<'_> {
    /// Source HTTP method.
    pub fn method(&self) -> Method {
        self.method
    }
    /// Validated Fabric URL; never log opaque identifiers or continuation values.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Injected credential's token, never for logging or persistence.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Exact JSON request bytes, or an empty body when absent.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Maximum decoded response bytes, enforced during transport streaming.
    pub fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Validated caller-owned headers for this command, never ambient defaults.
    pub fn headers(&self) -> &JobRequestHeaders {
        &self.headers
    }
}

impl fmt::Debug for JobRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRequest")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Job operation boundary, independent of HTTP implementations and the SDK facade.
///
/// Use the shared HTTP engine for deadlines, read retries and response bounds;
/// never replay mutations, follow redirects, select credentials or create a runtime.
pub trait JobExecutor: Send + Sync {
    /// Explicit header capability; existing executors accept only the empty contract.
    ///
    /// An implementation opting in must forward all supported fields through its
    /// checked shared request path, never silently discard them.
    fn supports_request_headers(&self, headers: &JobRequestHeaders) -> bool {
        headers.is_empty()
    }
    /// Execute one validated job request.
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>>;
    /// Wait using the caller's asynchronous timer, releasing local work on drop.
    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>>;
}

/// Injection seam for Python's conditional generic Fabric-operation reconciliation.
///
/// Implementations must reuse the shared generic LRO policy, not job polling or
/// Notebook publication. Dropping the future must cancel local work. This
/// capability is needed only when a synchronous cancellation response identifies
/// a generic operation instead of a job instance.
pub trait JobOperationReconciler: Send + Sync {
    /// Inspect the explicit header context; existing implementations have none.
    fn request_headers(&self) -> JobRequestHeaders {
        JobRequestHeaders::default()
    }
    /// Return a command-local reconciler preserving the selected request headers.
    ///
    /// Existing custom reconcilers fail explicitly rather than dropping a skill
    /// during generic-operation follow-ups. The default path needs no scoping.
    fn with_request_headers(
        &self,
        _headers: &JobRequestHeaders,
    ) -> Result<Arc<dyn JobOperationReconciler>, JobError> {
        Err(JobError::RequestHeadersUnsupported)
    }
    /// Reconcile the accepted generic operation and retain its actual HTTP result.
    fn reconcile<'a>(
        &'a self,
        accepted: &'a JobResponse,
    ) -> JobFuture<'a, Result<JobResponse, JobError>>;

    /// Reconcile with the command's cancellation signal.
    ///
    /// Implementations supporting operation evidence should override this method
    /// so cancellation is recorded by the shared operation session before return.
    /// The default preserves compatibility with existing custom reconcilers.
    fn reconcile_with_cancellation<'a>(
        &'a self,
        accepted: &'a JobResponse,
        cancelled: JobFuture<'a, ()>,
    ) -> JobFuture<'a, Result<JobResponse, JobError>> {
        Box::pin(cancellable(cancelled, self.reconcile(accepted)))
    }
}

/// Pinned source job types; spelling and casing are service-significant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobType {
    /// Spark job definition execution.
    Spark,
    /// Notebook execution (not definition publication).
    Notebook,
    /// Data pipeline execution.
    Pipeline,
    /// Lakehouse table maintenance execution.
    TableMaintenance,
    /// Graph model refresh.
    RefreshGraph,
}

impl JobType {
    /// Source `FabricJobType.value`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spark => "sparkjob",
            Self::Notebook => "RunNotebook",
            Self::Pipeline => "Pipeline",
            Self::TableMaintenance => "TableMaintenance",
            Self::RefreshGraph => "refreshGraph",
        }
    }

    /// Source `ITJobMap`, without broadening supported resources.
    pub fn from_item_type(item_type: &str) -> Option<Self> {
        match item_type {
            "SparkJobDefinition" => Some(Self::Spark),
            "Notebook" => Some(Self::Notebook),
            "DataPipeline" => Some(Self::Pipeline),
            "Lakehouse" => Some(Self::TableMaintenance),
            "GraphModel" => Some(Self::RefreshGraph),
            _ => None,
        }
    }
}

/// Validated job route, retaining opaque source identifiers.
#[derive(Clone, Eq, PartialEq)]
pub struct JobTarget {
    workspace: String,
    item: String,
    job_type: JobType,
}

impl JobTarget {
    /// Construct a target without credential acquisition or I/O.
    ///
    /// Rejects route-changing separators, escapes, controls and dot segments.
    pub fn new(workspace: &str, item: &str, job_type: JobType) -> Result<Self, JobError> {
        identifier(workspace)?;
        identifier(item)?;
        Ok(Self {
            workspace: workspace.into(),
            item: item.into(),
            job_type,
        })
    }

    /// Source job type selected for this resource.
    pub const fn job_type(&self) -> JobType {
        self.job_type
    }

    fn instances_path(&self) -> String {
        format!(
            "/v1/workspaces/{}/items/{}/jobs/instances",
            self.workspace, self.item
        )
    }

    fn schedules_path(&self) -> String {
        format!(
            "/v1/workspaces/{}/items/{}/jobs/{}/schedules",
            self.workspace,
            self.item,
            self.job_type.as_str()
        )
    }
}

impl fmt::Debug for JobTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobTarget")
            .field("job_type", &self.job_type)
            .finish_non_exhaustive()
    }
}

/// A service-accepted instance, never evidence of completed execution.
#[derive(Clone)]
pub struct RunHandle {
    target: JobTarget,
    id: String,
    accepted: JobResponse,
    endpoint: EndpointBinding,
}

impl RunHandle {
    /// Accepted instance ID, suitable for explicit status or cancellation calls.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Validated resource binding, preventing cross-resource polling.
    pub fn target(&self) -> &JobTarget {
        &self.target
    }

    /// Exact selected endpoint for subsequent status requests, not an advertised host.
    pub fn endpoint(&self) -> &EndpointBinding {
        &self.endpoint
    }

    /// Original accepted response, including headers needed by a shared reconciler.
    pub fn accepted_response(&self) -> &JobResponse {
        &self.accepted
    }

    /// Source `do_request` generic-operation branch, distinct from a job Location.
    pub fn requires_operation_reconciliation(&self) -> bool {
        let location = header(&self.accepted.headers, "location").unwrap_or("");
        !location.contains("/jobs/instances/")
            && (!location.is_empty()
                || header(&self.accepted.headers, "x-ms-operation-id")
                    .is_some_and(|id| !id.is_empty()))
    }
}

impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunHandle")
            .field("state", &"accepted, not completed")
            .finish_non_exhaustive()
    }
}

/// Source status interpretation, including its non-failing unknown-status branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    /// Waiting for execution.
    NotStarted,
    /// Executing.
    InProgress,
    /// Execution completed successfully.
    Completed,
    /// Service confirmed cancellation.
    Cancelled,
    /// Service deduplicated this run.
    Deduped,
    /// Execution failed.
    Failed,
    /// Future status: source warns, prints the row and returns without failure.
    Unknown,
}

/// Full ordered status response, retaining future fields and missing/null distinctions.
#[derive(Clone)]
pub struct JobStatus {
    value: Value,
    headers: BTreeMap<String, String>,
}

impl JobStatus {
    /// The source status string.
    pub fn status(&self) -> &str {
        // Typed status reads require a string. Only the explicit command policy
        // can retain a source non-string status, which classifies as unknown.
        self.value["status"].as_str().unwrap_or("")
    }

    /// Original status value for the opt-in source command policy.
    ///
    /// Typed SDK status reads still require a string; command consumers may
    /// need to display Python's unknown null/bool/number/container status.
    pub fn status_value(&self) -> &Value {
        &self.value["status"]
    }

    /// Interpret known statuses without rejecting future service states.
    pub fn state(&self) -> JobState {
        match self.status() {
            "NotStarted" => JobState::NotStarted,
            "InProgress" => JobState::InProgress,
            "Completed" => JobState::Completed,
            "Cancelled" => JobState::Cancelled,
            "Deduped" => JobState::Deduped,
            "Failed" => JobState::Failed,
            _ => JobState::Unknown,
        }
    }

    /// Borrow the service object in its original parsed key order.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Consume the ordered service object.
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl fmt::Debug for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobStatus")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// Job wait options; no timeout intentionally means wait until terminal or cancellation.
#[derive(Clone, Copy, Debug, Default)]
pub struct WaitOptions {
    /// Python accepts signed integer seconds, including zero/negative immediate timeout.
    pub timeout: Option<i64>,
    /// Positive custom interval overrides every response's Retry-After.
    pub polling_interval: Option<u64>,
}

impl WaitOptions {
    /// Validate the source relationship before submitting a job.
    pub fn validate(self) -> Result<(), JobError> {
        if self.polling_interval == Some(0) {
            return Err(JobError::InvalidPollingInterval);
        }
        if let (Some(timeout), Some(interval)) = (self.timeout, self.polling_interval)
            && (timeout <= 0 || interval >= timeout as u64)
        {
            return Err(JobError::PollingExceedsTimeout { timeout, interval });
        }
        Ok(())
    }
}

/// Explicit bounded response allocation.
#[derive(Clone, Copy, Debug)]
pub struct JobLimits {
    /// Maximum decoded bytes per HTTP response.
    pub max_response_bytes: NonZeroUsize,
    /// Maximum committed pages for instance/schedule listing.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated instance/schedule rows.
    pub max_items: NonZeroUsize,
}

impl Default for JobLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
        }
    }
}

/// Typed failures; Display never includes tokens, URLs or raw provider diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Caller supplied a skill header rejected before credentials or service I/O.
    #[error("invalid job request headers")]
    InvalidRequestHeaders,
    /// An injected executor/reconciler did not opt into the requested headers.
    #[error("the injected job executor or reconciler does not support request headers")]
    RequestHeadersUnsupported,
    /// Original shared-operation failure, including ID, raw diagnostics and uncertainty.
    #[error(transparent)]
    Operation(#[from] OperationError),
    /// The explicit selection is not Fabric.
    #[error("job operations require the Fabric audience")]
    InvalidAudience,
    /// The selected service, URL or accepted handle does not match the Fabric context.
    #[error("job request does not match the selected Fabric endpoint")]
    InvalidEndpoint,
    /// An ID could alter the validated route.
    #[error("invalid job route identifier")]
    InvalidIdentifier,
    /// The injected provider failed; no fallback is attempted.
    #[error("the injected credential could not acquire a Fabric token")]
    Credential,
    /// Shared HTTP execution failed.
    #[error("job request execution failed")]
    Execution(JobExecutionError),
    /// Shared service-error data, retained without treating it as instructions.
    #[error("Fabric rejected the job request")]
    Http {
        /// Actual HTTP status.
        status: u16,
        /// Source error-code precedence.
        code: String,
        /// Structured service response, when valid.
        service_error: Option<Box<FabricServiceError>>,
        /// Exact parsed service failure, including missing or future structured fields.
        raw_service_error: Option<Box<Value>>,
        /// Original service headers.
        headers: BTreeMap<String, String>,
    },
    /// A success-shaped but wrong HTTP status is not successful job execution.
    #[error("unexpected job response status {0}")]
    UnexpectedStatus(u16),
    /// Malformed/missing required service fields or unbound accepted location.
    #[error("Fabric returned an invalid job response")]
    InvalidResponse,
    /// Local cooperative cancellation drops in-flight work, not service rollback.
    #[error("job operation was cancelled")]
    Cancelled,
    /// Python's elapsed-wait timeout; CLI decides whether to cancel the remote job.
    #[error("job execution timed out")]
    TimedOut {
        /// Accounted source wait seconds, including completed request time.
        elapsed_seconds: u64,
    },
    /// Service confirmed execution failure; retain the actual row for source output.
    #[error("job execution failed")]
    Failed {
        /// Ordered service object.
        content: Value,
    },
    /// A custom polling interval must be positive.
    #[error("custom polling interval must be positive")]
    InvalidPollingInterval,
    /// Preserve Python's validation before any submission.
    #[error("custom polling interval cannot be greater than or equal to timeout")]
    PollingExceedsTimeout {
        /// Supplied timeout.
        timeout: i64,
        /// Supplied custom interval.
        interval: u64,
    },
    /// A caller's status observer failed; execution stops without hiding that failure.
    #[error("the caller's job status observer failed")]
    Observer,
    /// Python rejects a negative sleep rather than silently using a default.
    #[error("sleep length must be non-negative")]
    InvalidSleep,
    /// A syntactically valid service polling integer exceeds native duration bounds.
    #[error("job polling interval exceeds the supported duration bound")]
    PollingDelayLimit,
    /// Shared page collection failed with precise committed partial progress.
    #[error("job page collection failed")]
    Paging(Box<PagingError<JobError>>),
    /// A source-selected generic LRO cannot silently become job completion.
    #[error("this response requires the shared generic Fabric-operation reconciler")]
    OperationReconcilerRequired,
}

/// Reusable job service with caller-injected credential, HTTP execution and runtime.
pub struct JobClient {
    context: RequestContext,
    executor: Arc<dyn JobExecutor>,
    limits: JobLimits,
    headers: JobRequestHeaders,
}

impl JobClient {
    /// Store explicit dependencies without I/O, discovery, or runtime creation.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        executor: Arc<dyn JobExecutor>,
        limits: JobLimits,
    ) -> Self {
        Self::with_context(RequestContext::new(credential, selection), executor, limits)
    }

    /// Store the caller's exact credential, selection and endpoint without I/O.
    ///
    /// Every route, page and status request uses this immutable context. An
    /// audience/service mismatch fails before credential acquisition; no host
    /// is inferred from response data. Existing constructors remain public Fabric.
    pub fn with_context(
        context: RequestContext,
        executor: Arc<dyn JobExecutor>,
        limits: JobLimits,
    ) -> Self {
        Self {
            context,
            executor,
            limits,
            headers: JobRequestHeaders::default(),
        }
    }

    /// Inspect the exact immutable endpoint and injected credential identity.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Clone a command-local client with explicit validated request headers.
    ///
    /// The original client and its injected identity/executor remain unchanged.
    /// Unsupported executor capability fails before credentials or mutation.
    pub fn with_request_headers(&self, headers: JobRequestHeaders) -> Result<Self, JobError> {
        if !self.executor.supports_request_headers(&headers) {
            return Err(JobError::RequestHeadersUnsupported);
        }
        Ok(Self {
            context: self.context.clone(),
            executor: self.executor.clone(),
            limits: self.limits,
            headers,
        })
    }

    /// Inspect the explicit immutable request context, without identity acquisition.
    pub fn request_headers(&self) -> &JobRequestHeaders {
        &self.headers
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Value>,
    ) -> Result<JobResponse, JobError> {
        let mut url = self
            .context
            .endpoint()
            .url(path)
            .map_err(|_| JobError::InvalidEndpoint)?;
        if let Some(job_type) = query {
            url.query_pairs_mut().append_pair("jobType", job_type);
        }
        self.request_url(method, url, body, self.limits.max_response_bytes)
            .await
    }

    async fn request_url(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
        max_response_bytes: NonZeroUsize,
    ) -> Result<JobResponse, JobError> {
        let bytes = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| JobError::InvalidResponse)?
            .unwrap_or_default();
        let mut execution = AuthenticatedJob {
            executor: self.executor.as_ref(),
            method,
            body: bytes,
            max_response_bytes,
            headers: self.headers.clone(),
        };
        let mut never = pin!(std::future::pending());
        let response = self
            .context
            .authentication()
            .execute(ServiceOrigin::Fabric, &url, &mut execution, never.as_mut())
            .await
            .map_err(|error| match error {
                RequestError::InvalidAudience => JobError::InvalidAudience,
                RequestError::InvalidEndpoint => JobError::InvalidEndpoint,
                RequestError::Credential => JobError::Credential,
                RequestError::Execution(error) => JobError::Execution(error),
                RequestError::Cancelled => JobError::Cancelled,
            })?;
        if response.body.len() > max_response_bytes.get() {
            return Err(JobError::Execution(JobExecutionError::ResponseLimit));
        }
        if response.status >= 400 {
            return Err(service_failure(&response));
        }
        Ok(response)
    }

    /// List instances or schedules through the shared bounded token collector.
    ///
    /// Credentials are acquired for every page. On failure, [`JobError::Paging`]
    /// retains full committed pages; an incomplete list is never successful.
    pub async fn list(
        &self,
        target: &JobTarget,
        schedules: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<PagedValues, JobError> {
        let path = if schedules {
            target.schedules_path()
        } else {
            target.instances_path()
        };
        let url = self
            .context
            .endpoint()
            .url(&path)
            .map_err(|_| JobError::InvalidEndpoint)?;
        let pager = TokenPager::new(
            url,
            PagingLimits {
                max_pages: self.limits.max_pages,
                max_items: self.limits.max_items,
                max_response_bytes: self.limits.max_response_bytes,
            },
        )
        .map_err(|_| JobError::InvalidIdentifier)?;
        let result = pager
            .collect(
                |request| async move {
                    self.request_url(
                        Method::Get,
                        request.url().clone(),
                        None,
                        request.max_response_bytes(),
                    )
                    .await
                },
                cancelled,
            )
            .await
            .map_err(|error| JobError::Paging(Box::new(error)))?;
        if result.value.iter().any(|row| !row.is_object()) {
            return Err(JobError::InvalidResponse);
        }
        Ok(result)
    }

    /// Submit exactly once; 202 produces a validated asynchronous handle, never completion.
    pub async fn submit(
        &self,
        target: &JobTarget,
        execution_data: Option<Value>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RunHandle, JobError> {
        let body = execution_data.map(|data| json!({"executionData": data}));
        cancellable(cancelled, async {
            let response = self
                .request(
                    Method::Post,
                    &target.instances_path(),
                    Some(target.job_type.as_str()),
                    body.as_ref(),
                )
                .await?;
            expect_status(&response, 202)?;
            let location =
                header(&response.headers, "location").ok_or(JobError::InvalidResponse)?;
            let url = Url::parse(location).map_err(|_| JobError::InvalidResponse)?;
            let prefix = format!("{}/", target.instances_path());
            let id = url
                .path()
                .strip_prefix(&prefix)
                .ok_or(JobError::InvalidResponse)?;
            identifier(id).map_err(|_| JobError::InvalidResponse)?;
            if self
                .context
                .endpoint()
                .validate_link(location, &format!("{prefix}{id}"))
                .is_err()
                || !location.starts_with(&format!("https://{}/", self.context.endpoint().host()))
            {
                return Err(JobError::InvalidResponse);
            }
            Ok(RunHandle {
                target: target.clone(),
                id: id.into(),
                accepted: response,
                endpoint: self.context.endpoint().clone(),
            })
        })
        .await
    }

    /// Get an existing run; no schedule or definition-LRO substitution is performed.
    pub async fn status(
        &self,
        target: &JobTarget,
        id: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, JobError> {
        identifier(id)?;
        cancellable(cancelled, self.status_inner(target, id)).await
    }

    async fn status_inner(&self, target: &JobTarget, id: &str) -> Result<JobStatus, JobError> {
        let response = self
            .request(
                Method::Get,
                &format!("{}/{id}", target.instances_path()),
                None,
                None,
            )
            .await?;
        expect_status(&response, 200)?;
        let value = decode_object(&response)?;
        if !value.get("status").is_some_and(Value::is_string) {
            return Err(JobError::InvalidResponse);
        }
        Ok(JobStatus {
            value,
            headers: response.headers,
        })
    }

    /// Cancel using Python's synchronous HTTP-response policy, before any job wait.
    ///
    /// A generic operation result with HTTP 200/201/204 yields `None`, matching
    /// the source command's no-202/no-output branch; it does not assert that the
    /// job itself was cancelled. An accepted job response still yields a handle.
    /// Missing generic reconciliation fails explicitly only when needed.
    pub async fn cancel_for_wait(
        &self,
        target: &JobTarget,
        id: &str,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<RunHandle>, JobError> {
        let mut cancelled = pin!(cancelled);
        let mut handle = self.cancel(target, id, cancelled.as_mut()).await?;
        if handle.requires_operation_reconciliation() {
            let operations = operations.ok_or(JobError::OperationReconcilerRequired)?;
            let result = operations
                .reconcile_with_cancellation(&handle.accepted, Box::pin(cancelled.as_mut()))
                .await?;
            match result.status {
                200 | 201 | 204 => return Ok(None),
                202 => handle.accepted = result,
                status => return Err(JobError::UnexpectedStatus(status)),
            }
        }
        Ok(Some(handle))
    }

    /// Request remote cancellation once; acceptance is not terminal cancellation.
    pub async fn cancel(
        &self,
        target: &JobTarget,
        id: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RunHandle, JobError> {
        identifier(id)?;
        cancellable(cancelled, async {
            let response = self
                .request(
                    Method::Post,
                    &format!("{}/{id}/cancel", target.instances_path()),
                    None,
                    None,
                )
                .await?;
            expect_status(&response, 202)?;
            Ok(RunHandle {
                target: target.clone(),
                id: id.into(),
                accepted: response,
                endpoint: self.context.endpoint().clone(),
            })
        })
        .await
    }

    /// Wait for a job, preserving Python's initial sleep, elapsed accounting and states.
    ///
    /// `observe` is called for every decoded state, including failure/unknown.
    /// Dropping/cancelling this wait does not cancel the remote job.
    pub async fn wait(
        &self,
        handle: &RunHandle,
        options: WaitOptions,
        mut observe: impl FnMut(&JobStatus) -> Result<(), JobError> + Send,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, JobError> {
        self.wait_inner(handle, options, &mut observe, false, None, cancelled)
            .await
    }

    /// Wait with the source CLI's permissive status-response policy.
    ///
    /// Missing status remains an error. Present non-string statuses reach the
    /// unknown-status observer unchanged. Successful non-200 responses continue
    /// the source loop without a fabricated terminal result or extra sleep.
    /// Shared operation reconciliation is conditional on actual response headers.
    pub async fn wait_for_command(
        &self,
        handle: &RunHandle,
        options: WaitOptions,
        mut observe: impl FnMut(&JobStatus) -> Result<(), JobError> + Send,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, JobError> {
        self.wait_inner(handle, options, &mut observe, true, operations, cancelled)
            .await
    }

    async fn wait_inner(
        &self,
        handle: &RunHandle,
        options: WaitOptions,
        observe: &mut (impl FnMut(&JobStatus) -> Result<(), JobError> + Send),
        command_policy: bool,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobStatus, JobError> {
        options.validate()?;
        if handle.endpoint != *self.context.endpoint() {
            return Err(JobError::InvalidEndpoint);
        }
        cancellable(cancelled, async {
            if options.timeout.is_some_and(|timeout| timeout < 0) {
                return Err(JobError::InvalidSleep);
            }
            let mut interval =
                polling_interval(&handle.accepted.headers, options.polling_interval)?;
            if let Some(timeout) = options.timeout {
                interval = interval.min(timeout as u64);
            }
            self.executor
                .wait(Duration::from_secs(interval))
                .await
                .map_err(JobError::Execution)?;
            let mut elapsed = interval;
            while options
                .timeout
                .is_none_or(|timeout| elapsed < timeout as u64)
            {
                let started = Instant::now();
                let status = if command_policy {
                    let response = self
                        .response_for_command(
                            &handle.target,
                            JobCommandRoute::Status(&handle.id),
                            None,
                            true,
                            operations,
                            std::future::pending(),
                        )
                        .await?;
                    elapsed = elapsed.saturating_add(started.elapsed().as_secs());
                    if response.status != 200 {
                        continue;
                    }
                    let value = decode_object(&response)?;
                    if value.get("status").is_none() {
                        return Err(JobError::InvalidResponse);
                    }
                    JobStatus {
                        value,
                        headers: response.headers,
                    }
                } else {
                    let status = self.status_inner(&handle.target, &handle.id).await?;
                    elapsed = elapsed.saturating_add(started.elapsed().as_secs());
                    status
                };
                observe(&status)?;
                match status.state() {
                    JobState::Completed
                    | JobState::Cancelled
                    | JobState::Deduped
                    | JobState::Unknown => return Ok(status),
                    JobState::Failed => {
                        return Err(JobError::Failed {
                            content: status.value,
                        });
                    }
                    JobState::NotStarted | JobState::InProgress => {
                        interval = polling_interval(&status.headers, options.polling_interval)?;
                        self.executor
                            .wait(Duration::from_secs(interval))
                            .await
                            .map_err(JobError::Execution)?;
                        elapsed = elapsed.saturating_add(interval);
                    }
                }
            }
            Err(JobError::TimedOut {
                elapsed_seconds: elapsed,
            })
        })
        .await
    }

    /// Create a schedule with its already-translated configuration/enabled envelope.
    pub async fn create_schedule(
        &self,
        target: &JobTarget,
        payload: &Value,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, JobError> {
        cancellable(cancelled, async {
            let response = self
                .request(Method::Post, &target.schedules_path(), None, Some(payload))
                .await?;
            expect_status(&response, 201)?;
            schedule_object(&response)
        })
        .await
    }

    /// Get a schedule; a missing configuration is not replaced with a default.
    pub async fn get_schedule(
        &self,
        target: &JobTarget,
        id: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, JobError> {
        identifier(id)?;
        cancellable(cancelled, async {
            let response = self
                .request(
                    Method::Get,
                    &format!("{}/{id}", target.schedules_path()),
                    None,
                    None,
                )
                .await?;
            expect_status(&response, 200)?;
            schedule_object(&response)
        })
        .await
    }

    /// Patch a schedule once, without replaying a lost mutation response.
    pub async fn update_schedule(
        &self,
        target: &JobTarget,
        id: &str,
        payload: &Value,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, JobError> {
        identifier(id)?;
        cancellable(cancelled, async {
            let response = self
                .request(
                    Method::Patch,
                    &format!("{}/{id}", target.schedules_path()),
                    None,
                    Some(payload),
                )
                .await?;
            expect_status(&response, 200)?;
            schedule_object(&response)
        })
        .await
    }

    /// Delete a schedule once. Source completion status is 200, not 202 or 204.
    ///
    /// CLI confirmation is deliberately outside the SDK.
    pub async fn remove_schedule(
        &self,
        target: &JobTarget,
        id: &str,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<(), JobError> {
        identifier(id)?;
        cancellable(cancelled, async {
            let response = self
                .request(
                    Method::Delete,
                    &format!("{}/{id}", target.schedules_path()),
                    None,
                    None,
                )
                .await?;
            expect_status(&response, 200)
        })
        .await
    }
}

impl fmt::Debug for JobClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobClient")
            .field("credential", &"<injected>")
            .finish_non_exhaustive()
    }
}

struct AuthenticatedJob<'a> {
    executor: &'a dyn JobExecutor,
    method: Method,
    body: Vec<u8>,
    max_response_bytes: NonZeroUsize,
    headers: JobRequestHeaders,
}

impl AuthenticatedExecution for AuthenticatedJob<'_> {
    type Response = JobResponse;
    type Error = JobExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> JobFuture<'a, Result<Self::Response, Self::Error>> {
        self.executor.execute(JobRequest {
            method: self.method,
            url: url.clone(),
            token,
            body: std::mem::take(&mut self.body),
            max_response_bytes: self.max_response_bytes,
            headers: self.headers.clone(),
        })
    }
}

fn identifier(id: &str) -> Result<(), JobError> {
    if id.is_empty()
        || matches!(id, "." | "..")
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | '%' | '?' | '#' | ':' | ' '))
    {
        Err(JobError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn expect_status(response: &JobResponse, expected: u16) -> Result<(), JobError> {
    if response.status == expected {
        Ok(())
    } else {
        Err(JobError::UnexpectedStatus(response.status))
    }
}

fn decode_object(response: &JobResponse) -> Result<Value, JobError> {
    let value: Value =
        serde_json::from_slice(&response.body).map_err(|_| JobError::InvalidResponse)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(JobError::InvalidResponse)
    }
}

fn schedule_object(response: &JobResponse) -> Result<Value, JobError> {
    let value = decode_object(response)?;
    if value.get("id").is_some_and(Value::is_string) {
        Ok(value)
    } else {
        Err(JobError::InvalidResponse)
    }
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn polling_interval(
    headers: &BTreeMap<String, String>,
    custom: Option<u64>,
) -> Result<u64, JobError> {
    if let Some(custom) = custom {
        return Ok(custom);
    }
    let Some(value) = header(headers, "retry-after") else {
        return Ok(10);
    };
    let Some(value) = source_integer_text(value) else {
        return Ok(10);
    };
    let negative = value.starts_with('-');
    let seconds = value
        .strip_prefix(['+', '-'])
        .unwrap_or(&value)
        .parse::<u64>()
        .map_err(|_| JobError::PollingDelayLimit)?;
    if negative && seconds != 0 {
        return Err(JobError::InvalidSleep);
    }
    Ok(seconds)
}

fn service_failure(response: &JobResponse) -> JobError {
    let body = serde_json::from_slice::<Value>(&response.body).ok();
    let code = match response.status {
        401 | 403 => header(&response.headers, "x-ms-public-api-error-code")
            .filter(|v| !v.is_empty())
            .or_else(|| header(&response.headers, "x-ms-error-code").filter(|v| !v.is_empty()))
            .unwrap_or(if response.status == 401 {
                "Unauthorized"
            } else {
                "Forbidden"
            })
            .into(),
        404 => "NotFound".into(),
        _ => body
            .as_ref()
            .and_then(|v| v.get("errorCode"))
            .and_then(Value::as_str)
            .unwrap_or("UnknownError")
            .into(),
    };
    JobError::Http {
        status: response.status,
        code,
        raw_service_error: body.clone().map(Box::new),
        service_error: body
            .and_then(|v| serde_json::from_value(v).ok())
            .map(Box::new),
        headers: response.headers.clone(),
    }
}

/// Race caller cancellation against work; cancellation wins simultaneous readiness.
///
/// Useful to keep one cancellation future across a complete CLI command.
pub async fn cancellable<T>(
    cancelled: impl Future<Output = ()> + Send,
    work: impl Future<Output = Result<T, JobError>> + Send,
) -> Result<T, JobError> {
    let mut cancelled = pin!(cancelled);
    let mut work = pin!(work);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(JobError::Cancelled))
        } else {
            work.as_mut().poll(cx)
        }
    })
    .await
}
