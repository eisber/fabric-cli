<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Warehouse management reads

`fabric_sdk_sql_database::warehouse` supplies five typed native Fabric v1
descriptors: Warehouse metadata GET, single-page Warehouse LIST, connection
metadata GET, single-page restore-point LIST and exact restore-point GET.
Consumers inject the existing `ResourceOperations`/`OperationSession`; the module
creates no HTTP transport, runtime, credential provider, SQL driver or pager.

This additive module does not replace the existing SQLDatabase descriptors or
Warehouse table adapters. Its `WarehouseRead::ALL` inventory describes only these
five operations, not a configured host, permission grant or live capability.
The CLI consumer below delegates to these descriptors and their existing native
decoders; it adds no separate transport or SQL engine.

The separate [native definition retrieval](warehouse-definitions.md) slice adds
`WarehouseRequests::get_definition` and the facade's
`fabric_sdk::warehouse::read_definition`. Its POST/read-write/LRO contract is not
part of this five-GET inventory and must not inherit ordinary metadata permissions.

| Descriptor | Relative path under `https://api.fabric.microsoft.com/v1` | Required permission / delegated alternatives |
|---|---|---|
| `get` | `/workspaces/{workspaceId}/warehouses/{warehouseId}` | Read item; Warehouse or Item Read/ReadWrite |
| `list_page` | `/workspaces/{workspaceId}/warehouses?recursive=...&rootFolderId=...&continuationToken=...` | Viewer workspace; Workspace Read/ReadWrite |
| `connection` | `/workspaces/{workspaceId}/warehouses/{warehouseId}/connectionString?guestTenantId=...&privateLinkType=...` | Read Warehouse and Viewer workspace; Warehouse or Item Read/ReadWrite |
| `restore_points_page` | `/workspaces/{workspaceId}/warehouses/{warehouseId}/restorePoints?continuationToken=...` | Read or Write item; Warehouse or Item Read/ReadWrite |
| `restore_point` | `/workspaces/{workspaceId}/warehouses/{warehouseId}/restorePoints/{restorePointId}` | Read or Write item; Warehouse or Item Read/ReadWrite |

Audience is Fabric, not SQL or storage. Scopes are documented delegated
alternatives, not additional token requests or app-only authorization claims.
Provider roles, grants and availability remain authoritative.

## Identity and evidence

Workspace and Warehouse IDs are strict native GUID types. `WarehouseTarget`
rejects SQLDatabase, Lakehouse, SQLEndpoint and WarehouseSnapshot owners before
descriptor construction. Returned Warehouse workspace/item/type fields must
match the request; a logical ID never substitutes for the concrete item.
Restore-point keys are opaque strings, encoded once as one native path segment.
Only the exact restore-point GET uses the shared encoded-segment constructor;
GUID-only metadata/connection/list routes retain the strict identifier
constructor. Percent-looking keys are encoded as literal data, not pre-decoded,
and native keys with separators, controls or dot traversal remain rejected.
They are never parsed as UUIDs/timestamps, numerically sorted or selected by
newest/nearest time.

Connection and restore APIs omit owner IDs in their native responses.
`Owned<T>` retains the **selected request owner** separately from native data;
it does not pretend the service echoed or verified those IDs. Connection strings
remain inert metadata, including the native hostname-only representation.
No catalog is guessed from displayName, no hostname is trusted for connection,
and no SQL read-only guarantee is asserted. Guest-tenant and private-link options
select metadata only, never another credential or tenant.

Known fields validate native types. Optional metadata preserves missing versus
null; unknown restore creation modes and event fields remain inert data.
Implicit Debug/Display omit connection strings, names, restore data, cursors,
provider diagnostics and handles. Trusted consumers may explicitly inspect
native data and typed failure evidence.

## Completion and page bounds

Only completed HTTP 200 with the expected body is a valid result. Shared
service/authentication/cancellation/limit failures remain failures.
Unexpected acceptance retains its operation ID and is not polled; empty,
malformed, denied, partial-status and unknown-status responses cannot become
successful metadata. The existing SQLDatabase completion decoder is reused.

Each list returns exactly one service page, preserving order and duplicates.
Continuation token/URI absence, null and empty are retained; nonempty
continuations report `PageCoverage::Incomplete`. A foreign continuation URI is
inert and never followed. Consumers must use the existing shared paging policy
for traversal, origin/repeated-token checks and cumulative request/byte/deadline
bounds. TerminalPage is not a transactional or exhaustive CLI-list guarantee.
Warehouse folder recursion is always an explicit caller choice, not the API's
implicit true default.

No SQL query/execute, COPY/load, restore mutation, restore-point mutation,
definition deployment, audit, workspace SQL-pool setting, automatic retry,
monitor traversal, timeout cancellation or rollback is added.

## Bounded CLI consumer

