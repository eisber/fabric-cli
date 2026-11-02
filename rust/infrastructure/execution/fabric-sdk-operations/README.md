<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric operation sessions

`OperationSession` provides one request, acceptance, polling and result policy
for Fabric mutations and operation-backed reads. It installs no transport,
identity implementation, runtime or retry stack.

Inject credentials, explicit selection, `OperationExecutor`, finite limits and
cancellation. `with_context` binds every request and active operation/result
link to the caller's validated endpoint.

## Completion policies

| Policy | Behavior |
| --- | --- |
| `ReturnAccepted` | Preserve acceptance without polling |
| `WaitForCompletion` | Wait for terminal completion without fetching a resource |
| `WaitForResult` | Retrieve the validated operation result |
| `PythonSynchronous` | Poll advertised operations and retrieve advertised results; retain headerless acceptance |

`reconcile_accepted` consumes an actual already-received acceptance without
resubmitting the mutation. The initial request counts against the session
budget. A used session cannot silently reset its limits.

`FabricOperation` validates finite methods, Fabric v1 routes and body policy.
`from_resource` consumes a focused endpoint's validated operation.
The GET-only `with_source_read_operation_ids` option accepts a bounded opaque
operation ID; default mutation and reconciliation paths retain stricter ID
validation.

## Evidence and cancellation

`OperationError` retains typed classification, operation ID, raw service
evidence and `mutation_may_have_occurred`. Display is sanitized;
`resource_error` projects an explicit view without consuming original evidence.

`check_cancelled` checks the same latched signal between caller-owned phases.
Cancellation never polls a completed future again and never implies rollback.
Mutation uncertainty is recorded before the request can be polled.

Invalid retry intervals and other polling fallbacks remain inspectable through
`warnings()`. Request, response, delay and allocation limits are explicit.
No failed, cancelled or lost mutation response authorizes replay.

Focused clients retain ownership of endpoint payloads and resource-specific
validation. Storage and ARM use separate origin/audience boundaries.
