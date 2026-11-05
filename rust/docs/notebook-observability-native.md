<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook observability requests

This SDK slice implements real native request builders, not a local report,
Python bridge, new command root, model call, or production authentication host.
The caller supplies endpoints, authorization and the existing shared
`runtime::NativeExecutor`. Every returned `NativeRequest` can execute through
that seam; the SDK installs no HTTP stack, runtime, cache, logger or fallback.

## Provenance and denominators

Source: FNTK commit **`e250d5d32b04efcc5623a414a361727a93c271ce`**:

- `src/notebook_cli/client/monitor_client.py`: 44 non-constructor public
  callables, including cancellation, ticket retrieval and composite progress.
- `src/notebook_cli/client/public_monitor_client.py`: 18 such callables.
- `src/notebook_cli/client/resource_client.py`: six such callables.
- `src/notebook_cli/plugin.py`: 19 monitor leaves, five log leaves and six
  resource leaves. `handlers.py` supplies their public/workload dispatch
  semantics.

`notebook_observability::SOURCE_OPERATIONS` maps **all 68** scoped callables to
typed native views or plans and all **30** registered leaves to those views.
Two entries are composite plans; overlapping routes and source aliases are not
counted as unique REST operations. These 68 callables are a scoped subset, not
all 130 client member/constructor/property slots in the wider source inventory.
The catalog is callable mapping evidence, not proof of complete CLI or live
service parity.

## Endpoint and ownership contracts

Construct a `runtime::NativeEndpoint` using the exact caller-selected service
base. Its common validation rejects credentials, queries, fragments, unsafe
paths and non-loopback plain HTTP. This slice additionally requires these exact
path prefixes (an optional final slash is accepted):

| Facade | Required caller-supplied prefix |
| --- | --- |
| `PublicMonitor` | `/v1` on the selected Fabric public API origin |
| `WorkloadMonitor` Notebook | `/webapi/capacities/{capacity}/workloads/Notebook` |
| `WorkloadMonitor` optional SparkCore | `/webapi/capacities/{capacity}/workloads/SparkCore` |
| `Resources` | `/webapi/capacities/{capacity}/workloads/Notebook/Data/Direct/api/workspaces/{workspace}/artifacts/{artifact}/filesystem` |

These are not hard-coded production origins or discovered hosts. Notebook and
SparkCore can have different origins. Missing SparkCore yields
`UnsupportedCapability` for SparkCore operations; it never reuses a Notebook
endpoint/token implicitly. Authorization must be bound to the selected service
by the caller's executor. The builders deliberately cannot inject Authorization.
Workload monitor requests carry `x-ms-workload-resource-moniker: {artifact}`;
resource requests retain the source's lack of that header.

`LivyId`, `ActivityId`, `ApplicationId`, `AttemptId`, `SparkJobId`, `JobGroupId`,
`ContainerId`, `StageId` and `SqlExecutionId` are distinct validated types.
No conversion guesses that an activity is a Livy session, a Spark job is a
job group, an executor ordinal is a container, or an authored cell is a job
group. Compile-fail examples exercise those ownership distinctions when the
parent exports this module.

`WorkloadApplication` selects `Active` or `Completed(explicit_attempt)`.
Active uses `/activities/{activity}/hs/applications/{app}`; completed uses
`/activities/{activity}/applications/{app}/{attempt}`. There is no implicit
attempt `"1"`, latest-ID discovery, session cache, active/completed probe,
public-to-workload retry, or error-dependent fallback.

The public `PublicApplication` instead owns a public Livy ID. Optional attempts
are accepted only on source callables that advertise them. For example, public
executor log content **rejects** a supplied attempt instead of silently dropping
it. Public event logs require an explicit attempt. Job/stage/environment and
other unattempted public operations reject attempts they cannot represent.

## Complete callable bindings

All facade methods use ordinary get/list/update/delete/progress semantics.
Views below are typed parameters, not additional CLI registrations.

### Public AAD

All routes start
`/v1/workspaces/{workspace}/notebooks/{notebook}`.
Application routes then use
`/livySessions/{livy}/applications/{app}[/{attempt}]`.

