<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded read-command composition

Python source: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Integration base: `e0be370e6aade9d61ea46561e36a53b0ddc9bdec`.
This is a CLI consumer of the existing SDK/context/output APIs, not a new
REST implementation, identity backend, query language or CLI namespace.

| Source boundary | Native treatment |
| --- | --- |
| `parsers/fab_fs_parser.py` LS/dir/GET | Python command/flag spellings and multi-token path/query arguments; explicit unsupported options |
| `commands/fs/fab_fs.py` LS/GET routing | Immutable injected current/home/lookup through `fabric-cli-context`; no context mutation |
| `fab_mem_store._get_workspaces_from_api` | Real SDK workspaces, source case-insensitive known-type filtering and canonical suffixes |
| `ls/fab_fs_ls_workspace.exec` | Stable lowercase full-name sort, long enrichment, hidden root collections |
| `fab_cmd_ls_utils.enrich_workspaces_with_details` | Capacity SDK; missing/null/default and duplicate detail-ID semantics; single-list snapshot exception |
| `fab_mem_store._get_workspace_items_from_api` | Real generic item SDK; recognized suffixes, future-type filtering, flat listing |
| `fab_item_util.sort_ws_elems_by_config` | Stable lowercase short-name / Python enum-member-name ordering in both modes |
| `fab_cmd_ls_utils.format_and_print_output` | Default source columns or injected query before column filtering |
| `fab_item_util.get_item_with_definition` | Generic metadata helper boundary only, not definitions/specialized endpoints |
| `fab_cmd_get_utils.query_and_export` | Injected GET query/path discovery; no export or silent identity replacement |
| `fab_decorators.handle_exceptions` / `fab_ui` | Existing JSON envelope/streams/exits with fixed host time and explicit writer failures |

## Independent evidence

The new source-pinned
[command oracle](../tests/compatibility/commands/README.md) calls actual Python
command/helper code through fake auth/HTTP/context/time and no external I/O.
Existing eight workspace reference cases and source hashes remain unchanged.
Source quirks, native safety exceptions, and unsupported projection are reported
separately, not counted as exact parity.

Native consumers use both a standard-library-only fake SDK executor and the
real shared `HttpExecutor` with a fake Azure `HttpClient`, credential and timer.
These exercise requests/scopes, SDK pagination/retry, context paths, source
sorting/projection, hidden data, output streams/exits and write failures.
Parser subprocess regressions clear ambient credentials/tool environment.

### October 24 local evidence

After adopting the integration owner's exact SDK facade prerequisite
`fad81bf5f6b60835597b7627f97ac107d88966b2` (local cherry-pick
`534f3c67130567ba236ab4f670a74d4a1704cd84`), the final serialized validation
passed on Windows x64 MSVC with pinned Rust 1.99.1 and Python 3.13.15:

- 90 targeted native tests, including 18 read-composition consumers, five binary
  contracts, eight architecture checks, and existing context/output/SDK/HTTP
  consumers; three doctests. No failures, ignored or filtered tests.
- Strict `fabric-cli` all-target Clippy and workspace formatting checks.
- 25 actual probe outcomes: 16 exact source comparisons, four generic GET URI
  differences with otherwise exact output, three explicit SDK safety differences,
  and two verified `NotSupported` GET projection failures.
- 50 source/oracle guard tests, including the actual Python helper observations
  and a regression requiring the pinned `--output_format json` spelling.

The new oracle's initial command-input adapter incorrectly used `--output` for
JSON selection. The native parser rejected it; the adapter inputs were corrected
to the actual Python global flag without changing expected output/requests or
any existing source hash/golden. Initial failed receipts are retained separately
from the final all-passing receipt. SDK error handling also preserves the exact
source unsupported-GET message for root and the 13 excluded item types.
Existing dependency versions remain locked; only CLI dependency edges changed.

These are local targeted results, not new full-workspace, native CI, Linux/macOS,
query-provider, full GET, or live-service evidence.

### Combined native-query integration

Integration adds `PythonQuery` and `ReadHost::with_default_projection()` over
the actual `fabric-cli-query` evaluator. The merged read probe uses this
provider, not an expected-output callback. It asserts all 25 command outcomes:
16 exact source comparisons, four generic GET URI differences, three existing
SDK safety differences, and two successful GET path inventories with the
already documented typed-metadata key-order difference. The two earlier
disabled-provider failures are no longer gaps in the configured native-query
host. Original source expectations and the explicit `NoQuery` regression
contracts remain unchanged.

