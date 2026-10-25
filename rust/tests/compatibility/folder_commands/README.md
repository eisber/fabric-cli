<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent folder-command oracle

This additive reference owns **only this directory**. It does not change the
existing command, output, query, SDK folder, item, workspace, or HTTP fixtures.
`cases.json` is Microsoft-authored synthetic test data under the repository MIT
license; its `_copyright` attribution covers every JSON case.

Python is pinned to `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; the agreed
integration checkpoint is `d9df14a6acf7fe3d80adf3bb74ee10ad0a7ac92f`.
The unchanged command/workspace complete-tree validator checks all 259 Python
source files before AST loading. Full selected function/class ASTs are executed,
not translated folder selection, graph construction, equality, or sorting.

## Source execution

The reference executes:

- Both `fab_fs_ls_item.exec` and `fab_fs_ls_folder.exec`.
- `fab_cmd_fs_utils.get_ws_elements` / `sort_ws_elements`.
- `fab_item_util._get_elem_type` / `sort_ws_elems_by_config`.
- Actual memstore item-parent assignment, `get_folder`, folder root/nested
  extraction and complete graph assembly.
- Actual cache key functions and source cache wrappers/decorators, backed by
  genuine `cachetools.cached` / `TTLCache` with a frozen timer. Each command
  gets fresh caches. `FAB_CACHE_ENABLED=true` is the normal Python reference
  boundary; uncached/coherent-snapshot differences remain separate.
- Actual hierarchy classes/enums, item/folder API functions, `do_request`,
  recursive pagination, `ApiResponse`, Python JMESPath 1.0.1, source nargs/query
  wrappers and existing source-executing JSON output/error machinery.

Injected boundaries are scripted in-memory HTTP, fake scoped credentials,
explicit config/context, fixed UTC `2000-01-01T00:00:00Z`, headless private streams,
and frozen cache/HTTP clocks. Source imports can resolve only three injected
collaborators and the already-loaded JSON decoder; no genuine import loader is
permitted. Product execution is guarded against real filesystem reads/writes,
ambient auth, network/DNS, process creation, environment mutation and sleeps.
Preparing AST definitions and dependencies happens before execution.
The development-only `requirements.txt` pins `cachetools==5.5.2` and
`jmespath==1.0.1`; neither is a dependency of the shipped Rust libraries.

The graph safety classifier is **not a second graph implementation**: it only
proves bounded root reachability and refuses nonprogress before source invocation.
It cannot supply parents, hierarchy objects, ordering, selected rows or output.
Orphan, explicit-null parent, non-string parent, two-node cycle and self-cycle
graphs never execute the source `while len(json_folders) > 0` loop. Valid graphs execute the complete
real graph functions, including reverse-order multi-pass and duplicate-ID cases.
The early safety proof considers parents arriving on later pages; an additional
gate checks each actual source-aggregated API snapshot before graph iteration.

## Immutable cases and denominators

There is no record, update, baseline-regeneration, waiver, or bless option.
The manifest has a fixed canonical-LF Git blob pin (only checkout CRLF is
normalized) and a separate ordered/category inventory.
Changing expected results, source inputs, categories, order or case count fails
guards. Native observations never create or redefine expected values.

| Exclusive category | Cases | Meaning |
| --- | ---: | --- |
| Full exact | 32 | Exit, both stream bytes, ordered URLs and scope per request; reviewed per-ID swap below |
| Safety | 21 | Explicit native safety outcomes; five source nonprogress graphs are not run |
| Snapshot difference | 3 | Disabled-cache repeated reads/change and existing config=false workspace request gap |
| Unsupported query | 3 | Two unchanged numeric limits plus explicitly disabled native projection |

There are **59 native cases**, **54 terminating Python observations**, and
**five bounded nonprogress proofs**. The report has separate denominators and
no combined parity percentage. Equal output with unequal requests is never
reported as full exact parity.

### Reviewed native-only evidence corrections

The original `cases.json` bytes and Git blob
`dab9f9628d48989a44e7270a577771b2a94df447` remain unchanged, including every
Python expectation and the original category inventory. Before changing the
comparator, the coordinator independently reviewed exactly nine native-only
assumptions against pinned Python source and the existing Rust baseline
(October 25 task message 4886).

The separately hand-authored, content-pinned `native-boundaries.json` binds each
reviewed ID to its original native-expectation SHA-256 and category, historical
source/base blobs, a complete corrected contract and rationale. It is not a
waiver, an output generator or permission to change another case.

- `missing_items_value` and `malformed_items_json` retain **safety** with the
  existing item `InvalidResponse` message, not the guessed item-list wording.
- `cancel_before` retains **safety** with existing
  `LongRunningOperationCancelled` / `Operation cancelled` and zero requests.
- `item_count_limit`, `item_response_bytes`, `item_page_limit` and
  `foreign_items_continuation` retain **safety** with existing item-operation
  failure wording, not guessed folder-operation wording.
- `workspace_flat_disabled` moves from original exact to native
  **snapshot-difference**: output bytes match, but the unchanged old host makes
  only an item request, while Python also resolves the item's folder.
- `current_snapshot_name` moves from original snapshot-difference to native
  **exact**: both sides return empty data because source equality includes the
  name and parent chain. The original native ID-only assumption was incorrect.

The equal aggregate counts before/after review must not hide the last two ID
changes. The report emits every native classification ID. Guards reject content,
inventory, category, original-binding and evidence drift; provenance checks read
the exact historical Git blobs. Every native field and every original Python
field is asserted. Seven safety cases never receive parity credit. Original
failed receipts and nine-case original-versus-actual evidence are retained in
the implementation session artifacts.

Source findings retained independently:

- Items load first. A present `folderId` triggers folder lookup before item type
  evaluation, even for unknown types; missing type raises the real unexpected
  `KeyError` only after lookup. Native typed missing-type rejection is safety.
- With no item `folderId`, folders load after items. With caching enabled all
  parent resolution and final filtering reuse one real source cache snapshot.
- Raw API display names ending in `.Folder` retain that suffix and gain another.
- Duplicate folder IDs remain in the source graph; parent lookup is first-match.
  Folder equality includes name, type and recursive parents, not just IDs.
- Workspace listing applies real query/default-column and hidden/all handling.
  Folder-context listing bypasses query entirely, ignores hidden/all, and still
  filters children when folder-listing configuration is false.
- Source graph root detection tests absence of `parentFolderId`, not nullness.
  Nonprogress is never made executable by inventing a timeout.

## Native probe protocol

`--rust-probe` takes an explicit executable, launched once outside all product
guards using the unchanged command oracle's JSONL runner. It must be the actual
`folder_read_probe` public `ReadHost.with_folders` consumer with immutable
preloaded catalog/context, real SDK/shared `HttpExecutor` and real `PythonQuery`.
Expected-output callbacks, alternate folder algorithms and source stubs are not
valid probes.

Each input is:

```json
{"id":"example","command":["ls","--output_format","json"],"current_workspace":{"id":"w","displayName":"Current","type":"Workspace"},"current_folders":[{"id":"root","displayName":"Root"}],"config":{"folder_listing_enabled":true,"item_sort":"byname","show_hidden":false},"responses":[{"status":200,"body":{"value":[]}},{"status":200,"body":{"value":[{"id":"root","displayName":"Root"}]}}]}
```

The ordered `current_folders` chain is optional. Optional `cancellation:"before"`
must cause no credential/HTTP operation. `item_limits` / `folder_limits` accept
`max_pages`, `max_items`, `max_response_bytes`. A response `body` can be JSON or
a raw string; `headers` is optional. Native safety/snapshot cases may leave a scripted response tail unconsumed;
the exact independently pinned request list, rather than queue exhaustion,
is authoritative. Python scripts are consumed exactly.
Optional `config.projection:"none"` explicitly selects the native `NoQuery`
provider. Python still executes its actual query and has a separately pinned
source outcome. Folder-context query bypass remains the source branch; explicit
paths without a preloaded catalog entry remain outside this reference's context
boundary, rather than inventing a resolver or extra path lookup request.
The three snapshot cases use separate `python_input.responses` and
`python_expected` requests where required, never a request-equality waiver.
The probe caller joins Python query tokens with the existing native
`join_query_args` before constructing the public `ReadCommand`. The unchanged
LS parser's single-query-argument limitation remains a separate parser gap;
these handler/host comparisons do not claim complete executable parser parity.

One output line per input contains **exactly**:

```json
{"id":"example","exit_code":0,"stdout":"...","stderr":"","requests":["https://api.fabric.microsoft.com/v1/workspaces/w/items","https://api.fabric.microsoft.com/v1/workspaces/w/folders?recursive=True"],"scopes":[["https://api.fabric.microsoft.com/.default"],["https://api.fabric.microsoft.com/.default"]]}
```

Success/error streams are byte-exact source JSON envelopes with four-space
indentation, escaped Unicode and LF. The envelope timestamp is fixed, not
removed for comparison. Safety expectations require explicit command failures;
nonprogress and limits cannot become empty successes.

## Validation

Run from `rust` only through the owner's common validation mutex, with a private
target and the pinned development-only requirements:

```powershell
python -m black tests\compatibility\folder_commands\reference.py tests\compatibility\folder_commands\test_reference.py
python tests\compatibility\folder_commands\reference.py --check
python -m unittest discover -s tests\compatibility\folder_commands -p test_reference.py
python -m black --check tests\compatibility\folder_commands
python tests\compatibility\folder_commands\reference.py --rust-probe <absolute-folder_read_probe.exe>
```

Actual Windows local validation on October 25, 2026 passed **all 59 native
outcomes, 54 terminating Python observations, five nonprogress proofs and 36
guard tests**. Exact/safety/snapshot/unsupported denominators are **32/21/3/3**,
with the reviewed per-ID swap above. The public probe and all original Python
expectations are independently asserted; failed earlier receipts are retained.
Strict CLI Clippy, workspace formatting, new oracle Black checks and existing
protected command/query/folder references also passed. See the
[command compatibility matrix](../../../docs/folder-command-compatibility.md#executed-local-evidence)
for distinct inventories and remaining gaps. This is not integrated/native-CI
or cross-platform evidence.

Native-only safety contracts were hand-authored from explicit bounded policies
and, for the nine reviewed corrections, independently verified existing
contracts. They are not generated from Rust observations. Existing Python
expectations are unchanged.
Graph `NullParent`, `NonStringParent`, `UnresolvedParent` and `Cycle`, and dangling
item `ItemParent`, are `NotSupported` safety failures, not query-unsupported cases.
Malformed folder responses and missing item types are explicit `InvalidResponse`
failures. SDK traversal/body/continuation bounds retain `ReadFailed` failures.
Any deliberate contract disagreement requires owner review, not regeneration
from Rust output.
No live authentication, service call, write command, source mutation, CI or
cross-platform parity is claimed.
