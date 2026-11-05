// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline wire fixtures independently transcribed from pinned FNTK source.
#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_sdk::{
    notebook_observability::*,
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
        NativeResponse,
    },
};
use std::{
    collections::{BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const NB: &str = "https://notebook.test/webapi/capacities/cap/workloads/Notebook";
const SC: &str = "https://sparkcore.test/webapi/capacities/cap/workloads/SparkCore";
const FS: &str = "https://notebook.test/webapi/capacities/cap/workloads/Notebook/Data/Direct/api/workspaces/ws/artifacts/art/filesystem";

fn endpoint(value: &str) -> NativeEndpoint {
    NativeEndpoint::new(Url::parse(value).unwrap()).unwrap()
}

fn public() -> PublicMonitor {
    PublicMonitor::new(endpoint("https://public.test/v1"), "ws", "nb").unwrap()
}

fn workload() -> WorkloadMonitor {
    WorkloadMonitor::new(endpoint(NB), Some(endpoint(SC)), "cap", "ws", "art").unwrap()
}

fn resources() -> Resources {
    Resources::new(endpoint(FS), "cap", "ws", "art").unwrap()
}

fn public_app(attempt: bool) -> PublicApplication {
    PublicApplication::new(
        LivyId::new("livy").unwrap(),
        ApplicationId::new("app").unwrap(),
        attempt.then(|| AttemptId::new("attempt-7").unwrap()),
    )
}

fn workload_app(completed: bool) -> WorkloadApplication {
    WorkloadApplication::new(
        ActivityId::new("activity").unwrap(),
        ApplicationId::new("app").unwrap(),
        if completed {
            ApplicationLocation::Completed(AttemptId::new("attempt-7").unwrap())
        } else {
            ApplicationLocation::Active
        },
    )
}

fn query(request: &NativeRequest) -> Vec<(String, String)> {
    request
        .url()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn read(
    request: &NativeRequest,
    expected_url: &str,
    expected_query: &[(&str, &str)],
    workload: bool,
) {
    assert_eq!(request.method(), NativeMethod::Get);
    assert_eq!(request.effect(), NativeEffect::Read);
    let mut url = request.url().clone();
    url.set_query(None);
    assert_eq!(url.as_str(), expected_url);
    assert_eq!(
        query(request),
        expected_query
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<(String, String)>>()
    );
    assert!(request.body().is_none());
    assert!(request.bytes().is_none());
    assert_eq!(request.content_type(), "application/json");
    if workload {
        assert_eq!(
            request
                .headers()
                .get("x-ms-workload-resource-moniker")
                .map(String::as_str),
            Some("art")
        );
        assert_eq!(request.headers().len(), 1);
    } else {
        assert!(request.headers().is_empty());
    }
}

#[test]
fn all_public_callable_routes_queries_and_media_are_exact() {
    let client = public();
    let app = public_app(false);
    let attempted = public_app(true);
    let livy = LivyId::new("livy").unwrap();
    let stage = StageId::new("0").unwrap();
    let sql = SqlExecutionId::new("sql-0").unwrap();
    let container = ContainerId::new("container-9").unwrap();
    let extra = [
        ("startTime", "2026-11-05T00:00:00Z".into()),
        ("ignoredEmptyValue", String::new()),
    ];
    let cases: Vec<(PublicView<'_>, &str, Vec<(&str, &str)>, &str)> = vec![
        (
            PublicView::Jobs(&app),
            "/livySessions/livy/applications/app/jobs",
            vec![],
            "application/json",
        ),
        (
            PublicView::Stages {
                app: &app,
                stage: None,
            },
            "/livySessions/livy/applications/app/stages",
            vec![],
            "application/json",
        ),
        (
            PublicView::Stages {
                app: &app,
                stage: Some(&stage),
            },
            "/livySessions/livy/applications/app/stages/0",
            vec![],
            "application/json",
        ),
        (
            PublicView::Executors(&app),
            "/livySessions/livy/applications/app/allexecutors",
            vec![],
            "application/json",
        ),
        (
            PublicView::Environment(&app),
            "/livySessions/livy/applications/app/environment",
            vec![],
            "application/json",
        ),
        (
            PublicView::Sql {
                app: &app,
                execution: None,
                details: false,
            },
            "/livySessions/livy/applications/app/sql",
            vec![],
            "application/json",
        ),
        (
            PublicView::Sql {
                app: &app,
                execution: Some(&sql),
                details: true,
            },
            "/livySessions/livy/applications/app/sql/sql-0",
            vec![("details", "true")],
            "application/json",
        ),
        (
            PublicView::StorageRdd(&app),
            "/livySessions/livy/applications/app/storage/rdd",
            vec![],
            "application/json",
        ),
        (
            PublicView::Streaming(&app),
            "/livySessions/livy/applications/app/StreamingQuery",
            vec![],
            "application/json",
        ),
        (
            PublicView::ResourceUsage {
                app: &attempted,
                query: &extra,
            },
            "/livySessions/livy/applications/app/attempt-7/resourceUsage",
            vec![("startTime", "2026-11-05T00:00:00Z")],
            "application/json",
        ),
        (
            PublicView::ResourceSnapshot {
                app: &app,
                timestamp: "1730000000000",
                query: &[],
            },
            "/livySessions/livy/applications/app/resourceUsage/1730000000000",
            vec![],
            "application/json",
        ),
        (
            PublicView::DriverLog {
                app: &attempted,
                file: LogFile::Stderr,
            },
            "/livySessions/livy/applications/app/attempt-7/logs",
            vec![
                ("type", "driver"),
                ("fileName", "stderr"),
                ("isDownload", "true"),
            ],
            "*/*",
        ),
        (
            PublicView::DriverLogMetadata {
                app: &app,
                file: LogFile::Stdout,
            },
            "/livySessions/livy/applications/app/logs",
            vec![("type", "driver"), ("meta", "true"), ("fileName", "stdout")],
            "application/json",
        ),
        (
            PublicView::ExecutorLog {
                app: &app,
                container: &container,
                file: LogFile::Stderr,
            },
            "/livySessions/livy/applications/app/logs",
            vec![
                ("type", "executor"),
                ("containerId", "container-9"),
                ("fileName", "stderr"),
                ("isDownload", "true"),
            ],
            "*/*",
        ),
        (
            PublicView::ExecutorLogMetadata(&attempted),
            "/livySessions/livy/applications/app/attempt-7/logs",
            vec![("type", "executor"), ("meta", "true")],
            "application/json",
        ),
        (
            PublicView::LivyLog(&livy),
            "/livySessions/livy/applications/none/logs",
            vec![("type", "livy"), ("isDownload", "true")],
            "*/*",
        ),
        (
            PublicView::LivyLogMetadata(&livy),
            "/livySessions/livy/applications/none/logs",
            vec![("type", "livy"), ("meta", "true")],
            "application/json",
        ),
        (
            PublicView::EventLog(&attempted),
            "/livySessions/livy/applications/app/attempt-7/logs",
            vec![],
            "application/zip",
        ),
    ];
    assert_eq!(cases.len(), 18);
    for (view, suffix, fields, media) in cases {
        let request = client.get(view).unwrap();
        read(
            &request,
            &format!("https://public.test/v1/workspaces/ws/notebooks/nb{suffix}"),
            &fields,
            false,
        );
        assert_eq!(request.accept(), media);
    }
    let request = client
        .get(PublicView::LivySessions {
            continuation: Some("opaque+/=&other=wrong"),
        })
        .unwrap();
    read(
        &request,
        "https://public.test/v1/workspaces/ws/notebooks/nb/livySessions",
        &[("continuationToken", "opaque+/=&other=wrong")],
        false,
    );
    assert!(
        request
            .url()
            .as_str()
            .contains("opaque%2B%2F%3D%26other%3Dwrong")
    );
}

#[test]
fn all_workload_application_callable_routes_preserve_state_and_attempt() {
    let client = workload();
    let job = SparkJobId::new("job-0").unwrap();
    let group = JobGroupId::new("group-9").unwrap();
    let container = ContainerId::new("container-9").unwrap();
    let window = LogWindow {
        offset: 7,
        size: 31,
    };
    let cases: Vec<(ApplicationView<'_>, &str, Vec<(&str, &str)>)> = vec![
        (ApplicationView::Application, "", vec![]),
        (
            ApplicationView::Jobs { limit: 17 },
            "/filter/jobs",
            vec![("limit", "17")],
        ),
        (
            ApplicationView::Stages {
                job: &job,
                limit: 18,
            },
            "/filter/stages",
            vec![("job", "job-0"), ("limit", "18")],
        ),
        (
            ApplicationView::Diagnostic,
            "/enhancement/diagnostic",
            vec![],
        ),
        (ApplicationView::DataIo, "/datalist", vec![]),
        (ApplicationView::Executors, "/executors", vec![]),
        (ApplicationView::Environment, "/environment", vec![]),
        (ApplicationView::Sql, "/SQL", vec![]),
        (ApplicationView::StorageRdd, "/storage/rdd", vec![]),
        (ApplicationView::Streaming, "/StreamingQuery", vec![]),
        (ApplicationView::Graph, "/enhancement/graph", vec![]),
        (ApplicationView::ResourceUsage, "/resourceUsage", vec![]),
        (
            ApplicationView::ResourceOverview,
            "/resourceUsage/overview",
            vec![],
        ),
        (ApplicationView::EventLog, "/logs", vec![]),
        (
            ApplicationView::DriverLog {
                file: LogFile::Stderr,
                window,
            },
            "/driverlog/stderr",
            vec![("offset", "7"), ("size", "31")],
        ),
        (
            ApplicationView::DriverLogMetadata {
                file: LogFile::Stdout,
            },
            "/driverlog/metadata/stdout",
            vec![],
        ),
        (
            ApplicationView::ExecutorLog {
                container: &container,
                file: LogFile::Stdout,
                window,
            },
            "/yarnaggregationlog/containers/container-9/files/stdout",
            vec![("offset", "7"), ("size", "31")],
        ),
        (
            ApplicationView::ExecutorLogMetadata,
            "/yarnaggregationlog/metadata",
            vec![],
        ),
        (
            ApplicationView::CellLog {
                group: &group,
                file: LogFile::Stderr,
                window,
            },
            "/group-9/driverlog/stderr",
            vec![("offset", "7"), ("size", "31")],
        ),
        (ApplicationView::ReplMaps, "/replmaps", vec![]),
        (ApplicationView::Advice, "/advise", vec![]),
        (
            ApplicationView::AdviceByCell(&group),
            "/advise/jobGroups/group-9",
            vec![("outputMode", "UX")],
        ),
        (
            ApplicationView::ResourceSnapshot {
                timestamp: 1730000000000,
            },
            "/resourceUsage/1730000000000",
            vec![("jobLimit", "10"), ("executorLimit", "10")],
        ),
    ];
    assert_eq!(cases.len(), 23);
    for completed in [false, true] {
        let app = workload_app(completed);
        let base = if completed {
            "activities/activity/applications/app/attempt-7"
        } else {
            "activities/activity/hs/applications/app"
        };
        for (view, suffix, fields) in &cases {
            let request = client
                .get(WorkloadView::Application {
                    app: &app,
                    view: *view,
                })
                .unwrap();
            read(
                &request,
                &format!("{NB}/Data/Automatic/sparkui/api/v1/artifacts/art/{base}{suffix}"),
                fields,
                true,
            );
            assert_eq!(
                request.accept(),
                if matches!(view, ApplicationView::EventLog) {
                    "application/zip"
                } else {
                    "application/json"
                }
            );
        }
    }
}

#[test]
fn completed_only_and_state_independent_metadata_routes_are_exact() {
    let client = workload();
    let active = workload_app(false);
    let completed = workload_app(true);
    let container = ContainerId::new("container-9").unwrap();
    for (view, suffix) in [
        (ApplicationView::LoadingProgress, "loadingProgress"),
        (ApplicationView::DiagnosticMetrics, "diagnosticmetrics"),
    ] {
        assert_eq!(
            client
                .get(WorkloadView::Application { app: &active, view })
                .unwrap_err(),
            NativeError::UnsupportedCapability
        );
        let request = client
            .get(WorkloadView::Application {
                app: &completed,
                view,
            })
            .unwrap();
        read(
            &request,
            &format!(
                "{NB}/Data/Automatic/sparkui/api/v1/artifacts/art/activities/activity/applications/app/attempt-7/{suffix}"
            ),
            &[],
            true,
        );
    }
    for app in [&active, &completed] {
        let request = client
            .get(WorkloadView::Application {
                app,
                view: ApplicationView::RollingDriverMetadata {
                    file: LogFile::Stdout,
                    page: 2,
                },
            })
            .unwrap();
        read(
            &request,
            &format!(
                "{NB}/Data/Automatic/sparkui/api/v1/artifacts/art/activities/activity/applications/app/rollingdriverlog/metadata/stdout"
            ),
            &[("offset", "6000")],
            true,
        );
        let request = client
            .get(WorkloadView::Application {
                app,
                view: ApplicationView::ContainerMetadata {
                    container: &container,
                    file: LogFile::Stderr,
                    page: 3,
                },
            })
            .unwrap();
        read(
            &request,
            &format!(
                "{NB}/Data/Automatic/sparkui/api/v1/artifacts/art/activities/activity/applications/app/executorlogs/containers/container-9/metadata"
            ),
            &[("filenamePrefix", "stderr"), ("offset", "9000")],
            true,
        );
    }
}

#[test]
fn workload_activity_sessions_history_and_core_routes_are_exact() {
    let client = workload();
    let activity = ActivityId::new("activity").unwrap();
    let job = SparkJobId::new("job-0").unwrap();
    let cases = [
        (
            WorkloadView::Applications {
                activity: &activity,
                active: true,
            },
            "v1/artifacts/art/activities/activity/hs/applications",
            vec![],
        ),
        (
            WorkloadView::Applications {
                activity: &activity,
                active: false,
            },
            "v1/artifacts/art/activities/activity/applications",
            vec![],
        ),
        (
            WorkloadView::LivyLog {
                activity: &activity,
                window: LogWindow {
                    offset: 8,
                    size: 99,
                },
            },
            "v1/artifacts/art/activities/activity/livylog",
            vec![("offset", "8"), ("size", "99")],
        ),
        (
            WorkloadView::LivyLogMetadata(&activity),
            "v1/artifacts/art/activities/activity/livylog/metadata",
            vec![],
        ),
        (
            WorkloadView::SessionStatus(&activity),
            "versions/1/artifacts/art/sessions/activity",
            vec![],
        ),
        (
            WorkloadView::ActivitySummary(&activity),
            "v1/artifacts/art/activities/activity/summary",
            vec![],
        ),
        (
            WorkloadView::LivySessions,
            "versions/1/artifacts/art/sessions",
            vec![],
        ),
        (
            WorkloadView::HistoryServerProperties,
            "v1/historyServerProperties",
            vec![],
        ),
    ];
    for (view, suffix, fields) in cases {
        read(
            &client.get(view).unwrap(),
            &format!("{NB}/Data/Automatic/sparkui/api/{suffix}"),
            &fields,
            true,
        );
    }
    let base = format!("{SC}/SparkCoreService/automatic/v1/monitoring/workspaces/ws/artifacts/art");
    read(
        &client
            .get(WorkloadView::Jobs {
                skip: 9,
                top: 17,
                filter: Some("state eq 'Failed'"),
            })
            .unwrap(),
        &format!("{base}/jobs"),
        &[
            ("$skip", "9"),
            ("$top", "17"),
            ("$filter", "state eq 'Failed'"),
        ],
        true,
    );
    read(
        &client
            .get(WorkloadView::Jobs {
                skip: 0,
                top: 0,
                filter: None,
            })
            .unwrap(),
        &format!("{base}/jobs"),
        &[],
        true,
    );
    read(
        &client.get(WorkloadView::Job(&job)).unwrap(),
        &format!("{base}/jobs/job-0"),
        &[],
        true,
    );
    read(
        &client.get(WorkloadView::ReplStatus(&activity)).unwrap(),
        &format!("{base}/repls"),
        &[("$filter", "sessionId eq 'activity'")],
        true,
    );
    let quoted = ActivityId::new("a'b").unwrap();
    assert_eq!(
        query(&client.get(WorkloadView::ReplStatus(&quoted)).unwrap()),
        vec![("$filter".to_owned(), "sessionId eq 'a''b'".to_owned())]
    );
}

#[test]
fn metrics_and_anomalies_preserve_every_native_query_field() {
    let client = workload();
    let activity = ActivityId::new("activity").unwrap();
    let window = MetricsWindow {
        start: "2026-10-01T00:00:00Z",
        end: "2026-11-01T00:00:00Z",
        max_items: 13,
    };
    let request = client
        .get(WorkloadView::Metrics {
            window,
            activity: Some(&activity),
            running: true,
            trigger_type: Some("pipeline"),
        })
        .unwrap();
    read(
        &request,
        &format!("{NB}/Data/Automatic/sparkmetrics/api/v1/artifacts/art/infolist"),
        &[
            ("startTime", window.start),
            ("endTime", window.end),
            ("from", window.start),
            ("to", window.end),
            ("maxItems", "13"),
            ("src", "fntk"),
            ("artifactType", "SynapseNotebook"),
            ("activityId", "activity"),
            ("isRunning", "true"),
            ("triggerType", "pipeline"),
        ],
        true,
    );
    let request = client
        .get(WorkloadView::Anomalies { window, offset: 19 })
        .unwrap();
    read(
        &request,
        &format!("{NB}/Data/Automatic/sparkmetrics/api/v1/artifacts/art/anomalyinfolist"),
        &[
            ("from", window.start),
            ("to", window.end),
            ("maxItems", "13"),
            ("offset", "19"),
        ],
        true,
    );
    read(
        &client
            .get(WorkloadView::ActivityMetrics(&activity))
            .unwrap(),
        &format!("{NB}/Data/Automatic/sparkmetrics/api/v1/artifacts/art/activities/activity/info"),
        &[],
        true,
    );
}

#[test]
fn provider_post_read_and_delete_cancellation_disclose_effects() {
    let client = workload();
    let activity = ActivityId::new("activity").unwrap();
    for (job_type, segment) in [
        (JobType::Sessions, "sessions"),
        (JobType::Batches, "batches"),
    ] {
        let request = client.cancel(&activity, job_type).unwrap();
        assert_eq!(
            request.url().as_str(),
            format!("{NB}/Data/Automatic/sparkui/api/versions/1/artifacts/art/{segment}/activity")
        );
        assert_eq!(request.method(), NativeMethod::Delete);
        assert_eq!(request.effect(), NativeEffect::Cancel);
        assert!(request.body().is_none());
        let request = client.event_log_ticket(&activity, job_type).unwrap();
        assert_eq!(
            request.url().path(),
            "/webapi/capacities/cap/workloads/Notebook/sparkdiagnostic/api/v1/artifacts/art/activities/activity/GetSasTokenForEventLogs"
        );
        assert_eq!(request.method(), NativeMethod::Post);
        assert_eq!(request.effect(), NativeEffect::Read);
        assert_eq!(
            query(&request),
            vec![("jobType".to_owned(), segment.to_owned())]
        );
        assert!(request.body().is_none());
        assert!(request.bytes().is_none());
        assert_eq!(
            request
                .headers()
                .get("x-ms-workload-resource-moniker")
                .unwrap(),
            "art"
        );
    }
}

#[test]
fn six_filesystem_callables_preserve_route_headers_media_and_bytes() {
    let client = resources();
    let path = ResourcePath::new("builtin/space name.txt").unwrap();
    let list = client.list(&path, true).unwrap();
    assert_eq!(
        list.url().as_str(),
        format!("{FS}/workdir/builtin/space%20name.txt?recursive=true")
    );
    assert_eq!(list.headers().get("cache-control").unwrap(), "no-cache");
    assert_eq!(list.headers().len(), 1);
    assert_eq!(list.method(), NativeMethod::Get);
    assert_eq!(list.effect(), NativeEffect::Read);
    let root = client.list(&ResourcePath::new("").unwrap(), false).unwrap();
    assert_eq!(root.url().as_str(), format!("{FS}/workdir?recursive=false"));
    let get = client.get(&path).unwrap();
    assert_eq!(
        get.url().as_str(),
        format!("{FS}/workdir/builtin/space%20name.txt")
    );
    assert_eq!(get.accept(), "*/*");
    assert!(get.headers().is_empty());
    let bytes = vec![0, 255, 1, 0];
    let put = client
        .update(
            &path,
            bytes.clone(),
            "image/png",
            4,
            ResourceWriteAdmission::Upsert,
        )
        .unwrap();
    assert_eq!(put.method(), NativeMethod::Put);
    assert_eq!(put.effect(), NativeEffect::Update);
    assert_eq!(put.bytes(), Some(bytes.as_slice()));
    assert_eq!(put.content_type(), "image/png");
    assert!(put.body().is_none());
    assert_eq!(
        put.headers().get("ms-filesystem-entry-type").unwrap(),
        "file"
    );
    assert_eq!(put.headers().len(), 1);
    assert!(!put.headers().contains_key("if-none-match"));
    let text = client
        .update(
            &path,
            "hé".as_bytes().to_vec(),
            "text/plain; charset=utf-8",
            3,
            ResourceWriteAdmission::Upsert,
        )
        .unwrap();
    assert_eq!(text.bytes(), Some("hé".as_bytes()));
    assert_eq!(text.content_type(), "text/plain; charset=utf-8");
    let folder = client
        .update_folder(&path, ResourceWriteAdmission::Upsert)
        .unwrap();
    assert_eq!(folder.method(), NativeMethod::Put);
    assert_eq!(folder.effect(), NativeEffect::Update);
    assert_eq!(
        folder.headers().get("ms-filesystem-entry-type").unwrap(),
        "folder"
    );
    assert_eq!(folder.content_type(), "application/json");
    assert!(folder.body().is_none());
    assert!(folder.bytes().is_none());
    for (entry, recursive, fields) in [
        (EntryType::File, false, vec![]),
        (EntryType::Folder, false, vec![]),
        (EntryType::Folder, true, vec![("recursive", "true")]),
    ] {
        let delete = client.delete(&path, entry, recursive).unwrap();
        assert_eq!(delete.method(), NativeMethod::Delete);
        assert_eq!(delete.effect(), NativeEffect::Delete);
        assert_eq!(
            query(&delete),
            fields
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect::<Vec<(String, String)>>()
        );
        assert_eq!(
            delete.headers().get("ms-filesystem-entry-type").unwrap(),
            if entry == EntryType::File {
                "file"
            } else {
                "folder"
            }
        );
        assert!(delete.body().is_none());
    }
    read(
        &client.usage().unwrap(),
        &format!("{FS}/workdirUsage"),
        &[],
        false,
    );
}

#[test]
fn no_clobber_is_not_invented_from_a_source_docstring() {
    let client = resources();
    let path = ResourcePath::new("builtin/already-exists").unwrap();
    assert_eq!(
        client
            .update(
                &path,
                vec![1],
                "application/octet-stream",
                1,
                ResourceWriteAdmission::CreateOnly
            )
            .unwrap_err(),
        NativeError::UnsupportedCapability
    );
    assert_eq!(
        client
            .update_folder(&path, ResourceWriteAdmission::CreateOnly)
            .unwrap_err(),
        NativeError::UnsupportedCapability
    );
    assert!(
        !client
            .update(
                &path,
                vec![1],
                "application/octet-stream",
                1,
                ResourceWriteAdmission::Upsert
            )
            .unwrap()
            .headers()
            .contains_key("if-none-match")
    );
    assert_eq!(
        client
            .update(
                &path,
                vec![1, 2],
                "application/octet-stream",
                1,
                ResourceWriteAdmission::Upsert
            )
            .unwrap_err(),
        NativeError::Limit
    );
    assert!(
        client
            .update(
                &path,
                vec![1],
                "text/plain\r\nx-injected: yes",
                1,
                ResourceWriteAdmission::Upsert
            )
            .is_err()
    );
}

#[test]
fn wrong_profiles_missing_core_and_attempt_combinations_fail_before_dispatch() {
    for base in [
        "https://public.test",
        "https://public.test/v1/extra",
        "https://public.test/v2",
        NB,
        SC,
        FS,
    ] {
        assert!(PublicMonitor::new(endpoint(base), "ws", "nb").is_err());
    }
    assert!(WorkloadMonitor::new(endpoint(SC), None, "cap", "ws", "art").is_err());
    assert!(WorkloadMonitor::new(endpoint(NB), Some(endpoint(NB)), "cap", "ws", "art").is_err());
    assert!(WorkloadMonitor::new(endpoint(NB), None, "wrong-capacity", "ws", "art").is_err());
    assert!(Resources::new(endpoint(FS), "cap", "wrong-workspace", "art").is_err());
    assert!(Resources::new(endpoint(NB), "cap", "ws", "art").is_err());
    let without_core = WorkloadMonitor::new(endpoint(NB), None, "cap", "ws", "art").unwrap();
    assert_eq!(
        without_core
            .get(WorkloadView::Jobs {
                skip: 0,
                top: 50,
                filter: None
            })
            .unwrap_err(),
        NativeError::UnsupportedCapability
    );
    let app = public_app(true);
    let container = ContainerId::new("container").unwrap();
    assert_eq!(
        public()
            .get(PublicView::ExecutorLog {
                app: &app,
                container: &container,
                file: LogFile::Stderr
            })
            .unwrap_err(),
        NativeError::UnsupportedCapability
    );
    assert_eq!(
        public().get(PublicView::Jobs(&app)).unwrap_err(),
        NativeError::UnsupportedCapability
    );
    assert!(
        public()
            .get(PublicView::EventLog(&public_app(false)))
            .is_err()
    );
    assert!(
        public()
            .get(PublicView::Sql {
                app: &public_app(false),
                execution: None,
                details: true
            })
            .is_err()
    );
}

#[test]
fn unsafe_paths_ids_and_zero_overflow_bounds_are_rejected() {
    for value in [
        "", ".", "..", "a/b", "a\\b", "a%2fb", "a?b", "a#b", "a\0b", "a\r\nb",
    ] {
        assert!(LivyId::new(value).is_err());
        assert!(ActivityId::new(value).is_err());
        assert!(ApplicationId::new(value).is_err());
        assert!(AttemptId::new(value).is_err());
        assert!(SparkJobId::new(value).is_err());
        assert!(JobGroupId::new(value).is_err());
        assert!(ContainerId::new(value).is_err());
        assert!(StageId::new(value).is_err());
        assert!(SqlExecutionId::new(value).is_err());
    }
    for path in [
        "/absolute",
        "a/",
        "a//b",
        "./a",
        "a/../b",
        "C:/file",
        "a\\b",
        "a%2fb",
        "a?b",
        "a#b",
        "a\0b",
        "https://bad",
    ] {
        assert!(ResourcePath::new(path).is_err(), "{path:?}");
    }
    let root = ResourcePath::new("").unwrap();
    assert!(resources().get(&root).is_err());
    assert!(resources().delete(&root, EntryType::Folder, true).is_err());
    assert!(
        resources()
            .update_folder(&root, ResourceWriteAdmission::Upsert)
            .is_err()
    );
    assert!(
        resources()
            .delete(&ResourcePath::new("file").unwrap(), EntryType::File, true)
            .is_err()
    );
    let client = workload();
    let app = workload_app(false);
    let activity = ActivityId::new("activity").unwrap();
    assert!(
        client
            .get(WorkloadView::Application {
                app: &app,
                view: ApplicationView::Jobs { limit: 0 }
            })
            .is_err()
    );
    assert!(
        client
            .get(WorkloadView::Application {
                app: &app,
                view: ApplicationView::RollingDriverMetadata {
                    file: LogFile::Stdout,
                    page: u64::MAX
                }
            })
            .is_err()
    );
    assert!(
        client
            .get(WorkloadView::LivyLog {
                activity: &activity,
                window: LogWindow { offset: 0, size: 0 }
            })
            .is_err()
    );
    assert!(
        client
            .get(WorkloadView::Metrics {
                window: MetricsWindow {
                    start: "",
                    end: "end",
                    max_items: 10
                },
                activity: None,
                running: false,
                trigger_type: None
            })
            .is_err()
    );
    assert!(
        public()
            .get(PublicView::ResourceSnapshot {
                app: &public_app(false),
                timestamp: "../escape",
                query: &[]
            })
            .is_err()
    );
    assert!(
        public()
            .get(PublicView::LivySessions {
                continuation: Some("bad\r\nvalue")
            })
            .is_err()
    );
}

#[test]
fn media_decoding_is_memory_only_exact_and_redacted() {
    for media in [
        "text/plain",
        "application/json",
        "application/xml",
        "Application/Problem+JSON",
    ] {
        let response = response(200, "hé".as_bytes(), Some(media));
        match ResourceContent::from_response(response).unwrap() {
            ResourceContent::Text(text) => assert_eq!(text, "hé"),
            ResourceContent::Binary(_) => panic!("expected text"),
        }
    }
    for media in [Some("image/png"), None] {
        match ResourceContent::from_response(response(200, &[0, 255, 0], media)).unwrap() {
            ResourceContent::Binary(bytes) => assert_eq!(bytes, vec![0, 255, 0]),
            ResourceContent::Text(_) => panic!("expected bytes"),
        }
    }
    assert_eq!(
        ResourceContent::from_response(response(404, b"private", Some("text/plain"))).unwrap_err(),
        NativeError::Http(404)
    );
    assert_eq!(
        ResourceContent::from_response(response(200, &[255], Some("text/plain"))).unwrap_err(),
        NativeError::InvalidResponse
    );
    assert!(!format!("{:?}", ResourceContent::Text("private".into())).contains("private"));
}

fn response(status: u16, body: &[u8], media: Option<&str>) -> NativeResponse {
    NativeResponse {
        status,
        body: body.to_vec(),
        content_type: media.map(str::to_owned),
        headers: Default::default(),
    }
}

#[test]
fn signed_tickets_never_format_the_secret_or_follow_a_link() {
    let ticket = SignedEventLogTicket::from_response(response(
        200,
        br#"{"sasUrl":"https://storage.test/events?sig=private-secret"}"#,
        Some("application/json"),
    ))
    .unwrap();
    assert_eq!(
        ticket.expose_url(),
        "https://storage.test/events?sig=private-secret"
    );
    assert_eq!(format!("{ticket:?}"), "SignedEventLogTicket(<redacted>)");
    for body in [
        br#"{"sasUrl":"http://storage.test/a?sig=secret"}"#.as_slice(),
        br#"{"sasUrl":"https://user:password@storage.test/a"}"#.as_slice(),
        br#"{"sasUrl":""}"#.as_slice(),
        br#"{"sasUrl":"https://storage.test/a#fragment"}"#.as_slice(),
        br#"{"sasUrl":"https://storage.test/a?sig=secret\ninjected"}"#.as_slice(),
        br#"{}"#.as_slice(),
    ] {
        assert!(
            SignedEventLogTicket::from_response(response(200, body, Some("application/json")))
                .is_err()
        );
    }
}

struct FakeExecutor {
    results: Mutex<VecDeque<Result<NativeResponse, NativeError>>>,
    calls: AtomicUsize,
}

impl FakeExecutor {
    fn new(results: Vec<Result<NativeResponse, NativeError>>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

impl NativeExecutor for FakeExecutor {
    fn execute<'a>(
        &'a self,
        _: &'a NativeRequest,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = self
            .results
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected dispatch");
        Box::pin(async move { result })
    }
}

#[tokio::test]
async fn progress_plans_are_bounded_and_do_not_return_partial_success() {
    let app = public_app(false);
    assert!(public().progress(&app, 1).is_err());
    let plan = public().progress(&app, 2).unwrap();
    assert_eq!(plan.requests().len(), 2);
    assert_eq!(plan.coverage(), PlanCoverage::ApplicationSnapshot);
    assert!(plan.requests()[0].url().path().ends_with("/jobs"));
    assert!(plan.requests()[1].url().path().ends_with("/stages"));
    let executor = FakeExecutor::new(vec![
        Ok(response(200, b"[]", None)),
        Err(NativeError::TimedOut),
    ]);
    assert!(matches!(
        plan.execute(&executor, 20, 40).await,
        Err(NativeError::TimedOut)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    let executor = FakeExecutor::new(vec![
        Ok(response(200, b"[]", None)),
        Ok(response(403, b"no", None)),
    ]);
    assert!(matches!(
        plan.execute(&executor, 20, 40).await,
        Err(NativeError::Http(403))
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    let executor = FakeExecutor::new(vec![
        Ok(response(200, b"[]", None)),
        Ok(response(200, b"[]", None)),
    ]);
    let result = plan.execute(&executor, 20, 40).await.unwrap();
    assert_eq!(result.responses.len(), 2);
    assert_eq!(result.coverage, PlanCoverage::ApplicationSnapshot);
}

#[tokio::test]
async fn response_bounds_and_failures_stop_without_retry_or_empty_substitution() {
    let plan = public().progress(&public_app(false), 2).unwrap();
    let executor = FakeExecutor::new(vec![]);
    assert!(matches!(
        plan.execute(&executor, 0, 20).await,
        Err(NativeError::InvalidInput)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    let executor = FakeExecutor::new(vec![Ok(response(200, b"oversized", None))]);
    assert!(matches!(
        plan.execute(&executor, 2, 20).await,
        Err(NativeError::Limit)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    let executor = FakeExecutor::new(vec![Ok(response(200, b"[]", None))]);
    assert!(matches!(
        plan.execute(&executor, 20, 2).await,
        Err(NativeError::Limit)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    let executor = FakeExecutor::new(vec![Err(NativeError::Cancelled)]);
    assert!(matches!(
        plan.execute(&executor, 20, 40).await,
        Err(NativeError::Cancelled)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn selected_job_progress_honestly_exposes_partial_coverage() {
    let client = workload();
    let app = workload_app(true);
    let jobs = [
        SparkJobId::new("job-0").unwrap(),
        SparkJobId::new("job-3").unwrap(),
    ];
    assert!(client.progress_selected(&app, &jobs, 100, 2).is_err());
    let plan = client.progress_selected(&app, &jobs, 100, 3).unwrap();
    assert_eq!(plan.coverage(), PlanCoverage::SelectedJobs);
    assert_eq!(plan.requests().len(), 3);
    assert_eq!(
        query(&plan.requests()[1]),
        vec![
            ("job".to_owned(), "job-0".to_owned()),
            ("limit".to_owned(), "100".to_owned())
        ]
    );
    assert_eq!(
        query(&plan.requests()[2]),
        vec![
            ("job".to_owned(), "job-3".to_owned()),
            ("limit".to_owned(), "100".to_owned())
        ]
    );
    assert!(
        client
            .progress_selected(&app, &[jobs[0].clone(), jobs[0].clone()], 100, 3)
            .is_err()
    );
    assert_eq!(
        client
            .progress_selected(&app, &[], 100, 1)
            .unwrap()
            .coverage(),
        PlanCoverage::SelectedJobs
    );
}

#[tokio::test]
async fn workload_progress_uses_only_response_ids_and_requires_every_stage_component() {
    let client = workload();
    let plan = client.progress(&workload_app(false), 100, 3).unwrap();
    assert!(plan.initial().url().path().ends_with("/filter/jobs"));
    let jobs = br#"{"jobs":[{"jobId":0,"status":"RUNNING"},{"jobId":"failed-job","status":"FAILED"},{"jobId":2,"status":"SUCCEEDED"}]}"#;
    let executor = FakeExecutor::new(vec![
        Ok(response(200, jobs, None)),
        Ok(response(200, b"[]", None)),
        Ok(response(200, br#"{"stages":[]}"#, None)),
    ]);
    let observed = plan.execute(&executor, 1024, 4096).await.unwrap();
    assert_eq!(observed.coverage, PlanCoverage::ObservedJobsSnapshot);
    assert_eq!(observed.stages.len(), 2);
    assert_eq!(observed.stages[0].0.as_str(), "0");
    assert_eq!(observed.stages[1].0.as_str(), "failed-job");
    assert_eq!(executor.calls.load(Ordering::SeqCst), 3);
    let executor = FakeExecutor::new(vec![
        Ok(response(200, jobs, None)),
        Ok(response(200, b"[]", None)),
        Err(NativeError::TimedOut),
    ]);
    assert!(matches!(
        plan.execute(&executor, 1024, 4096).await,
        Err(NativeError::TimedOut)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn malformed_or_truncated_progress_is_not_an_empty_success() {
    let client = workload();
    let plan = client.progress(&workload_app(false), 100, 1).unwrap();
    for jobs in [
        br#"{}"#.as_slice(),
        br#"{"jobs":null}"#.as_slice(),
        br#"{"jobs":[{"jobId":0}]}"#.as_slice(),
        br#"{"jobs":[{"jobId":"../wrong","status":"RUNNING"}]}"#.as_slice(),
        br#"{"jobs":[],"continuationToken":"next"}"#.as_slice(),
        br#"{"jobs":[{"jobId":0,"status":"RUNNING"}]}"#.as_slice(),
    ] {
        let executor = FakeExecutor::new(vec![Ok(response(200, jobs, None))]);
        assert!(plan.execute(&executor, 1024, 4096).await.is_err());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }
    let executor = FakeExecutor::new(vec![Ok(response(200, b"[]", None))]);
    let observed = plan.execute(&executor, 1024, 4096).await.unwrap();
    assert!(observed.stages.is_empty());
    assert_eq!(observed.coverage, PlanCoverage::ObservedJobsSnapshot);
    let plan = client.progress(&workload_app(false), 100, 2).unwrap();
    let executor = FakeExecutor::new(vec![
        Ok(response(200, br#"[{"jobId":0,"status":"FAILED"}]"#, None)),
        Ok(response(200, b"{}", None)),
    ]);
    assert!(matches!(
        plan.execute(&executor, 1024, 4096).await,
        Err(NativeError::InvalidResponse)
    ));
}

#[test]
fn full_catalog_covers_all_source_callables_and_thirty_registered_leaves() {
    assert_eq!(SOURCE_OPERATIONS.len(), 68);
    for (family, expected) in [
        (SourceFamily::Workload, 44),
        (SourceFamily::Public, 18),
        (SourceFamily::Resource, 6),
    ] {
        let entries = SOURCE_OPERATIONS
            .iter()
            .filter(|entry| entry.family == family)
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), expected);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.source_member)
                .collect::<BTreeSet<_>>()
                .len(),
            expected
        );
        assert!(entries.iter().all(|entry| !entry.binding.is_empty()));
    }
    assert_eq!(
        SOURCE_OPERATIONS
            .iter()
            .filter(|entry| entry.method.is_none())
            .count(),
        2
    );
    let leaves = SOURCE_OPERATIONS
        .iter()
        .flat_map(|entry| entry.source_leaves.iter().copied())
        .collect::<BTreeSet<_>>();
    let expected = [
        "monitor list-apps",
        "monitor get-diagnostic",
        "monitor get-stages",
        "monitor get-progress",
        "monitor get-executors",
        "monitor get-environment",
        "monitor get-sql",
        "monitor get-resources",
        "monitor get-advice",
        "monitor get-advice-cell",
        "monitor get-data-io",
        "monitor get-graph",
        "monitor get-replmaps",
        "monitor get-session-status",
        "monitor get-activity-summary",
        "monitor list-jobs",
        "monitor list-metrics",
        "monitor get-anomalies",
        "monitor get-activity-metrics",
        "logs get-driver",
        "logs get-executor",
        "logs list-executors",
        "logs get-livy",
        "logs get-cell",
        "resource list",
        "resource get",
        "resource put",
        "resource mkdir",
        "resource delete",
        "resource usage",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(leaves, expected);
}

#[tokio::test]
async fn workload_progress_rejects_non_object_collection_entries() {
    let client = workload();
    let plan = client.progress(&workload_app(false), 100, 2).unwrap();
    let jobs = br#"{"jobs":[{"jobId":0,"status":"RUNNING"}]}"#;
    for malformed in ["null", "true", "0", "1.5", r#""stage""#, "[]", "[{}]"] {
        for entries in [
            format!("[{malformed}]"),
            format!(r#"[{{"stageId":1}},{malformed}]"#),
        ] {
            for wrapped in [false, true] {
                let stages = if wrapped {
                    format!(r#"{{"stages":{entries}}}"#)
                } else {
                    entries.clone()
                };
                let executor = FakeExecutor::new(vec![
                    Ok(response(200, jobs, None)),
                    Ok(response(200, stages.as_bytes(), None)),
                ]);
                assert!(matches!(
                    plan.execute(&executor, 1024, 4096).await,
                    Err(NativeError::InvalidResponse)
                ));
                assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
            }
        }
        for entries in [
            format!("[{malformed}]"),
            format!(r#"[{{"jobId":0,"status":"RUNNING"}},{malformed}]"#),
        ] {
            for wrapped in [false, true] {
                let jobs = if wrapped {
                    format!(r#"{{"jobs":{entries}}}"#)
                } else {
                    entries.clone()
                };
                let executor = FakeExecutor::new(vec![Ok(response(200, jobs.as_bytes(), None))]);
                assert!(matches!(
                    plan.execute(&executor, 1024, 4096).await,
                    Err(NativeError::InvalidResponse)
                ));
                assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
    let plan = client.progress(&workload_app(false), 100, 3).unwrap();
    let jobs = br#"[{"jobId":0,"status":"RUNNING"},{"jobId":1,"status":"FAILED"}]"#;
    let executor = FakeExecutor::new(vec![
        Ok(response(200, jobs, None)),
        Ok(response(200, br#"{"stages":[{"stageId":0}]}"#, None)),
        Ok(response(200, br#"{"stages":[null]}"#, None)),
    ]);
    assert!(matches!(
        plan.execute(&executor, 1024, 4096).await,
        Err(NativeError::InvalidResponse)
    ));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn workload_progress_preserves_unknown_collection_fields() {
    let client = workload();
    let plan = client.progress(&workload_app(false), 100, 2).unwrap();
    let job = r#"{"jobId":0,"status":"RUNNING","unknownJobField":{"nested":[null,42]}}"#;
    let stage = r#"{"stageId":1,"unknownStageField":{"nested":[false,null]}}"#;
    for wrapped_jobs in [false, true] {
        for wrapped_stages in [false, true] {
            let jobs = if wrapped_jobs {
                format!(r#"{{"jobs":[{job}],"unknownJobsEnvelope":null}}"#)
            } else {
                format!("[{job}]")
            };
            let stages = if wrapped_stages {
                format!(r#"{{"stages":[{stage},{{}}],"unknownStagesEnvelope":{{"version":2}}}}"#)
            } else {
                format!(r#"[{stage},{{}}]"#)
            };
            let executor = FakeExecutor::new(vec![
                Ok(response(200, jobs.as_bytes(), None)),
                Ok(response(200, stages.as_bytes(), None)),
            ]);
            let observed = plan.execute(&executor, 1024, 4096).await.unwrap();
            assert_eq!(observed.coverage, PlanCoverage::ObservedJobsSnapshot);
            assert_eq!(observed.jobs.body, jobs.as_bytes());
            assert_eq!(observed.stages.len(), 1);
            assert_eq!(observed.stages[0].0.as_str(), "0");
            assert_eq!(observed.stages[0].1.body, stages.as_bytes());
            assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        }
    }
}
