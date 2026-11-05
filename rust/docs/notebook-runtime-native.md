<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook runtime: first SDK wave

This is a native Rust implementation, not a Python bridge. It is **not**
end-to-end Notebook/CLI acceptance. The integration owner must export
`pub mod notebook_runtime;` and `pub mod runtime;` from the SDK facade.
This lane intentionally does not modify facade exports, manifests, the lock,
authentication, shared HTTP infrastructure or a CLI.

## Provenance and interfaces

FNTK source revision: `e250d5d32b04efcc5623a414a361727a93c271ce`.
The contracts were checked against:

| Source under `src/notebook_cli` | Native implementation |
| --- | --- |
| `constants.py:171-199`, `client/http_utils.py:88-101` | Exact Notebook workload scoping and native routes |
| `client/notebook_client.py` | Content GET/PUT/HEAD; Jupyter collection/active/get/create/patch/delete; kernel restart/interrupt |
| `models/models.py:209-349`, `tests/test_notebook_preservation.py` | Lossless JSON snapshots and explicit, bounded cell transforms |
| `models/lakehouse.py` | Dependencies/trident precedence, sibling-preserving default-Lakehouse edits |
| `client/jupyter_protocol.py`, `client/ws_client.py:286-318,371-466,516-541,587-628` | Execute/control messages, correlation, native outputs/replies and cancellation ACK semantics |
| `models/models.py:353-456`, `tests/test_cancellation.py` | Jupyter 5.3 envelopes, distinct local/message IDs, partial-output and completion/cancel races |

Public entry points:

- `NotebookRuntime`: explicit origin/scope, request builders and async operations
  over the **existing** `crate::runtime::NativeExecutor`.
- `content::{NotebookDocument, ContentLimits, Cell, CellType, CellSelector,
  CellPosition, WritePolicy, SaveEffect, DefaultLakehouse}`.
- `SessionId`, `KernelId`, `MessageId`, `CommId`, `ExecutionId`: independent
  types, never aliases with automatic conversions or ambient lookup.
- `CreateSession`, `JupyterSession`, `HttpOutcome`, `ContentMetadata`, `ResumeBinding`.
- `protocol::{ExecuteRequest, ControlRequest, SynapseControl, JupyterMessage,
  ExecutionTracker, ExecuteReply, LiveOutput, ControlReply, LivyStatus}` and
  distinct execution, local observation and cancellation state enums.

All public APIs are documented under the workspace's `missing_docs = deny`.
The test crate also denies `unnameable_test_items`.

## Caller-owned native HTTP

The caller supplies an explicit `NativeEndpoint` workload **origin**, capacity,
workspace and Notebook artifact ID. The client appends:

```text
/webapi/capacities/{capacity}/workloads/Notebook/Data/Automatic/api
/workspaces/{workspace}/artifacts/{notebook}
```

No cluster, capacity, endpoint, credential, token or account discovery occurs.
The host must bind the existing authenticated executor to this exact selected
service and its MWC authorization. A public Fabric `/v1` credential/endpoint
must not be substituted. Origin validation is shared, not reimplemented.

| Suffix relative to that scope | Native method | Declared effect |
| --- | --- | --- |
| `/content` | GET / HEAD | Read |
| `/content` | PUT | Update: **whole Notebook** |
| `/jupyterApi/versions/1/api/sessions` | GET / POST | Read / Create |
| `/jupyterApi/versions/1/api/sessions/active` | GET | Read |
| `/jupyterApi/versions/1/api/sessions/{session}` | GET / PATCH / DELETE | Read / Update / Delete |
| `/jupyterApi/versions/1/api/kernels/{kernel}/restart` | POST, no body | Kernel lifecycle |
| `/jupyterApi/versions/1/api/kernels/{kernel}/interrupt` | POST, no body | Kernel-wide cancellation request |

Detailed session GET emits `?detailed=true`; false omits the query, as in the
source. Creation emits `name`, `path`, `type: "notebook"` and `kernel.name`,
plus optional nonempty `kernel.synapseExtension`. Empty options are omitted as
in the pinned source. Empty path uses the source's
`/notebook-cli/{name}` fallback. Kernel response `synapse_extension` and all
other provider fields remain in the returned JSON; names are not rewritten.
PATCH takes an explicit nonempty native JSON object, not guessed settings.
Session ID `active` is rejected for an individual-session route.

The client uses the shared `NativeRequest::execute(executor, max_bytes)` bound
and dispatches each operation exactly once. JSON content retains explicit
`application/json` media; no raw bytes or protocol headers are added by these
routes, including no synthetic `If-Match`. It does not install retries,
pollers, a runtime, cancellation, rollback or automatic cleanup. An executor's
`TimedOut`, `Cancelled` or `OutcomeUnknown` remains that error. HTTP failures keep
their actual status. Response bytes and session counts are checked even if the
executor violates its budget. Unsupported collection continuations fail instead
of being reported as a complete empty/list success. Unknown session state is raw
metadata, not ready compute. HTTP 202 mutation results retain 202, not completion.

