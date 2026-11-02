// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    MaintenanceExecutor, MaintenanceOutcome, TableError, TableErrorKind, TableFuture, TablePath,
};
use fabric_sdk_jobs::{
    JobClient, JobError, JobExecutionError, JobOperationReconciler, JobState, JobTarget, JobType,
    WaitOptions,
};
use serde_json::Value;
use std::{future::poll_fn, pin::pin, task::Poll};

/// Source lifecycle events without terminal I/O, logger or ambient state.
#[derive(Clone, Debug, PartialEq)]
pub enum MaintenanceEvent {
    /// Submission returned a validated instance Location.
    Created {
        /// Accepted job ID.
        instance_id: String,
        /// Explicit wait configuration.
        timeout: Option<i64>,
    },
    /// A decoded job state, including Failed/Unknown service rows.
    Status {
        /// Source service status spelling.
        status: String,
        /// Ordered service row for source diagnostics/presentation.
        content: Value,
    },
    /// Python's elapsed timeout; cancellation is a separate subsequent request.
    TimedOut {
        /// Accepted job ID.
        instance_id: String,
        /// Source-accounted elapsed seconds.
        elapsed_seconds: u64,
        /// Explicit caller snapshot of `job_cancel_ontimeout`.
        cancelling: bool,
    },
}

/// TABLE adapter over the released generic jobs SDK, not a second polling engine.
///
/// The caller owns credentials, execution/time, operation reconciliation and
/// configuration. Dropping/cancelling local work never implicitly cancels a
/// remote job. Only an observed timeout with `cancel_on_timeout` submits cancel.
pub struct JobMaintenance<'a> {
    /// Actual credential-injected generic job client.
    pub jobs: &'a JobClient,
    /// Validated before submission; defaults preserve no source timeout.
    pub wait: WaitOptions,
    /// Explicit configuration snapshot, not an ambient/default assumption.
    pub cancel_on_timeout: bool,
    /// Shared generic Fabric LRO for source-selected cancellation responses.
    pub operations: Option<&'a dyn JobOperationReconciler>,
}

impl JobMaintenance<'_> {
    async fn execute(
        &self,
        table: &TablePath,
        data: Value,
        job_type: JobType,
        observe: &mut (dyn FnMut(MaintenanceEvent) -> Result<(), TableError> + Send),
        cancelled: TableFuture<'_, ()>,
    ) -> Result<MaintenanceOutcome, TableError> {
        self.wait
            .validate()
            .map_err(|error| job_error(error, None, false))?;
        let target = JobTarget::new(table.workspace(), table.item(), job_type)
            .map_err(|error| job_error(error, None, false))?;
        let mut cancelled = pin!(cancelled);
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return Err(TableError::new(TableErrorKind::Cancelled));
        }
        let handle = self
            .jobs
            .submit(&target, Some(data), cancelled.as_mut())
            .await
            .map_err(|error| job_error(error, None, true))?;
        let id = handle.id();
        observe(MaintenanceEvent::Created {
            instance_id: id.into(),
            timeout: self.wait.timeout,
        })
        .map_err(|error| evidence(error, id))?;
        let mut observer_error = None;
        let result = self
            .jobs
            .wait(
                &handle,
                self.wait,
                |status| {
                    observe(MaintenanceEvent::Status {
                        status: status.status().into(),
                        content: status.as_value().clone(),
                    })
                    .map_err(|error| {
                        observer_error = Some(error);
                        JobError::Observer
                    })
                },
                cancelled.as_mut(),
            )
            .await;
        if let Some(error) = observer_error {
            return Err(evidence(error, id));
        }
        match result {
            Ok(status) if status.state() == JobState::Completed => {
                Ok(MaintenanceOutcome::Completed {
                    instance_id: id.into(),
                })
            }
            Ok(status) => Ok(MaintenanceOutcome::TerminalWithoutOutput {
                instance_id: id.into(),
                status: status.status().into(),
            }),
            Err(JobError::TimedOut { elapsed_seconds }) => {
                observe(MaintenanceEvent::TimedOut {
                    instance_id: id.into(),
                    elapsed_seconds,
                    cancelling: self.cancel_on_timeout,
                })
                .map_err(|error| evidence(error, id))?;
                if !self.cancel_on_timeout {
                    return Ok(MaintenanceOutcome::TimedOutWithoutCancellation {
                        instance_id: id.into(),
                        elapsed_seconds,
                    });
                }
                let accepted = self
                    .jobs
                    .cancel_for_wait(&target, id, self.operations, cancelled.as_mut())
                    .await
                    .map_err(|error| job_error(error, Some(id), true))?;
                match accepted {
                    Some(_) => Ok(MaintenanceOutcome::CancellationAccepted {
                        instance_id: id.into(),
                    }),
                    None => Ok(MaintenanceOutcome::TerminalWithoutOutput {
                        instance_id: id.into(),
                        status: "CancellationReconciled".into(),
                    }),
                }
            }
            Err(error) => Err(job_error(error, Some(id), true)),
        }
    }
}

