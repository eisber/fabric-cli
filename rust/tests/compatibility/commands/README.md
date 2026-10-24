<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Source-pinned read-command oracle

This independent reference covers tenant workspace `ls`, current-workspace item
`ls`, and **bounded generic metadata** `get .` output. It is not full `get` parity.
All new inputs and semantic expectations in `cases.json` are hand-authored,
synthetic, and secret-free. Separately hand-authored native safety, unsupported,
and schema-order assertions live in `native-boundaries.json`; they never replace
the protected source expectations. There is no recording, update,
snapshot-generation, or pin-refresh mode.

## Pinned source and actual execution

Python source: `microsoft/fabric-cli` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
The unchanged workspace reference's complete-tree SHA-256 validator checks all
259 Python/YAML source files before execution. The existing output reference
also checks its executed source blobs. A mismatch requires owner review, not
regenerated expectations.

The oracle parses complete, named source definitions into private namespaces;
it does not translate their algorithms or import the installed product package.
The existing item reference provides the definition compiler and minimal
case-insensitive response headers. The existing output reference provides real
source output/error/decorator definitions with private streams and frozen time.
No external Python dependency is needed.

Actual source call paths:

| Reference | Executed boundary |
| --- | --- |
| Workspace `ls` | `fab_fs_ls_workspace.exec` → real memory-store workspace enumeration → real `Workspace`/enum constructors → sorting/projection → real output helpers |
| Item `ls` | `fab_fs_ls_item.exec` → `get_ws_elements` → real memory-store item enumeration → real `Item`/enum constructors → `sort_ws_elems_by_config` → projection → real output helpers |
| Metadata `get .` | `get_item_with_definition(..., obtain_definition=False)` → real item API helper → `query_and_export(..., query=None)` → real `fab_jmespath.search`/`_get_json_paths` → real output helpers |
| Every request | Real API helper → `fab_api_client.do_request` → injected fake auth and scripted HTTP session, including the real pagination and error mapping |

Only auth, HTTP, configuration, current context, time, logging, and headless
console collaborators are substituted. Source failures use the real
`handle_exceptions` decorator; the old missing-value case reaches the real main
unexpected-error helper. Current context is checked before and after execution.
Every response must be consumed, every HTTP attempt must acquire the Fabric
scope first, and any unplanned request fails.

The offline boundary denies product file reads **and** writes, networking,
identity imports, subprocesses, directory/environment mutation, and sleeping.
Fixture/source reads and the explicitly requested native probe invocation occur
outside this boundary. The reference creates no scratch directory or files.

## Independent inventory

- **Seven unchanged workspace command cases** are read directly from
  `../workspaces/cases.json`, not copied or edited. Their original expectations,
  including all additional API/exception fields, remain in that file. This
  command comparator reuses their output/exit/request expectations; the original
  workspace oracle retains responsibility for its extra API/exception fields.
- The eighth old case, `query_projection`, remains unchanged and belongs to its
  existing oracle. This bounded native slice does not implement JMESPath or
  pretend that rejecting a query is query parity.
- **18 new protected source cases**: 12 list cases and six metadata-get helpers.
  Lists exercise empty data, by-name/by-type ordering, enum-name versus
  display-value ordering, case-stable duplicate IDs, Unicode/slashes, unknown
  future-type filtering, hidden collections through both flags and config,
  untruncated long IDs, token pagination, and 401/403/404 mapping.
  Metadata cases exercise default property paths, depth/array/null/empty
  semantics, and 400/401/403/404 mapping.
- **One source-only case**, empty metadata, records that `query_and_export`
  prints nothing for an empty path list. A native typed response may reject
  missing required fields; this is not counted as native parity.

Default no-query metadata output is an insertion-ordered **list of property
paths**, not the raw metadata object. Default traversal stops at depth four.
Empty lists contribute a path; empty dictionaries do not. This runs the actual
source traversal and introduces no JMESPath implementation.

## Explicit generic-GET differences and omissions

The bounded Rust command intentionally uses
`/v1/workspaces/{workspace_id}/items/{item_id}`. The actual Python metadata helper
uses `format_mapping`, for example `notebooks`, `lakehouses`, or `reports`.
Every GET case therefore contains separately hand-authored `python_requests`
and native `expected.requests`, and requires `comparison: specialized-uri`.
Each side must satisfy its own exact URI expectation. For all six GET cases,
**only that request URI now differs**; stdout bytes, stderr bytes, exit status,
scope values/counts, and correlation ID must otherwise match. The two successful
GETs previously had the historical projection/schema boundaries below, which
remain protected evidence rather than current expectations. Dropping a declared
boundary or treating either side's URI as the other's fails the guards.

