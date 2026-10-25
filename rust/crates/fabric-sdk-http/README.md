<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared handwritten SDK HTTP execution

`fabric-sdk-http` adapts one caller-owned `azure_core::http::HttpClient` and
`ExecutionTimer` to the workspace, Notebook and generic item executor interfaces.
The SDK facade does not depend on this adapter; consumers can continue supplying
their own executors. The crate belongs to the SDK layer and has no identity,
deployment, CLI, transport implementation, or global runtime dependency.

The additive `FolderExecutor` adapter supports folder LIST and GET through that
same private engine, without another retry/deadline/streaming implementation.
See the [folder SDK](../fabric-sdk-folders/README.md) for query/model semantics,
typed errors and explicit safety exceptions. External folder consumer tests
cover both methods, shared retries/deadlines, partial errors, exact/+1 4 MiB
streaming and cancellation/drop. This adds no CLI handler or production backend.

`HttpExecutor::new` only stores explicitly injected implementations and validates
finite options. Default options are a 240-second **overall HTTP execution**
deadline and a 60-second maximum retry/poll delay. Every response is streamed
into a bounded buffer; limit failures retain workspace progress and Notebook
mutation uncertainty. Header names are normalized by Azure's header type.
Status, headers, empty 202 responses, and exact request/response bytes remain
available to the existing operation policies.

`HttpExecutor::execute_fabric(method, url, token, body, limit)` is an additive
public adapter seam for further handwritten Fabric service families. It calls
the same engine as the existing typed adapters, including the decoded-byte cap,
overall deadline, GET-only retries and mutation no-replay behavior. It retains
raw status/headers/body: a table-load 202 is still only accepted, not completed.
The response uses the existing shared `WorkspaceResponse` wire shape without
performing a workspace operation.

The service client acquires the explicitly selected Fabric token and interprets
service-specific responses. This seam accepts only
`https://api.fabric.microsoft.com:443`; storage, ARM, Power BI and arbitrary
origins remain rejected before transport I/O. It does not add custom headers or
infer a token audience. Dropping the future cancels local work; it is not rollback.
Keep focused service executor traits independent of this facade-dependent
adapter to avoid a facade -> service -> HTTP -> facade dependency cycle.
Thin composition adapters must not reproduce its retry or deadline policies.
`HttpExecutor::wait_for(delay)` exposes the same bounded asynchronous timer for
those adapters, without shadowing `NotebookExecutor::wait` or replacing a
whole-job deadline. Zero delays do not start a sleep; excessive delays fail.

The HTTP client must provide decoded response chunks, disable redirects and
its own automatic retries, and release local work when its future is dropped.
The adapter does not inspect or override an opaque client's configuration.
Use established HTTP libraries: this is not an HTTP/TLS implementation, default
client factory, or approval of proxy/certificate settings. Only the existing
public-cloud Fabric origin is accepted; no credentials are discovered.

Consumers can explicitly select the separate
[`fabric-sdk-transport`](../fabric-sdk-transport/README.md) crate's `hyper-rustls`
feature for a reusable native client and `TokioTimer`. That optional backend
enforces its own redirect/replay, trust/proxy and bounded decompression
configuration; this executor and the SDK facade do not depend on it.

The timer must use the consumer's asynchronous runtime, wake the future after
the requested delay, and cancel the local wait when dropped. Do not implement
it with blocking sleeps. Dropping a request future cancels local work, not a
server-side operation. Deadline/cancellation checks also remain responsive to
streams that continually yield immediately ready empty chunks.

## Python source mapping and compatibility boundary

Reference source: `src/fabric_cli/client/fab_api_client.py` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

| Source behavior | Implemented boundary |
| --- | --- |
| `_get_session`: reusable Session/HTTPS adapter | Reusable injected HTTP client; no global session or ambient configuration |
| GET status retries: total=3, factor=1, 502/503/504 | Up to four requests; backoff 0, 2, 4 seconds; nonzero integer Retry-After takes precedence |
| urllib3 Retry-After statuses 413/429/503 | GET retry classification includes 413/429 when Retry-After is present |
| `do_request`: four headerless 429 responses | Four outer iterations and ten-second waits, then explicit retry exhaustion; adapter budget restarts on each iteration |
| Fabric token and JSON Content-Type | Uses the operation-supplied token; sends exact body/query and JSON Content-Type |
| `_poll_operation` sleeps asynchronously in Rust | Notebook executor waits use the caller timer and bounded policy; operation/result logic stays in `fabric-sdk` |
| Request timeout defaults to 240 seconds | Rust uses an overall execution deadline, including reads and retry waits, not Requests' per-attempt socket timeout |

The seven immutable [retry reference cases](../../tests/compatibility/http/README.md)
are checked against the real Python HTTPS retry factory/urllib3 and this
adapter. Other tests exercise workspace traversal and Notebook
202 -> Running -> Succeeded -> actual result retrieval through the shared
executor. The tests are offline, with synthetic HTTP clients and virtual time.

This is **not full Python parity**. Explicit remaining differences/gates:

- No automatic mutation retry, including POST 429, per the approved reconciliation
  rule. Python's outer loop retries 429 for mutations; a lost/uncertain mutation
  must not be blindly replayed here.
- No I/O/connect retry or redirect following; HTTP-date Retry-After is explicitly
  rejected instead of silently interpreted. Negative/overflowed intervals fail.
  Retry delays above the caller's maximum fail before waiting.
- Body/total-time bounds and fixed-origin validation are deliberate Rust safety
  requirements, not observed Python guarantees. An underlying transport can
  allocate a chunk before handing it to this adapter; its own buffering must
  also be bounded.
- This crate supplies no production HTTP/TLS factory. The separately selected
  transport has bounded offline proxy/trust/decompression evidence, not general
  enterprise certification. CLI User-Agent/skill headers, sovereign clouds,
  private links, OneLake/ARM API execution and identity backends remain outside
  this slice. There are no new CLI commands.
- The shared executor does not replace the existing SDK's operation-specific
  success-status, decoding, continuation, error, or LRO rules.

No private specifications, generator output, runtime bridge, or production
credential fallback is included.

The additive item adapter lives in `src/items.rs` and calls the same private
execution method; it does not copy headers, retries, streaming or deadline code.
Its facade integration tests exercise item LIST pagination and GET recovery
through shared GET retries, response limits and virtual deadlines. See the
[item source/compatibility contract](../fabric-sdk-items/README.md).

## Capacity adapter

`HttpExecutor` also implements the focused `CapacityExecutor` through the private
shared execution method and existing error mapping. It installs no second HTTP,
retry, deadline, TLS, timer, or credential path. The additive adapter preserves
capacity continuation/header/error semantics in the service crate and retains
explicit partial progress on HTTP failures. Public offline consumer tests cover
encoded continuation, source error precedence, GET gateway/429 retries and
exhaustion, exact/+1 byte bounds, and deadline mapping. See the
[capacity SDK boundary](../fabric-sdk-capacities/README.md); no ARM or CLI
capacity support is implied.