impl MaintenanceExecutor for JobMaintenance<'_> {
    fn run<'a>(
        &'a self,
        table: &'a TablePath,
        data: Value,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        Box::pin(async move {
            self.execute(
                table,
                data,
                JobType::TableMaintenance,
                &mut |_| Ok(()),
                cancelled,
            )
            .await
        })
    }
    fn run_observed<'a>(
        &'a self,
        table: &'a TablePath,
        data: Value,
        observe: &'a mut (dyn FnMut(MaintenanceEvent) -> Result<(), TableError> + Send),
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        Box::pin(async move {
            self.execute(table, data, JobType::TableMaintenance, observe, cancelled)
                .await
        })
    }
    fn run_observed_for_job<'a>(
        &'a self,
        table: &'a TablePath,
        data: Value,
        job_type: JobType,
        observe: &'a mut (dyn FnMut(MaintenanceEvent) -> Result<(), TableError> + Send),
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        Box::pin(async move {
            self.execute(table, data, job_type, observe, cancelled)
                .await
        })
    }
}

fn evidence(mut error: TableError, id: &str) -> TableError {
    error.operation_id = Some(id.into());
    error.mutation_may_have_occurred = true;
    error
}

fn job_error(error: JobError, id: Option<&str>, submitted: bool) -> TableError {
    if let JobError::Operation(operation) = error {
        return TableError {
            operation_id: operation
                .operation_id
                .clone()
                .or_else(|| id.map(str::to_owned)),
            mutation_may_have_occurred: operation.mutation_may_have_occurred || submitted,
            kind: TableErrorKind::SharedOperation(Box::new(operation)),
        };
    }
    let kind = match error {
        JobError::Credential => TableErrorKind::Credential,
        JobError::Cancelled | JobError::Execution(JobExecutionError::Cancelled) => {
            TableErrorKind::Cancelled
        }
        JobError::Execution(JobExecutionError::TimedOut) => TableErrorKind::TimedOut,
        JobError::Execution(JobExecutionError::ResponseLimit) => TableErrorKind::Limit,
        JobError::Execution(_) => TableErrorKind::Execution,
        JobError::Http {
            status,
            code,
            service_error,
            ..
        } => TableErrorKind::Http {
            status,
            code: Some(code),
            message: service_error.map(|error| error.message),
        },
        JobError::Failed { content } => TableErrorKind::Maintenance {
            code: "JobFailed".into(),
            message: format!("Job instance '{}' Failed", id.unwrap_or("")),
            content: Some(Box::new(content)),
        },
        JobError::InvalidPollingInterval => TableErrorKind::Maintenance {
            code: "InvalidInput".into(),
            message: "custom polling interval must be positive".into(),
            content: None,
        },
        JobError::PollingExceedsTimeout { timeout, interval } => TableErrorKind::Maintenance {
            code: "InvalidInput".into(),
            message: format!(
                "Custom polling interval ({interval}s) cannot be greater than or equal to timeout ({timeout}s)"
            ),
            content: None,
        },
        JobError::OperationReconcilerRequired => TableErrorKind::Maintenance {
            code: "NotSupported".into(),
            message: "Cancellation requires the shared generic Fabric-operation reconciler".into(),
            content: None,
        },
        JobError::InvalidSleep => TableErrorKind::Maintenance {
            code: "UnexpectedError".into(),
            message: "sleep length must be non-negative".into(),
            content: None,
        },
        JobError::InvalidAudience
        | JobError::InvalidEndpoint
        | JobError::InvalidIdentifier
        | JobError::InvalidRequestHeaders => TableErrorKind::InvalidInput,
        JobError::RequestHeadersUnsupported => TableErrorKind::Maintenance {
            code: "NotSupported".into(),
            message: "The selected job adapter does not support request headers".into(),
            content: None,
        },
        JobError::PollingDelayLimit => TableErrorKind::Limit,
        JobError::Operation(_) => unreachable!("shared operation handled above"),
        JobError::UnexpectedStatus(_) | JobError::InvalidResponse => {
            TableErrorKind::InvalidResponse
        }
        JobError::TimedOut { .. } => TableErrorKind::TimedOut,
        JobError::Observer => TableErrorKind::Execution,
        JobError::Paging(_) => TableErrorKind::InvalidResponse,
    };
    let may_have_mutated = submitted
        && !matches!(
            kind,
            TableErrorKind::Credential | TableErrorKind::InvalidInput
        );
    TableError {
        kind,
        operation_id: id.map(str::to_owned),
        mutation_may_have_occurred: may_have_mutated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_job_endpoint_is_input_failure_without_dispatch_uncertainty() {
        for (submitted, id) in [(false, None), (true, Some("existing-instance"))] {
            let error = job_error(JobError::InvalidEndpoint, id, submitted);
            assert!(matches!(error.kind, TableErrorKind::InvalidInput));
            assert_eq!(error.operation_id.as_deref(), id);
            assert!(!error.mutation_may_have_occurred);
        }
    }
}
