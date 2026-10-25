<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Folder-aware read-command composition

Python source: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Approved Rust base: `d9df14a6acf7fe3d80adf3bb74ee10ad0a7ac92f`.
This additive, caller-configured slice supplies workspace LS with
`folder_listing_enabled=true` and folder-context LS. It does not configure the
`fab-rs` executable for live execution.

## Public composition

Build the existing `ReadHost` with an explicit SDK client, executors, immutable
lookup/current/home snapshots, configuration, timestamp and projection. Borrow it
with `host.with_folders(&folder_executor)`, then call `dispatch` or `run` on that
wrapper. The same caller-owned `fabric_sdk_http::HttpExecutor` can provide all
four SDK executor interfaces. Native query evaluation remains
`ReadHost::with_default_projection()`; `NoQuery` remains an explicit opt-out.
`with_limits(ItemListLimits, FolderListLimits)` selects existing SDK safety
bounds for these new branches only.

The wrapper does not add fields to `ReadHost` or change its struct-literal
consumers. Non-folder commands, tenant/root LS, workspace LS with folder
presentation disabled, GET, unsupported options and errors outside the new
branches delegate to the existing host. Immutable lookup may be consulted to
select the new branch; it never loads services or modifies the host snapshot.
A folder path not preloaded by the caller retains the existing context lookup
failure, not an ambient cache or synchronous HTTP fallback.

## Source mapping and preserved behavior

| Pinned source | Native behavior |
| --- | --- |
| `commands/fs/ls/fab_fs_ls_item.py::exec` | Workspace hidden collections and source default-column/query selection |
| `commands/fs/ls/fab_fs_ls_folder.py::exec` | Folder presentation even when configuration is false; no hidden collections or query evaluation |
| `utils/fab_cmd_fs_utils.py::{get_ws_elements,sort_ws_elements}` | Items first, then direct-child folders/items; long rows contain name and opaque ID only |
| `utils/fab_mem_store.py::_get_workspace_items_from_api` | Folder assignment when `folderId` is **present**, including null; folder lookup precedes item type validation; unknown types are omitted |
| `utils/fab_mem_store.py::{_get_workspace_folders_from_api,_extract_root_folders,_extract_nested_folders,get_folder}` | Root means **absence** of `parentFolderId`; root-first, repeated-pass assembly order; first assembled matching ID wins |
| `utils/fab_item_util.py::{_get_elem_type,sort_ws_elems_by_config}` | Stable lowercase short-name and Python enum-member ordering, including mixed-case `Folder` sort key |
| `core/hiearchy/{fab_element,fab_folder,fab_item}.py`, `core/fab_types.py` | Case-preserving raw display names, structural parent equality, canonical suffixes and opaque IDs |
| Existing SDK/query/output libraries | Shared HTTP policies, native query limits, JSON truthiness/wrapping, streams, exit codes and explicit writer errors |

Raw service folder names are not path tokens: a service display name
`Already.FoLdEr` renders as `Already.FoLdEr.Folder`. Suffix canonicalization in
path resolution does not strip service names. Folder-context `--query` is
intentionally ignored, even if invalid or no provider is configured, because
the source folder handler prints sorted rows directly. Workspace queries still
run through the real native provider after source column selection.
An absent workspace query uses default columns; a present empty query invokes
the source path inventory under the default handler-list contract. This
distinction preserves the synthetic source `query=[""]` boundary. For actual
scalar LS/dir arguments from `parse_args`, opt into
`with_folders(&executor).with_scalar_query()`: both absent and scalar empty
queries use ordinary columns, exactly like original registration plus the LS
helper. The public command is never rewritten, and the original host/layout and
legacy handler default remain unchanged.

The graph uses compact interned contexts, not recursive parent equality or
repeated full-list retries. A priority queue preserves the exact source pass
and input order, including child-before-parent inputs and stable duplicate ties.
Each folder is emitted at most once, and each dependency is scheduled at most
once: O(n log n) graph work and O(n) retained nodes under the SDK row bound.
No recursive graph destruction or ancestor expansion is required. Full context
equality, rather than ID equality alone, selects direct children.

## Explicit safety and snapshot boundaries

These are **not** approved Python behavior improvements or parity successes.
The pinned Python graph loop does not progress on several malformed inputs.
No command output can be meaningfully compared to a nonterminating source.

| Input | Native category / output | Source treatment |
| --- | --- | --- |
| Present null `parentFolderId` | `NotSupported`, `NullParent safety boundary` | Not a root; source nonprogress |
| Non-string `parentFolderId` | `NotSupported`, `NonStringParent safety boundary` | Cannot match string IDs; source nonprogress |
| Parent ID absent from all rows | `NotSupported`, `UnresolvedParent safety boundary` | Source nonprogress |
| Remaining cyclic/unreachable dependencies with all parent IDs present | `NotSupported`, `Cycle safety boundary` | Source nonprogress |
| Recognized item with present `folderId` that is null, non-string or unmatched | `NotSupported`, `ItemParent safety boundary` | Source constructs a null parent, then asserts while selecting elements |
| Missing item type | Existing `InvalidResponse` diagnostic | Source unexpected `KeyError` |
| Malformed SDK fields, byte/count/page/continuation limits | Existing typed SDK failure, sanitized failed envelope | Separate SDK safety behavior, never partial successful rows |

Graph error precedence is null/non-string rejection during input validation,
then unresolved parent versus cyclic dependencies after bounded scheduling.
No orphan is dropped, reparented or promoted to the root. Unknown item types
are omitted only after source-ordered folder loading; they do not turn an
invalid folder graph into success.

