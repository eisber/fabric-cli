<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# OneLake shared HTTP adapter

`OneLakeHttpExecutor::new(Arc<HttpExecutor>)` implements the focused
`OneLakeExecutor` contract using the shared executor's explicit
`execute_bound(PublicOrigin::OneLake, ...)` boundary. The existing
`execute_fabric` API is not relaxed.

The consumer supplies the Azure HTTP client, timer and execution options to
`HttpExecutor`, and separately injects an Azure `TokenCredential` and explicit
`Audience::OneLake` selection into `OneLakeClient`. Construction does no I/O.
Only the fixed public DFS host is used; the required scope is
`https://storage.azure.com/.default`. The adapter installs no authentication
backend, runtime, HTTP/TLS/proxy stack, paging, retries or operation poller.

The focused SDK's GET list/read and HEAD properties requests retain their exact
URL, byte limit, body/headers and token. Errors map explicitly to the focused
SDK's execution categories. Caller cancellation drops the shared exchange, and
the shared timer continues to own deadline/read-retry policy. HEAD remains
single-attempt, exactly as the existing shared policy specifies; GET status
retries do not reacquire credentials or recompose the resource request.

The synthetic public consumer performs actual directory listing (including
continuation), `_delta_log` file reading and HEAD property retrieval through the
focused SDK, this adapter and the real shared HTTP engine. It does not implement
table schema/Delta interpretation: those remain with the table owner.
Additional cases exercise exact streaming caps, GET retries, HEAD behavior,
credential selection/failure, deadlines, service errors and cancellation/drop.

Source behavior is pinned to Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`client/fab_api_onelake.py` and `client/fab_api_client.py`.
The shared execution prerequisite is exact
`ef17b8096fae51ca94d701cdec11f101902868a1`.
Its explicit origin/header/body bounds and no ambient credentials/no mutation
replay policy are preserved, not bypassed. Private links, sovereign hosts,
OneLake mutation verbs, CLI front-door integration and live proof are not
established by this read adapter.
