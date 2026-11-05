// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ApplicationView, SparkJobId, WorkloadApplication, WorkloadMonitor, WorkloadView};
use crate::runtime::{NativeError, NativeExecutor, NativeRequest, NativeResponse};
use azure_core::http::Url;
use serde::Deserialize;

/// A plan's semantic coverage, independent of successful HTTP transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanCoverage {
    /// Public application-wide jobs and stages, observed sequentially, not atomic.
    ApplicationSnapshot,
    /// Jobs plus stages only for explicitly selected Spark job IDs.
    SelectedJobs,
    /// Jobs and stages for every running/failed job returned by the bounded query.
    /// A finite provider limit still does not establish unbounded app coverage.
    ObservedJobsSnapshot,
}

/// Bounded, ordered native reads; execution is all-or-error.
///
/// Partial observations are dropped on any failure, never replaced with `[]`.
/// Successful selected-job plans still advertise partial semantic coverage.
pub struct RequestPlan {
    requests: Vec<NativeRequest>,
    coverage: PlanCoverage,
}

impl RequestPlan {
    pub(crate) fn new(
        requests: Vec<NativeRequest>,
        max_requests: usize,
        coverage: PlanCoverage,
    ) -> Result<Self, NativeError> {
        if max_requests == 0 || requests.is_empty() {
            return Err(NativeError::InvalidInput);
        }
        if requests.len() > max_requests {
            return Err(NativeError::Limit);
        }
        Ok(Self { requests, coverage })
    }

    /// Ordered exact requests for the caller's existing execution pipeline.
    pub fn requests(&self) -> &[NativeRequest] {
        &self.requests
    }

    /// Explicit semantic coverage; selected jobs do not imply all stages.
    pub const fn coverage(&self) -> PlanCoverage {
        self.coverage
    }

    /// Execute once per component with per-response and total memory bounds.
    ///
    /// # Errors
    /// Returns the original executor error, non-success HTTP status, or a bound
    /// failure; no successful prefix is returned after a later component fails.
    pub async fn execute(
        &self,
        executor: &dyn NativeExecutor,
        max_response_bytes: usize,
        max_total_bytes: usize,
    ) -> Result<PlanResponses, NativeError> {
        if max_response_bytes == 0 || max_total_bytes == 0 {
            return Err(NativeError::InvalidInput);
        }
        let mut remaining = max_total_bytes;
        let mut responses = Vec::with_capacity(self.requests.len());
        for request in &self.requests {
            if remaining == 0 {
                return Err(NativeError::Limit);
            }
            let response = request
                .execute(executor, max_response_bytes.min(remaining))
                .await?;
            if !(200..300).contains(&response.status) {
                return Err(NativeError::Http(response.status));
            }
            remaining = remaining
                .checked_sub(response.body.len())
                .ok_or(NativeError::Limit)?;
            responses.push(response);
        }
        Ok(PlanResponses {
            responses,
            coverage: self.coverage,
        })
    }
}

/// Successful complete transport sequence, retaining honest semantic coverage.
pub struct PlanResponses {
    /// Ordered bounded raw provider responses; nothing is defaulted or renamed.
    pub responses: Vec<NativeResponse>,
    /// Selection or application-snapshot scope, not a live-parity certification.
    pub coverage: PlanCoverage,
}

/// Dynamic source progress plan with bounded response-driven job IDs.
pub struct WorkloadProgressPlan {
    monitor: WorkloadMonitor,
    app: WorkloadApplication,
    initial: NativeRequest,
    limit: u32,
    max_requests: usize,
}

impl WorkloadProgressPlan {
    pub(crate) fn new(
        monitor: WorkloadMonitor,
        app: WorkloadApplication,
        limit: u32,
        max_requests: usize,
    ) -> Result<Self, NativeError> {
        if max_requests == 0 {
            return Err(NativeError::InvalidInput);
        }
        let initial = monitor.get(WorkloadView::Application {
            app: &app,
            view: ApplicationView::Jobs { limit },
        })?;
        Ok(Self {
            monitor,
            app,
            initial,
            limit,
            max_requests,
        })
    }

    /// The exact initial jobs query, available before any execution.
    pub fn initial(&self) -> &NativeRequest {
        &self.initial
    }

