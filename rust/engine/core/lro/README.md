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

`FabricOperation::with_accept` opts into finite `OperationAccept::ArrowV1` or
`ArrowV2` response negotiation. The selected value reaches the initial request
and result retrieval; JSON operation-state polling keeps its existing defaults.
`OperationRequest::accept` exposes the validated selection to the injected
executor. No arbitrary/authentication header override is exposed, and existing
constructors remain unchanged. `reconcile_accepted_with_accept` supplies the
same result negotiation for an already-received 202 without replaying its
original request.

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

## Explicit ARM GET reconciliation

The additive `ArmGetOperation::new` validates bodyless subscription/resource
routes. `OperationSession::for_arm` selects only the built-in
`management.azure.com` origin and requires `Audience::Arm`.
`execute_arm_get` and `reconcile_arm_get` apply Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`
`client/fab_api_client.py::{_handle_azure_async_op,_poll_operation}` behavior
using the same bounded request/timer/poll loop as Fabric, not a second poller.

ARM 201/202 requires an `Azure-AsyncOperation` header or, only when absent,
`Location`. The former checks terminal JSON status; the latter waits for HTTP
200 without interpreting the body. Both return the original accepted bytes with
status 200. ARM never fetches a Fabric `/result`, including when the terminal
response has a Location header. Unsafe origins, normalized/traversing paths,
oversized links, duplicate headers, cancellation and finite limits fail explicitly.
Accepted IDs and raw terminal errors remain inspectable; read-only GET does not
set mutation uncertainty.

`OperationRequest::origin` lets existing shared HTTP adapters enforce this
explicit boundary. Default constructors, `FabricOperation`, Fabric completion
policies, existing endpoint overrides and GUID-only defaults are unchanged.
ARM sessions cannot execute Fabric operations or silently choose another origin.
Storage remains a separately owned boundary.

`tests/arm.rs` is a direct offline SDK consumer covering initial 201/202, both
header routes, terminal/original-result priority, limits, cancellation, audience
and origin rejection. These tests require execution with the existing standalone
and endpoint-context targets; their addition is not a passing receipt.

Focused clients retain ownership of endpoint payloads and resource-specific
validation.
