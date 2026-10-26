<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit SDK endpoint and request context

`fabric-core` owns immutable `EndpointBinding` and `RequestContext` values.
Service clients, the existing HTTP executor and the existing operation session
compose these values; no SDK library reads environment variables, selects an
identity backend, caches tokens, initializes a runtime or adds another HTTP
engine or poller.

## Public API

Existing `Client::new`, `WorkspaceClient::new`, `ItemClient::new`,
`HttpExecutor::new` and `OperationSession::new` retain their public-cloud
defaults. Their old signatures and public request/response option literals are
unchanged. Explicit configuration is additive:

```rust,no_run
use std::sync::Arc;
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, EndpointError,
    RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk::Client;
use fabric_sdk_items::ItemClient;
use fabric_sdk_workspaces::WorkspaceClient;

fn clients(
    credential: Arc<dyn TokenCredential>,
) -> Result<(Client, WorkspaceClient, ItemClient), EndpointError> {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com")?;
    let selection =
        CredentialSelection::new("explicit-account", "explicit-tenant", Audience::Fabric)
            .expect("nonempty explicit selectors");
    let context = RequestContext::with_endpoint(credential, selection, endpoint);
    Ok((
        Client::with_context(context.clone()),
        WorkspaceClient::with_context(context.clone()),
        ItemClient::with_context(context),
    ))
}
```

Compose the same endpoint with `HttpExecutor::new(...)? .with_endpoint(binding)`.
The executor independently checks the selected HTTPS origin before sending
Authorization; configuring only a service client does not relax a default
public-only executor. `OperationSession::with_context(&context, executor,
limits, cancelled)` binds initial requests, polling and result retrieval to
that context. `FabricOperation` and `ResourceOperation` still describe relative
typed routes, not arbitrary network destinations.

Cloning a context clones the original credential `Arc`, not a token, provider,
login session or cache. Each authenticated request acquires its token through
one shared `RequestAuthentication::execute` primitive. It checks cancellation,
explicit origin/audience and URL before acquisition, then cancellation-wraps
acquisition and execution. Service adapters retain their own typed status,
response, partial-progress and error policies. HTTP streaming caps, deadlines,
GET retries and mutation no-replay remain in `fabric-sdk-http`; operation
budgets, reconciliation and uncertain-side-effect evidence remain in
`fabric-sdk-operations`.

Implementers of `AuthenticatedExecution` can return the documented
`AuthenticatedFuture<'a, Response, Error>` alias. It preserves the same boxed
`Send` future and operation-specific result types without another execution layer.

## Configuration boundary and fixed audiences

The pinned Python baseline is
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Its
`core/fab_constant.py` reads the four hostname overrides at import;
`client/fab_api_client.py::do_request` constructs service URLs and separately
requests the selected scope. `tests/test_commands/test_find.py` and operation
cassettes identify `dailyapi.fabric.microsoft.com`. There is no inferred
`api.msit` host or daily token audience.

The CLI configuration owner can map these inputs to `EndpointBinding::with_host`.
The SDK accepts hostname strings, **not URLs**, and performs no environment read.

| CLI input | Explicit `ServiceOrigin` | Public default host | API path | Fixed scope |
| --- | --- | --- | --- | --- |
| `FAB_API_ENDPOINT_FABRIC` | `Fabric` | `api.fabric.microsoft.com` | `/v1` | `https://api.fabric.microsoft.com/.default` |
| `FAB_API_ENDPOINT_ONELAKE` | `OneLake` | `onelake.dfs.fabric.microsoft.com` | none | `https://storage.azure.com/.default` |
| `FAB_API_ENDPOINT_AZURE` | `Arm` | `management.azure.com` | caller's ARM route/version query | `https://management.azure.com/.default` |
| `FAB_API_ENDPOINT_POWER_BI` | `PowerBi` | `api.powerbi.com` | `/v1.0/myorg` | `https://api.fabric.microsoft.com/.default` |

`ServiceOrigin` is also reexported under the existing HTTP `PublicOrigin` name.
Neither the request host nor an opaque token is used to infer an audience.
Power BI deliberately retains Python's Fabric scope mapping.

## Safety and compatibility boundaries

Hostname overrides use validated ASCII DNS labels with exact-host or
dot-boundary suffix matching for `fabric.microsoft.com`, `powerbi.com` and
`management.azure.com`. URL schemes, user information, ports, path/query/fragment,
controls, whitespace, IP addresses, percent-encoded hosts, empty/dot labels and
trailing dots are not hostname syntax. This is intentionally stricter than
Python's regex and slash-splitting validator; it does not reproduce those
permissive parsing quirks.
Python's broader `management.<label>.<label>` pattern is not sovereign-cloud
support in this SDK; public ARM routing remains explicitly bounded.

Requests and active continuation/LRO links must use HTTPS, port 443 and the
**exact selected host** with no credentials or fragment. Collection continuation
paths remain exact, with the opaque token encoded onto the original collection
URL. A public link from a daily-bound client is foreign, as is a different
Microsoft host. There is no automatic redirect, authority change, new cloud
claim or credential forwarding to arbitrary advertised `Location` values.
Default strict status handling, source-only GET-202 opt-in and GUID policy are
unchanged.

Python also derives workspace private-link URLs in
`_transform_workspace_url_for_private_link_if_needed` and propagates advertised
operation hosts. This slice does not discover or derive private-link hosts,
switch hosts during an operation, or claim private-link parity. A caller can
explicitly supply an eligible hostname binding; its continuation/poll/result
chain must remain on that selected host.

## First vertical and migration matrix

| Consumer | Context support in this slice | Remaining work |
| --- | --- | --- |
| Workspace LIST and enriched metadata GET | Direct client and facade share context, authenticated execution and selected-host continuation validation | CLI configuration/composition |
| Generic item LIST/GET and specialized metadata GET | Direct client and facade share context and selected-host continuation validation | CLI configuration/composition |
| Shared HTTP executor | Selected Fabric endpoint in typed adapters; explicit bound seam supports all four origin/scope pairs | Each additional family must supply the same binding |
| Shared operation session | Explicit-context initial, poll and result routing; unchanged existing poller | Additional family callers must choose `with_context` |
| Resource operation path/query encoding | Relative route encoding has no service hostname dependency | No change to source query semantics |
| Shared token pager | Caller-supplied collection remains the origin/route authority | Context-enabled callers supply a binding-built URL; no new pager |
| Capacity and folder focused clients/facade | Existing public-only construction remains | Migrate request context and continuations in a separate bounded slice |
| Notebook facade/deployment | Existing public-default composition remains | Adopt explicit operation context without changing Notebook policy |
| OneLake focused SDK/HTTP, jobs, transfer, access and storage family adapters | Not migrated here | Coordinate released family changes and explicit binding adoption with their owners |
| Native CLI environment configuration | Not implemented here | Central CLI owner maps all four hostname variables and injects contexts/executors |

The additive external `fabric-sdk-http/tests/endpoint_context.rs` consumer
exercises default/public and daily workspace LIST/metadata GET, item LIST/GET,
continuation requests and actual operation result retrieval through the existing
HTTP request adapter. It also covers all four explicit hostname configuration
shapes, fixed scopes, shared credential identity and rejection before
credential/send where applicable. These are offline synthetic HTTP boundaries,
not live service, native CLI environment, private-link or whole-SDK parity.
Existing independent source/oracle fixtures are not rewritten.