    /// Execute jobs and every required stage read with one finite budget.
    ///
    /// # Errors
    /// Fails on any HTTP/executor error, missing response shape/IDs/status,
    /// duplicate job IDs, truncation indication, or exhausted request/byte bounds.
    /// Previously read components are never returned as partial success.
    pub async fn execute(
        &self,
        executor: &dyn NativeExecutor,
        max_response_bytes: usize,
        max_total_bytes: usize,
    ) -> Result<ProgressResponses, NativeError> {
        if max_response_bytes == 0 || max_total_bytes == 0 {
            return Err(NativeError::InvalidInput);
        }
        let mut remaining = max_total_bytes;
        let jobs =
            execute_bounded(&self.initial, executor, max_response_bytes, &mut remaining).await?;
        let value: serde_json::Value =
            serde_json::from_slice(&jobs.body).map_err(|_| NativeError::InvalidResponse)?;
        let records = collection(&value, "jobs")?;
        // A full limited page may conceal more jobs. Never certify it complete.
        let mut selected = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for job in records {
            let status = job
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or(NativeError::InvalidResponse)?;
            let id = match job.get("jobId") {
                Some(serde_json::Value::String(value)) => value.clone(),
                Some(serde_json::Value::Number(value)) if value.as_u64().is_some() => {
                    value.to_string()
                }
                _ => return Err(NativeError::InvalidResponse),
            };
            if !seen.insert(id.clone()) {
                return Err(NativeError::InvalidResponse);
            }
            let id = SparkJobId::new(id).map_err(|_| NativeError::InvalidResponse)?;
            if matches!(status, "RUNNING" | "FAILED") {
                selected.push(id);
            }
        }
        if selected.len().checked_add(1).ok_or(NativeError::Limit)? > self.max_requests {
            return Err(NativeError::Limit);
        }
        let mut stages = Vec::with_capacity(selected.len());
        for job in selected {
            let request = self.monitor.get(WorkloadView::Application {
                app: &self.app,
                view: ApplicationView::Stages {
                    job: &job,
                    limit: self.limit,
                },
            })?;
            let response =
                execute_bounded(&request, executor, max_response_bytes, &mut remaining).await?;
            let value: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| NativeError::InvalidResponse)?;
            collection(&value, "stages")?;
            stages.push((job, response));
        }
        Ok(ProgressResponses {
            jobs,
            stages,
            coverage: PlanCoverage::ObservedJobsSnapshot,
        })
    }
}

fn collection<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, NativeError> {
    if let Some(object) = value.as_object() {
        for continuation in [
            "continuationToken",
            "continuationUri",
            "nextLink",
            "@odata.nextLink",
        ] {
            if object
                .get(continuation)
                .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
            {
                return Err(NativeError::Limit);
            }
        }
    }
    value
        .as_array()
        .or_else(|| value.get(key).and_then(serde_json::Value::as_array))
        .ok_or(NativeError::InvalidResponse)
}

async fn execute_bounded(
    request: &NativeRequest,
    executor: &dyn NativeExecutor,
    max_response_bytes: usize,
    remaining: &mut usize,
) -> Result<NativeResponse, NativeError> {
    if *remaining == 0 {
        return Err(NativeError::Limit);
    }
    let response = request
        .execute(executor, max_response_bytes.min(*remaining))
        .await?;
    if !(200..300).contains(&response.status) {
        return Err(NativeError::Http(response.status));
    }
    *remaining = remaining
        .checked_sub(response.body.len())
        .ok_or(NativeError::Limit)?;
    Ok(response)
}

/// Complete bounded workload progress observation; no synthetic report fields.
pub struct ProgressResponses {
    /// Actual native jobs response, not an inferred application collection.
    pub jobs: NativeResponse,
    /// Required stage responses paired with their exact observed Spark job IDs.
    pub stages: Vec<(SparkJobId, NativeResponse)>,
    /// Finite observed query coverage, never unbounded app or live parity.
    pub coverage: PlanCoverage,
}

/// Sensitive provider-issued SAS ticket. Debug is always redacted.
///
/// This object never downloads, logs, opens or persists its signed URL.
pub struct SignedEventLogTicket {
    sas_url: String,
}

#[derive(Deserialize)]
struct TicketWire {
    #[serde(rename = "sasUrl")]
    sas_url: String,
}

impl SignedEventLogTicket {
    /// Decode the exact successful provider ticket without exposing diagnostics.
    ///
    /// # Errors
    /// Rejects HTTP failures, malformed JSON, empty/non-HTTPS or credentialed URLs.
    pub fn from_response(response: NativeResponse) -> Result<Self, NativeError> {
        if !(200..300).contains(&response.status) {
            return Err(NativeError::Http(response.status));
        }
        let ticket: TicketWire =
            serde_json::from_slice(&response.body).map_err(|_| NativeError::InvalidResponse)?;
        if ticket.sas_url.chars().any(char::is_control) {
            return Err(NativeError::InvalidResponse);
        }
        let url = Url::parse(&ticket.sas_url).map_err(|_| NativeError::InvalidResponse)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(NativeError::InvalidResponse);
        }
        Ok(Self {
            sas_url: ticket.sas_url,
        })
    }

    /// Explicitly expose the sensitive provider URL to an authorized caller.
    ///
    /// Never include this value in logs, errors or generic request diagnostics.
    pub fn expose_url(&self) -> &str {
        &self.sas_url
    }
}

impl std::fmt::Debug for SignedEventLogTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SignedEventLogTicket(<redacted>)")
    }
}