| Source callable | Typed binding and actual route/query |
| --- | --- |
| `list_livy_sessions` | `PublicView::LivySessions`: `/livySessions`, optional exact `continuationToken` |
| `get_jobs` | `Jobs`: unattempted `/jobs` |
| `get_stages` | `Stages`: unattempted `/stages[/{stage}]`; **not** a per-job filter |
| `get_executors` | `Executors`: unattempted `/allexecutors` |
| `get_environment` | `Environment`: unattempted `/environment` |
| `get_sql` | `Sql`: unattempted `/sql[/{execution}]`; `details=true` only with an execution |
| `get_storage_rdd` | `StorageRdd`: unattempted `/storage/rdd` |
| `get_streaming` | `Streaming`: unattempted `/StreamingQuery` |
| `get_resource_usage` | `ResourceUsage`: `/resourceUsage`, explicit native query pairs |
| `get_resource_snapshot` | `ResourceSnapshot`: `/resourceUsage/{timestamp}`, explicit native query pairs |
| `get_driver_log` | `DriverLog`: `/logs?type=driver&fileName={file}&isDownload=true` |
| `get_driver_log_metadata` | `DriverLogMetadata`: `/logs?type=driver&meta=true&fileName={file}` |
| `get_executor_log` | `ExecutorLog`: unattempted `/logs?type=executor&containerId={container}&fileName={file}&isDownload=true` |
| `get_executor_log_metadata` | `ExecutorLogMetadata`: `/logs?type=executor&meta=true` |
| `get_livy_log` | `LivyLog`: `/livySessions/{livy}/applications/none/logs?type=livy&isDownload=true` |
| `get_livy_log_metadata` | `LivyLogMetadata`: same literal `none` route, `type=livy&meta=true` |
| `get_event_log` | `EventLog`: attempted `/logs`, ZIP representation |
| `get_progress` | `PublicMonitor::progress`: bounded ordered jobs + stages plan |

Public Livy history and public jobs are intentionally **not renamed application
records** merely because the FNTK `list-apps` handler sometimes returns them.
Public stages cannot satisfy a Spark-job-filtered workload stage request.
No unsupported public diagnostic, advice, graph, REPL or metrics route is
fabricated: use an explicitly bound workload view for those capabilities.
As in the source keyword-query implementation, empty resource-usage/snapshot
query values are omitted rather than emitted as empty parameters.

### Notebook and SparkCore MWC

Unless noted, append
`/Data/Automatic/sparkui/api/v1/artifacts/{artifact}` to the Notebook prefix.
`ApplicationView` routes append the explicit active/completed application path
described above.

