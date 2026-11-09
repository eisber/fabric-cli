// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded cancellation of one explicitly identified Notebook run.
//!
//! This API issues one cancellation POST using the selected client's context.
//! It never discovers metadata, reconciles generic operations, waits, retries or
//! follows a returned Location. HTTP 202 acknowledges the request only; it is
//! not evidence of terminal cancellation or rollback.

use crate::{
    EndpointBinding, JobClient, JobError, JobExecutionError, JobExecutor, JobFuture, JobRequest,
    JobRequestHeaders, JobResponse, JobTarget, JobType,
};
use std::{
    error::Error,
    fmt,
    future::Future,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Explicit Notebook workspace, item and job-instance identifiers.
///
/// Identifiers must have the ASCII GUID shape `8-4-4-4-12`. Both hexadecimal
/// cases and the nil GUID are accepted syntactically; no existence or Notebook
/// type lookup is performed. Supplied spelling is preserved.
#[derive(Clone, Eq, PartialEq)]
pub struct NotebookRun {
    workspace_id: String,
    item_id: String,
    instance_id: String,
}

impl NotebookRun {
    /// Validate all three identifiers without credentials or service I/O.
    ///
    /// Returns [`JobError::InvalidIdentifier`] for anything other than a
    /// 36-byte ASCII hexadecimal GUID with hyphens in the canonical positions.
    pub fn new(workspace_id: &str, item_id: &str, instance_id: &str) -> Result<Self, JobError> {
        for id in [workspace_id, item_id, instance_id] {
            if id.len() != 36
                || !id.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
            {
                return Err(JobError::InvalidIdentifier);
            }
        }
        Ok(Self {
            workspace_id: workspace_id.into(),
            item_id: item_id.into(),
            instance_id: instance_id.into(),
        })
    }

    /// Workspace containing the explicitly selected Notebook.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Explicit Notebook item identifier.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    /// Explicit job-instance identifier; never a generic operation identifier.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn validate_location(
        &self,
        endpoint: &EndpointBinding,
        location: &str,
    ) -> Result<(), JobError> {
        let (_, raw_path) = location
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
            .ok_or(JobError::InvalidResponse)?;
        let segments: Vec<_> = raw_path.splitn(9, '/').collect();
        let [
            "v1",
            "workspaces",
            workspace,
            "items",
            item,
            "jobs",
            "instances",
            instance,
        ] = segments.as_slice()
        else {
            return Err(JobError::InvalidResponse);
        };
        if !workspace.eq_ignore_ascii_case(&self.workspace_id)
            || !item.eq_ignore_ascii_case(&self.item_id)
            || !instance.eq_ignore_ascii_case(&self.instance_id)
        {
            return Err(JobError::InvalidResponse);
        }
        let expected_path =
            format!("/v1/workspaces/{workspace}/items/{item}/jobs/instances/{instance}");
        endpoint
            .validate_link(location, &expected_path)
            .map(|_| ())
            .map_err(|_| JobError::InvalidResponse)
    }
}

impl fmt::Debug for NotebookRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotebookRun").finish_non_exhaustive()
    }
}

/// Acknowledgement of a cancellation request, not a terminal job result.
///
/// The receipt binds the known run to the caller-selected endpoint and retains
/// the actual HTTP 202 response, including untrusted service headers and body.
#[derive(Clone)]
pub struct CancellationReceipt {
    target: NotebookRun,
    endpoint: EndpointBinding,
    response: JobResponse,
}

impl CancellationReceipt {
    /// Run identified by the cancellation POST.
    pub fn target(&self) -> &NotebookRun {
        &self.target
    }

    /// Original client endpoint; never selected from response data.
    pub fn endpoint(&self) -> &EndpointBinding {
        &self.endpoint
    }

    /// Actual accepted response, not a synthesized completion result.
    pub fn response(&self) -> &JobResponse {
        &self.response
    }
}

impl fmt::Debug for CancellationReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationReceipt")
            .field("status", &self.response.status)
            .field("terminal_cancellation_confirmed", &false)
            .finish_non_exhaustive()
    }
}

