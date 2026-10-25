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
| mkdir/create item | Typed collection, generic metadata, folder ID, Lakehouse/Warehouse/Notebook payloads, MountedDataFactory ASCII identifier subset, SQLDatabase New/Restore/RestoreDeleted modes, explicit KQL/DTB parent paths, shared source-synchronous waiting | Templates/dependencies, Python-default definition JSON encoding, remaining type-specific options, central dispatch |
| set workspace | Enriched-read ordering, blocklist/simple path, top-level projection, separate Spark PATCH | Main enriched read adapter, central dispatch |
| set item/folder | Prompt ordering, metadata PATCH, renamed cache/context effect | Definition seam and friendly Notebook aliases |
| rm/del item/folder | Confirmation/force/hard, exact route/query, accepted/completed messages, cache effects | Central context transition |
| rm/del workspace | Forced workspace DELETE only | Non-force item selection, bulk/tenant orchestration |
| exists workspace/item/folder | Truthy context ID; string message, no extra request | Central dispatch |
| cp/copy, mv/move | SDK folder move endpoint only | Actual command orchestration and definition seam |
| assign/unassign | Capacity SDK endpoints and CLI confirmation/linkage checks; primary metadata only | Domain branch, central dispatch |
| start/stop | Mirrored-database POST status/action routes, state/force/prompt checks, source stop-substring quirk | Capacity ARM branch, central dispatch |
| ln/mklink | Not implemented | Shortcut command/SDK operations |
| OneLake dependent reads | Focused SDK list/read/HEAD plus concrete shared HTTP adapter; CLI `ls` projection/query/long and `exists` request/output consumers | General GET, item-root/hidden resolution, mutations, shortcuts, central dispatch |

`ResourceOutcome::Accepted` is never completion evidence. Python's acceptance
messages/cache eviction are preserved independently. `WaitForResult` may succeed
only with a reconciled actual result. Missing/cancelled/failed/unsafe operations
must retain explicit errors and mutation uncertainty; automatic replay is not
allowed. SDKs depend only on SDK/core layers, never CLI, deployment or identity.

`fabric-sdk-operations` extracts the existing Notebook request/poll/result policy.
The Notebook facade preserves its public signatures, error classifications and
final resource checks through a private compatibility adapter. Direct SDK users
can inject `OperationSession` into focused mutation clients without a CLI
dependency. `FabricMutationBackend` is a thin CLI consumer of that same session
and the existing shared HTTP execution/timer. Synthetic Azure HTTP requests test
the actual handler/SDK/operation/HTTP chain, asynchronous result retrieval,
request bounds, lost PATCH/DELETE responses and explicit 202 versus 204.
The error comparison additionally executes the original `do_request`, exception
classes and `handle_exceptions` decorator. It compares HTTP header/body precedence,
partial errors, period stripping, authorization exits and server operation-error
dictionary/list/scalar messages. Exact parsed failures remain SDK data; the CLI
explicitly renders them, and SDK Display remains sanitized. No live auth,
credential cache or actual network is involved in that original-source probe.

Offline fixtures use synthetic metadata and execution exclusively. Online work
remains unexecuted and authorization-blocked pending explicit tenant/client,
resource/type allowlist, disposable workspace and cleanup scope. No login or live
write is a prerequisite for continuing the offline port.
