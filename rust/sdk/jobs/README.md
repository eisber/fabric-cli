<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job SDK

`JobClient` provides item job execution, status, cancellation, listing and
schedules. Definition publication and Fabric operation reconciliation are
separate APIs.

Inject a credential, explicit selection and `Arc<dyn JobExecutor>`.
`with_context` retains a caller-selected endpoint across pages, run handles,
status polls and operation results. Construction creates no HTTP client,
runtime, identity backend, cache or logger.

## Operations

| API | Result |
| --- | --- |
| `submit` | Resource-bound `RunHandle` after HTTP 202 and a validated instance location |
| `status` | Ordered service fields and typed state |
| `list` | Ordered pages with actual page counts and partial-error evidence |
| `cancel` | Accepted cancellation, not proof that the job stopped |
| `cancel_for_wait` | Conditional generic-operation reconciliation |
| `wait` | Asynchronous state polling with caller-selected timeout policy |
| Schedule create/get/update/remove | Typed routes, payloads and status validation |

`response_for_command` is an explicit route-limited raw response policy for
command consumers. It does not weaken the typed SDK methods.

## Request-scoped headers

`JobRequestHeaders::with_skill` selects only `x-ms-fabric-skill`. Missing or
empty values emit no header. Validation rejects controls, invalid leading
whitespace and oversized values before credentials or I/O.

`with_request_headers` returns an immutable scoped clone. Executors must
advertise and forward requested headers; otherwise requests fail explicitly.
No mutable header state is shared between commands.

## Reconciliation and cancellation

### Bounded Notebook cancellation

`cancellation::NotebookRun::new(workspace_id, item_id, instance_id)` requires
three ASCII GUID-shaped identifiers. Uppercase hexadecimal and nil GUIDs are
accepted syntactically, without checking existence or item type.

`cancellation::request(&client, &run, cancelled)` reuses `JobClient::cancel`
for one empty-body cancellation POST. It preserves the caller's credential,
Fabric audience, endpoint, request headers and response limits. It performs no
metadata lookup, status GET, retry, wait, generic-operation reconciliation or
remote cleanup. Existing cancellation APIs retain their behavior.

HTTP 202 returns a `CancellationReceipt` with the known target, original
endpoint and raw accepted response. Any returned Location must name that exact
job-instance route on that endpoint, without an encoded route, query or fragment;
only the three GUID segments compare case-insensitively. Static path segments,
raw URL syntax and the selected origin retain strict validation. The Location
is never followed. Missing Location is accepted because the cancellation
route already identifies the run. A receipt means **request accepted**, not
terminal cancellation or rollback.

`request` returns a boxed `CancellationError`, retaining the original `JobError`, intended target, selected
`endpoint`, any actual received `response`, and `mutation_may_have_occurred`.
Responses remain available after Location, status or SDK byte-bound validation
fails. Their status and correlation headers are retained. The original body is
retained only within the client's `max_response_bytes` bound. An oversized body
is checked before cloning and omitted entirely, with an empty retained body and
`response_body_omitted: true`; omitted bytes must not be treated as a complete
response body. `response` is `None` and `response_body_omitted` is false if the
executor never returned a complete response, such as transport loss, in-flight
cancellation or a transport-enforced streaming byte-limit failure; partial
responses are not fabricated.

The mutation flag is false before executor dispatch,
including pending credential cancellation, and conservatively true once the
executor future is polled, including HTTP rejection, response loss or local
in-flight cancellation. Neither the flag nor local cancellation authorizes a
replay or proves a remote state change. Display is sanitized; raw service
details must be handled as untrusted data.

### Existing generic-operation reconciliation

`SharedJobOperations` delegates accepted responses to the shared operation
engine without replaying the initial mutation. Context-aware constructors retain
the original credential and endpoint; header-aware adapters carry the same
request-scoped fields.

`JobError::Operation` retains raw service data, operation ID and mutation
uncertainty. Dropping local work does not cancel the remote job. Callers must
request cancellation explicitly and wait for service confirmation when needed.
UI confirmation and command-level timeout-cancellation defaults belong to
the application, not this SDK.

## V2 run foundation

`run` is an additive, host-neutral policy. It does not change `JobType`,
`RunNotebook`, v1 request bodies, `WaitOptions`, or existing cancellation APIs.
Native Notebook, synchronous UDF and borrowed-session Livy adapters supply their
own payload/schema/binding and native result interpretation. CLI/root/artifact
aliases, SDK and MCP consumers use the same validated plan and evidence; those
consumer integrations are separate work, not implemented by this foundation.

### Preflight and budget

Construct a `RunOwner` and `RunTarget`, then call `RunPreflight::validate` with
adapter-declared `RunCapabilities`, `RunOptions`, `RunInput`, a `RunOutput` sink
and a pure native schema validator. Wrong owners, saved Notebook cells, deferred
one-shot targets, unsupported formats/wait, invalid input/duration and existing
output destinations fail before credentials or HTTP. The native validator must
also reject locally detectable binding/schema conflicts without remote schema
fetches or code execution. Input omission, JSON null and empty objects are not
interchangeable; the foundation inserts no argument defaults.