| Source callable | Typed binding and route/query |
| --- | --- |
| `list_jobs` | `WorkloadView::Jobs`: SparkCore `/SparkCoreService/automatic/v1/monitoring/workspaces/{workspace}/artifacts/{artifact}/jobs`; native `$skip`, `$top`, `$filter` |
| `get_job` | `WorkloadView::Job`: same SparkCore route `/{job}` |
| `cancel_job` | `WorkloadMonitor::cancel`: **DELETE** Notebook `/Data/Automatic/sparkui/api/versions/1/artifacts/{artifact}/{sessions-or-batches}/{activity}`; `NativeEffect::Cancel` |
| `list_applications` | `WorkloadView::Applications`: `/activities/{activity}/[hs/]applications`; explicit state |
| `get_application` | `ApplicationView::Application`: exact application path |
| `get_app_jobs` | `Jobs`: `/filter/jobs?limit={positive_limit}` |
| `get_app_stages` | `Stages`: `/filter/stages?job={spark_job}&limit={positive_limit}` |
| `get_diagnostic` | `Diagnostic`: `/enhancement/diagnostic` |
| `get_data_io` | `DataIo`: `/datalist` |
| `get_executors` | `Executors`: `/executors` |
| `get_environment` | `Environment`: `/environment` |
| `get_sql_queries` | `Sql`: `/SQL` (case preserved) |
| `get_storage_rdd` | `StorageRdd`: `/storage/rdd` |
| `get_streaming` | `Streaming`: `/StreamingQuery` |
| `get_job_graph` | `Graph`: `/enhancement/graph` |
| `get_resource_usage` | `ResourceUsage`: `/resourceUsage` |
| `get_resource_overview` | `ResourceOverview`: `/resourceUsage/overview` |
| `get_event_log` | `EventLog`: `/logs`, ZIP representation |
| `get_rolling_driver_log_metadata` | `RollingDriverMetadata`: **non-HS, unattempted** `/activities/{activity}/applications/{app}/rollingdriverlog/metadata/{file}?offset={page*3000}` |
| `get_executor_container_metadata` | `ContainerMetadata`: **non-HS, unattempted** `/activities/{activity}/applications/{app}/executorlogs/containers/{container}/metadata?filenamePrefix={file}&offset={page*3000}` |
| `get_livy_log` | `WorkloadView::LivyLog`: `/activities/{activity}/livylog?offset={offset}&size={positive_size}` |
| `get_livy_log_metadata` | `WorkloadView::LivyLogMetadata`: `/activities/{activity}/livylog/metadata` |
| `get_driver_log` | `DriverLog`: `/driverlog/{file}?offset={offset}&size={positive_size}` |
| `get_driver_log_metadata` | `DriverLogMetadata`: `/driverlog/metadata/{file}` |
| `get_executor_log` | `ExecutorLog`: `/yarnaggregationlog/containers/{container}/files/{file}?offset={offset}&size={positive_size}` |
| `get_executor_log_metadata` | `ExecutorLogMetadata`: `/yarnaggregationlog/metadata` |
| `get_cell_log` | `CellLog`: `/{job_group}/driverlog/{file}?offset={offset}&size={positive_size}` |
| `get_session_status` | `WorkloadView::SessionStatus`: versioned `/Data/Automatic/sparkui/api/versions/1/artifacts/{artifact}/sessions/{activity}` |
| `get_repl_status` | `WorkloadView::ReplStatus`: SparkCore `/repls?$filter=sessionId eq '{activity}'`; literal quotes escaped before query encoding |
| `get_replmaps` | `ReplMaps`: `/replmaps` |
| `get_activity_summary` | `WorkloadView::ActivitySummary`: `/activities/{activity}/summary` |
| `get_metrics_list` | `WorkloadView::Metrics`: Notebook `/Data/Automatic/sparkmetrics/api/v1/artifacts/{artifact}/infolist` |
| `get_anomalies` | `WorkloadView::Anomalies`: same metrics prefix `/anomalyinfolist` |
| `get_activity_metrics` | `WorkloadView::ActivityMetrics`: same metrics prefix `/activities/{activity}/info` |
| `list_livy_sessions` | `WorkloadView::LivySessions`: versioned `/Data/Automatic/sparkui/api/versions/1/artifacts/{artifact}/sessions` |
| `get_livy_session` | `WorkloadView::SessionStatus`: source alias, same versioned session route |
| `get_advice` | `Advice`: `/advise` |
| `get_advice_by_cell` | `AdviceByCell`: `/advise/jobGroups/{job_group}?outputMode=UX` |
| `get_progress` | `WorkloadMonitor::progress`: bounded jobs then every observed RUNNING/FAILED job's stages |
| `get_event_log_sas_token` | `WorkloadMonitor::event_log_ticket`: **POST** Notebook `/sparkdiagnostic/api/v1/artifacts/{artifact}/activities/{activity}/GetSasTokenForEventLogs?jobType={sessions-or-batches}`; **`NativeEffect::Read`** |
| `get_loading_progress` | `LoadingProgress`: completed `/loadingProgress` |
| `get_diagnostic_metrics` | `DiagnosticMetrics`: completed `/diagnosticmetrics` |
| `get_resource_snapshot` | `ResourceSnapshot`: `/resourceUsage/{milliseconds}?jobLimit=10&executorLimit=10` |
| `get_history_server_properties` | `WorkloadView::HistoryServerProperties`: `/Data/Automatic/sparkui/api/v1/historyServerProperties`, no artifact segment |

