<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource mutation command composition

This CLI-layer library translates bounded resource branches of Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It is not a new command namespace,
parser, production host, or claim that the generic verbs are complete.

`MutationHost` consumes the focused workspace/item/folder mutation operations,
an injected `MutationUi` and the existing enriched metadata-read host through
`MutationMetadata`. `create`, `set`, `remove`/`remove_with_options`, and `exists`
return existing `OutputInput` values and ordered `CacheEffect` values. The main
CLI owner must apply those effects and refresh affected current contexts.
Credentials, clocks, config, prompts, cache and output streams are not discovered
or initialized globally. `ResourceOperations` is the shared-policy injection seam.
`FabricMutationBackend` connects it to the reusable SDK `OperationSession` and the
existing `HttpExecutor::execute_fabric`/`wait_for` adapter. Notebook deployment and
generic mutations share one bounded request/poll/result implementation, not a
second HTTP/retry/LRO stack. Direct SDK consumers can pass `OperationSession`
without depending on this CLI crate.

The original Python files are `commands/fs/fab_fs_exists.py`,
`commands/fs/{mkdir,rm,set}/fab_fs_*_{workspace,item,folder}.py`,
`utils/fab_cmd_{mkdir,set,rm}_utils.py`, `utils/fab_jmespath.py`,
`client/fab_api_{workspace,item,folders,utils}.py`, and the pinned
`core/fab_config/command_support.yaml`.

## Preserved behavior

- Workspace/item parameter help precedes duplicate checks. Empty folder params
  create a folder. Keys are lowercased, including unknown workspace parameters.
- Workspace `capacityName=none` means omission, not JSON null; configured capacity
  selection is injected by the host. Missing defaults fail before dispatch.
- Item creation uses the source type collection, includes null `folderId` at the
  root, and uses Python's default synchronous operation policy. A headerless 202
  remains explicit acceptance without a fabricated created item. Item deletion uses the
  generic `/items/{id}` route. `--hard` has its own prompt, warning and query.
- Workspace SET reads enriched metadata before confirmation; folder/item SET read
  after confirmation. SET patches only the changed top-level property. Workspace
  Spark settings use their own endpoint and omit the `sparkSettings` wrapper.
- EXISTS outputs the string message `true`/`false` from the context's truthy ID;
  it does not send a redundant GET or reinterpret lookup failures as absence.
- Delete acceptance prints “is being deleted”, not “deleted”. Source cache
  eviction on acceptance is represented separately from completion evidence.
- A declined prompt is not a service cancellation. Operation errors retain
  operation ID and uncertain-side-effect evidence.

## Incomplete branches

The entire assigned generic-verb family remains backlog until actual front-door
integration and independent request/output contracts cover all resources/options.
This bounded slice does not include tenant/workspace bulk selection, definition
SET, Report/SemanticModel/MirroredDatabase templates, automatic dependent-item
creation, hidden administrative resource branches, general OneLake GET/context
composition, Fabric definition copy/move, shortcuts, capacity ARM lifecycle or
domain assignment commands.
Those branches must not be routed to successful-looking generic fallbacks.

SQLDatabase creation includes New, Restore and RestoreDeletedDatabase modes,
source required-parameter failures, default `ById`, optional collation and signed/
underscored integer retention values. Integer payloads retain the existing
i64/u64 JSON boundary; arbitrary Python big integers are not silently rounded.
Supplying SQL options without a creation mode preserves the source's omission.

Mirrored-database `start`/`stop` read status through POST before confirmation;
force never skips the state check. The source stop substring-membership quirk
is preserved rather than silently fixed. Capacity assignment uses only an
explicitly selected capacity ID/full name. Unassignment reads primary workspace
metadata and validates the actual linkage before confirmation, without enriched
metadata calls or ambient capacity discovery.

The first SDK path boundary explicitly rejects URL/traversal/encoded identifiers.
This is a safety boundary, not a claim of Python opaque-path equivalence.
SET uses the existing finite i64/u64 JSON boundary and rejects unsupported values
before mutation rather than silently rounding wide integers to floats. Ordinary
non-JSON input still follows Python's intentional string fallback.
MountedDataFactory currently supports ASCII identifier strings that need no JSON
escaping. Other definition-serialization values fail explicitly until the shared
Python-default JSON encoder seam is available; they are not emitted with a
different base64 payload while claiming source fidelity.
Structured service errors are data, never instructions or credentials.
The CLI error adapter preserves Python's 401/403 header-code precedence,
404 diagnostic, partial Fabric error bodies, trailing-period normalization and
authorization exit selection. Server-side operation failures retain the exact
parsed error value and use Python-compatible dictionary/list/scalar formatting;
the SDK's Display implementation never automatically emits that untrusted data.
The development-only error comparison executes the pinned `do_request`,
exception classes and `handle_exceptions` decorator with synthetic auth, network
and time. No real authentication module or ambient config is imported.

## OneLake read dependencies

`OneLakeReadHost` composes `OneLakeClient` and an injected `OneLakeExecutor` for
the OneLake `ls` and `exists` branches, using the real shared HTTP adapter in its
offline executable consumer. It returns existing `OutputInput` and the source's
`show_headers` flag; it does not edit the separately owned front door.
LS mutates names/types before applying the existing query evaluator, or projects
the source columns in order for short/long output. Page/service row order and
duplicates are preserved. `.Shortcut` and `.shortcut` are removed from all local
path occurrences exactly as pinned Python does; string `"false"` remains truthy.
EXISTS sends the source directory-list request: a complete empty listing is
`"true"`, a real 404 is `"false"`, and permissions, malformed/incomplete data,
credentials, cancellation and other execution failures remain typed errors.
Malformed/absent path lists explicitly follow the SDK safety boundary rather
than Python's success-shaped fallback.

General OneLake GET metadata, item-root enumeration, hidden-resource resolution,
and shortcut metadata/target orchestration remain separate backlog
items. HTTP errors beyond the source's standard 401/403/404 diagnostics retain
the SDK's sanitized message; full storage-body diagnostics are not claimed.

## OneLake write composition

`OneLakeWriteHost` consumes explicit `OneLakeWriteTarget` metadata rather than
adding a second context resolver. Directory mkdir, file/directory rm, Lakehouse
Files rename, a caller-read local-byte upload helper and nonrecursive
OneLake-to-OneLake file copy execute through the same focused SDK/shared HTTP.
The source writable-root matrix, params-only help, confirmation/force notices,
query/header spelling, binary media types/byte positions, source-empty checks
and output spelling/order are retained. The rename source header deliberately
has no added leading slash, matching the original helper's ID-path extraction.

File copy uses headers and bytes from the actual source GET, not an extra HEAD.
Directory/shortcut destinations append the original file name; directory/table/
shortcut sources reject recursive copy. The upload helper takes actual bytes
from a caller-owned filesystem host: local filesystem discovery, reads/writes
and path-error orchestration are not claimed as implemented by that helper.
Shortcut removal remains an explicit separately routed Fabric handler gap, never
a recursive DFS delete fallback.

`OneLakeWriteResult` retains actual accepted/completed response or upload-stage
evidence independently of source messages. In particular, Python's `Done` after
successful append/flush requests is retained even if those responses are 202;
their `Accepted` states are not rewritten as completed storage effects. Failed
later stages preserve mutation uncertainty and previously received stages without
printing Done, replaying a write or inventing rollback/cleanup.
The original-Python comparison executes pinned request/handler functions; its
wait flag comes from the original `do_request` assignment, not from whether an
individual handler explicitly assigned `args.wait`.
No live sign-in, cache extraction, service mutation or online parity is authorized
by this library or its tests.
