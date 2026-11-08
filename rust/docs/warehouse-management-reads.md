<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Warehouse management reads

`fabric_sdk_sql_database::warehouse` supplies five typed native Fabric v1
descriptors: Warehouse metadata GET, single-page Warehouse LIST, connection
metadata GET, single-page restore-point LIST and exact restore-point GET.
Consumers inject the existing `ResourceOperations`/`OperationSession`; the module
creates no HTTP transport, runtime, credential provider, SQL driver or pager.

This additive module does not replace the existing SQLDatabase descriptors or
Warehouse table adapters. It adds no executable CLI leaf or global command
grammar. Its `WarehouseRead::ALL` inventory describes only these five operations,
not a configured host, permission grant or live capability. Common and
artifact-facade hosts can consume the same descriptors without duplicating
handlers; standalone native host registration remains separate work.

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