HEAD executes and checks a bodyless HTTP 200. `head_content_metadata` returns
`ContentMetadata` with the actual observed ETag, read through the shared
`NativeResponse::header` contract. Lookup is case-insensitive, bounded and rejects
ambiguous case-variant duplicates (even equal values), malformed headers and
over-budget metadata before copying a value. An absent ETag remains `None`;
strong/weak values are retained verbatim without synthesis or caching.
`get_etag` provides the source-equivalent single-HEAD convenience API, while
`head_content` validates the same response and discards its metadata.

An observed ETag is **not** an `If-Match` guarantee: no protocol header is added
to a subsequent save and `WritePolicy::RequireGuarded` remains unsupported.
GET/PUT wrappers do not attach their response ETags to a content revision;
a separate HEAD describes only that observation, not an earlier read/save.
The host must supply actual response headers; absence is never filled from a cache.

## Bounded, lossless persisted content

`NotebookDocument` stores full JSON rather than projecting through a lossy schema.
Imported source strings/line arrays, unknown fields, metadata, attachments, MIME
outputs, IDs and absent/null/empty distinctions survive unchanged. Object member
formatting and whitespace are not preserved: this is JSON-value fidelity, not
byte-for-byte round-tripping. Accepted input is nbformat 4 with a nonnegative
minor version and explicit valid cell types/source. Missing/null optional fields
are retained. Invalid typed data is rejected, not silently defaulted.

Default budgets are 16 MiB encoded content, 10,000 cells, 1 MiB source per cell,
and 10,000 saved output records per cell. Callers may supply other nonzero finite
budgets. Import and every transformation enforce them. Size preflight serializes
borrowed `Serialize` values directly into a bounded counting writer, stopping at
the first write that exceeds the budget. It does not construct an intermediate
`serde_json::Value` or an unbounded encoded buffer.
JSON parsing retains serde_json's recursion limit.

Destination budgets are independent of the source object's original limits.
A full content PUT validates the borrowed source document against all destination
content limits before cloning it once into the request. Session creation checks
the borrowed prepared payload against the destination byte limit before copying;
session PATCH checks its already-owned payload before dispatch. Rejected requests
leave the source revision/payload unchanged and do not call the executor.

A selector requires `document.selector(explicit_index)`. An insertion boundary
requires `document.position(explicit_index)` (including the end position).
Both bind to the immutable **local snapshot**, not an arbitrary supplied revision
string, inferred cell ID, or implicit zero index. Clones share that revision;
separately imported snapshots do not. Successful changes produce a new revision
and invalidate old selectors. No-op type conversion preserves the same revision.

Source replacement modifies only source. New code cells receive empty outputs
and null execution count. Explicit conversion to code removes attachments and
initializes execution fields; conversion away from code removes outputs/count;
Markdown/raw attachment semantics remain intact. Setting language and parameter
tags changes persisted metadata only. Parameter removal preserves other tags;
setting an existing parameter flag does not reorder it. Saved outputs may be
absent/null/empty and are **never** current live results or freshness evidence.

All persisted operations, including a one-cell deletion or Lakehouse setting,
use `SaveEffect::WholeNotebook` and an entire `/content` PUT. The caller must pass
`WritePolicy::AllowUnguarded`; `RequireGuarded` returns `UnsupportedCapability`
before dispatch. The pinned source sends no `If-Match`. Local selector checking
does **not** prevent remote lost updates; this is neither atomic cell editing,
compare-and-swap, optimistic concurrency nor a read/modify/write transaction.

### Persisted default Lakehouse

Read-only `default_lakehouse` uses non-null `metadata.dependencies` before
`metadata.trident`. An empty canonical object still shadows the entire legacy
branch. A malformed canonical branch is an error, never a fallback to legacy.
Reads neither migrate metadata nor restart compute.

Setting the default copies all effective dependencies when migrating from legacy,
preserves unknown Lakehouse fields and `known_lakehouses`, updates a matching
known display name where the source does, and appends a missing known binding.
It removes only `trident.lakehouse`, retaining legacy siblings. The canonical
branch's other dependencies remain intact. This is a whole source save, **not**
a live Lakehouse switch or session/kernel restart.

## Offline Jupyter/Synapse protocol

`ExecuteRequest` prepares the source's shell-channel `execute_request`, Jupyter
5.3 header, caller-owned IDs, `code`, `silent`, `store_history`, empty
`user_expressions`, `allow_stdin=false`, `stop_on_error=true`.
It submits ephemeral source, not a durable saved-cell identifier.

`comm_open` prepares deterministic `synapse:control`, `synapse:status` and
`synapse:query` channels. Controls use `comm_msg` with
`content.data = {type, content, parent_msg_id}`:

- `cancel_cell_execution` carries the **remote execute message ID** as a string;
  its control request ID is different.