Additional public-consumer tests execute SDK row filtering/multiselect,
GET path discovery, `"."`, scalar false and numeric filtering, and malformed,
type-invalid and explicitly unsupported query errors through command envelopes.
All 50 command oracle/guard tests pass on the merged tree. This proves bounded
composition, not live-host, specialized GET or complete JMESPath parity.

### Metadata-preserving follow-on

Generic GET now uses additive `Client::get_item_metadata`/`ItemMetadata`.
Required `id` and `displayName` remain validated strings; the complete parsed
HTTP `Value` reaches real `PythonQuery` without typed reserialization. Unknown
fields, known-field positions, null/missing types, nested key order and JSON
number types are retained. Existing typed `Item` fields, imports, struct
literals, LIST and `get_item` behavior are unchanged.

All 25 protected command outcomes remain asserted. Both successful metadata
inventories now match original Python output bytes and order; historical
typed-order evidence remains unchanged and requires an explicit comparator
mode. The honest exclusive categories are **16 fully source-exact, six
generic-URI differences and three SDK safety differences**, with zero current
metadata-order or unsupported-projection failures. This is 18 exact LS/inventory
output cases plus four exact service-error output cases, but not 18 fully
source-exact requests: both successful GETs also retain specialized Python URIs
(`notebooks` and `lakehouses`) versus native `items`. No URI difference is
waived or relabeled as full parity.

Four additive original-source comparisons and shared-HTTP public consumers
exercise unknown-before-known/reordered keys, null/missing/non-string types,
nested objects, Unicode, integer/float distinctions and actual parser duplicate
semantics. Existing GET cancellation, status/error precedence, response limits
and sanitized diagnostics remain in force.

## Gaps and intentional boundaries

Default no-query LS is executable without the query library. Generic GET
executes its real SDK read and uses built-in `PythonQuery` for path inventory
when the host selects `with_default_projection()`. Hosts may override that
provider or explicitly opt out with `NoQuery`; neither choice configures
production credentials, transport or context.

The [source-routed metadata follow-on](specialized-get-compatibility.md) now
selects the exact pinned collection URI for all 31 command-supported types and
preserves the 13 source command rejections. Both command comparators explicitly
use `--request-mode source-routed`; historical generic-route and metadata-order
evidence remains unchanged. The original 25 observations now have 22 fully
exact outcomes and three unchanged SDK safety differences, with no URI-only,
metadata-order or unsupported-projection differences. The earlier counts above
record their respective historical checkpoints, not current routing.

Definitions, connections/schedules and specialized augmentation remain outside
this bounded metadata contract. Source-exact metadata-helper requests and output
are not full `fab_fs_get_item.exec` parity.

Root long enrichment reuses its original workspace listing rather than
repeating Python's workspace request. It therefore uses an explicit coherent
snapshot, not Python's potentially changing second list. Context lookup snapshots
are caller-loaded; TTL caches, automatic cache-miss retry, folder-parent discovery
and identity/persistent context loading remain unported.

Foreign continuation URIs and malformed pages retain SDK safety failures rather
than Python's ignored URI / unexpected-error behavior. The old Python oracle
expectations remain protected; these differences are not parity successes.
Service 401/403/404 mappings use Python messages and SDK-selected error codes;
other structured service failures retain source messages with trailing periods
removed by the output library. Request IDs/headers/provider diagnostic text are
not printed. Caller cancellation remains an ordinary command failure (exit 1),
not a keyboard interrupt (exit 2).

Text output, `get --output/--verbose/--force`, workspace/folder/OneLake/virtual
collection GET/LS and folder-listing configuration fail explicitly. No live
Fabric calls, selected production HTTP/TLS/runtime, credentials, installation,
release, or cross-platform parity is claimed.

## Executable host gap

`fab-rs` recognizes LS/dir/GET but intentionally returns typed `NotConfigured`
on stderr (exit 1) rather than acquiring credentials or inventing a timestamp,
transport or context. Help/version still succeed on stdout. Invalid commands
remain sanitized JSON errors on stderr (exit 2). A production host must be
explicitly designed and reviewed before ordinary users can run live commands.
