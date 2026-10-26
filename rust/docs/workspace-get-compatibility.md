<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace metadata GET

Python source: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust integration base: `fad3ea7fee3a3e4e66205ffcf34d6b943bb416d7`.
This is a bounded in-memory metadata vertical, not full CLI, export, live service,
authentication or production HTTP/TLS-host parity.

## Source and consumers

`commands/fs/fab_fs_get.py::exec_command` joins GET query/output tokens and
dispatches a resolved `Workspace` to `commands/fs/get/fab_fs_get_workspace.py::exec`.
That handler sets `ws_id`, requests primary metadata, then independently tries
three augmentations in this order:

| Helper in `client/fab_api_workspace.py` | Fixed route after `/v1/workspaces/{id}` | Field assignment |
| --- | --- | --- |
| `get_workspace` | No suffix | Original ordered object |
| `ls_workspace_managed_private_endpoints` | `/managedPrivateEndpoints` | `managedPrivateEndpoints = body["value"]` |
| `get_workspace_spark_settings` | `/spark/settings` | `sparkSettings = body` |
| `acl_list_from_workspace` | `/roleAssignments` | `roleAssignments = body["value"]` |

All helpers use actual `fab_api_client.do_request`, Fabric scope, status/error
precedence and `_handle_successful_response` paging. Only primary status 200
enters the handler's augmentation/output branch. Each optional block catches
`Exception`; only status 200 assigns its field. An omitted augmentation does not
delete a preexisting primary field. Replacing an existing field retains its
original position. Finally, `fab_cmd_get_utils.query_and_export` invokes
`fab_jmespath.search` and the existing JSON output/decorator boundary.

The additive `WorkspaceClient::get_workspace_metadata` and facade
`Client::get_workspace_metadata` accept the existing `WorkspaceExecutor`,
workspace ID, `WorkspaceListLimits` and cancellation future. They return
`WorkspaceMetadataResult`: ordered validated `WorkspaceMetadata` plus three
typed `WorkspaceEnrichmentOutcome` observations. Unknown/null/numeric/nested
service data is retained. Required `id` and `displayName` must be strings, like
the existing typed metadata APIs; the old `Workspace`, LIST, errors and public
struct literals are unchanged.

The existing `ReadHost` resolves a workspace from the immutable caller-loaded
context snapshot, calls this facade API, and passes enriched metadata directly
to the real `PythonQuery` provider. Default/empty queries discover paths; dot
selects the object; scalar values use existing Python-compatible stringification.
Empty projected containers or strings emit zero bytes, matching the complete
source single-command dispatch; no empty success envelope is fabricated.
`FolderReadHost` delegates workspace GET to that same host. Neither host gains
required fields, parser options, a second REST stack or fixture-specific logic.
Cancellation is checked after query projection before output is committed.

## Shared policies and intentional boundaries

Workspace LIST and metadata reuse the same private read and page-accumulation
policies. The existing shared `HttpExecutor` accepts the same validated
`WorkspaceRequest`; no adapter, dependency, runtime or auth-backend change is
needed. Caller-selected account/tenant/Fabric audience remain caller-bound.
Opaque IDs are encoded as individual path segments; arbitrary URLs, endpoint
overrides and route-injecting IDs are not accepted.

Each optional collection must finish bounded traversal before its field is
assigned. Defaults remain 100 pages, 100,000 items and 4 MiB per response.
Nonterminal continuation origins and exact resource paths are validated; tokens
are opaque, query-encoded, cycle-checked and isolated between steps. The actual
HTTP client retains its bounded streaming, retry and per-request deadline policy.

Optional service errors and malformed responses omit the field and expose a
typed observation, including failures after earlier pages. No partial collection
is inserted and later optional steps still run. CLI omission emits no extra
diagnostic bytes. SDK error/result Debug excludes metadata, provider messages,
service codes, resource IDs and continuation tokens.

Credential/execution failure, cancellation, timeout, unsafe continuation,
repeated token or exceeded bounds abort explicitly at any step. These existing
SDK safety boundaries intentionally differ from Python's catch-all optional
silence. They must not be counted as fully source-exact outcomes.

Python's generic helper accepts 201/204, but the workspace handler skips primary
output for those statuses. Native primary metadata rejects them explicitly.
A single optional 201/204 is omitted. A 201 collection page followed by terminal
200 is merged; Python instead truncates a collection when a trailing subtree
ends at 201/204. Native traversal fails explicitly instead of printing truncated
data. Primary/Spark continuations are unsupported safety failures rather than
generic recursive metadata merging. Non-object Spark settings and malformed
collection shapes retain explicit typed omission, not arbitrary successful data.

Text output, export, verbose/force, prompting, filesystem writes, cache loading,
identity discovery, mutations and all other GET families remain outside scope.
The executable still returns `NotConfigured` without an explicitly supplied
production host. No source quirk is implicitly approved for a future redesign.

## Independent evidence

The new [workspace GET oracle](../tests/compatibility/workspace_get/README.md)
has separate manually authored expectations and complete pinned original source
hashes. It executes the original handler, API helpers, HTTP/status/paging,
query and output definitions offline, then compares native full requests,
scopes, stdout/stderr bytes and exits. Source-exact, safety and unsupported
observations are separate; native test counts are not a parity denominator.
All older oracle directories, source pins, fixtures and goldens remain unchanged.

The independently frozen
[typed-domain consumer inventory](../tests/compatibility/workspace_get/CONSUMERS.md)
has **58 distinct source cases: 44 source-exact, 12 safety, two unsupported**.
Both actual native probes assert the complete separately classified protocol,
producing 116 observations, not 116 source cases. All 58 original-source
contracts and 14 consumer guards passed locally on Windows x64/MSVC.

The original 70-case source inventory and its 20 guards also passed, unchanged.
All original inputs were additionally observed through both probes: **13
source-equal and 57 different per probe**, with no unavailable inputs or skips.
Most original bodies lack the task-mandated string metadata identity fields;
status/continuation/shape differences remain explicit. Those original fixtures
are not an all-passing native gate, and the typed-domain suite does not overwrite
their outputs, categories or proposed native expectations. There is no approved
blanket native overlay or overall feature-percentage claim.

Public focused SDK, facade, raw parser/context, shared Azure HTTP, both read-host,
query, cancellation/drop, exact-limit and writer-failure tests complement that
oracle. Parent integration and fresh provider CI remain separate gates.
