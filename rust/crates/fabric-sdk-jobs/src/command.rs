// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in source command response policy, separate from typed SDK contracts.

use crate::{
    JobClient, JobError, JobOperationReconciler, JobResponse, JobTarget, PagedValues, PagingLimits,
    RunHandle, TokenPager, cancellable, header, identifier, service_failure,
};
use azure_core::http::{Method, Url};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
};

/// One of the pinned job API routes; no arbitrary URL or method is accepted.
#[derive(Clone, Copy, Debug)]
pub enum JobCommandRoute<'a> {
    /// Submit an instance with an optional execution-data envelope.
    Submit,
    /// Read one instance.
    Status(&'a str),
    /// Request cancellation of one instance.
    Cancel(&'a str),
    /// Create a schedule.
    CreateSchedule,
    /// Read one schedule.
    GetSchedule(&'a str),
    /// Patch one schedule.
    UpdateSchedule(&'a str),
    /// Remove one schedule.
    RemoveSchedule(&'a str),
}

impl JobClient {
    /// Execute a source command request without imposing typed response fields.
    ///
    /// This opt-in path retains bounds, credential injection, one-attempt
    /// mutations and conditional shared-operation reconciliation. Callers must
    /// inspect the actual HTTP status before interpreting JSON or reporting an
    /// effect. Existing typed SDK methods retain their stricter contracts.
    pub async fn response_for_command(
        &self,
        target: &JobTarget,
        route: JobCommandRoute<'_>,
        payload: Option<&Value>,
        synchronous: bool,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobResponse, JobError> {
        let (method, path, query) = match route {
            JobCommandRoute::Submit => (
                Method::Post,
                target.instances_path(),
                Some(target.job_type.as_str()),
            ),
            JobCommandRoute::CreateSchedule => (Method::Post, target.schedules_path(), None),
            JobCommandRoute::Status(id) | JobCommandRoute::Cancel(id) => {
                identifier(id)?;
                let cancel = matches!(route, JobCommandRoute::Cancel(_));
                (
                    if cancel { Method::Post } else { Method::Get },
                    format!(
                        "{}/{id}{}",
                        target.instances_path(),
                        if cancel { "/cancel" } else { "" }
                    ),
                    None,
                )
            }
            JobCommandRoute::GetSchedule(id)
            | JobCommandRoute::UpdateSchedule(id)
            | JobCommandRoute::RemoveSchedule(id) => {
                identifier(id)?;
                let method = match route {
                    JobCommandRoute::GetSchedule(_) => Method::Get,
                    JobCommandRoute::UpdateSchedule(_) => Method::Patch,
                    _ => Method::Delete,
                };
                (method, format!("{}/{id}", target.schedules_path()), None)
            }
        };
        let mut cancelled = pin!(cancelled);
        let response = cancellable(
            cancelled.as_mut(),
            self.request(method, &path, query, payload),
        )
        .await?;
        self.command_reconcile(response, synchronous, operations, cancelled.as_mut())
            .await
    }

    async fn command_reconcile(
        &self,
        response: JobResponse,
        synchronous: bool,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<JobResponse, JobError> {
        if !matches!(response.status, 200 | 201 | 202 | 204) {
            return Err(service_failure(&response));
        }
        let location = header(&response.headers, "location").unwrap_or("");
        if response.status == 202
            && synchronous
            && !location.contains("/jobs/instances/")
            && (!location.is_empty()
                || header(&response.headers, "x-ms-operation-id").is_some_and(|id| !id.is_empty()))
        {
            operations
                .ok_or(JobError::OperationReconcilerRequired)?
                .reconcile_with_cancellation(&response, Box::pin(cancelled))
                .await
        } else {
            Ok(response)
        }
    }

    /// Collect source command rows using the existing bounded token pager.
    ///
    /// Source-false `value` fields become empty rows. No object-row restriction
    /// is imposed here: CLI projection owns that source behavior. Non-200 on
    /// the first request yields no command output, not completed work. Later
    /// page failures still retain partial progress and can never become success.
    pub async fn list_for_command(
        &self,
        target: &JobTarget,
        schedules: bool,
        operations: Option<&dyn JobOperationReconciler>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<PagedValues>, JobError> {
        let path = if schedules {
            target.schedules_path()
        } else {
            target.instances_path()
        };
        let url = Url::parse(&format!("https://api.fabric.microsoft.com{path}"))
            .map_err(|_| JobError::InvalidIdentifier)?;
        let pager = TokenPager::new(
            url,
            PagingLimits {
                max_pages: self.limits.max_pages,
                max_items: self.limits.max_items,
                max_response_bytes: self.limits.max_response_bytes,
            },
        )
        .map_err(|_| JobError::InvalidIdentifier)?;
        let first = AtomicBool::new(true);
        let no_output = AtomicBool::new(false);
        let rows = pager
            .collect(
                |request| {
                    let first_page = first.swap(false, Ordering::Relaxed);
                    let no_output = &no_output;
                    async move {
                        let response = self
                            .request_url(
                                Method::Get,
                                request.url().clone(),
                                None,
                                request.max_response_bytes(),
                            )
                            .await?;
                        let mut response = self
                            .command_reconcile(response, true, operations, pending())
                            .await?;
                        if response.status == 200 {
                            no_output.store(false, Ordering::Relaxed);
                        }
                        if first_page && response.status != 200 {
                            no_output.store(true, Ordering::Relaxed);
                            let has_pages = header(&response.headers, "x-ms-continuation")
                                .is_some_and(|token| !token.is_empty())
                                || serde_json::from_slice::<Value>(&response.body)
                                    .ok()
                                    .and_then(|value| value.get("continuationToken").cloned())
                                    .is_some_and(|token| source_truthy(&token));
                            response.status = 200;
                            if !has_pages {
                                response.body = br#"{"value":[]}"#.to_vec();
                                response.headers.clear();
                            }
                        }
                        if response.status == 200 {
                            let mut body: Value = serde_json::from_slice(&response.body)
                                .map_err(|_| JobError::InvalidResponse)?;
                            if body.get("value").is_some_and(|value| !source_truthy(value)) {
                                body["value"] = json!([]);
                                response.body = serde_json::to_vec(&body)
                                    .map_err(|_| JobError::InvalidResponse)?;
                            }
                        }
                        Ok(response)
                    }
                },
                cancelled,
            )
            .await
            .map_err(|error| JobError::Paging(Box::new(error)))?;
        Ok((!no_output.load(Ordering::Relaxed)).then_some(rows))
    }
}

impl RunHandle {
    /// Bind an actual accepted response to its source instance, without claiming completion.
    ///
    /// A caller-supplied ID is used for cancellation. Submission Locations must
    /// still match the exact Fabric origin and item route; malformed/missing
    /// Locations remain an explicit safety failure rather than an invented ID.
    pub fn from_command_response(
        target: &JobTarget,
        response: JobResponse,
        cancelled_id: Option<&str>,
    ) -> Result<Option<Self>, JobError> {
        if response.status != 202 {
            return Ok(None);
        }
        let id = if let Some(id) = cancelled_id {
            identifier(id)?;
            id.to_owned()
        } else {
            let location =
                header(&response.headers, "location").ok_or(JobError::InvalidResponse)?;
            let prefix = format!(
                "https://api.fabric.microsoft.com{}/",
                target.instances_path()
            );
            let id = location
                .strip_prefix(&prefix)
                .ok_or(JobError::InvalidResponse)?;
            identifier(id).map_err(|_| JobError::InvalidResponse)?;
            id.to_owned()
        };
        Ok(Some(Self {
            target: target.clone(),
            id,
            accepted: response,
        }))
    }
}

fn source_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) if value.is_i64() => value.as_i64() != Some(0),
        Value::Number(value) if value.is_u64() => value.as_u64() != Some(0),
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