This deliberately does **not** execute `fab_fs_get_item.exec`, which ordinarily
obtains definitions and augments the result with connections, schedules,
Environment libraries/Spark metadata, or mirrored-database status. It does not
cover definition decoding/export, files, output directories, query expressions,
deep traversal flags, or full get routing. None of the six helper comparisons
is evidence of full Python `get` command parity.

**Every GET is sent to the actual native probe**, including successful service
responses. The default `--get-mode metadata-preserving` requires both discovery
outputs to match protected Python bytes/order. The actual read host calls
`get_item_metadata`, retaining the complete validated ordered HTTP object,
then uses real `PythonQuery`. Neither known fields nor extras are reconstructed
through legacy `Item`; no expected-output callback is involved.

Historical modes are explicit, never automatically selected for a failing probe:

- `--get-mode historical-no-projection` requires the original exact
  `NotSupported` envelope, exit 1, generic metadata request and scope. These are
  unsupported evidence, not skipped cases or parity.
- `--get-mode historical-typed-order` requires unchanged historical typed-schema
  order from `native-boundaries.json`. Legacy `Item` still serializes `id`,
  `displayName`, then alphabetical `BTreeMap` extras. That historical order
  differs from the source and from the current preserving GET consumer.

Both modes still assert all 25 outcomes when used with their corresponding
historical probe. They deliberately reject a current successful/source-ordered
probe. `--with-get-projection` remains a compatibility flag requiring an explicit
probe; current metadata-preserving behavior is already the default. Use
`--get-mode` to request historical evidence. `NoQuery` opt-out behavior also
remains covered by public Rust consumers.

Additional `metadata_reference.py` cases use hand-authored raw response strings
and path expectations, not changes to `cases.json`: unknown-before-known keys,
reordered fields, null/missing/non-string type, nested order/Unicode/numbers,
and duplicate keys. They execute the original helper and `json.loads` parser.
Duplicate keys retain the last value in the first position, matching the actual
ordered `serde_json::Value` parser; this is parsed-data behavior, not an arbitrary
new duplicate or lexical-body preservation contract.

Generic metadata helper evidence is not source command-support evidence. In
particular, the source full `get` command rejects Dashboard, PaginatedReport,
SQLEndpoint, MirroredWarehouse, Datamart, OrgApp, MetricSet, ApacheAirflowJob,
Exploration, RetailDataManager, Healthcaredatasolution,
SustainabilityDataSolution, and AISkill. The Notebook helper cases do not
override that routing policy or claim full Notebook GET behavior.

The native host/SDK owns request safety: safe resource-segment validation,
trusted service origin, bounded responses/pagination, typed malformed-response
failures, and no ambient credential fallback. These are native safety seams,
not claims that Python enforces identical restrictions. This reference never
passes arbitrary user URLs or unsafe resource paths to Python and then treats
that as authorization to weaken native safety. Its allowed paths are only
tenant root, omitted current workspace path, and `get .` with an injected item
and parent workspace. Explicit metadata lookup, folders, folder-listing-enabled
configuration, other hidden collection commands, and mutations are outside
this bounded oracle. No optional metadata protocol is needed.

## Separate native safety evidence

The source oracle and old eight-case file remain unchanged. Three cases assert
both the protected Python outcome and a different, complete native contract:

| Case | Protected Python behavior | Exact native safety assertion |
| --- | --- | --- |
| Old `token_pagination` | Ignore foreign continuation URI, follow tokens for three requests, then successful sorted data | `InvalidContinuation` currently maps to exit 1, code `ReadFailed`, message `Workspace listing did not complete`, command `ls`; exactly one original-origin request/scope and no partial success |
| Old `missing_value` | Main unexpected-error output: `UnexpectedError`, message `value`, no command | Exit 1, code `InvalidResponse`, message `Fabric returned an invalid workspace-list page`, command `ls`; exactly one request/scope |
| New `item_token_pagination` | Ignore foreign continuation URI, follow token for two requests, then successful sorted data | Item SDK also rejects the URI: exit 1, code `ReadFailed`, message `Item operation did not complete`, command `ls`; exactly one original-origin request/scope |

These are explicit safety exceptions, **not source parity, ignored failures,
or changed goldens**. The native probe must permit an unconsumed response-script
suffix when the real SDK intentionally rejects continuation before another
request; it must not consume fake responses just to empty the script.
The source harness, which follows all scripted source pages, still requires
every response consumed.

