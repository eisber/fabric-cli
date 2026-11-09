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
| `dataagents` | Typed public DataAgent metadata, exact definitions and explicit-stage settings/datasource pages |
| `representations` | Native multipart definition/part and OneLake file reads |
| `variable_libraries` | Metadata-first VariableLibrary reads and explicitly disclosed native source/value-set shapes |
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
[explicit metadata retrieval](../../authoring-docs/explicit-get.md) for the shared read
contract and the separate CLI local-file adapter.

Related focused crates provide jobs, OneLake, access, catalog, raw API execution
and tables. Use them directly for their service-specific contracts.
The [artifact-first table adapters](../../authoring-docs/artifact-first-tables.md) are
available directly from the focused `fabric-sdk-tables` crate under parent-owned
modules. They are not reexported here: that edge would cycle through the shared
HTTP test consumers. Their native executors must be supplied by the caller;
discovery is not proof of a configured CLI/MCP host.

Environment publication uses one bodyless release-version POST and the shared
operation engine. Immediate host approval and explicit limits are required.
An HTTP 200 running native state is incomplete, not successful publication;
`targetVersion` is not a generic operation handle. See
[Environment native publishing](../../authoring-docs/environment-native-publish.md) for
permissions, concurrency, lifecycle and unsupported boundaries.

The SQL modules reexport the focused SDK leaves. Metadata requests use the
caller's shared executor; they do not connect to SQL, retrieve definitions
implicitly, or register a CLI command. Definition validation preserves native
parts and bytes without granting filesystem or deployment authority. These
modules do not implement database mutations, TDS queries, or a full backup.

### Variable Library reads

`variable_libraries::read_metadata` is the metadata-only entry: one generic item
GET, strict selected workspace/item UUID and `VariableLibrary` response checks,
and no source or reference enrichment. An absent active-set property stays absent;
the SDK does not invent a default name or infer effective consumer values.

`read_source` requires `SourceDisclosure::ExplicitlyRequested`. Even value-set
shape inspection retrieves the whole native definition, including default and
inactive values. It reuses `representations::read_definition` with the exact
`VariableLibraryV1` format, generic item `getDefinition` route and caller-owned
HTTP/credential/cancellation policies. HTTP 202 returns an accepted handle, not
an empty definition, and no new poller or metadata fallback runs. Complete
operation-result bytes use `SourceRead::from_completed_result` under the same
explicit disclosure and byte/part bounds.

The completed source keeps original parsed multipart data, part order and
encoded payload bytes behind `native_definition()`. `shape()` exposes variable
names/types/default presence, alternate-set names/override counts and explicit
display ordering, without values, notes or descriptions. Shapes are source
structure, not a resolved runtime snapshot. Omitted versus explicitly empty
ordering/override lists remain distinct. Incidental Debug/errors redact
source, service diagnostics, IDs and names. Raw source has no implicit Serialize
implementation; hosts must explicitly select and protect any source output.

Required `variables.json` and `settings.json`, optional `.platform`, and exact
`valueSets/<name>.json` parts are checked before returning shapes. Malformed,
duplicate, unsupported or over-budget data fails rather than becoming zero
overrides. The three recognized variables/settings/value-set JSON parts must
declare their exact respective published `1.0.0` `$schema` URI; missing,
non-string, mismatched and future-version markers return `UnsupportedSource`.
No schema URI is fetched. This version guard is not a general schema engine or
blanket unknown-field rejection. Unknown native type tokens and extra JSON fields remain source data;
missing defaults are reported as absent, not repaired. This is structural read
validation, not complete authoring/type/schema validation. Names follow the
published common `1.0.0` grammar: variable names exclude spaces; value-set
containers permit internal ASCII spaces but no leading/trailing spaces. Both
are bounded at 255 ASCII characters, without trimming or spelling normalization.
The conceptual prose's 256-character limit is not silently substituted for that
schema version. Optional defaults/override lists/ordering remain optional, and
neither structural validity nor schema-version recognition proves service
acceptance. No schema URLs or item/connection references are followed.

The [native definition contract](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/variable-library-definition)
and [generic definition API](https://learn.microsoft.com/en-us/rest/api/fabric/core/items/get-item-definition)
are the source binding. [VariableLibrary metadata](https://learn.microsoft.com/en-us/rest/api/fabric/variablelibrary/items/get-variable-library)
requires item Read, while [source retrieval](https://learn.microsoft.com/en-us/rest/api/fabric/variablelibrary/items/get-variable-library-definition)
requires Read and Write (`VariableLibrary.ReadWrite.All` or `Item.ReadWrite.All`)
and appropriate encrypted-label usage rights. The generic item route follows its
own documented delegated Item scopes; the SDK does not grant or request consent.
The [CI/CD contract](https://learn.microsoft.com/en-us/fabric/cicd/variable-library/variable-library-cicd)
keeps active selection outside source. These reads do not activate, publish,
fetch live consumer values, manage secrets, list all workspace libraries, or
register an executable CLI/MCP command. Existing v1 behavior/defaults are unchanged.

## Execution and safety

Callers supply executors, finite limits and cancellation futures. The facade
installs no transport, TLS backend, runtime, identity implementation, global
logger or ambient credential discovery.

The optional [shared HTTP executor](../core/http/executor/README.md) implements
focused interfaces over one request policy. The separate
[native transport](../core/http/transport/README.md) selects HTTP/TLS explicitly.

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

The [DataAgent public read slice](../../authoring-docs/dataagent-reads.md) uses the existing
Fabric operation session, generic ordered metadata and exact-definition decoder.
Its optional registry entries resolve generic actions and the `data-agent`
facade to the same binding. Staging and published configuration are explicit,
public Preview resources; no stage or runtime variant is inferred. Datasource
reads return one page with visible continuation evidence, not an exhaustive
collection. Stored instructions, native multipart source, served documents,
ontology navigation and hosted MCP answering remain distinct capabilities.

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
