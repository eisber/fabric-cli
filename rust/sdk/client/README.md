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
| `metadata` | Explicit primary workspace or generic item metadata, without enrichment |
| `representations` | Native multipart definition/part and OneLake file reads |
| `observe` | Bounded observation of existing operation and item-job handles |
| `documents` | Injected served-docs provider and bounded, coverage-aware graph |
| `environment_publish` | Explicit staged Environment publication, protected receipts and version-correlated observation |
| `folders` | Folder listing and lookup with explicit query options |
| `capacities` | Fabric capacity listing |
| `notebooks` | Prepared Notebook definitions and publication |
| `runtime` | Validated caller-owned Notebook/Livy runtime request and execution contracts |
| `notebook_runtime` | Native Notebook content/session/kernel requests and offline protocol state |
| `notebook_observability` | Explicit public/workload monitoring, logs and Notebook resource requests |
| `livy` | Explicit Fabric/native Livy profiles, owned handles, JSON results and bounded uploads |
| `livy_arrow` (opt-in) | Raw framed-v2 decoding, request contracts and in-memory replay policy |
| `sql_databases` | Typed SQLDatabase metadata GET and single-page LIST requests |
| `sql_definitions` | Bounded native DACPAC/SQL project definition validation and encoding |

`get_item_metadata` preserves the insertion-ordered response object separately
from the typed `Item` representation. Workspace enrichments retain typed optional
omissions rather than fabricating empty fields.

The additive `MetadataHost` composes only the explicitly addressed metadata read,
with caller-selected response and compact-result limits. It does not select a
definition from query text. `get_workspace_primary_metadata` skips all optional
enrichments without changing the existing enriched API. See
[explicit metadata retrieval](../../docs/explicit-get.md) for the shared read
contract and the separate CLI local-file adapter.

Related focused crates provide jobs, OneLake, access, catalog, raw API execution
and tables. Use them directly for their service-specific contracts.
The [artifact-first table adapters](../../docs/artifact-first-tables.md) are
available directly from the focused `fabric-sdk-tables` crate under parent-owned
modules. They are not reexported here: that edge would cycle through the shared
HTTP test consumers. Their native executors must be supplied by the caller;
discovery is not proof of a configured CLI/MCP host.

Environment publication uses one bodyless release-version POST and the shared
operation engine. Immediate host approval and explicit limits are required.
An HTTP 200 running native state is incomplete, not successful publication;
`targetVersion` is not a generic operation handle. See
[Environment native publishing](../../docs/environment-native-publish.md) for
permissions, concurrency, lifecycle and unsupported boundaries.

The SQL modules reexport the focused SDK leaves. Metadata requests use the
caller's shared executor; they do not connect to SQL, retrieve definitions
implicitly, or register a CLI command. Definition validation preserves native
parts and bytes without granting filesystem or deployment authority. These
modules do not implement database mutations, TDS queries, or a full backup.

## Execution and safety

Callers supply executors, finite limits and cancellation futures. The facade
installs no transport, TLS backend, runtime, identity implementation, global
logger or ambient credential discovery.

The optional [shared HTTP executor](../http/executor/README.md) implements
focused interfaces over one request policy. The separate
[native transport](../http/transport/README.md) selects HTTP/TLS explicitly.

Paging preserves order and partial-error evidence. Mutations retain acceptance,
operation IDs and uncertain effects. Failed delivery, cancellation or lost
responses never authorize automatic replay.

Runtime requests keep native HTTP methods, effects, JSON bodies, explicit
binary media and provider headers separate. `NativeEndpoint` accepts an
explicit HTTPS service base, or loopback HTTP for offline transports; it does
not discover a Notebook workload, acquire an MWC token or switch identity.
Owner identifiers are validated before dispatch. Authorization, redirects,
timeouts and streaming limits remain the injected executor's responsibility.
The request wrapper additionally checks a nonzero response limit and dispatches
exactly once. It preserves the actual HTTP status: 202 is not workload success.

These are additive SDK contracts, not registration of a v2 CLI command or proof
that a native extension is available on every Fabric gateway. No default
authentication, subprocess bridge, background daemon or remote cleanup is
installed. Debug output omits URLs, resource identifiers and executable content.

The `livy-arrow` feature enables the raw Arrow protocol module and its existing
workspace SHA-256 dependency. It does not install an HTTP streaming host or a
native typed IPC/decompression implementation. Framed bytes are not a single
standard Arrow IPC file. Native-only Livy extensions are rejected on the modeled
Fabric profile; profile selection never silently changes interpreter or service.

Notebook content edits replace the whole notebook with explicit unguarded-write
admission; local snapshot selectors are not server concurrency protection.
Notebook Jupyter/WebSocket execution, scheduler jobs and Lakehouse Livy REST
statements are separate capabilities. The SDK protocol state machine does not
claim that a real WebSocket was connected or that Spark started.

From `rust/`, run the synthetic injected-credential example:

```text
cargo run --offline --locked -p fabric-sdk --example injected_credentials
```
