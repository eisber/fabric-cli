// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native GraphModel refresh submission, distinct from generic job submission.
//!
//! The v1 job methods and their frozen wire policy are unchanged. This binding
//! uses the documented graph-specific route, no body, and one POST. It never
//! polls metadata, starts a query, cancels remote work or reconciles a generic LRO.

use crate::{
    JobClient, JobError, JobExecutionError, JobExecutor, JobFuture, JobRequest, JobRequestHeaders,
    JobResponse, JobTarget, JobType, RunHandle,
};
use azure_core::http::Method;
use std::{
    fmt,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Explicit workspace and GraphModel identifiers, not an item-type inference.
#[derive(Clone, Eq, PartialEq)]
pub struct GraphTarget {
    workspace: String,
    graph: String,
}

impl GraphTarget {
    /// Validate the two canonical GUIDs without credentials or metadata reads.
    ///
    /// # Errors
    /// Rejects malformed or route-changing identifiers.
    pub fn new(workspace: &str, graph: &str) -> Result<Self, JobError> {
        if !guid(workspace) || !guid(graph) {
            return Err(JobError::InvalidIdentifier);
        }
        Ok(Self {
            workspace: workspace.into(),
            graph: graph.into(),
        })
    }

    /// Original workspace ID.
    pub fn workspace_id(&self) -> &str {
        &self.workspace
    }

    /// Original GraphModel ID.
    pub fn graph_id(&self) -> &str {
        &self.graph
    }
}

impl fmt::Debug for GraphTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphTarget").finish_non_exhaustive()
    }
}

/// A native response, with completion and asynchronous acceptance kept distinct.
pub struct RefreshReceipt {
    target: GraphTarget,
    response: JobResponse,
    run: Option<RunHandle>,
}

impl RefreshReceipt {
    /// Exact requested graph.
    pub fn target(&self) -> &GraphTarget {
        &self.target
    }

    /// Actual bounded response, not an invented completion payload.
    pub fn response(&self) -> &JobResponse {
        &self.response
    }

    /// A validated job instance only when the service returned HTTP 202.
    pub fn run(&self) -> Option<&RunHandle> {
        self.run.as_ref()
    }
}

impl fmt::Debug for RefreshReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshReceipt")
            .field("status", &self.response.status)
            .field("accepted", &self.run.is_some())
            .finish_non_exhaustive()
    }
}

/// Failed submission with scoped, byte-bounded response and dispatch evidence.
pub struct RefreshError {
    /// Original SDK failure; raw service data is explicit, not printed by Display.
    pub source: JobError,
    /// Exact intended graph.
    pub target: GraphTarget,
    /// Complete response metadata, if received.
    pub response: Option<JobResponse>,
    /// True when the oversized body was omitted rather than retained as complete.
    pub response_body_omitted: bool,
    /// Dispatch began; this is not proof that work was accepted or completed.
    pub mutation_may_have_occurred: bool,
}

impl fmt::Debug for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshError")
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Graph refresh submission failed; inspect retained dispatch evidence")
    }
}

impl std::error::Error for RefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Default)]
struct Evidence {
    response: Option<JobResponse>,
    omitted: bool,
}

struct SubmissionExecutor {
    inner: Arc<dyn JobExecutor>,
    dispatched: Arc<AtomicBool>,
    evidence: Arc<Mutex<Evidence>>,
}

impl JobExecutor for SubmissionExecutor {
    fn supports_request_headers(&self, headers: &JobRequestHeaders) -> bool {
        self.inner.supports_request_headers(headers)
    }

    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            self.dispatched.store(true, Ordering::SeqCst);
            let bound = request.max_response_bytes().get();
            let result = self.inner.execute(request).await;
            if let Ok(response) = &result {
                let omitted = response.body.len() > bound;
                *self
                    .evidence
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Evidence {
                    response: Some(JobResponse {
                        status: response.status,
                        body: if omitted {
                            Vec::new()
                        } else {
                            response.body.clone()
                        },
                        headers: response.headers.clone(),
                    }),
                    omitted,
                };
            }
            result
        })
    }

    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        self.inner.wait(delay)
    }
}

