<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native runtime over the existing Fabric HTTP executor

`fabric_sdk_http::native_runtime::NativeFabricExecutor` composes the public
`fabric_sdk::runtime::NativeExecutor` contract at
`c52a842e082a377e5fc2a179451f07ff521305df` with the existing HTTP engine. It
dispatches requests; it is not a request-description adapter or a new engine.
Direct SDK consumers supply their existing caller-bound `RequestContext` and
`HttpExecutor` to `NativeFabricExecutor::new(context, http)`. Construction performs
no credential or network access and returns `Result<Self, NativeError>`.

The adapter implements `NativeExecutor::execute(&self, &NativeRequest, usize)`.
It also works through `NativeRequest::execute(&executor, max_response_bytes)`.
Both entry points reject a zero response bound without credential or HTTP access.
The adapter is cloneable; its `Debug` implementation prints neither context
metadata, URLs, credentials nor payloads.

## Binding and authentication

The context must select `Audience::Fabric` and `ServiceOrigin::Fabric`. The
context and HTTP executor must contain exactly equal `EndpointBinding` values.
A daily or MSIT context therefore requires that same explicitly configured HTTP
binding; an unconfigured public executor cannot silently execute daily requests.
No endpoint is selected from a request, environment, CLI setting or token.

Requests must use HTTPS, the exact selected host, port 443, no user information
or fragment, and the binding's literal `/v1` path or a descendant beginning
`/v1/`. `/v10`, `/v1-other`, another allowed host, and another port fail with
`InvalidInput`. The existing `EndpointBinding` hostname policy is reused without
expansion. An origin outside that policy fails with `UnsupportedCapability`
before credential access, including loopback native endpoints.

After all URL, header and body preflight, core's `RequestContext::execute` /
`AuthenticatedExecution` primitive borrows the injected `TokenCredential`,
requests only `https://api.fabric.microsoft.com/.default` with no token request
options, and invokes the HTTP seam once. There is no SDK identity implementation,
credential discovery, fallback, subprocess, refresh loop or claim inspection.
Caller account/tenant metadata cannot verify the injected credential's actual
configuration. Acquisition failure maps to the native contract's sanitized
`InvalidResponse`; invalid token syntax maps to `InvalidInput`. Provider error
text is never forwarded. Token acquisition has the existing core lifecycle,
not a newly invented timeout; the caller can drop the whole future.

## Wire behavior and finite work

All six `NativeMethod` variants map explicitly to Azure's corresponding method.
GET and HEAD must declare `NativeEffect::Read`; the adapter cannot put a declared
mutation through the shared engine's replayable GET path.

JSON bodies retain their native representation and insertion order. Raw bytes
are borrowed unchanged, not encoded as a JSON string. Explicit `accept`,
`content_type` and provider headers, including conditional headers, reach
`HttpExecutor::execute_bound`. Its security-header validator is reused before
acquiring a token; the adapter additionally rejects `keep-alive` and
`proxy-connection`. Authorization, Host, cookies, proxy authentication,
hop-by-hop fields and transport length overrides are not caller-controlled.

Preflight bounds are inclusive:

| Surface | Bound |
| --- | --- |
| Serialized URL | 16 KiB |
| Explicit request headers | 64, including Accept and Content-Type |
| Request header metadata | 16 KiB, counting name + value + 4 per field |
| Raw or serialized JSON body | 4 MiB, the existing HTTP request cap |
| JSON depth | 64 values on a root-to-leaf path, counting the root |
| JSON nodes | 65,536 values, counting root and container children |
| Response bytes | Caller's nonzero bound, enforced while streaming |
| Response header metadata | Existing native header contract: 64 fields / 16 KiB |

JSON is first walked iteratively with finite depth and node budgets, before
recursive serialization. Children are budgeted before being added to the walk.
A non-allocating bounded counting writer measures actual escaped JSON wire
bytes. Only a body within the 4 MiB cap receives a serialized buffer allocation.
This rejects large strings, escaped expansion, deep trees and very wide arrays
before credential access, without first building an oversized serialized body.
Request budget failures are `Limit`; malformed or forbidden fields are
`InvalidInput`. Accept and Content-Type count toward the header budget even for
bodyless requests.

