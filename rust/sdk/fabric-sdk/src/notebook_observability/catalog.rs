// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::runtime::NativeMethod;

/// Source class family, not a credential fallback preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceFamily {
    /// `PublicMonitorClient`, public AAD API.
    Public,
    /// `MonitorClient`, Notebook/SparkCore MWC APIs.
    Workload,
    /// `ResourceClient`, Notebook resource filesystem.
    Resource,
}

/// One source callable mapped to a real typed native builder or bounded plan.
pub struct OperationCatalogEntry {
    /// Pinned Python source class family.
    pub family: SourceFamily,
    /// Exact source callable name.
    pub source_member: &'static str,
    /// Rust typed view or facade method.
    pub binding: &'static str,
    /// Actual native verb; `None` denotes a composite request plan.
    pub method: Option<NativeMethod>,
    /// Overlapping registered source leaves; not new Rust CLI registrations.
    pub source_leaves: &'static [&'static str],
}

macro_rules! op {
    ($family:ident, $source:literal, $binding:literal, $method:ident $(, $leaf:literal)*) => {
        OperationCatalogEntry {
            family: SourceFamily::$family,
            source_member: $source,
            binding: $binding,
            method: Some(NativeMethod::$method),
            source_leaves: &[$($leaf),*],
        }
    };
}

