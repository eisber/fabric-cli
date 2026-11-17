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

## Fixed OCV feedback adapter

`execute_feedback(feedback::Environment, &AccessToken, &[u8])` submits exactly one
authenticated JSON POST, using only these fixed endpoint/scope pairs:

| Environment | Endpoint | Required delegated scope |
| --- | --- | --- |
| `Production` | `https://petrol.office.microsoft.com/v1/feedback/authenticated` | `https://petrol.office.microsoft.com/Feedback.ReadWrite` |
| `Ppe` | `https://petrol-int.office.microsoft.com/v1/feedback/authenticated` | `https://petrol-int.office.microsoft.com/Feedback.ReadWrite` |

The caller serializes the JSON body and acquires its token for exactly
`Environment::scope()`. Tokens are opaque: this adapter cannot verify audience
or permissions and does not acquire or refresh credentials. Empty tokens,
tokens containing control characters, and empty or oversized request bodies fail
before network or timer activity. Request and decoded response bodies are each
capped at 64 KiB (65,536 bytes).

The adapter accepts no URL/header overrides, ignores the general endpoint
selection, and does not extend `ServiceOrigin` or `EndpointBinding`. It delegates
to the existing private execution engine and overall deadline. All statuses,
including 202, 429 and 503, remain raw responses; POSTs are never replayed or
followed by retry waits. Transport/stream failure, response overflow, timeout or
cancellation cannot prove that a submission did not occur. No new transport,
runtime, identity provider or live-service support claim is implied.

## Operation integration

The `OperationExecutor` adapter translates typed request and response fields
and delegates execution and waits. `OperationSession` retains ownership of
credentials, cancellation, operation budgets, polls and result retrieval.
There is no second long-running-operation loop.

Dropping a future stops local waiting and releases owned resources; it does not
roll back a dispatched mutation. Errors retain original status and uncertainty.
No transport implementation, identity provider, CLI, deployment engine or
global runtime is installed by this crate.

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

## Explicit-scope native streaming

`native_stream::{NativeScopeStreamExecutor, NativeStreamPreflight}` adds a
caller-selected HTTPS endpoint, literal HTTPS audience and immutable service
preflight without changing the existing Fabric adapter. The same names are
available in `scoped_native_stream`. This adapter reuses native request
preparation and the existing streaming dispatch, deadline, metadata, body and
uncertainty mechanics.

All generic request guards and caller preflight run before credential access.
Exactly one token request uses the literal scope, then service preflight and
the original remaining budget are checked again immediately before the single
HTTP send. A server timeout that no longer fits must fail without clamping or
dispatch. The caller owns service permissions and query policy; this adapter
does not infer them from an endpoint or parse query languages.

See [explicit-scope streaming](../../../../authoring-docs/scoped-native-stream-http.md)
for injection, redirect/retry assumptions, compatibility and unverified live
authorization boundaries. The compatibility namespace and package identity
remain `fabric_sdk_http::native_stream`, independently of physical SDK layout.
