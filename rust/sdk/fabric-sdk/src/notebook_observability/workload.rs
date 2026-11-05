// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    ActivityId, ApplicationId, AttemptId, ContainerId, JobGroupId, JobType, LogFile, LogWindow,
    PlanCoverage, RequestPlan, SparkJobId, WorkloadProgressPlan, positive, require_prefix, validate_scope,
};
use crate::runtime::{NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeRequest};

/// Explicit SparkUI history-server route, never probed or guessed.
#[derive(Clone, Debug)]
pub enum ApplicationLocation {
    /// Running application uses `/hs/applications/{app}`.
    Active,
    /// Completed application requires `/applications/{app}/{attempt}`.
    Completed(AttemptId),
}

/// Workload activity/application scope, distinct from public Livy ownership.
#[derive(Clone, Debug)]
pub struct WorkloadApplication {
    activity: ActivityId,
    application: ApplicationId,
    location: ApplicationLocation,
}

impl WorkloadApplication {
    /// Bind the exact activity, application and caller-selected history route.
    pub fn new(activity: ActivityId, application: ApplicationId, location: ApplicationLocation) -> Self {
        Self { activity, application, location }
    }

    fn path(&self) -> Vec<&str> {
        let mut path = vec!["activities", self.activity.as_str()];
        match &self.location {
            ApplicationLocation::Active => path.extend(["hs", "applications", self.application.as_str()]),
            ApplicationLocation::Completed(attempt) => path.extend(["applications", self.application.as_str(), attempt.as_str()]),
        }
        path
    }

    fn metadata_path(&self) -> Vec<&str> {
        vec!["activities", self.activity.as_str(), "applications", self.application.as_str()]
    }
}