Durations are positive integer `ms`, `s`, `m` or `h`, at most inclusive `24h`.
Unitless, fractional, signed, zero and overflowing durations are invalid.
Defaults are 30 seconds for Notebook/borrowed Livy submission, five minutes with
wait, and 120 seconds for synchronous UDF with either wait spelling. The deferred
one-shot default is ten minutes; defining it does not enable the capability.
An explicit duration replaces the default.

`RunSession::new` starts one deadline after local preflight. Inject `RunClock`
using the caller's monotonic clock and existing asynchronous timer. Remote
binding resolution goes through `phase`; credential acquisition, POST, streaming,
observation/retry waits and output commit share this same budget. At the deadline
no new request starts. Local interruption is latched, wins before polling work,
and never requests remote cancellation, rollback or session deletion.

### Executor and native evidence handoff

`JobExecutor::with_run_budget(remaining)` is an opt-in factory for a scoped
executor over the **same** injected HTTP client/runtime/timer. Existing executors
reject v2 before credential acquisition. A v2 adapter must honor the exact
remaining timeout, including streaming and internal GET retries, without a
hidden 240-second exchange cap or early retry at/after the total deadline.
Provider Retry-After is a minimum, not an interval to clip. No new transport or
retry engine is needed: configure/wrap the existing shared HTTP execution seam
and enforce the absolute budget at its request boundaries.

`execute_run` receives an independent bounded `RunResponseRecorder`, and returns
`RunExchange::Complete` or `Failed { error, observed }`. Streaming-aware adapters
publish actual status, headers and bounded body progress to the recorder before
each suspension. The session can therefore snapshot known acceptance after
dropping a pending losing future, without polling it again to recover evidence.
An immediately returned failure can also carry actual evidence in `observed`.
The compatibility default cannot recover headers that its underlying executor
discarded; it does not invent evidence. Native host wiring must implement this
seam before claiming interrupted-stream coverage. No factory is installed
implicitly and no new production UDF endpoint/audience is inferred.

`submit` makes at most one native POST, including on 401/429/5xx, transport loss,
stream/parse failure, timeout or output failure. Any further mutation attempt
returns `Replay`. Native adapters must validate their exact owner-bound URL and
serialize their native payload before submission; this foundation does not
guess a UDF route, translate parameter dialects or fall back to legacy execution.

`RunEvidence` separates `NotSubmitted`, `Accepted`, `Completed`, `Failed`,
`Partial` and `Unknown` from local phase failures. It retains the original target
and endpoint, conservative dispatch uncertainty, original submit response, last
observation, response-completeness/omission flags, native handle and interpreted
result. `RunFailure` snapshots that evidence. HTTP acceptance is not execution
completion. A synchronous UDF adapter uses `record` and `correlate`; correlation
IDs never become async get/cancel handles. Use `interpret` for a pure native
result/verdict decoder inside the original deadline. Already decoded evidence is
retained if the deadline wins its final verdict. Result `null`, `false` and zero
remain native values rather than missing output.

`RunHandle::job` validates an exact Notebook item-instance Location.
`RunHandle::operation` identifies a real generic operation and rejects UDF
targets. `RunHandle::livy` binds actual session/statement IDs (including zero) to
an owner-scoped monitor supplied by the verified Lakehouse binding; the binding
owns version, route casing and audience. Collection, foreign-origin and
cross-owner Locations cannot become valid job handles. Handles and raw response
data are not for diagnostics or ambient credential selection.

### Shared observation and output

`observe` performs GET-only observation of the exact handle, with no submit,
cancel or DELETE. `SchedulerObserver` uses native scheduler status; failed,
cancelled, deduplicated and unknown states are never completed success. Livy
adapters implement `RunObserver` to require both native statement state and
output status. Providers supply the minimum Retry-After (including zero); absent
headers use five seconds. Invalid, ambiguous and unsupported HTTP-date values
fail explicitly. If a delay cannot fit strictly before the deadline, observation
returns `Deadline` with evidence rather than issuing an early GET.

The existing facade observer and v2 bridge both use the extracted
`fabric_sdk_operations::polling` driver, native status classification and
Retry-After parser. Legacy defaults (one second; zero rejected) remain explicit
legacy policy, not altered v2 expectations. The shared
`deadline::MonotonicDeadline` backs both `NativeDeadline` and `RunDeadline`;
the facade retains its original API and caller-runtime semantics. No dependency
from the jobs package to the SDK facade or CLI is introduced.

`RunOutput::preflight` checks no-clobber before work; `commit` must perform an
atomic final no-clobber check. A later collision/disk/stream failure returns a
local error while preserving remote outcome/result/handle, never rerunning work.
A successful local commit remains recorded even if the deadline/interruption
wins the final verdict. Platform-specific sinks and final CLI exit/wire mapping
remain consumer-owned integration requirements.

The offline `tests/run_foundation.rs` target covers the neutral contract.
Existing facade observation/runtime and v1 job fixtures cover the shared-producer
extraction without modifying their independent expected behavior.
