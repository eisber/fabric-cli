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
| mkdir/create item | Typed collection, folder/description, Lakehouse/Warehouse/Notebook, SQL modes, SemanticModel/GenericMirror raw templates, KQL/DTB explicit or automatic dependencies and source batch/cache/partial evidence | Report/dynamic Mirrored templates and Python-default encoder, remaining type-specific options, central dispatch |
| set workspace | Enriched-read ordering, blocklist/simple path, top-level projection, separate Spark PATCH | Main enriched read adapter, central dispatch |
| set item/folder/shortcut | Metadata PATCH/cache effects; shortcut read-before-confirmation, full definition update, target.type removal and create/delete rename evidence | Item definition seam and friendly Notebook aliases, central dispatch |
| rm/del item/folder | Confirmation/force/hard, exact route/query, accepted/completed messages, cache effects | Central context transition |
| rm/del workspace/tenant | Force catalog/count/workspace deletion; non-force supported Item-only selection with sort/hard/cache/output rules; tenant selection/confirmation and accepted/partial evidence | Existing main catalog/UI/context integration and broader bulk/cross-workspace consumers |
| exists workspace/item/folder | Truthy context ID; string message, no extra request | Central dispatch |
| cp/copy, mv/move | Within-workspace folder move consumer with parent-cache/count/evidence rules; OneLake Lakehouse Files rename, nonrecursive remote file copy, caller-read local-byte upload helper | Fabric definition/cross-workspace/bulk orchestration and local filesystem orchestration |
| assign/unassign | Capacity SDK endpoints and CLI confirmation/linkage checks; primary metadata only | Domain branch, central dispatch |
| start/stop | Mirrored-database POST status/action routes, state/force/prompt checks, source stop-substring quirk | Capacity ARM branch, central dispatch |
| ln/mklink | Shortcut create/JSON/resolved-target, remove and SET CLI consumers over Fabric SDK/shared operation policy; SDK metadata GET | GET/export command projection, context/front-door composition |
| OneLake | SDK item roots/reads/source metadata/storage mutations/shortcut metadata over concrete shared HTTP; CLI item-root and directory `ls`/`get`/`exists`, directory create, DFS remove/rename/copy/upload and shortcut helpers | Hidden resolution, local filesystem/export and central dispatch |

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