/// Submit one native graph refresh using the caller's bound job client.
///
/// HTTP 200 is synchronous completion without an invented job ID. HTTP 202 must
/// provide the exact selected-host core job-instance Location and a positive
/// integer Retry-After. Only GUID coordinates compare case-insensitively.
/// Local cancellation or transport loss never causes replay or remote cancel.
///
/// # Errors
/// Preserves credential/service failures, unsafe acceptance, byte limits and
/// dispatch uncertainty. No generic operation or query fallback is attempted.
pub async fn submit(
    client: &JobClient,
    target: &GraphTarget,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<RefreshReceipt, Box<RefreshError>> {
    let dispatched = Arc::new(AtomicBool::new(false));
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let scoped = JobClient {
        context: client.context.clone(),
        executor: Arc::new(SubmissionExecutor {
            inner: client.executor.clone(),
            dispatched: dispatched.clone(),
            evidence: evidence.clone(),
        }),
        limits: client.limits,
        headers: client.headers.clone(),
    };
    let result = crate::cancellable(cancelled, async {
        let response = scoped
            .request(
                Method::Post,
                &format!(
                    "/v1/workspaces/{}/graphModels/{}/jobs/refreshGraph/instances",
                    target.workspace, target.graph
                ),
                None,
                None,
            )
            .await?;
        let run = match response.status {
            200 => None,
            202 => {
                let location =
                    checked_header(&response, "location")?.ok_or(JobError::InvalidResponse)?;
                let path = location
                    .strip_prefix("https://")
                    .and_then(|rest| rest.split_once('/'))
                    .map(|(_, path)| path)
                    .ok_or(JobError::InvalidResponse)?;
                let segments: Vec<_> = path.split('/').collect();
                let [
                    "v1",
                    "workspaces",
                    workspace,
                    "items",
                    item,
                    "jobs",
                    "instances",
                    id,
                ] = segments.as_slice()
                else {
                    return Err(JobError::InvalidResponse);
                };
                if !workspace.eq_ignore_ascii_case(&target.workspace)
                    || !item.eq_ignore_ascii_case(&target.graph)
                    || !guid(id)
                {
                    return Err(JobError::InvalidResponse);
                }
                scoped
                    .context
                    .endpoint()
                    .validate_link(location, &format!("/{path}"))
                    .map_err(|_| JobError::InvalidResponse)?;
                retry_after(&response)?;
                Some(RunHandle {
                    target: JobTarget::new(
                        &target.workspace,
                        &target.graph,
                        JobType::RefreshGraph,
                    )?,
                    id: (*id).into(),
                    accepted: response.clone(),
                    endpoint: scoped.context.endpoint().clone(),
                })
            }
            status => return Err(JobError::UnexpectedStatus(status)),
        };
        Ok(RefreshReceipt {
            target: target.clone(),
            response,
            run,
        })
    })
    .await;
    result.map_err(|source| {
        let mut evidence = evidence.lock().unwrap_or_else(|error| error.into_inner());
        Box::new(RefreshError {
            source,
            target: target.clone(),
            response: evidence.response.take(),
            response_body_omitted: evidence.omitted,
            mutation_may_have_occurred: dispatched.load(Ordering::SeqCst),
        })
    })
}

/// Required first-poll delay from a native accepted graph response.
///
/// # Errors
/// Rejects missing, duplicated, zero, signed, padded or overflowing values.
pub fn retry_after(response: &JobResponse) -> Result<Duration, JobError> {
    let value = checked_header(response, "retry-after")?.ok_or(JobError::InvalidResponse)?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(JobError::InvalidResponse);
    }
    let seconds = value
        .parse::<u64>()
        .map_err(|_| JobError::InvalidResponse)?;
    if seconds == 0 {
        return Err(JobError::InvalidResponse);
    }
    Ok(Duration::from_secs(seconds))
}

fn checked_header<'a>(response: &'a JobResponse, name: &str) -> Result<Option<&'a str>, JobError> {
    let mut values = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = values.next().map(|(_, value)| value.as_str());
    if values.next().is_some() {
        return Err(JobError::InvalidResponse);
    }
    Ok(value)
}

fn guid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