/// Source-supported application views, including callable views absent from CLI.
#[derive(Clone, Copy)]
pub enum ApplicationView<'a> {
    /// Application detail.
    Application,
    /// Fabric filtered jobs.
    Jobs { limit: u32 },
    /// Stages filtered by an exact Spark job, not an app-wide approximation.
    Stages { job: &'a SparkJobId, limit: u32 },
    /// Existing provider performance diagnosis, never an LLM call.
    Diagnostic,
    /// Input/output dataset metadata.
    DataIo,
    /// Spark executors, distinct from YARN containers.
    Executors,
    /// Spark environment.
    Environment,
    /// Workload Spark SQL history uses uppercase `SQL`.
    Sql,
    /// Persisted RDDs.
    StorageRdd,
    /// Streaming query history.
    Streaming,
    /// Fabric job graph.
    Graph,
    /// Resource usage.
    ResourceUsage,
    /// Resource usage overview.
    ResourceOverview,
    /// Event-log bytes, no filesystem download.
    EventLog,
    /// Rolling log metadata; source always uses the unattempted non-HS path.
    RollingDriverMetadata { file: LogFile, page: u64 },
    /// Container metadata; source always uses the unattempted non-HS path.
    ContainerMetadata { container: &'a ContainerId, file: LogFile, page: u64 },
    /// Exact driver log byte window.
    DriverLog { file: LogFile, window: LogWindow },
    /// Driver log metadata.
    DriverLogMetadata { file: LogFile },
    /// Exact YARN container log byte window.
    ExecutorLog { container: &'a ContainerId, file: LogFile, window: LogWindow },
    /// YARN container collection metadata.
    ExecutorLogMetadata,
    /// Per-job-group driver log, not authored cell index.
    CellLog { group: &'a JobGroupId, file: LogFile, window: LogWindow },
    /// Runtime cell-to-job-group mapping.
    ReplMaps,
    /// Existing grouped provider advice.
    Advice,
    /// Existing job-group advice in UX representation.
    AdviceByCell(&'a JobGroupId),
    /// Completed history-server loading progress.
    LoadingProgress,
    /// Completed history-server diagnostic metrics.
    DiagnosticMetrics,
    /// Millisecond point-in-time resource snapshot.
    ResourceSnapshot { timestamp: u64 },
}

/// Explicit caller-owned time window; no ambient clock or default range.
#[derive(Clone, Copy)]
pub struct MetricsWindow<'a> {
    /// Start timestamp in the provider's source representation.
    pub start: &'a str,
    /// End timestamp in the provider's source representation.
    pub end: &'a str,
    /// Positive maximum results.
    pub max_items: u32,
}

impl MetricsWindow<'_> {
    fn query(self) -> Result<Vec<(&'static str, String)>, NativeError> {
        if self.start.is_empty() || self.end.is_empty() {
            return Err(NativeError::InvalidInput);
        }
        Ok(vec![("from", self.start.into()), ("to", self.end.into()), ("maxItems", positive(self.max_items)?)])
    }
}

/// Non-composite Notebook/SparkCore workload reads.
#[derive(Clone, Copy)]
pub enum WorkloadView<'a> {
    /// Explicit active or completed collection, with no automatic fallback.
    Applications { activity: &'a ActivityId, active: bool },
    /// One application-native view.
    Application { app: &'a WorkloadApplication, view: ApplicationView<'a> },
    /// SparkCore jobs, preserving native OData query fields.
    Jobs { skip: u64, top: u32, filter: Option<&'a str> },
    /// SparkCore job detail.
    Job(&'a SparkJobId),
    /// Workload Livy log window.
    LivyLog { activity: &'a ActivityId, window: LogWindow },
    /// Workload Livy log metadata.
    LivyLogMetadata(&'a ActivityId),
    /// Session status; same route as the source's get_livy_session callable.
    SessionStatus(&'a ActivityId),
    /// SparkCore REPLs filtered by exact activity ID.
    ReplStatus(&'a ActivityId),
    /// Activity summary.
    ActivitySummary(&'a ActivityId),
    /// Spark metrics info list with all source query options.
    Metrics { window: MetricsWindow<'a>, activity: Option<&'a ActivityId>, running: bool, trigger_type: Option<&'a str> },
    /// Anomaly info list.
    Anomalies { window: MetricsWindow<'a>, offset: u64 },
    /// One activity's metrics.
    ActivityMetrics(&'a ActivityId),
    /// Native workload Livy-session collection, not public history.
    LivySessions,
    /// History-server configuration.
    HistoryServerProperties,
}

/// Explicit dynamic Notebook and optional SparkCore MWC endpoint bindings.
#[derive(Clone)]
pub struct WorkloadMonitor {
    notebook: NativeEndpoint,
    sparkcore: Option<NativeEndpoint>,
    workspace: String,
    artifact: String,
}

impl std::fmt::Debug for WorkloadMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkloadMonitor(<caller-selected>)")
    }
}

impl WorkloadMonitor {
    /// Bind exact caller-selected workload prefixes.
    ///
    /// Notebook base must end `/webapi/capacities/{capacity}/workloads/Notebook`;
    /// SparkCore, when supplied, must end the corresponding `/SparkCore` prefix.
    /// No Notebook-host or credential fallback supplies missing SparkCore.
    ///
    /// # Errors
    /// Rejects unsafe IDs and mismatched capability prefixes.
    pub fn new(notebook: NativeEndpoint, sparkcore: Option<NativeEndpoint>, capacity: &str, workspace: &str, artifact: &str) -> Result<Self, NativeError> {
        validate_scope(&[capacity, workspace, artifact])?;
        require_prefix(&notebook, &format!("/webapi/capacities/{capacity}/workloads/Notebook"))?;
        if let Some(endpoint) = &sparkcore {
            require_prefix(endpoint, &format!("/webapi/capacities/{capacity}/workloads/SparkCore"))?;
        }
        Ok(Self { notebook, sparkcore, workspace: workspace.into(), artifact: artifact.into() })
    }

    fn notebook_request(&self, prefix: &[&str], suffix: &[&str], query: &[(&str, String)], method: NativeMethod, effect: NativeEffect) -> Result<NativeRequest, NativeError> {
        let mut path = prefix.to_vec();
        path.extend_from_slice(suffix);
        self.notebook.request(method, &path, query, None, effect)?.with_header("x-ms-workload-resource-moniker", &self.artifact)
    }

    fn sparkui(&self, suffix: &[&str], query: &[(&str, String)], versioned: bool, method: NativeMethod, effect: NativeEffect) -> Result<NativeRequest, NativeError> {
        let mut prefix = vec!["Data", "Automatic", "sparkui", "api"];
        if versioned { prefix.extend(["versions", "1"]); } else { prefix.push("v1"); }
        self.notebook_request(&prefix, suffix, query, method, effect)
    }

    fn core(&self, suffix: &[&str], query: &[(&str, String)]) -> Result<NativeRequest, NativeError> {
        let endpoint = self.sparkcore.as_ref().ok_or(NativeError::UnsupportedCapability)?;
        let mut path = vec!["SparkCoreService", "automatic", "v1", "monitoring", "workspaces", self.workspace.as_str(), "artifacts", self.artifact.as_str()];
        path.extend_from_slice(suffix);
        endpoint.request(NativeMethod::Get, &path, query, None, NativeEffect::Read)?.with_header("x-ms-workload-resource-moniker", &self.artifact)
    }

    /// Build one native workload observation; positive routes are source-backed.
    ///
    /// # Errors
    /// Rejects unsupported endpoint profiles and invalid IDs/query bounds.
    pub fn get(&self, view: WorkloadView<'_>) -> Result<NativeRequest, NativeError> {
        let mut query = Vec::new();
        let mut path = vec!["artifacts", self.artifact.as_str()];
        let mut versioned = false;
        match view {
            WorkloadView::Application { app, view } => return self.application(app, view),
            WorkloadView::Jobs { skip, top, filter } => {
                if skip != 0 { query.push(("$skip", skip.to_string())); }
                if top != 0 { query.push(("$top", top.to_string())); }
                if let Some(filter) = filter {
                    if filter.is_empty() { return Err(NativeError::InvalidInput); }
                    query.push(("$filter", filter.into()));
                }
                return self.core(&["jobs"], &query);
            }
            WorkloadView::Job(job) => return self.core(&["jobs", job.as_str()], &[]),
            WorkloadView::ReplStatus(activity) => {
                // OData string literals escape single quotes, independently of URL encoding.
                query.push(("$filter", format!("sessionId eq '{}'", activity.as_str().replace('\'', "''"))));
                return self.core(&["repls"], &query);
            }
            WorkloadView::Applications { activity, active } => {
                path.extend(["activities", activity.as_str()]);
                if active { path.push("hs"); }
                path.push("applications");
            }
            WorkloadView::LivyLog { activity, window } => {
                path.extend(["activities", activity.as_str(), "livylog"]);
                query = window.query()?;
            }
            WorkloadView::LivyLogMetadata(activity) => path.extend(["activities", activity.as_str(), "livylog", "metadata"]),
            WorkloadView::SessionStatus(activity) => {
                path.extend(["sessions", activity.as_str()]);
                versioned = true;
            }
            WorkloadView::ActivitySummary(activity) => path.extend(["activities", activity.as_str(), "summary"]),
            WorkloadView::LivySessions => {
                path.push("sessions");
                versioned = true;
            }
            WorkloadView::HistoryServerProperties => path = vec!["historyServerProperties"],
            WorkloadView::Metrics { window, activity, running, trigger_type } => {
                query.extend([("startTime", window.start.into()), ("endTime", window.end.into())]);
                query.extend(window.query()?);
                query.extend([("src", "fntk".into()), ("artifactType", "SynapseNotebook".into())]);
                if let Some(activity) = activity { query.push(("activityId", activity.as_str().into())); }
                if running { query.push(("isRunning", "true".into())); }
                if let Some(trigger) = trigger_type {
                    if trigger.is_empty() { return Err(NativeError::InvalidInput); }
                    query.push(("triggerType", trigger.into()));
                }
                path.push("infolist");
                return self.notebook_request(&["Data", "Automatic", "sparkmetrics", "api", "v1"], &path, &query, NativeMethod::Get, NativeEffect::Read);
            }
            WorkloadView::Anomalies { window, offset } => {
                query = window.query()?;
                query.push(("offset", offset.to_string()));
                path.push("anomalyinfolist");
                return self.notebook_request(&["Data", "Automatic", "sparkmetrics", "api", "v1"], &path, &query, NativeMethod::Get, NativeEffect::Read);
            }
            WorkloadView::ActivityMetrics(activity) => {
                path.extend(["activities", activity.as_str(), "info"]);
                return self.notebook_request(&["Data", "Automatic", "sparkmetrics", "api", "v1"], &path, &[], NativeMethod::Get, NativeEffect::Read);
            }
        }
        self.sparkui(&path, &query, versioned, NativeMethod::Get, NativeEffect::Read)
    }

    fn application(&self, app: &WorkloadApplication, view: ApplicationView<'_>) -> Result<NativeRequest, NativeError> {
        let mut path = vec!["artifacts", self.artifact.as_str()];
        path.extend(match view {
            ApplicationView::RollingDriverMetadata { .. } | ApplicationView::ContainerMetadata { .. } => app.metadata_path(),
            _ => app.path(),
        });
        let mut query = Vec::new();
        let mut accept = "application/json";
        match view {
            ApplicationView::Application => {}
            ApplicationView::Jobs { limit } => {
                path.extend(["filter", "jobs"]);
                query.push(("limit", positive(limit)?));
            }
            ApplicationView::Stages { job, limit } => {
                path.extend(["filter", "stages"]);
                query.extend([("job", job.as_str().into()), ("limit", positive(limit)?)]);
            }
            ApplicationView::Diagnostic => path.extend(["enhancement", "diagnostic"]),
            ApplicationView::DataIo => path.push("datalist"),
            ApplicationView::Executors => path.push("executors"),
            ApplicationView::Environment => path.push("environment"),
            ApplicationView::Sql => path.push("SQL"),
            ApplicationView::StorageRdd => path.extend(["storage", "rdd"]),
            ApplicationView::Streaming => path.push("StreamingQuery"),
            ApplicationView::Graph => path.extend(["enhancement", "graph"]),
            ApplicationView::ResourceUsage => path.push("resourceUsage"),
            ApplicationView::ResourceOverview => path.extend(["resourceUsage", "overview"]),
            ApplicationView::EventLog => { path.push("logs"); accept = "application/zip"; }
            ApplicationView::RollingDriverMetadata { file, page } => {
                path.extend(["rollingdriverlog", "metadata", file.as_str()]);
                query.push(("offset", page.checked_mul(3000).ok_or(NativeError::Limit)?.to_string()));
            }
            ApplicationView::ContainerMetadata { container, file, page } => {
                path.extend(["executorlogs", "containers", container.as_str(), "metadata"]);
                query.extend([("filenamePrefix", file.as_str().into()), ("offset", page.checked_mul(3000).ok_or(NativeError::Limit)?.to_string())]);
            }
            ApplicationView::DriverLog { file, window } => {
                path.extend(["driverlog", file.as_str()]);
                query = window.query()?;
            }
            ApplicationView::DriverLogMetadata { file } => path.extend(["driverlog", "metadata", file.as_str()]),
            ApplicationView::ExecutorLog { container, file, window } => {
                path.extend(["yarnaggregationlog", "containers", container.as_str(), "files", file.as_str()]);
                query = window.query()?;
            }
            ApplicationView::ExecutorLogMetadata => path.extend(["yarnaggregationlog", "metadata"]),
            ApplicationView::CellLog { group, file, window } => {
                path.extend([group.as_str(), "driverlog", file.as_str()]);
                query = window.query()?;
            }
            ApplicationView::ReplMaps => path.push("replmaps"),
            ApplicationView::Advice => path.push("advise"),
            ApplicationView::AdviceByCell(group) => {
                path.extend(["advise", "jobGroups", group.as_str()]);
                query.push(("outputMode", "UX".into()));
            }
            ApplicationView::LoadingProgress | ApplicationView::DiagnosticMetrics => {
                if !matches!(app.location, ApplicationLocation::Completed(_)) { return Err(NativeError::UnsupportedCapability); }
                path.push(if matches!(view, ApplicationView::LoadingProgress) { "loadingProgress" } else { "diagnosticmetrics" });
            }
            ApplicationView::ResourceSnapshot { timestamp } => {
                let timestamp = timestamp.to_string();
                path.push("resourceUsage");
                path.push(&timestamp);
                query.extend([("jobLimit", "10".into()), ("executorLimit", "10".into())]);
                return self.sparkui(&path, &query, false, NativeMethod::Get, NativeEffect::Read);
            }
        }
        self.sparkui(&path, &query, false, NativeMethod::Get, NativeEffect::Read)?.with_accept(accept)
    }

    /// Build a native cancellation, disclosing its remote cancel effect.
    ///
    /// # Errors
    /// Fails before dispatch for invalid scoped route segments.
    pub fn cancel(&self, activity: &ActivityId, job_type: JobType) -> Result<NativeRequest, NativeError> {
        self.sparkui(&["artifacts", &self.artifact, job_type.as_str(), activity.as_str()], &[], true, NativeMethod::Delete, NativeEffect::Cancel)
    }

    /// Request a signed event-log ticket: provider POST, semantically a read.
    ///
    /// No ticket is followed or logged. Decode using `SignedEventLogTicket`.
    ///
    /// # Errors
    /// Fails before dispatch for invalid scoped route segments.
    pub fn event_log_ticket(&self, activity: &ActivityId, job_type: JobType) -> Result<NativeRequest, NativeError> {
        self.notebook_request(&["sparkdiagnostic", "api", "v1"], &["artifacts", &self.artifact, "activities", activity.as_str(), "GetSasTokenForEventLogs"], &[("jobType", job_type.as_str().into())], NativeMethod::Post, NativeEffect::Read)
    }

    /// Build progress reads for an explicitly selected set of Spark jobs.
    ///
    /// Coverage is `SelectedJobs`, never falsely advertised as all app stages.
    /// Callers may select running/failed jobs from the separately observed jobs
    /// response; no ID is guessed and no failed stage read becomes `[]`.
    ///
    /// # Errors
    /// Rejects insufficient request budgets, duplicate IDs and invalid bounds.
    pub fn progress_selected(&self, app: &WorkloadApplication, jobs: &[SparkJobId], limit: u32, max_requests: usize) -> Result<RequestPlan, NativeError> {
        if jobs.len().checked_add(1).ok_or(NativeError::Limit)? > max_requests { return Err(NativeError::Limit); }
        let mut seen = std::collections::BTreeSet::new();
        let mut requests = vec![self.application(app, ApplicationView::Jobs { limit })?];
        for job in jobs {
            if !seen.insert(job.as_str()) { return Err(NativeError::InvalidInput); }
            requests.push(self.application(app, ApplicationView::Stages { job, limit })?);
        }
        RequestPlan::new(requests, max_requests, PlanCoverage::SelectedJobs)
    }

    /// Plan source progress: jobs, then stages for every observed running/failed job.
    ///
    /// IDs come only from the actual jobs response, never from another ID kind
    /// or cache. Stage failures fail the entire observation rather than returning
    /// empty-success substitutions. This is bounded and sequential, not atomic.
    ///
    /// # Errors
    /// Rejects zero bounds and invalid scoped requests before dispatch.
    pub fn progress(&self, app: &WorkloadApplication, limit: u32, max_requests: usize) -> Result<WorkloadProgressPlan, NativeError> {
        WorkloadProgressPlan::new(self.clone(), app.clone(), limit, max_requests)
    }
}
