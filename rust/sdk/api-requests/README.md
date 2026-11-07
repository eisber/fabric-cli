<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit API execution

`ApiClient::new` accepts a credential, explicit `CredentialSelection` and
transport-neutral `Arc<dyn Executor>`. Construction performs no I/O.

`Request::fabric` validates Fabric v1 route segments. `send` returns raw status,
body and headers, including service failures. `require_status` applies the
requested status policy. Raw requests never poll implicitly.

## Collection and completion policies

`collect` uses `TokenPager` for ordered rows, duplicates, opaque continuation
tokens, origin/path validation and cycle detection. Errors retain partial
progress. Catalog continuations replace the search body with the token and
original page size.

`send_completed` reconciles a received Fabric acceptance through
`OperationSession::reconcile_accepted`. It never resends the original mutation.
Failures retain operation IDs, raw service evidence and mutation uncertainty.

`with_arm_get_reconciliation(limits)` is a separate explicit read-only opt-in.
ARM GET singleton/collection requests then use
`OperationSession::execute_arm_get` for initial 201/202 before the existing pager
decodes their original response bytes. Both ARM header policies share the SDK's
finite poller; there is no second HTTP/timer loop. Bodies and custom headers are
rejected for this bodyless subscription/resource GET surface instead of being
silently discarded. Default clients and raw API behavior remain unchanged.

For this ARM opt-in only, collection cancellation is passed cooperatively into
the active fetch/session. The pager waits for the typed cancellation result
instead of dropping an accepted operation and losing its handle. Collection
failures retain the original operation error and prior complete pages. Ordinary
Fabric/raw collection cancellation keeps the existing outer-pager priority.

## Origins and request validation

`Origin`, `RawRequestOptions` and `Request::raw` select finite public origin and
audience pairs for Fabric, OneLake, ARM and Power BI. Arbitrary hosts or
audiences are not inferred from URLs. Executor support is checked before token
acquisition; unsupported extended requests fail explicitly.

URL, body and header allocations are bounded. Authentication, host, proxy and
hop-by-hop headers cannot be overridden. JSON bodies use the shared ordered
encoder with explicit numeric and Unicode validation.

## Dispatch observations

`with_query_dispatch_trace` opts into a caller-owned `QueryDispatchTrace`.
It records the last validated request handed to the executor, not an unconsumed
continuation token. It does not retain bearer tokens, bodies or header history,
and debug formatting omits URL/query data.

Cancellation and dropping a future stop local work, not remote mutations.
No retry engine, transport, runtime or credential discovery is installed.

## Additive v2 invocation policy

`v2_policy` provides a dependency-free policy component for future API v2
orchestration. It does not change `ApiClient::send`, existing CLI arguments,
source-compatible retry behavior, or any authentication/permission policy.
It does not register protocol adapters or enable automatic LRO/paging.

One `InvocationBudget` follows the complete invocation. The host supplies readings
from one monotonic clock, checks the same deadline during credential acquisition
and every exchange/wait, debits each actual transport dispatch with `before_send`,
and records every decoded chunk once. Polling, retries, result retrieval and
paging must not construct fresh budgets. Failed/retried responses count toward
the cumulative decoded-byte budget even when their bodies are discarded.

The `Controls` values for observation, paging and retries are independent.
The host applies observation/paging preferences; only retry admission is enforced
directly by the policy component. A page-scoped opt-out is not a hard-cap error.
`ReplaySafety::DeclaredSafeRead` must come from the exact operation contract,
not the HTTP method, request name, or a provider retry header. Mutations
(including PUT) and unknown raw operations use `ReplaySafety::Never`.
Successful nonterminal polling is not an error retry.

Policy limits default to 240 seconds total, 100 pages, 256 dispatches, 4 MiB per
response, 64 MiB cumulative decoded responses, and two additional retries for
eligible requests. These are isolated library defaults, not activated CLI
defaults or a server-side/network/cost limit. Serialized output and retained
data need their own host-side bounds. A provider-required delay is never
shortened to fit the deadline.

Cancellation and budget failures preserve counters and terminate local policy
progress; they do not establish remote cancellation, rollback or completion.
The eventual orchestration layer must retain native responses, operation handles,
effects and completeness independently. Transport adapters must suppress hidden
retries and redirects for these counters to describe actual dispatches.

## Native v2 observations

`v2_outcome` keeps native HTTP status, safe response headers and exact body bytes
separate from operation state, effects and coverage. It preserves empty and binary
bodies without substituting a string, decoding arbitrary bytes as JSON, merging
unrelated arrays, or rewriting an accepted status to 200.

Response order and duplicate data remain intact. Retention is explicitly bounded;
a failed append retains prior observations and returns an error rather than
clipping a response into valid-looking data. Unknown completeness remains unknown,
and response/page coverage is not an all-results claim. The component does not
interpret provider JSON or classify an operation as complete from HTTP status.

The header policy retains a documented narrow set and exposes the count of
omitted headers. Authentication, cookies and arbitrary Location values are not
published as safe response metadata. Protocol adapters must validate continuation
and monitor destinations separately before sending credentials. Debug output
contains counts/state, not response bodies, header values or operation identifiers.

These are additive SDK building blocks, not a released v2 CLI response schema,
protocol implementation, or live support claim. Existing v1 response rendering
and compatibility fixtures remain unchanged.
