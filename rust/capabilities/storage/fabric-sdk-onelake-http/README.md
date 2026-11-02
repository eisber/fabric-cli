<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# OneLake HTTP adapter

`OneLakeHttpExecutor::new(Arc<HttpExecutor>)` implements `OneLakeExecutor`
through the shared `execute_bound(PublicOrigin::OneLake, ...)` interface.
Fabric-only HTTP methods remain restricted to Fabric.

Supply the HTTP client, asynchronous timer and execution options separately.
`OneLakeClient` receives an explicitly injected credential with
`Audience::OneLake`; storage requests use
`https://storage.azure.com/.default`.

The adapter preserves validated URLs, methods, bytes, safe headers, tokens and
streaming limits. Execution failures map to the focused SDK's typed categories.
The shared engine owns deadlines and GET retries; HEAD and mutations remain
single-attempt. No second transport, pager, authentication backend or poller
is installed.

Write results retain create, append and flush stages. HTTP 202 stays accepted,
not completed. Cancellation preserves possible side effects and never replays
or rolls back a write.

Offline consumers exercise listing, continuation, file reads, properties and
writes through the complete SDK/adapter/shared-HTTP chain. Delta schema
interpretation and table orchestration belong to the table SDK.
