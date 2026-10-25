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

`JobOperationReconciler` is the injection contract for the source HTTP helper's
generic-operation branch, not a second LRO engine. If a synchronous cancellation
actually selects that branch without a configured reconciler, it fails explicitly.
Ordinary accepted job-instance/no-operation-header responses need no reconciler.
The generic-operation implementation remains owned by the shared-operation lane.

See the [job compatibility matrix](../../docs/job-compatibility.md) for
source behavior, independent offline contracts, bounded safety differences,
and remaining front-door/live validation gates.
