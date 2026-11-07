<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Artifact-first table adapters

Tables are typed subresources of their parent artifacts, not independent
workspace items. The additive SDK entry points are:

```text
fabric_sdk_tables::lakehouses::tables
fabric_sdk_tables::kql_databases::tables
fabric_sdk_tables::warehouses::tables
```

The existing `fabric-sdk-tables` package contains shared implementation code.
Its package name does not introduce a public `artifact table` command or a
universal table loader. Existing Python and Rust v1 table operations and their
compatibility fixtures retain their original contracts.
Use the focused crate directly; adding its dependency to the umbrella
`fabric-sdk` would create a cycle through existing shared HTTP test consumers.

## Parent-specific capabilities

Each parent exposes `tables::describe(TableOperation)`, backed by the same
front-end-neutral operation description. Schema and statistics are get
representations, not new common verbs.

Discovery distinguishes an SDK adapter, an additional binding requirement, and
an unsupported operation. An SDK adapter still needs a caller-supplied executor;
the descriptor does not prove host configuration, authentication, target
permission, service availability or platform support.

| Parent | Loading mechanism | Completion evidence |
| --- | --- | --- |
| Lakehouse | Native Load Table from existing same-item OneLake `Files` content | Validated Fabric operation reconciliation |
| KQLDatabase | Explicit inline CSV or storage-source ingestion | Native extent results and errors |
| Warehouse | Native SQL `COPY INTO` into an existing table | All result sets and terminal TDS evidence |

These are different source schemas, effects, authorization rules and protocols.
No adapter silently uploads, stages, converts, truncates, switches engines or
changes identities. Lakehouse overwrite must be selected explicitly; it is not
a portable Kusto or Warehouse load mode.

Only Lakehouse supports the Delta optimize/vacuum bindings. V-Order is opt-in.
The new vacuum policy requires at least 168 hours and never changes workspace
retention safety settings. A maintenance job's acceptance, completion,
cancellation and deduplication remain different outcomes.

## Identity and query boundaries

Clients use caller-injected Azure credentials and execution interfaces. They
do not initialize a runtime, discover ambient credentials, prompt, invoke
another CLI, install query extensions or create a credential cache.

Fabric load/maintenance, OneLake reads, Kusto and TDS do not share an
interchangeable token audience. Parent resolution must retain exact scoped
identities and an explicit native endpoint/catalog binding.

Kusto and Warehouse query adapters require a read-enforcing execution boundary.
A table target supplies native parent context; it is not a promise that an
arbitrary query can access only that table. SQL prefix tests or textual
table-name substitution do not enforce read-only effects or table isolation.
Hosts must not advertise these query capabilities without their required
enforcement.

## Deliberate gates

- Table copy is unsupported until its consistency and collision contract is
  implemented. Parallel blob copying does not establish a coherent Delta
  snapshot or copy permissions/catalog metadata.
- Schema-enabled Lakehouse loading requires a separate verified beta profile.
  The legacy adapter must not drop a schema and retry the unqualified route.
- Lakehouse native catalog statistics, SQL endpoint health and query require
  additional bindings. The compatibility schema reader uses JSON Delta logs;
  it does not implement checkpoint reconstruction.
- Kusto queued/high-volume ingestion is not an implicit alternative to inline
  or storage pull, and raw-input limits are not service-size guarantees.
- Warehouse source format/authorization support must follow its typed adapter;
  external-storage credentials are not automatically valid for OneLake.
- Kusto/TDS executors are caller-supplied SDK boundaries. These additions do
  not install native Kusto HTTP/TDS drivers or mount v2 executable commands.

Dropping a future or reaching a deadline stops local observation, not remote
work. Retain accepted handles and uncertainty. Never replay a mutation because
its response, result stream or output delivery failed.

Kusto response handling keeps protocol-specific diagnostics separate: v2 uses
`QueryCompletionInformation.Level`, while v1 resolves opaque table names through
the final table of contents before inspecting `QueryStatus.Severity`. Invalid or
incomplete mappings fail rather than treating status rows as primary results.
Native in-row error objects retain prior rows as evidence only; partial query
data must not be consumed as a correct subset.

## Source and verification scope

Implementation starts from Rust checkpoint
`a9df45501ec53d921c6fc765c726c32979b85931`. Native API references include
[Lakehouse Load Table](https://learn.microsoft.com/en-us/rest/api/fabric/lakehouse/tables/load-table),
[Lakehouse schema/load restrictions](https://learn.microsoft.com/en-us/fabric/data-engineering/lakehouse-api),
[Kusto inline ingestion](https://learn.microsoft.com/en-us/kusto/management/data-ingestion/ingest-inline?view=microsoft-fabric),
[Kusto storage ingestion](https://learn.microsoft.com/en-us/kusto/management/data-ingestion/ingest-into-command?view=microsoft-fabric),
and [Fabric COPY INTO](https://learn.microsoft.com/en-us/sql/t-sql/statements/copy-into-transact-sql?view=fabric).

Offline tests exercise injected credential/executor boundaries, native request
construction, wrong-target rejection, partial and uncertain results, cancellation
and descriptor consistency. Test source or passing mocks alone do not prove
native driver, live tenant, CLI/MCP host or cross-platform readiness.
