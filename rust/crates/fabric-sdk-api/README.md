<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit API execution

`ApiClient::new` accepts an Azure `TokenCredential`, explicit
`CredentialSelection`, and transport-neutral `Arc<dyn Executor>`. Construction
performs no I/O. `Request::fabric` validates route segments under the fixed public
Fabric v1 origin. `send` returns raw status/body/headers, including failures;
`require_status` is the explicit completion/error boundary. No accepted response
is automatically considered complete.

`collect` adapts the released shared `TokenPager` for the access/catalog families.
It preserves order and duplicates, handles header-before-body continuation tokens,
checks origins/paths/cycles, and retains exact partial progress on error.
Catalog continuation replaces the search body with `continuationToken/pageSize`.
All calls take caller-owned cancellation futures; dropping a call cancels local
work, not remote mutations. No retry/TLS/LRO engine or runtime is installed here.

SDK crates depend only on SDK/core interfaces, not HTTP/facade/identity/CLI.
The current actual shared-HTTP adapter is `ResourceHttpExecutor` in the CLI
composition crate. This avoids a facade -> service -> HTTP -> facade cycle.
`Origin`, `RawRequestOptions` and `Request::raw` bind the finite public source
origins before credentials: Fabric, OneLake, ARM, and Power BI (the last explicitly
uses the pinned source Fabric scope). URI query values override supplied params
in place, with Python `urlencode` punctuation. Request body/URL/header bytes and
header count are finite; authentication/host/proxy/hop-by-hop headers cannot be
overridden. Arbitrary/sovereign/private hosts are not inferred.

`Executor::supports` must explicitly advertise extended requests. Its default
allows only Fabric without custom headers. Missing transport capability returns
`UnsupportedRequest` **before token acquisition**, not an incorrect-audience
request or success-shaped stub. `ResourceHttpExecutor` now implements the released
`execute_bound` seam for all four fixed public origins and safe custom headers.
The original `execute_fabric` and its typed consumers remain Fabric-only.
See [access compatibility](../../docs/access-compatibility.md).

The exact `e850b32437e0e9171e8f097bfbddb93d57986c3f` field selector preserves
`accessDetails`/`domains` response bytes and caps without root-key reserialization.
Only endpoint-specific decoding and serialized source-text observation remain
here; continuation traversal, yield, limits and cancellation use `TokenPager`.

`json_body` measures scalar representations through `fabric-core-json` before
allocating a complete request. Scalar scratch allocation is finite (at most six
times the request bound); final encoded bytes retain Python ASCII escaping,
spaces, order and the shared numeric policy. The same released encoder formats
merged ACL `source_text`, preserving source substring matching. Wide integers,
nonfinite values and unpaired surrogates remain explicit unported JSON domains.

`send_completed` reconciles an actual Fabric 202 using the released
`OperationSession::reconcile_accepted` without resending the mutation. `Executor`
provides its existing asynchronous timer. Errors retain operation ID, raw service
failure and mutation uncertainty; in-flight cancellation never implies rollback.
Headerless acceptance is not completed work. Raw `send` never implicitly polls.

`QueryDispatchTrace` is an opt-in caller-owned last-dispatch observation.
`with_query_dispatch_trace` never installs automatic query carry; ordinary clients
remain stateless. Its snapshot exposes the validated request actually handed to
the executor after credential acquisition, including on later errors, rather than
the next unconsumed response token. Before-dispatch failures leave it unchanged.
No body/bearer token/header/history is retained; Debug omits the URL/query. See
[dispatched-query compatibility](../../docs/query-dispatch-compatibility.md)
for the exact logical-executor versus transport-delivery boundary and source
dictionary lifetime distinctions.
