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