/// All 68 non-constructor public callables in the three assigned source clients.
///
/// Pinned FNTK revision: `e250d5d32b04efcc5623a414a361727a93c271ce`.
/// Counts are 44 workload, 18 public, six resource, with overlapping routes and
/// two composite plans. These are not all 130 repository-wide client slots.
pub const SOURCE_OPERATIONS: &[OperationCatalogEntry] = &[
    op!(Workload, "list_jobs", "WorkloadView::Jobs", Get, "monitor list-jobs"),
    op!(Workload, "get_job", "WorkloadView::Job", Get),
    op!(Workload, "cancel_job", "WorkloadMonitor::cancel", Delete),
    op!(Workload, "list_applications", "WorkloadView::Applications", Get, "monitor list-apps"),
    op!(Workload, "get_application", "ApplicationView::Application", Get),
    op!(Workload, "get_app_jobs", "ApplicationView::Jobs", Get),
    op!(Workload, "get_app_stages", "ApplicationView::Stages", Get, "monitor get-stages"),
    op!(Workload, "get_diagnostic", "ApplicationView::Diagnostic", Get, "monitor get-diagnostic"),
    op!(Workload, "get_data_io", "ApplicationView::DataIo", Get, "monitor get-data-io"),
    op!(Workload, "get_executors", "ApplicationView::Executors", Get, "monitor get-executors"),
    op!(Workload, "get_environment", "ApplicationView::Environment", Get, "monitor get-environment"),
    op!(Workload, "get_sql_queries", "ApplicationView::Sql", Get, "monitor get-sql"),
    op!(Workload, "get_storage_rdd", "ApplicationView::StorageRdd", Get),
    op!(Workload, "get_streaming", "ApplicationView::Streaming", Get),
    op!(Workload, "get_job_graph", "ApplicationView::Graph", Get, "monitor get-graph"),
    op!(Workload, "get_resource_usage", "ApplicationView::ResourceUsage", Get, "monitor get-resources"),
    op!(Workload, "get_resource_overview", "ApplicationView::ResourceOverview", Get),
    op!(Workload, "get_event_log", "ApplicationView::EventLog", Get),
    op!(Workload, "get_rolling_driver_log_metadata", "ApplicationView::RollingDriverMetadata", Get),
    op!(Workload, "get_executor_container_metadata", "ApplicationView::ContainerMetadata", Get),
    op!(Workload, "get_livy_log", "WorkloadView::LivyLog", Get, "logs get-livy"),
    op!(Workload, "get_livy_log_metadata", "WorkloadView::LivyLogMetadata", Get),
    op!(Workload, "get_driver_log", "ApplicationView::DriverLog", Get, "logs get-driver"),
    op!(Workload, "get_driver_log_metadata", "ApplicationView::DriverLogMetadata", Get),
    op!(Workload, "get_executor_log", "ApplicationView::ExecutorLog", Get, "logs get-executor"),
    op!(Workload, "get_executor_log_metadata", "ApplicationView::ExecutorLogMetadata", Get, "logs list-executors"),
    op!(Workload, "get_cell_log", "ApplicationView::CellLog", Get, "logs get-cell"),
    op!(Workload, "get_session_status", "WorkloadView::SessionStatus", Get, "monitor get-session-status"),
    op!(Workload, "get_repl_status", "WorkloadView::ReplStatus", Get),
    op!(Workload, "get_replmaps", "ApplicationView::ReplMaps", Get, "monitor get-replmaps"),
    op!(Workload, "get_activity_summary", "WorkloadView::ActivitySummary", Get, "monitor get-activity-summary"),
    op!(Workload, "get_metrics_list", "WorkloadView::Metrics", Get, "monitor list-metrics"),
    op!(Workload, "get_anomalies", "WorkloadView::Anomalies", Get, "monitor get-anomalies"),
    op!(Workload, "get_activity_metrics", "WorkloadView::ActivityMetrics", Get, "monitor get-activity-metrics"),
    op!(Workload, "list_livy_sessions", "WorkloadView::LivySessions", Get),
    op!(Workload, "get_livy_session", "WorkloadView::SessionStatus", Get),
    op!(Workload, "get_advice", "ApplicationView::Advice", Get, "monitor get-advice"),
    op!(Workload, "get_advice_by_cell", "ApplicationView::AdviceByCell", Get, "monitor get-advice-cell"),
    OperationCatalogEntry { family: SourceFamily::Workload, source_member: "get_progress", binding: "WorkloadMonitor::progress", method: None, source_leaves: &["monitor get-progress"] },
    op!(Workload, "get_event_log_sas_token", "WorkloadMonitor::event_log_ticket", Post),
    op!(Workload, "get_loading_progress", "ApplicationView::LoadingProgress", Get),
    op!(Workload, "get_diagnostic_metrics", "ApplicationView::DiagnosticMetrics", Get),
    op!(Workload, "get_resource_snapshot", "ApplicationView::ResourceSnapshot", Get),
    op!(Workload, "get_history_server_properties", "WorkloadView::HistoryServerProperties", Get),
    op!(Public, "list_livy_sessions", "PublicView::LivySessions", Get),
    op!(Public, "get_jobs", "PublicView::Jobs", Get),
    op!(Public, "get_stages", "PublicView::Stages", Get, "monitor get-stages"),
    op!(Public, "get_executors", "PublicView::Executors", Get, "monitor get-executors"),
    op!(Public, "get_environment", "PublicView::Environment", Get, "monitor get-environment"),
    op!(Public, "get_sql", "PublicView::Sql", Get, "monitor get-sql"),
    op!(Public, "get_storage_rdd", "PublicView::StorageRdd", Get),
    op!(Public, "get_streaming", "PublicView::Streaming", Get),
    op!(Public, "get_resource_usage", "PublicView::ResourceUsage", Get, "monitor get-resources"),
    op!(Public, "get_resource_snapshot", "PublicView::ResourceSnapshot", Get),
    op!(Public, "get_driver_log", "PublicView::DriverLog", Get, "logs get-driver"),
    op!(Public, "get_driver_log_metadata", "PublicView::DriverLogMetadata", Get),
    op!(Public, "get_executor_log", "PublicView::ExecutorLog", Get, "logs get-executor"),
    op!(Public, "get_executor_log_metadata", "PublicView::ExecutorLogMetadata", Get, "logs list-executors"),
    op!(Public, "get_livy_log", "PublicView::LivyLog", Get, "logs get-livy"),
    op!(Public, "get_livy_log_metadata", "PublicView::LivyLogMetadata", Get),
    op!(Public, "get_event_log", "PublicView::EventLog", Get),
    OperationCatalogEntry { family: SourceFamily::Public, source_member: "get_progress", binding: "PublicMonitor::progress", method: None, source_leaves: &["monitor get-progress"] },
    op!(Resource, "list_files", "Resources::list", Get, "resource list"),
    op!(Resource, "get_file", "Resources::get", Get, "resource get"),
    op!(Resource, "put_file", "Resources::update", Put, "resource put"),
    op!(Resource, "mkdir", "Resources::update_folder", Put, "resource mkdir"),
    op!(Resource, "delete", "Resources::delete", Delete, "resource delete"),
    op!(Resource, "get_usage", "Resources::usage", Get, "resource usage"),
];