Metrics require an explicit time range; there is no ambient clock. Info-list
query fields are `startTime`, `endTime`, `from`, `to`, `maxItems`, `src=fntk`,
`artifactType=SynapseNotebook`, optional `activityId`, optional
`isRunning=true` and optional `triggerType`. Anomalies use `from`, `to`,
`maxItems`, `offset`. Query values are URL-encoded, not concatenated into routes.
Rolling metadata page multiplication is checked for overflow.

### Resource filesystem MWC

Every file/folder operation requires a validated `ResourcePath`, a
provider-relative slash-delimited path, **not** a local destination.
Absolute paths, leading/trailing slashes, empty interior segments, dot
segments, backslashes, drive/URI prefixes, escapes and controls fail locally.
An empty path is allowed only for listing the root, never file get/write/delete.

| Source callable | Typed facade and exact request |
| --- | --- |
| `list_files` | `Resources::list`: **GET** `/workdir[/{path}]?recursive=true-or-false`; `Cache-Control: no-cache` |
| `get_file` | `Resources::get`: **GET** `/workdir/{path}`; accept arbitrary file media, exact response bytes |
| `put_file` | `Resources::update`: **PUT** `/workdir/{path}`; `Ms-FileSystem-Entry-Type: file`; explicit `Content-Type`, exact bounded bytes, no JSON string wrapping |
| `mkdir` | `Resources::update_folder`: bodyless **PUT** `/workdir/{path}`; `Ms-FileSystem-Entry-Type: folder`; source shared default `Content-Type: application/json` |
| `delete` | `Resources::delete`: **DELETE** `/workdir/{path}`; explicit file/folder header; `recursive=true` only for recursive folders, otherwise absent |
| `get_usage` | `Resources::usage`: **GET** `/workdirUsage`; actual provider JSON |

The source docstring mentions `If-None-Match: *`, but its implementation does
**not send it**. Native PUT is an upsert that can replace existing content.
`ResourceWriteAdmission::Upsert` is mandatory for file/folder PUT;
`CreateOnly` returns `UnsupportedCapability` before dispatch. No racy GET/PUT
existence check, fabricated create-only guarantee, or unevidenced precondition
header is added. Both resource PUTs disclose `NativeEffect::Update`, not an
enforced create.

`ResourceContent::from_response` classifies text/json/xml media as text and
other/absent media as binary, never writes a file and rejects failed HTTP
responses. Its optional convenience text decoder is strict UTF-8 rather than
the source HTTP library's charset guessing/lossy decoding. The unmodified
`NativeResponse` remains available to callers needing another text codec.
Listings and quota bodies stay provider-native JSON: the source supplies no
stable typed entry/quota schema, so this slice does not fabricate one.

## Bounded execution and honest coverage

Single requests use common `NativeRequest::execute`, which delegates once to
the caller's executor and checks its response bound. The caller must enforce
authorization, deadlines, cancellation, streaming limits, no redirects and no
automatic mutation replay as required by the common contract. A raw successful
HTTP status is not proof of application completion.

`PublicMonitor::progress` returns a `RequestPlan` with jobs then app-wide stages.
`WorkloadMonitor::progress` returns a `WorkloadProgressPlan`: read bounded jobs,
validate the actual job IDs/statuses, then read stages for each observed
RUNNING/FAILED job. Provider-derived IDs are observations, not guessed aliases.
Unknown response shapes, unsafe/duplicate IDs, indicated continuations,
insufficient request budgets, stage failures and byte-bound failures return an
error, never fabricated empty arrays or a successful prefix.
Decoded workload jobs and required stage collections accept bare arrays or
their native named envelopes, but every array entry must be an object. Null,
scalar and nested-array entries produce `InvalidResponse`, including a malformed
later stage component after earlier components succeeded. Unknown fields in
record objects and envelopes remain intact in the returned response bytes;
this structural check does not invent a closed provider schema.