The HTTP response's actual status, bounded raw body, optional Content-Type and
headers are returned without interpreting service success or following Location.
202 remains acceptance, not completion. Error statuses and redirect statuses are
returned as observed; no service poll, redirect request or cancellation request
is started. Response headers can contain sensitive data and are not for logging.

## One execution engine and uncertainty

Only the existing `execute_bound` engine performs HTTP execution. The caller's
existing `ExecutionTimer` and `ExecutionOptions` retain the shared overall HTTP
deadline, decoded streaming cap, finite GET status retries, and bounded backoff.
Read retries do not reacquire credentials or restart the deadline. HEAD and all
write methods execute once, including 429, 503, transport and streaming errors.
No second timer, retry loop, runtime, TLS implementation or HTTP client is
created. The caller-supplied Azure HTTP client must already disable automatic
retries and redirects, stream bodies, and release local work when dropped, as
required by `HttpExecutor`.

For GET/HEAD, deadline expiry is `TimedOut`, a response byte/header bound failure
is `Limit`, and transport, invalid retry metadata or exhausted retries are
`InvalidResponse`. For other methods, transport loss, HTTP deadline expiry,
response streaming bounds or invalid response metadata return `OutcomeUnknown`:
the adapter must not imply that a submitted mutation failed or can be replayed.
This conservative classification also applies to a POST declared as a
provider-defined read. The HTTP seam does not expose a dispatch receipt, so
even a write deadline that happens to expire before wire dispatch is
conservatively uncertain. Preflight and credential failures remain distinguishable
and never imply dispatch.

Dropping acquisition, dispatch or response-stream futures is local cancellation
only. This API has no separate cancellation signal and cannot return an error to
a caller that dropped its future. It does not roll back remote work or send a
remote cancel request. A caller retaining a service receipt owns reconciliation.

## Deliberate native and MWC boundary

This closes the Fabric JSON/native-runtime execution gap only for requests under
an explicitly selected existing Fabric binding. It does not establish
authentication for arbitrary Livy/Jupyter/native origins, MWC/workload clusters
such as `*.pbidedicated.windows.net`, or a custom loopback service. Non-Fabric
service contexts are also `UnsupportedCapability`, even Power BI with the same
token audience. There is no new MWC authentication decision, bootstrap,
discovery, origin whitelist or network host for those capabilities. Their
caller-owned authorized adapter remains a separate explicit gap.

## Integration and offline evidence

The bounded lane owns only the adapter, its integration tests and this document.
The coordinator must add `pub mod native_runtime;` to the HTTP crate's `lib.rs`
and promote the already workspace-managed `fabric-core` and `serde_json` entries
from dev dependencies to normal dependencies. These are existing core/JSON
dependencies, not an SDK-to-identity edge. No manifest, lock, facade, core,
identity, CLI or engine change is part of this lane.

`tests/native_runtime.rs` imports the final public module and denies
`unnameable_test_items`. Its Azure HTTP client, credential, timer and streams
are offline fakes surrounding the production execution path. It covers exact
requests/scopes/headers/media; GET 200 and POST 202; all finite methods and raw
bytes; shared GET retry/deadline policy; no mutation replay; lost responses;
streaming caps/deadlines; local future disposal; coherent endpoints/audiences;
foreign/native/MWC origins; forbidden headers; zero limits; exact and excessive
URL/header/body budgets; escaped JSON size and depth/node budgets.

Compilation, Clippy and test execution are not claimed for this lane: the host
has no available Rust toolchain or installed WSL distribution, and the
coordinator-owned module/dependency integration is intentionally absent. After
integration, run the focused offline test and all-target check using the pinned
toolchain and the normal validation lease:

```text
cargo test --offline --locked -p fabric-sdk-http --test native_runtime
cargo check --offline --locked -p fabric-sdk-http --all-targets
cargo clippy --offline --locked -p fabric-sdk-http --all-targets -- -D warnings
cargo fmt --all -- --check
```

These are required follow-up checks, not completed evidence. No live service,
credential discovery, authentication or platform compatibility claim follows
from these offline contracts.
