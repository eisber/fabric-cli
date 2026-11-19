<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# V2 schema-discovery preview

The opt-in `v2-discovery` feature adds a separate `fab-cli-discovery` binary.
The separate local-host package retains the `fab-cli` executable; distinct
preview names prevent Cargo output collisions when both candidates are built.
The existing Python CLI, `fab-rs` grammar, mutation handlers and compatibility
fixtures are unchanged. `fab-rs` remains the package's default executable,
including when additional features are enabled.

This preview implements discovery, not v2 mutation execution. Its operation
commands require `--schema` and cannot submit the described operation. Starting
an MCP server or installing authentication is not part of this feature.
An additional native feature can bind the existing read-only data-schema host.

## Operation contracts

Run these commands from `rust/`:

```text
cargo run --locked -p fabric-cli --features v2-discovery --bin fab-cli-discovery -- create /New.Workspace --schema
cargo run --locked -p fabric-cli --features v2-discovery --bin fab-cli-discovery -- update /Existing.Workspace --schema
cargo run --locked -p fabric-cli --features v2-discovery --bin fab-cli-discovery -- delete /Existing.Workspace --schema
cargo run --locked -p fabric-cli --features v2-discovery --bin fab-cli-discovery -- run /Existing.Workspace/Analysis.Notebook --schema
```

`--schema` inspects the exact registered operation contract. It does not read a
payload, stdin, configuration or credentials, and does not create, update,
delete or run anything. Execution-required files or parameters are not required
for discovery. An unknown option, malformed target or unsupported representation
fails instead of being ignored.

The initial contract denominator is deliberately narrow:

| Target | Contract | Limits |
| --- | --- | --- |
| Prospective Workspace | Create input | Structural metadata fields only; no existence or permission check |
| Existing Workspace | Metadata update input | Native patch coverage, not arbitrary source replacement or concurrency guarantees |
| Existing Workspace | Delete | No generic request body; no recoverability or safe-cascade promise |
| Notebook | Default job run | No-body execution contract only; dynamic parameters and session configuration are not discovered |

Type-qualified names support local contract selection, not authoritative identity
resolution. Descriptors identify the target as unverified, authorization as not
evaluated, and v2 mutation execution as unconfigured. A create destination does
not have to exist to inspect its contract.

The neutral `fabric-sdk-discovery` registration supplies the contract and its
constraints. CLI discovery, direct SDK consumers, facade aliases, input
validation and MCP schema projection use this registration. The public SDK
facade reexports the same types through `fabric_sdk::discovery`; it does not
maintain another schema catalog.

Generated JSON describes the supported local contract, not all service
capabilities. Structural validation does not establish service authorization,
resource existence, semantic validity, atomic preconditions or successful
execution. Native definitions and function signatures without an exact
registered contract fail explicitly; there is no generic passthrough JSON.

MCP projection is inert metadata for an embedding host, not a registered or
executable MCP tool. The host still owns dispatch, permissions and approval.

## Local human help

The separate preview also renders help for one exact lexical target:

```text
fab-cli-discovery help /Prospective.Workspace
fab-cli-discovery help /Sales.Workspace --verb delete
fab-cli-discovery help /Sales.Workspace/Analysis.Notebook --verb run
```

`help` uses the existing neutral operation registrations, in canonical-ID order.
An optional `--verb` narrows to an exact registered pair; an unsupported pair is
a nonzero capability error with local help guidance, not a successful empty
list. The current denominator is the four resource contracts listed above.
Definitions, `docs/schema`, function facets and other unregistered targets
cannot borrow a parent contract.

Human output includes effects, prerequisites and input limitations from the same
registration as the SDK and MCP schemas. It reports lexical/unverified target
selection, unevaluated authorization and unconfigured mutation execution.
Command-corpus coverage is explicitly limited to the local registry. Docs are
not indexed or searched by this projection; it does not fetch source links.
Keyword/docs search is outside this slice.

`fabric_sdk::discovery::help::TargetHelp` exposes the same selected operation
contracts to independent consumers. Its `render()` text is for people, not a
machine parsing interface: use the retained contracts, schemas and inert MCP
projection for structured discovery.

Local help is immediately ready without an async runtime and never consults an
injected data-schema provider, settings, identity, HTTP, updater, stdin or files.
Caller cancellation prevents success output. The existing 4 MiB output bound
also applies to human help; writer failures propagate rather than claiming
success or replaying any described operation. `fab-rs` help and grammar remain
unchanged.

## Data schema is a different capability

Bare `schema <target>` requests backend data metadata rather than an operation
input contract. For a supported Lakehouse table path, the standalone preview
without native features has no live data provider and returns a nonzero
`NotConfigured` error, not an
empty schema or a login prompt. Unsupported paths fail capability selection
before host configuration.

An embedding host can explicitly inject the existing table resolver, table
client, executor, limits and cancellation. The initial adapter supports physical
Lakehouse Delta table paths only. It delegates to `TableClient::schema_document`,
which reuses the existing JSON-commit metadata reader and shared OneLake
execution. It does not introduce a second log scanner, pager, HTTP client,
credential chain or runtime.

### Opt-in native data schema

Build the **same** `fab-cli-discovery` executable with `v2-discovery,native-read`
for the existing Windows broker host, or `v2-discovery,headless` for an explicitly
selected unattended identity:

```text
cargo run --locked -p fabric-cli --features v2-discovery,native-read --bin fab-cli-discovery -- schema /Sales.Workspace/Book.Lakehouse/Tables/orders
cargo run --locked -p fabric-cli --features v2-discovery,headless --bin fab-cli-discovery -- schema /Sales.Workspace/Book.Lakehouse/Tables/dbo/orders
```

