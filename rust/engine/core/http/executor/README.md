<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared SDK HTTP execution

`HttpExecutor` adapts a caller-owned `azure_core::http::HttpClient` and
`ExecutionTimer` to focused SDK executor interfaces. Clients can supply other
executors without depending on this adapter.

## Execution policy

`HttpExecutor::new` validates finite options without sending requests.
Default execution uses a 240-second overall deadline and a maximum 60-second
retry/poll delay. Responses are streamed into bounded buffers.

GET retries share one policy. Mutations are never automatically replayed.
Raw status, headers and bytes remain available to service-specific completion
policies; HTTP acceptance is not interpreted as completed work.

`execute_fabric` exposes the same execution engine to Fabric service adapters.
`wait_for` uses the supplied asynchronous timer; zero delays do not sleep and
excessive delays fail.

## Endpoint binding

`with_endpoint(EndpointBinding)` selects an explicit validated service endpoint.
Its binding must match the service client's request context. Fabric-only methods
reject storage, ARM and Power BI selections before transport I/O.

`execute_bound` supports explicitly selected public-origin families:

| Origin | Default HTTPS host | Audience |
| --- | --- | --- |
| Fabric | `api.fabric.microsoft.com` | Fabric |
| OneLake | `onelake.dfs.fabric.microsoft.com` | Storage |
| ARM | `management.azure.com` | ARM |
| Power BI | `api.powerbi.com` | Fabric |

Requests cannot infer credentials or audiences from URLs. Origin mismatches,
userinfo, fragments, non-HTTPS ports and unsafe headers are rejected.
Authentication, host, proxy credentials, cookies and hop-by-hop headers cannot
be overridden. URL, body and header allocations remain bounded.

## Operation integration

The `OperationExecutor` adapter translates typed request and response fields
and delegates execution and waits. `OperationSession` retains ownership of
credentials, cancellation, operation budgets, polls and result retrieval.
There is no second long-running-operation loop.

Dropping a future stops local waiting and releases owned resources; it does not
roll back a dispatched mutation. Errors retain original status and uncertainty.
No transport implementation, identity provider, CLI, deployment engine or
global runtime is installed by this crate.

The opt-in `notebook-workload` feature adds a separate, explicitly selected
Notebook read adapter with cryptographically verified Fabric/PBI principal
continuity. See the [Notebook workload read foundation](../../../../authoring-docs/notebook-workload-foundation.md)
for credential opt-in, exact origins and routes, bounded token exchange, and
qualification gaps. It does not change `NativeFabricExecutor` or legacy Power BI
REST routing, and it adds no Fabric CLI routes or platform-readiness claim.

## Pull-based native Fabric streaming

`native_stream::NativeFabricStreamExecutor::new(context, http)` implements the
SDK's `NativeStreamExecutor` using the same explicit Fabric credential and
endpoint policy as `NativeFabricExecutor`. `open` returns the actual HTTP body
without collecting or polling it. Every method, including GET and inline POST,
dispatches once; this stream path does not enter the buffered retry loop.

`NativeStreamLimits` bounds exposed fragments and total HTTP body bytes before
copying. One absolute `NativeDeadline` covers credentials, headers, pending body
reads and caller pauses. The existing injected timer races pending work, and
supplying a fresh read deadline cannot extend the opening deadline. EOF,
errors, dropping a body or cancelling a polled read dispose local resources,
never remote work. Actual statuses/media/headers and mutation uncertainty remain
explicit.

See [native streaming boundaries and evidence](../../../../authoring-docs/native-stream-http.md)
for disposal semantics, metadata bounds, offline instrumented tests and the
Azure header-duplicate visibility limitation. No new transport/runtime/auth
engine, native/MWC origin policy or live-service support claim is implied.