/// A cancellation failure with dispatch-bound mutation uncertainty.
///
/// Original SDK/service details remain available through `source`. Display and
/// Debug do not echo identifiers, credentials, routes or provider diagnostics.
pub struct CancellationError {
    /// Original typed SDK error, including raw service data when available.
    pub source: JobError,
    /// Explicit run the caller intended to cancel.
    pub target: NotebookRun,
    /// Original client endpoint, never inferred from response data.
    pub endpoint: EndpointBinding,
    /// Actual response received from the executor, even if SDK validation failed.
    ///
    /// None when no complete response was returned, including credential
    /// failure, in-flight cancellation, transport loss or a streaming byte-limit
    /// failure. No response is fabricated from partial transport evidence.
    /// An oversized body is omitted; inspect `response_body_omitted` before
    /// treating the retained response body as complete.
    pub response: Option<JobResponse>,
    /// Whether a received response body exceeded the client's byte bound.
    ///
    /// When true, `response` retains the actual status and headers but has an
    /// empty body. Oversized bytes are neither cloned nor retained as evidence.
    /// False when no complete response was received or the full body was within
    /// the bound; an omitted body must not be interpreted as a complete body.
    pub response_body_omitted: bool,
    /// Whether executor dispatch began; not proof of any remote state change.
    ///
    /// False for local validation, credential failure or cancellation before
    /// dispatch. True once the executor future is polled, including a rejected
    /// response, lost response, invalid Location or in-flight cancellation.
    pub mutation_may_have_occurred: bool,
}

impl fmt::Debug for CancellationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationError")
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .finish_non_exhaustive()
    }
}

impl fmt::Display for CancellationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mutation_may_have_occurred {
            f.write_str("Notebook cancellation request failed; remote mutation may have occurred")
        } else {
            f.write_str("Notebook cancellation request failed before dispatch")
        }
    }
}

impl Error for CancellationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Default)]
struct ResponseEvidence {
    response: Option<JobResponse>,
    body_omitted: bool,
}

struct TrackingExecutor {
    inner: Arc<dyn JobExecutor>,
    dispatched: Arc<AtomicBool>,
    received: Arc<Mutex<ResponseEvidence>>,
    max_response_bytes: NonZeroUsize,
}

impl JobExecutor for TrackingExecutor {
    fn supports_request_headers(&self, headers: &JobRequestHeaders) -> bool {
        self.inner.supports_request_headers(headers)
    }

    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            // Constructing an execution future is not evidence of dispatch.
            self.dispatched.store(true, Ordering::SeqCst);
            let result = self.inner.execute(request).await;
            if let Ok(response) = &result {
                let body_omitted = response.body.len() > self.max_response_bytes.get();
                let retained = JobResponse {
                    status: response.status,
                    body: if body_omitted {
                        Vec::new()
                    } else {
                        response.body.clone()
                    },
                    headers: response.headers.clone(),
                };
                *self
                    .received
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = ResponseEvidence {
                    response: Some(retained),
                    body_omitted,
                };
            }
            result
        })
    }

    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        self.inner.wait(delay)
    }
}

/// Request cancellation of exactly one Notebook run using one POST.
///
/// Reuses [`JobClient::cancel`] without changing its existing policy. A returned
/// Location must identify the exact job instance on the selected endpoint; a
/// generic operation, different run, encoded route, query or fragment is rejected.
/// Only the three GUID segments compare case-insensitively; fixed route segments
/// and the selected endpoint retain their strict validation.
/// A missing Location is accepted because the POST already identifies the run.
/// No Location is followed and no wait or generic reconciliation is performed.
///
/// Local cancellation drops pending credential/HTTP work, not the remote job.
/// An error retains the selected endpoint, byte-bounded response evidence and
/// conservative mutation uncertainty. Oversized bodies are explicitly omitted,
/// not truncated or reported as complete. The caller must not infer terminal
/// cancellation, rollback or permission to replay. Errors are boxed to keep the
/// result size bounded while preserving all structured failure evidence.
pub async fn request(
    client: &JobClient,
    target: &NotebookRun,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CancellationReceipt, Box<CancellationError>> {
    let dispatched = Arc::new(AtomicBool::new(false));
    let received = Arc::new(Mutex::new(ResponseEvidence::default()));
    let scoped = JobClient {
        context: client.context.clone(),
        executor: Arc::new(TrackingExecutor {
            inner: client.executor.clone(),
            dispatched: dispatched.clone(),
            received: received.clone(),
            max_response_bytes: client.limits.max_response_bytes,
        }),
        limits: client.limits,
        headers: client.headers.clone(),
    };
    let result = async {
        let service_target =
            JobTarget::new(target.workspace_id(), target.item_id(), JobType::Notebook)?;
        let handle = scoped
            .cancel(&service_target, target.instance_id(), cancelled)
            .await?;
        for (_, location) in handle
            .accepted
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("location"))
        {
            target.validate_location(scoped.context.endpoint(), location)?;
        }
        Ok(CancellationReceipt {
            target: target.clone(),
            endpoint: scoped.context.endpoint().clone(),
            response: handle.accepted,
        })
    }
    .await;
    result.map_err(|source| {
        let mut evidence = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Box::new(CancellationError {
            source,
            target: target.clone(),
            endpoint: client.context.endpoint().clone(),
            response: evidence.response.take(),
            response_body_omitted: evidence.body_omitted,
            mutation_may_have_occurred: dispatched.load(Ordering::SeqCst),
        })
    })
}