Use the existing [native identity configuration](native-read-host.md); there is
no schema-specific credential chain, static token fallback or automatic login.
This preview requires an explicit `FAB_RUST_AUTH_MODE`, including `broker` when
using an existing saved Windows account; enabling a native feature alone does
not select an identity or open a saved profile.
Only a lexically valid bare `schema` selects native setup. No arguments, invalid
selectors, help, version, target help and operation `--schema` stay fully local,
including in native builds. Help's "live data host not configured" text describes
the default/injected discovery host, not a credential/configuration probe.
Unsupported native hosts or absent identity configuration fail nonzero.

The one-shot host starts at the tenant root with no home, navigation persistence
or general settings-store initialization. The shared TABLE factory binds the
same account/tenant to Fabric and OneLake audiences, and its command-local
resolver establishes workspace/item IDs and physical-path existence. Resolved
shortcut classification is rejected **before** it is flattened into a local
path, including a shortcut schema ancestor. Legacy TABLE behavior is unchanged.
`NativeSchemaProvider` composes that resolver, the existing Delta provider and
shared bounded OneLake HTTP executor; raw TABLE reads and loads are not mounted.
Only public Fabric/OneLake endpoint bindings are supported in this slice.

The native command has the existing HTTP host's finite command watchdog,
covering identity selection, connection, resolution and metadata reads.
Ctrl+C drops local work; neither cancellation nor timeout implies remote rollback.
TABLE request/log/response limits and the serialized-output bound remain intact.
Schema bytes are buffered until success and interruption is checked again before
publication. A schema-owned standard stream adapter retains stdout and stderr
handles without changing the execution host's output helpers. The native runtime
supervises output through the owned stdout handle, not the process-wide Rust
stdout lock. Deadline or cancellation stops local waiting
even when a pipe reader stays open without draining; an in-flight write may still
complete, so output is explicitly unconfirmed and may be partial. Reads and output
are never replayed. Bounded runtime shutdown does not wait indefinitely for that
worker. On Windows this host requires stdout redirected to a file or pipe; a
console is rejected before identity setup rather than writing UTF-8 bytes through
a duplicated console handle without the required Unicode console adapter.
Diagnostics also use an owned handle and a bounded worker, with at most 100 ms
of waiting within the original deadline. A full merged stdout/stderr pipe can
prevent delivery of the diagnostic, but still produces a nonzero process exit;
there is no synchronous stderr fallback that can hang after output is abandoned.

Offline process and injected-host tests cover the binding without signing in or
contacting Fabric. They are not live authentication, service or release evidence.

Selectors containing `.Shortcut` or `.shortcut` are rejected before resolution,
and resolved table/schema components are checked again before storage access.
The legacy resolver and Delta reader strip these markers; rejecting them here
prevents schema discovery from silently reading a different existing table.
Direct `TableClient::schema_document` callers receive the same protection:
normalization-sensitive local paths fail `InvalidInput` before credential or
storage access. The existing v1 `schema` and `schema_string` APIs keep their
legacy normalization behavior.

The additive data document preserves the native schema and column types.
Unknown nullability remains null; false is not replaced with a default.
Completeness and source revision remain unknown when the reader cannot establish
them. JSON-commit extraction does not reconstruct checkpoint-only state or prove
a consistent latest snapshot.

The existing `table schema` command retains its v1 output. Readable schema
documentation under `get <target>/docs/schema` and native authoring definitions
under `src/` remain separate representations. This preview does not implement
new docs providers or infer structured models from Markdown.

## Failure and effect boundaries

Successful operation discovery writes a newline-terminated JSON descriptor to
stdout. Errors are nonzero and written separately to stderr. The preview has a
4 MiB output cap including the final newline; data-schema output also respects
the smaller `TableLimits.max_response_bytes` bound. Byte-limit failures emit
no success bytes. A writer failure can leave partial stdout, but propagates as
an error rather than claiming successful output or retrying remote work.

Unsupported Kusto, SQL, XMLA, native definition and dynamic function-schema
bindings do not fall back to another engine, a row query or an export API.
Cancellation stops local observation; it is not remote cancellation or rollback.

The preview binary shares a Cargo package with existing mutation adapters.
Those dependencies remain in its package graph. The no-mutation guarantee is
the discovery head's parser/dispatch invariant, not a claim that mutation code
is physically absent from the dependency graph.

## Source contracts

- [Create Workspace][create] and [Update Workspace][update] define the metadata
  request fields. The inspected documentation source revision is
  `9feaea0cb4ae5c7942ca78399640061545c3df95` in
  `MicrosoftDocs/fabric-rest-apis-docs`.
- [Run On Demand Item Job][run] defines job submission and explicitly describes
  `executionData` as job-type-specific. An opaque object is not proof of an
  exact Notebook parameter schema.
- [Notebook public APIs][notebook] establish Notebook job availability, not
  discovery of arbitrary function arguments or complete session settings.
- [OneLake API access][onelake] and [ADLS Path List][list] establish the
  storage-read boundary. Source-specific OneLake continuation and extension
  behavior must not be silently replaced with assumed generic ADLS behavior.

These are source references, not live API, authorization or platform evidence.

[create]: https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/create-workspace
[update]: https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/update-workspace
[run]: https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/run-on-demand-item-job
[notebook]: https://learn.microsoft.com/en-us/fabric/data-engineering/notebook-public-api
[onelake]: https://learn.microsoft.com/en-us/fabric/onelake/onelake-access-api
[list]: https://learn.microsoft.com/en-us/rest/api/storageservices/datalakestoragegen2/path/list