One complete command-local folder snapshot is loaded lazily on the first
present item `folderId`, or after item construction if none is present. This
matches a stable source cache snapshot, but does not implement TTL cache access,
refresh or Python's repeated calls when caching is disabled. Independent proof
must label any request-count/snapshot difference separately from exact output.
SDK cancellation, service failures and all incomplete traversal/body failures
prevent successful output. Cancellation is not a keyboard interrupt.

The SDK defaults remain 100 pages, 100,000 rows and 4 MiB per response for
items and folders. Explicitly supplied smaller bounds are supported, without
raising, disabling or silently bypassing existing limits. The raw SDK folder
LIST/GET models and operations are unchanged.

## Independent proof

The new `tests/compatibility/folder_commands` reference executes complete,
source-pinned Python definitions and private collaborators with a frozen clock
and guarded offline dependencies. Hand-authored expectations are independent of
the Rust implementation. Existing command, workspace, folder, context, output
and query source cases/digests are unchanged.

`folder_read_probe` is a separate offline JSONL consumer, not a product mode.
It uses the public wrapper, SDK facade, real shared HTTP executor with injected
Azure `HttpClient`/credential/timer, native query evaluator, and actual
caller-owned output streams. It reports observations, never expected callbacks.
Exact source outcomes and safety/unsupported/snapshot outcomes have distinct
denominators; no overall parity percentage is meaningful.

The original 59 fixtures use a synthetic handler-list query protocol. Its
explicit legacy probe adapter joins those tokens using `join_query_args`.
Actual original LS/dir registration instead defines a scalar query, while GET
defines `nargs+`; a handler-list fixture is not raw LS argv evidence.
The [query-argument follow-on](query-argument-compatibility.md) adds a separate
`raw_argv=true` probe path that passes tokens directly to public `parse_args`,
without joining. The old protocol remains only to preserve the unchanged
handler oracle. These are distinct evidence boundaries, not a new executable
mode or expected-output callback.

### Executed local evidence

Final serialized validation completed October 25, 2026 at **06:32:18 UTC** on
`x86_64-pc-windows-msvc`, Rust **1.99.1** (`58cf8f9c3`), Python **3.13.15**.
All heavy commands used the assigned `Local\FabricRustParityValidation` mutex,
private artifact target/environment, one build job and one test thread.

| Evidence | Observed result |
| --- | --- |
| Actual public folder probe | All 59 native outcomes asserted: **32 source-exact, 21 safety, 3 snapshot differences, 3 unsupported** |
| Independent Python reference | **54 terminating command observations and 5 bounded nonprogress proofs**; 36 guard tests passed |
| CLI native contracts | 54 passed: 19 new folder/graph consumers, 22 existing reads, 5 binary contracts, 8 architecture checks |
| Existing SDK regressions | 110 tests and 2 doctests passed, including folder/item/workspace/facade/shared HTTP consumers |
| Existing context/output/query regressions | 28 tests and 3 doctests passed |
| Protected old command comparison | All 25 outcomes asserted: 16 fully source-exact, 6 generic GET URI differences, 3 SDK safety differences; 56 guards passed |
| Protected query comparison | 108 exact and 10 unsupported of 118; 15 guards passed, existing limits unchanged |
| Protected folder SDK reference | 26 comparable and 13 Python-only quirk cases; 6 guards passed |
| Strict CLI all-target Clippy, workspace formatting, new oracle Black check | Passed |

These are distinct inventories, not an overall parity percentage; repeated test
runs are not added to the native total of 192 tests and 5 doctests. The SDK and
leaf runs preceded the final CLI/oracle run; their source was unchanged.

The original new reference contained nine native-only assumptions that disagreed
with independent pinned source or existing base contracts. The coordinator
reviewed the full original/actual/Python evidence before approving a separate
immutable nine-ID native-evidence overlay. Original `cases.json` and all Python
expectations remain byte-identical (Git blob
`dab9f9628d48989a44e7270a577771b2a94df447`); old fixtures/digests are untouched.
The new overlay blob is `c2a058e6b040aecab2ba9a0f02cfb217ebe9ed24`.

Seven corrected item-error/cancellation diagnostics remain **safety**.
`workspace_flat_disabled` is a **snapshot difference**, not exact: the old host
makes one request while Python also loads the assigned parent folder.
`current_snapshot_name` is **source-exact**: structural equality returns empty
data on both sides, contrary to the original native-only ID-based assumption.
These two IDs swap classifications explicitly; coincident aggregate counts
cannot conceal the correction. Historical source/base blobs, original
expectation hashes/categories and every corrected native protocol field are
guarded. See the [reference review record](../tests/compatibility/folder_commands/README.md#reviewed-native-only-evidence-corrections).

All failed receipts are retained. Initial issues included missing development
dependencies, test-only HTTP header/runtime assumptions, caller query token
adaptation and the source-present empty-query distinction fixed only in the new
branch. Restoration followed an actual missing-dependency and offline-restore
failure, using only a private artifact venv. No original expected output was
generated from Rust.

The final successful receipt is `folder-final-receipt-4.json`; complete commands
are in `validate-folder-final.ps1`, with earlier SDK/leaf commands and receipts
retained in the assigned session artifacts. Total acquired gate time across
all attempts was **777.719 seconds**, with no individual run over ten minutes.
The mutex released normally; a **06:32:56 UTC** snapshot found no Cargo/Rust/
Python/Java/link heavy process.

## Remaining boundaries

No authentication backend, transport/TLS selection, ambient time/cache/current
directory, definition/mutation/REPL/text scope, live Fabric call, executable
production host, push, PR, release, or platform parity is supplied here.
Improvements to source nonprogress handling, cache snapshots or folder-query
consistency require a separate decision. Knowledge relay records those proposals
without treating them as approved behavior.