## Native probe contract

The caller supplies an already-built probe with `--rust-probe PATH`. The oracle
never builds Rust. The probe must call the real public async
`fabric_cli::ReadHost::dispatch` path with the default SDK operation and fake
executor/credential, not print an expected-shaped stand-in.

Input is UTF-8 JSON Lines, one case per line:

```json
{"id":"example","command":["get",".","--output_format","json"],"current_workspace":{"id":"ws","displayName":"Current","type":"Workspace"},"current_item":{"id":"item","displayName":"Synthetic","type":"Notebook"},"responses":[{"status":200,"body":{"id":"item","displayName":"Synthetic","type":"Notebook"}}],"config":{"show_hidden":false,"item_sort":"byname","folder_listing_enabled":false}}
```

`body` may be JSON or an unparsed response string; `headers` is optional.
`current_item`'s parent is `current_workspace`. There is no lookup I/O for these
cases. Config keys are optional. Source-only malformed responses are not sent
to the native probe.

Output is one UTF-8 JSON line for each input, in order, with **exactly**:

```json
{"id":"example","exit_code":0,"stdout":"...","stderr":"","requests":["https://api.fabric.microsoft.com/v1/workspaces/ws/items/item"],"scopes":[["https://api.fabric.microsoft.com/.default"]]}
```

Command stdout is Python-compatible JSON with timestamp
`2000-01-01T00:00:00Z`, four-space indentation, ASCII Unicode escaping, and one
terminal LF; JSON failures are also on stdout. Stderr is empty in these cases.
The probe transport itself is JSON Lines with LF. Requests are complete
absolute URLs and scopes record one actual credential request per HTTP attempt.
New expectations compare exact bytes. Only the old workspace semantic
expectations strip the timestamp; the actual Python/native old-case comparison
still checks exact streams.

## Validation and guards

Run these **only after the parent grants the shared-host heavy-work mutex**.
From the repository root, using the already-built example executable:

```powershell
python rust\tests\compatibility\commands\reference.py
python rust\tests\compatibility\commands\reference.py --rust-probe rust\target\debug\examples\read_probe.exe
python rust\tests\compatibility\commands\metadata_reference.py --rust-probe rust\target\debug\examples\read_probe.exe
python -m unittest discover -s rust\tests\compatibility\commands -p "test_*.py"
```

Use the actual target directory/executable suffix on the host. `--case ID`
selects one non-query case. Without selection the script checks **25 protected
source observations plus one source-only observation**. All three native modes
execute and assert **all 25 actual native outcomes**:

| Exclusive assertion category | Historical `NoQuery` | Historical typed order | Default preserving metadata |
| --- | ---: | ---: | ---: |
| Fully exact source bytes/requests | 16 | 16 | 16 |
| Exact GET output with generic-URI difference | 4 | 4 | 6 |
| Separate native safety failures | 3 | 3 | 3 |
| Actual explicit `NotSupported` GET failures | 2 | 0 | 0 |
| Separate successful metadata schema/order evidence | 0 | 2 | 0 |

The two metadata-order differences are eliminated, not relabeled or hidden.
Counting outputs alone gives 18 exact LS/inventory cases and four exact
service-error cases, but **not 18 fully source-exact cases**: the two successful
GET cases also use Python `notebooks`/`lakehouses` versus native `items`.
All six URI differences remain independently asserted. Matching output is not
full GET parity; the three safety differences are not successes.

Every mode requires all protected Python expectations unchanged.
An absent probe is reported as **not requested**, never as native parity or
proof that an unsupported native failure was actually observed.

The unittest suite has **56 tests**, retaining all original source/guard tests
and adding current-versus-historical mode and synthetic raw-metadata controls.
Guards reject source/pin identity drift, validator bypass, inventory erosion,
expectation edits, stream/exit/request/scope drift, line-ending/format/timestamp/
Unicode-escaping changes, undeclared URI differences, trivial fixtures,
expected-payload replay, unused/unplanned responses, unsupported query/path/
folder seams, side effects, nondeterminism, erased native-boundary inventories,
false unsupported successes, metadata-order equivalence, current-mode acceptance
of historical ordering, and counting source-only/unsupported/safety evidence as
native parity. Native shared-HTTP consumers additionally assert complete query
output bytes and types for identity, keys and numeric-array projection, not only
default inventory. Execution receipts belong in the task/integration handoff;
local Windows evidence is not cross-platform or live-service evidence.
