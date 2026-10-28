<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric SDK

`fabric-sdk` is the facade for Fabric workspace, item, folder, capacity and
Notebook APIs. It delegates to focused service crates and reexports shared wire
and error types so applications can choose either interface.

## Construct a client

`Client::new` stores an injected `Arc<dyn TokenCredential>` and explicit
`CredentialSelection` without acquiring tokens or performing I/O.
`Client::with_context(RequestContext)` accepts a validated endpoint binding.
Selection metadata is not proof of account, tenant or token claims.

Configure executor bindings consistently with the client. Initial requests,
active continuations, operation polls and result retrieval must remain on the
selected endpoint with the appropriate audience.

## Service APIs

| Module | Operations |
| --- | --- |
| `workspaces` | Workspace listing, ordered metadata and optional enrichment |
| `items` | Item listing, generic lookup and finite collection-specific metadata |
| `folders` | Folder listing and lookup with explicit query options |
| `capacities` | Fabric capacity listing |
| `notebooks` | Prepared Notebook definitions and publication |

`get_item_metadata` preserves the insertion-ordered response object separately
from the typed `Item` representation. Workspace enrichments retain typed optional
omissions rather than fabricating empty fields.

Related focused crates provide jobs, OneLake, access, catalog, raw API execution
and tables. Use them directly for their service-specific contracts.

## Execution and safety

Callers supply executors, finite limits and cancellation futures. The facade
installs no transport, TLS backend, runtime, identity implementation, global
logger or ambient credential discovery.

The optional [shared HTTP executor](../../infrastructure/http/fabric-sdk-http/README.md) implements
focused interfaces over one request policy. The separate
[native transport](../../infrastructure/http/fabric-sdk-transport/README.md) selects HTTP/TLS explicitly.

Paging preserves order and partial-error evidence. Mutations retain acceptance,
operation IDs and uncertain effects. Failed delivery, cancellation or lost
responses never authorize automatic replay.

From `rust/`, run the synthetic injected-credential example:

```text
cargo run --offline --locked -p fabric-sdk --example injected_credentials
```
