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
