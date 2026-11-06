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
an MCP server, installing authentication or composing a live data-schema host
is not part of this feature.

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

## Data schema is a different capability

Bare `schema <target>` requests backend data metadata rather than an operation
input contract. For a supported Lakehouse table path, the standalone preview
has no live data provider and returns a nonzero `NotConfigured` error, not an
empty schema or a login prompt. Unsupported paths fail capability selection
before host configuration.

An embedding host can explicitly inject the existing table resolver, table
client, executor, limits and cancellation. The initial adapter supports physical
Lakehouse Delta table paths only. It delegates to `TableClient::schema_document`,
which reuses the existing JSON-commit metadata reader and shared OneLake
execution. It does not introduce a second log scanner, pager, HTTP client,
credential chain or runtime.

Selectors containing `.Shortcut` or `.shortcut` are rejected before resolution,
and resolved table/schema components are checked again before storage access.
The legacy resolver and Delta reader strip these markers; rejecting them here
prevents schema discovery from silently reading a different existing table.

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
