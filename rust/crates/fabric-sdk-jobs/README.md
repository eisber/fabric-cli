<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job SDK

`fabric-sdk-jobs` translates item job execution and schedules from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Notebook definition publishing
and its operation LRO are separate APIs, not job execution.

Callers provide an `Arc<dyn TokenCredential>`, explicit `CredentialSelection`,
and `Arc<dyn JobExecutor>`. The service creates no runtime, HTTP client,
identity backend, CLI host, cache, or logger. Its normal dependency graph
contains only SDK/core crates. `JobRequest` exposes validated Fabric routes,
methods, bounded response allocation, token and exact request bytes.
`JobResponse` and `JobExecutionError` reuse the existing workspace wire types;
structured failures retain `FabricServiceError`.

## Explicit request headers

`JobRequestHeaders::with_skill(Some(value))` selects only the source
`x-ms-fabric-skill` header. `None` and an empty value emit no header. It cannot
override authentication, origin, audience or another header. Values are not
discovered from environment variables or mutable global context, and Debug
does not expose them. Controls, a leading ASCII space and values beyond the shared
header-byte bound are rejected before credential acquisition or I/O.

`JobClient::with_request_headers` returns an immutable command-scoped clone;
existing `new` and endpoint APIs keep their signatures and empty default.
`JobRequest::headers` exposes the explicit contract to an injected executor.
Existing executors accept only empty headers unless they override
`supports_request_headers` and actually forward the fields. A nonempty header
on a legacy executor fails explicitly, never silently disappears.

For a shared generic-operation reconciler that carries the same request
context, use `SharedJobOperations::from_job_executor` with that job executor.
It adapts the shared operation engine's validated requests through the same
header-aware executor, without another LRO/retry/credential policy. The existing
`SharedJobOperations::new` remains available for ordinary `OperationExecutor`
implementations. Such a legacy reconciler must explicitly support command
headers before a CLI command can select them.

`JobTarget::new(workspace, item, JobType::TableMaintenance)` supports the table
consumer without introducing another execution/polling implementation.
`JobType` includes all five source job types, not just notebooks and pipelines.

| API | Result |
| --- | --- |
| `submit` | A resource-bound `RunHandle` only after HTTP 202 with a valid instance Location |
| `status` | Ordered service fields and a source-compatible `JobState` |
| `list` | Shared `TokenPager`, actual page counts and explicit partial failures |
| `cancel` | An accepted handle, never a claim of terminal cancellation |
| `cancel_for_wait` | Source conditional generic-operation reconciliation, through an injected shared policy |
| `wait` | Actual status polling using the caller's async time; observes every state |
| Schedule create/get/update/remove | Source routes, bodies and completion statuses |

Dropping/cancelling local work does not cancel a remote job or roll back a
mutation. The CLI owns the Python timeout-cancellation configuration and
schedule-removal confirmation. SDK callers can explicitly cancel a handle and
wait for service-confirmed cancellation.

`SharedJobOperations` is the concrete `JobOperationReconciler` over
`fabric-sdk-operations::OperationSession::reconcile_accepted` with the source
`PythonSynchronous` policy. It receives an already-accepted response and never
submits/replays the cancellation. Callers inject credentials, explicit selection,
an `OperationExecutor` and limits. `JobError::Operation` retains the exact shared
`OperationError`, including known operation ID, raw service data, cancellation
classification and mutation uncertainty; none are recast as fabricated HTTP
or `JobFailed` errors.

`reconcile_with_cancellation` routes cancellation into the shared session before
returning evidence. Existing custom reconcilers can continue implementing
`reconcile`; their default cancellation race does not promise the concrete
bridge's operation evidence. Ordinary job-instance/no-operation-header accepted
responses bypass generic reconciliation. Missing required capability still
fails explicitly, rather than silently reporting completion.

See the [job compatibility matrix](../../docs/job-compatibility.md) for
source behavior, independent offline contracts, bounded safety differences,
and remaining front-door/live validation gates.