The existing opt-in `fab-cli-workspace` get/list head now delegates Warehouse
reads through `warehouse_reads::WarehouseReadHost`. Its workspace operations,
explicit budgets, data-only JMESPath, stdout and atomic no-clobber file output
remain unchanged. No binary, global flag or v1 behavior is added. Select the
existing `workspace-read-v2` feature, plus an existing native host feature for
service execution. Without an injected/configured host, reads fail explicitly.

The following Warehouse-only mapping was selected for this implementation.
It is not a universal facet grammar for other item types. Here `W` and `I` mean
concrete native UUIDs, not names or literal placeholder strings.

| Existing command head | Address | Native result |
|---|---|---|
| `get` | `/W.Workspace/I.Warehouse` | Warehouse metadata only |
| `get` | `/W.Workspace/I.Warehouse/settings/connection` | Inert connection fields |
| `get` | `/W.Workspace/I.Warehouse/restore-points/KEY` | One exact native opaque key |
| `list` | `/W.Workspace/warehouses` | One native Warehouse page |
| `list` | `/W.Workspace/I.Warehouse/restore-points` | One native restore-point page |

`artifact warehouse get|list` uses the identical delegates, results, errors and
delivery policy. For the Warehouse collection, the facade also accepts
`artifact warehouse list /W.Workspace`. Workspace facades reject Warehouse
paths. SQLDatabase, SQLEndpoint, Lakehouse and snapshot suffixes never select
Warehouse routes. Names and untyped machine selectors are not newly admitted
by this explicit-UUID head.

Every read still requires `--max-bytes` and `--timeout`; LIST also requires
`--limit`. Warehouse collection LIST requires explicit `--recursive true|false`;
restore-point LIST rejects recursion. These are existing head options, not new
defaults. Root-folder selection and guest-tenant/private-link connection options
remain typed-adapter inputs, not new CLI flags. `KEY` is a literal decoded key:
percent-looking text is not URL-decoded and is encoded exactly once by the SDK.
Slashes, backslashes, controls, empty keys and dot traversal are rejected.

LIST always returns `data: []` or an array of whole native records, retaining
service order, duplicates and unknown fields. One page is acquired; there is no
implicit all-pages traversal. If the entire page cannot fit the record/data
bound, the read fails without returning a prefix or skipping rows with a
next-page token. Foreign routes/origins and repeated native tokens fail through
the shared pager. URI-only continuation is rejected as unresumable rather than
misreported as a complete collection.

A valid page with continuation has `status: partial`, incomplete coverage and
exit code 1, but no invented service/budget error. It is not complete collection
success. `meta.nativeRead` retains selected request ownership, native continuation
observations and coverage outside projected `data`. The CLI has no portable
resume flag or cursor: any native token there is explicitly informational.
The typed seam additionally returns a process-local continuation bound to the
credential object, selected account/tenant/endpoint, exact collection, native
options and output bounds. Reusing it for another selection fails before HTTP.
No metadata/SQL cost or snapshot consistency guarantee is implied.

The separate existing `fab-cli-v2` GET backend also dispatches explicitly typed
Warehouse metadata to the native Warehouse route instead of generic item GET.
Its source/docs/file/run facets remain unsupported for Warehouse. Ordinary GET
never requests a definition, opens SQL, or falls back to another item endpoint.

Definition output remains deferred until the independently implemented native
SDK definition binding is reviewed and released. The existing
`src/definition[/formats/...][/parts/...]` representation seam is retained;
this consumer does not guess SQL source formats or use generic definition POST
as a fallback. A SQL project (`sqlproj`) describes source/project structure;
a DACPAC is a deployment/schema package. Neither is table data, a restore point,
or a permission to execute/deploy SQL. Export/packaging semantics remain separate.

## Sources and offline evidence

The assigned selected Warehouse facade root is
`5b3592d6-103d-4093-b876-143d13d67259`, revision 12. Source reconciliation used
the owner-authorized `cc183dd43140350eb1f23af0636332150d1f6d50` base.

Native references:
[Get Warehouse](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/items/get-warehouse),
[List Warehouses](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/items/list-warehouses),
[Get Connection String](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/items/get-connection-string),
[List Restore Points](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/restore-points/list-restore-points),
[Get Restore Point](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/restore-points/get-restore-point).
Connection/restore references were captured at documentation source
`b4d803a1e7f495553fcb7ef509391016e676d91d`; metadata/list references at
`5df184c567fafea7284357607ebac79d6301a698`.

`sdk/sql/database/tests/warehouse.rs` covers native routes/options, opaque ID
encoding, owner/type/shape mismatch, native optional fields, page evidence,
status/acceptance failures, protected diagnostics and injected shared execution
with denied/cancelled/credential cases. Existing SQLDatabase tests still cover
the shared completion policy. Tests are offline and do not request credentials
from the environment or contact Fabric/SQL services.
