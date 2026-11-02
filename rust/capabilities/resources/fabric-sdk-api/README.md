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
