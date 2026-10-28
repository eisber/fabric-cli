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

`SharedJobOperations` delegates accepted responses to the shared operation
engine without replaying the initial mutation. Context-aware constructors retain
the original credential and endpoint; header-aware adapters carry the same
request-scoped fields.

`JobError::Operation` retains raw service data, operation ID and mutation
uncertainty. Dropping local work does not cancel the remote job. Callers must
request cancellation explicitly and wait for service confirmation when needed.
UI confirmation and command-level timeout-cancellation defaults belong to
the application, not this SDK.