- `cancel_all_cell_execution` is explicitly session-wide.
- `replay_outputs` carries `{parent_msg_id: original_execute_id}` and is always
  explicit; timeout/resume never calls it.
- `start_livy_session`, `set_livy_session_options` are lifecycle controls, not
  session REST creation or local reconnect.
- `ping` is a connectivity control, not compute readiness.

`ExecutionTracker` starts `Prepared`. Only after the caller's actual host send
may it record submission; construction/encoding never claims success.
It correlates outputs/replies using `parent_header.msg_id`, not the caller-local
execution handle or incoming kernel sender-session ID. Duplicate message IDs are
ignored. It retains stream string/array data, complete MIME bundles and metadata,
errors/tracebacks, execution counters and unknown output/reply fields. Live
outputs are separate types and never written into a saved cell automatically.
Error output and kernel idle are not terminal. Following the pinned source,
`execute_reply` is terminal; later outputs are ignored. This is not a guarantee
that every Jupyter producer's cross-channel late output has been drained.

Cancellation records an actual caller-host control send, then independently
awaits the ACK. Both outer and nested parent IDs must match. A supplied comm ID
must also match; an omitted comm ID is accepted as in source fixtures. The first
control reply is authoritative for the local ACK state; a later reply cannot
replace it. `ok` with a non-null error is rejected. Unknown status is not success.
Control ACK can arrive before or after a terminal execution reply; neither changes
that terminal result. Partial outputs remain visible on rejection, timeout,
disconnect and error. Cancellation is not rollback.

`timeout`, `cancel_observation` and `disconnect` alter **local observation only**.
No remote cancel/replay/re-execution is emitted. Resuming local observation
requires the exact original session/kernel binding and does not reconnect the
network or resubmit source. `LivyStatus` retains native startup/lifecycle state
and errors separately from code outcomes; unknown states remain unknown.

Default protocol limits are 4 MiB/frame, 1 MiB/code, 20,000 correlated messages,
10,000 live outputs and 16 MiB total retained output content. Invalid payloads or
limits preserve prior tracking data rather than fabricating a terminal result.
Inbound validation sizes the borrowed envelope directly. Outbound execute, comm
open and control construction sizes a borrowed wire envelope, including headers,
wrappers and JSON string escaping, before materializing any payload `Value`.
Livy control options are borrowed during preflight, not cloned first. Retained
output count/byte limits are checked against borrowed incoming content before
copying it into execution state. Existing caller-owned input allocation is not
counted as a new SDK allocation, and an encoded-byte limit is not a total-process
heap cap.
Errors/tracebacks/source/MIME data are caller-visible data and must not be logged
without an explicit redaction policy.

## Explicit network-host gaps and validation

`NotebookRuntime::require_websocket_host` and `resume` return
`UnsupportedCapability`. The pure protocol state machine is implemented, but no
WS network client is claimed. Remaining prerequisites:

1. A coordinator-reviewed native WebSocket transport using the existing
   caller-owned runtime/authorization, correct `wss` kernel-channel route,
   Synapse/MWC subprotocol authentication, origin binding and redirect denial.
2. Frame/message streaming limits, cancellation-safe send/receive, deadlines,
   connection disposal and authenticated comm establishment in that host.
3. Explicit startup/control ordering and Spark readiness observation;
   REST session creation alone is insufficient.
4. Reconnect reconciliation by original binding, with explicit policy for replay
   and late output. No automatic source re-execution.
5. Facade exports, native/shared-HTTP host composition and independent combined
   Cargo checks before integration. No live service or cross-platform proof yet.

Monitoring, resources/files, public session history, daemon management, telemetry
attribution, widget-output enrichment and all model/Copilot features are outside
this first core wave. They are not represented by successful stubs.

Offline tests in `sdk/fabric-sdk/tests/notebook_runtime.rs` exercise exact
routes/payloads/effects, lossless transforms, selectors and invalid types/bounds,
Lakehouse precedence/siblings, injected HTTP failures/once-only dispatch,
saved-vs-live outputs, protocol correlation, cancellation races/partial outputs
and timeout/resume without remote cancel or replay. Adversarial donor/destination
tests cover smaller content/session/frame/output limits, exact escaped-wire byte
boundaries, source preservation and no dispatch on rejection. Serializer probes
also verify that bounded preflight stops before visiting later payload fields
or materializing a rejected outgoing message. Header tests cover actual ETags,
absent metadata, duplicate/malformed/oversized rejection, once-only HEAD dispatch
and unchanged unguarded write policy. The initial lane environment
had no Rust toolchain. The coordinator subsequently restored a session-local
Cargo/rustfmt/clippy 1.99.1 toolchain and reported MSVC 18 availability, and queued
focused validation under the shared heavy-validation mutex. Workers still have
no granted compilation slot. Rust tests, formatting and `missing_docs` compilation
remain **unexecuted in this lane**, not passed; queued parent validation is not a
test result.
Static source checks and `git diff --check` are separate evidence, not replacements
for an authorized all-target check.
