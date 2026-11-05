// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    ApplicationId, AttemptId, ContainerId, LivyId, LogFile, PlanCoverage, RequestPlan,
    SqlExecutionId, StageId, require_prefix, validate_scope,
};
use crate::runtime::{NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeRequest};

/// Explicit public Livy/application ownership and optional native attempt.
#[derive(Clone, Debug)]
pub struct PublicApplication {
    livy: LivyId,
    application: ApplicationId,
    attempt: Option<AttemptId>,
}

impl PublicApplication {
    /// Select only the supplied IDs; no workload-ID conversion or latest lookup.
    pub fn new(livy: LivyId, application: ApplicationId, attempt: Option<AttemptId>) -> Self {
        Self { livy, application, attempt }
    }

    fn path(&self, include_attempt: bool) -> Vec<&str> {
        let mut path = vec!["livySessions", self.livy.as_str(), "applications", self.application.as_str()];
        if include_attempt {
            if let Some(attempt) = &self.attempt {
                path.push(attempt.as_str());
            }
        }
        path
    }

    fn without_attempt(&self) -> Result<Vec<&str>, NativeError> {
        if self.attempt.is_some() {
            return Err(NativeError::UnsupportedCapability);
        }
        Ok(self.path(false))
    }
}

/// All non-composite public monitoring callables at the pinned FNTK revision.
#[derive(Clone, Copy)]
pub enum PublicView<'a> {
    /// Notebook Livy history, not an application collection.
    LivySessions { continuation: Option<&'a str> },
    /// Spark jobs in this public application.
    Jobs(&'a PublicApplication),
    /// All stages or one exact stage; no public job-filter route is evidenced.
    Stages { app: &'a PublicApplication, stage: Option<&'a StageId> },
    /// All Spark executors (`allexecutors`, not container log metadata).
    Executors(&'a PublicApplication),
    /// Observed Spark configuration.
    Environment(&'a PublicApplication),
    /// SQL history, optionally one execution with details.
    Sql { app: &'a PublicApplication, execution: Option<&'a SqlExecutionId>, details: bool },
    /// Spark persisted RDDs.
    StorageRdd(&'a PublicApplication),
    /// Streaming query history.
    Streaming(&'a PublicApplication),
    /// Resource usage; native query fields are explicitly supplied.
    ResourceUsage { app: &'a PublicApplication, query: &'a [(&'a str, String)] },
    /// One resource snapshot, timestamp preserved as an opaque segment.
    ResourceSnapshot { app: &'a PublicApplication, timestamp: &'a str, query: &'a [(&'a str, String)] },
    /// Driver log download response, never a local file.
    DriverLog { app: &'a PublicApplication, file: LogFile },
    /// Driver log metadata.
    DriverLogMetadata { app: &'a PublicApplication, file: LogFile },
    /// Container log; specifying an attempt is unsupported, not silently ignored.
    ExecutorLog { app: &'a PublicApplication, container: &'a ContainerId, file: LogFile },
    /// Container list metadata, not Spark executors.
    ExecutorLogMetadata(&'a PublicApplication),
    /// Livy log uses the source's literal `applications/none`.
    LivyLog(&'a LivyId),
    /// Livy log metadata.
    LivyLogMetadata(&'a LivyId),
    /// Event-log bytes; requires an explicit attempt, no default guessed ID.
    EventLog(&'a PublicApplication),
}

/// Public AAD monitoring builder rooted at the caller's exact `/v1` base.
#[derive(Clone)]
pub struct PublicMonitor {
    endpoint: NativeEndpoint,
    workspace: String,
    notebook: String,
}

impl std::fmt::Debug for PublicMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PublicMonitor(<caller-selected>)")
    }
}

impl PublicMonitor {
    /// Bind an explicit Fabric API base and owner scope without credential access.
    ///
    /// # Errors
    /// Rejects a non-`/v1` profile and unsafe scope IDs.
    pub fn new(endpoint: NativeEndpoint, workspace: &str, notebook: &str) -> Result<Self, NativeError> {
        validate_scope(&[workspace, notebook])?;
        require_prefix(&endpoint, "/v1")?;
        Ok(Self { endpoint, workspace: workspace.into(), notebook: notebook.into() })
    }

    /// Build one source-backed public GET; no MWC fallback is attempted.
    ///
    /// # Errors
    /// Rejects unsafe IDs/query values and unsupported attempt combinations.
    pub fn get(&self, view: PublicView<'_>) -> Result<NativeRequest, NativeError> {
        let mut query = Vec::new();
        let mut accept = "application/json";
        let path = match view {
            PublicView::LivySessions { continuation } => {
                if let Some(token) = continuation {
                    if token.is_empty() { return Err(NativeError::InvalidInput); }
                    query.push(("continuationToken", token.to_owned()));
                }
                vec!["livySessions"]
            }
            PublicView::Jobs(app) => append(app.without_attempt()?, &["jobs"]),
            PublicView::Stages { app, stage } => {
                let mut path = append(app.without_attempt()?, &["stages"]);
                if let Some(stage) = stage { path.push(stage.as_str()); }
                path
            }
            PublicView::Executors(app) => append(app.without_attempt()?, &["allexecutors"]),
            PublicView::Environment(app) => append(app.without_attempt()?, &["environment"]),
            PublicView::Sql { app, execution, details } => {
                if details && execution.is_none() { return Err(NativeError::InvalidInput); }
                let mut path = append(app.without_attempt()?, &["sql"]);
                if let Some(execution) = execution { path.push(execution.as_str()); }
                if details { query.push(("details", "true".into())); }
                path
            }
            PublicView::StorageRdd(app) => append(app.without_attempt()?, &["storage", "rdd"]),
            PublicView::Streaming(app) => append(app.without_attempt()?, &["StreamingQuery"]),
            PublicView::ResourceUsage { app, query: fields } => {
                query.extend(fields.iter().filter(|(_, value)| !value.is_empty()).cloned());
                append(app.path(true), &["resourceUsage"])
            }
            PublicView::ResourceSnapshot { app, timestamp, query: fields } => {
                query.extend(fields.iter().filter(|(_, value)| !value.is_empty()).cloned());
                append(app.path(true), &["resourceUsage", timestamp])
            }
            PublicView::DriverLog { app, file } => {
                query.extend([("type", "driver".into()), ("fileName", file.as_str().into()), ("isDownload", "true".into())]);
                accept = "*/*";
                append(app.path(true), &["logs"])
            }
            PublicView::DriverLogMetadata { app, file } => {
                query.extend([("type", "driver".into()), ("meta", "true".into()), ("fileName", file.as_str().into())]);
                append(app.path(true), &["logs"])
            }
            PublicView::ExecutorLog { app, container, file } => {
                query.extend([("type", "executor".into()), ("containerId", container.as_str().into()), ("fileName", file.as_str().into()), ("isDownload", "true".into())]);
                accept = "*/*";
                append(app.without_attempt()?, &["logs"])
            }
            PublicView::ExecutorLogMetadata(app) => {
                query.extend([("type", "executor".into()), ("meta", "true".into())]);
                append(app.path(true), &["logs"])
            }
            PublicView::LivyLog(livy) | PublicView::LivyLogMetadata(livy) => {
                let metadata = matches!(view, PublicView::LivyLogMetadata(_));
                query.extend([("type", "livy".into()), (if metadata { "meta" } else { "isDownload" }, "true".into())]);
                if !metadata { accept = "*/*"; }
                vec!["livySessions", livy.as_str(), "applications", "none", "logs"]
            }
            PublicView::EventLog(app) => {
                if app.attempt.is_none() { return Err(NativeError::InvalidInput); }
                accept = "application/zip";
                append(app.path(true), &["logs"])
            }
        };
        let mut owner = vec!["workspaces", self.workspace.as_str(), "notebooks", self.notebook.as_str()];
        owner.extend(path);
        self.endpoint.request(NativeMethod::Get, &owner, &query, None, NativeEffect::Read)?.with_accept(accept)
    }

    /// Build both public progress components, all-or-error at execution.
    ///
    /// # Errors
    /// Rejects insufficient request budgets and unsupported attempts.
    pub fn progress(&self, app: &PublicApplication, max_requests: usize) -> Result<RequestPlan, NativeError> {
        RequestPlan::new(vec![self.get(PublicView::Jobs(app))?, self.get(PublicView::Stages { app, stage: None })?], max_requests, PlanCoverage::ApplicationSnapshot)
    }
}

fn append<'a>(mut path: Vec<&'a str>, suffix: &[&'a str]) -> Vec<&'a str> {
    path.extend_from_slice(suffix);
    path
}
