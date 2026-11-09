<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job SDK

`JobClient` provides item job execution, status, cancellation, listing and
schedules. Definition publication and Fabric operation reconciliation are
separate APIs.

The additive `graph_refresh` module implements the documented GraphModel-specific
refresh POST with no body, synchronous 200 versus scoped 202 acceptance, and
dispatch-bound uncertainty. It does not alter legacy job routes or wait policy.
The [graph refresh preview](../../docs/graph-refresh-v2.md) uses the existing
neutral SDK observer instead of graph metadata or query polling.

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

## Pure published UDF validation

`udf::validate_arguments` validates one direct JSON argument object using a
caller-supplied `UdfInputSchema` and explicit `UdfJsonLimits`. The schema owner
must check its entire supported schema before validating values. The consumer
does not infer required fields, nullability, defaults or a schema dialect from
metadata, examples or source. Unsupported constructs fail explicitly; no
remote references are fetched and no source is executed.

The consumer rejects duplicate keys, including escaped and nested duplicates,
trailing content, integer overflow and caller byte/depth/node limit violations.
Object member names and values both count toward the node budget. It preserves
the original request bytes and native JSON types. Omission, JSON null and an
explicit default value stay distinct; defaults are never inserted.

Depth counts simultaneously nested objects and arrays, including the root
container. `UdfJsonLimits::validate` supports depth configurations from 1 through
`MAX_UDF_JSON_DEPTH` (127), inclusive. The pinned `serde_json` 1.0.154 decoder
starts its guarded recursion budget at 128 and rejects the container that would
decrement it to zero. The consumer therefore rejects larger configurations with
`UdfValidationError::UnsupportedLimits` **before input parsing or schema
callbacks**, even for shallow or malformed input. It does not silently label
otherwise valid deep JSON as `InvalidJson` or disable the decoder's guard.
Input deeper than a supported caller limit remains `JsonLimit`. This ceiling
is a local decoder capability, not a function-schema or service limit and not
a production default; byte and node budgets remain explicitly caller-selected.

`udf::UdfResponse::from_received` interprets bounded native response evidence.
Only HTTP 200 plus a complete, matching
`{functionName, invocationId, status, output, errors}` envelope with
`status: "Succeeded"` and empty errors establishes success. False, zero, null,
arrays and strings are valid outputs. Missing fields, unknown statuses,
contradictory HTTP/body data and unexpected 202 are not success. HTTP 403 can
carry `ResponseTooLarge`; it is not automatically a permission failure.

Raw bytes, parsed native fields, status, errors and invocation correlation are
available through explicit getters, not diagnostic formatting. An oversized
body is omitted entirely, never truncated into valid JSON. The eventual host
must enforce byte limits while streaming and preserve observed remote evidence
when later local output delivery fails. `invocationId` is correlation only, not
a monitor, cancellation or scheduler handle.

Use `UdfResponse::from_partial` when HTTP EOF was not established. Parseable
native fields remain available, but a valid JSON prefix saying `Succeeded`
cannot override incomplete transport evidence.

Complete and partial response consumers also reject unsupported depth
configurations before parsing or copying the body. They retain the actual HTTP
status with `UdfProtocolIssue::UnsupportedLimits`, no body/native fields, and no
success claim. This configuration error takes precedence over malformed or
oversized body data.

These are executable **offline consumers**, not a published-function invocation
adapter, command registration or production capability. Exact approved
owner/function/endpoint/schema/context binding, the execution audience/identity
handoff and the released shared run foundation remain integration gates.
No endpoint is derived and no credentials, HTTP, polling or cancellation are
performed. The synthetic `udf_validation` target supplies one explicit fixture
schema; its keyword choices and test byte budgets are not production defaults.

The selected synchronous run contract uses one 120-second client budget with
or without `--wait`; explicit positive integer `ms`, `s`, `m` or `h` durations
up to 24 hours replace it. Shared run infrastructure owns duration parsing,
deadline ties and dispatch/output evidence. The documented 100-second public
service limit is separate: a longer client budget does not extend that limit
or establish cancellation, rollback or an asynchronous protocol.

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
each suspension. A drop-safe request guard snapshots known acceptance into the
surviving session when either the executor or the enclosing submit/observe
future is dropped, without polling it again to recover evidence. Dispatch
uncertainty is stored in session evidence before invoking the executor; a
dropped credential acquisition still retains zero mutation dispatch.
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
result. A response-capture failure is retained explicitly in
`response_capture_error`, including during future drop. `RunFailure` snapshots
that evidence. HTTP acceptance is not execution
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
data are not for diagnostics or ambient credential selection. Once bound, a
session's execution identity is immutable: rebinding the same handle is allowed,
but another run on the same owner/endpoint is rejected before credentials or
GET. An existing-run session may import its first handle, but cannot submit a
new mutation; independent executions require independent sessions.

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

## Existing-session Livy statement contract

`livy::StatementInput::from_json` is pure validation of the closed
`{code, language}` object. Code must be nonblank and is preserved unchanged.
Explicit languages `pyspark`, `scala`, `sql` and `r` map to native kinds
`pyspark`, `spark`, `sql` and `sparkr`. `body()` contains exactly `code` and
`kind`. Unknown fields, missing language, cell input and native-kind overrides
are rejected without credential, file or network access.

`livy::StatementObservation` retains the native JSON response, including
statement ID zero, the full media map, errors, traceback and unknown fields.
Numeric and string IDs remain distinct. Missing IDs never become a session,
application, attempt or REPL ID. Debug and error diagnostics omit code and
provider values; native evidence belongs only on protected result surfaces.

Statement success requires `state=available` and `output.status=ok`.
Waiting/running is not success; idle sessions, unknown states, missing output
status and contradictory state/status are unverified. Native error and
cancelled states remain distinct. Explicit truncation/partial evidence is
separate from execution success and must not become exhaustive delivery.
Transport bounds and sink errors remain additional common-result evidence.

This module currently provides input/result interpretation, **not a registered
execution capability**. The released shared run deadline/evidence interfaces
and LH-12's authoritative route/version/casing and injected identity-context
descriptor are prerequisites for submit/observe integration. No endpoint or
audience is guessed. No session creation, newest-idle discovery, replay,
timeout cancellation, deletion, one-shot, high-concurrency or Notebook
interactive protocol is implemented.

The source-shaped contract uses the Livy task acceptance selected on
2026-11-05 and Fabio `557f098ec087c1626c1d892db09360083ff95105`
(`src/commands/spark_livy.rs`) as documentary provenance, not live service
evidence. The official guide at documentation revision
`ab44f2b599ae5d366212f67accb6731d07ad4215` and source route casing/auth
discrepancies remain LH-12 binding gates. Legacy `JobType::Notebook`,
RunNotebook wire requests and frozen v1 oracles are unchanged.