`WorkloadMonitor::progress_selected` is an additional explicit subset plan.
Its `SelectedJobs` coverage remains partial even on full transport success.
Dynamic workload observations use `ObservedJobsSnapshot`: finite returned jobs
and their required stages, **not** an unbounded or atomic app snapshot. A full
limited page can hide more jobs; the API does not claim exhaustive coverage.
Public plans use `ApplicationSnapshot`, meaning the scope of the two reads, not
an atomic or paged-to-completion snapshot. Per-component and total response byte
bounds and finite request counts remain caller inputs.

Source oddities are retained or made explicit, not disguised as parity:
Python's SQL callable ignores its state argument and probes active first;
native callers select the history profile and attempt directly. Python progress
swallows per-job stage errors into empty lists; native progress deliberately
fails instead. Python's default completed attempt `"1"` and cache/ID fallback
are not native identity evidence. Source REPL filter interpolation does not
escape literal quotes; the native builder escapes OData literals before URL
encoding. The PUT docstring/implementation discrepancy is documented above.
No independent review or external discussion publication is implied by these
recorded observations.

`SignedEventLogTicket` decodes `sasUrl`, redacts Debug and requires explicit
`expose_url()` to access the secret. No signed link is followed, rewritten into
a Fabric request, logged, cached, persisted or downloaded.

## Validation and integration boundary

Base checkpoint: **`2cca2b509c2e0b7c0f7bf0968f9fbe8e427bfb1f`**.
Owned files are only this document, `src/notebook_observability/**` and
`tests/notebook_observability.rs`. The coordinator owns the `lib.rs` export;
this lane does not edit manifests, locks, identity, transport, CLI or peers.

Authored offline Rust coverage: **20 top-level tests**, plus three compile-fail
ID-kind examples. Tests cover all source-native route families, exact query
order/values, methods, headers, raw binary/text uploads, content classification,
active/completed attempts, metadata exceptions, the 68-callable/30-leaf catalog,
wrong profiles, unsafe paths/IDs, unsupported attempt combinations, create-only
admission, overflow/zero budgets, redaction, per-response/aggregate limits,
cancellation, stage failures and honest partial coverage.

Validation repair cycle 1 starts at coordinator checkpoint
**`2772f204c60a0ec3786730ceb435d65611301edf`**. The repair adds semantic docs for
51 public enum fields and two regression tests covering non-object jobs/stage
entries in bare and enveloped arrays, mixed valid/invalid entries, failure in a
later required stage component, and preservation of unknown fields. These new
tests are authored, not locally executed; builds remain coordinator-serialized.

Checks actually run in this lane, without building:

1. Python AST comparison of the pinned three client classes to the Rust catalog:
   exact sets **44 + 18 + 6**, no missing or duplicate scoped callable.
2. Registered leaf inventory: exact distinct counts **19 + 5 + 6**.
3. **69 isolated offline source-oracle comparisons**: 45 workload application
   route/query/header cases against authored Rust fixture suffixes, 18 public
   route/query cases, six resource route/query/header/body cases. Only the
   source class ASTs were evaluated with synthetic dependencies and recording
   HTTP stubs; no source authentication, package startup or transport ran.
4. Static owned-source boundary checks: no filesystem/process/HTTP/auth/runtime
   or logging implementation.
5. Static binding/fixture checks: all 68 catalog bindings name declared members;
   all **58 typed view variants** (17 public, 27 workload application, 14 workload
   top-level) appear in the offline tests; the original 18 test attributes remain
   at crate root and all three compile-fail examples are present. This is not a
   Rust type check or execution result.
6. Git whitespace and exclusive-path checks.

These checks do **not** compile or execute Rust. No Cargo/toolchain build or
installation is performed under the coordinator's current build prohibition.
`cargo`, `rustc` and `rustfmt` were unavailable in this session's PATH; the
coordinator must export the module and run formatting, all-target compilation,
targeted tests and relevant downstream checks in its authorized build window:

```powershell
Set-Location rust
cargo +1.99.1 fmt --all -- --check
cargo +1.99.1 test -p fabric-sdk --test notebook_observability
cargo +1.99.1 test -p fabric-sdk --doc
```

Independent review and combined-candidate validation remain coordinator gates.
No live Fabric, Spark, signed-link download, authentication, user settings,
push, PR, merge or deployment was performed. No complete CLI/live parity is
claimed.
