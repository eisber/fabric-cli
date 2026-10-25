<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource mutation compatibility

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust integration baseline: `cc908ce08617f92601aa2dd68622797ccbcde9e7`.
Later Python command-policy additions in the initial checkout are not silently
adopted: for example, pinned Python rejects OrgApp mutations.

The following is a bounded implementation matrix, not a replacement for the
independent full command inventory. “Handler” means a public library consumer,
not central parser/front-door integration or live parity.

| Command/resource | Implemented handler behavior | Remaining contract |
| --- | --- | --- |
| mkdir/create workspace | Parameter help, duplicate/capacity checks, lowercased payload, actual POST, output/cache effects | Central dispatch, live scope |
| mkdir/create folder | Root/nested parent omission, optional description, POST, output/cache effects | Central dispatch |
| mkdir/create item | Typed collection, generic metadata, folder ID, Lakehouse/Warehouse/Notebook/MountedDataFactory payloads, explicit KQL/DTB parent paths | Templates/dependencies, SQL modes, all options, LRO shared-policy adapter |
| set workspace | Enriched-read ordering, blocklist/simple path, top-level projection, separate Spark PATCH | Main enriched read adapter, central dispatch |
| set item/folder | Prompt ordering, metadata PATCH, renamed cache/context effect | Definition seam and friendly Notebook aliases |
| rm/del item/folder | Confirmation/force/hard, exact route/query, accepted/completed messages, cache effects | Central context transition |
| rm/del workspace | Forced workspace DELETE only | Non-force item selection, bulk/tenant orchestration |
| exists workspace/item/folder | Truthy context ID; string message, no extra request | Central dispatch |
| cp/copy, mv/move | SDK folder move endpoint only | Actual command orchestration and definition seam |
| assign/unassign | SDK capacity assignment endpoints only | CLI validation/orchestration |
| start/stop | Not implemented | Capacity ARM and mirrored-database state machine |
| ln/mklink | Not implemented | Shortcut command/SDK operations |
| OneLake/hidden resource branches | Not part of first mutation slice | Dedicated focused SDK, context and CLI consumers |

`ResourceOutcome::Accepted` is never completion evidence. Python's acceptance
messages/cache eviction are preserved independently. `WaitForResult` may succeed
only with a reconciled actual result. Missing/cancelled/failed/unsafe operations
must retain explicit errors and mutation uncertainty; automatic replay is not
allowed. SDKs depend only on SDK/core layers, never CLI, deployment or identity.

Offline fixtures use synthetic metadata and execution exclusively. Online work
remains unexecuted and authorization-blocked pending explicit tenant/client,
resource/type allowlist, disposable workspace and cleanup scope. No login or live
write is a prerequisite for continuing the offline port.
