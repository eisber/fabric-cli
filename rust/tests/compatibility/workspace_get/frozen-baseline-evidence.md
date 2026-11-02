<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent workspace GET source oracle

This directory is additive and independently owned. It changes no product
source, Rust code, Cargo file, old oracle, old fixture, or old query golden.
Inputs and expectations in `cases.json` were authored manually **before any
Python source execution or Rust comparison**. There is no recording, update,
bless, regeneration, or waiver mode.

## Provenance and execution

- Integration baseline: `fad3ea7fee3a3e4e66205ffcf34d6b943bb416d7`.
- Original Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
- Complete `src/fabric_cli` Git tree:
  `e8bcb0d47d7b705d665c3202da2770eafe65e22a`.
- Frozen hand-authored manifest Git blob:
  `f63fd4307f8712504c23c38e381c6ea994696e9e`.
- Independently frozen additive `status-cases.json` Git blob:
  `fb605e652be16b395c59d51ff9f22b6726744276`.
- Independently frozen additive `truthiness-cases.json` Git blob:
  `3cf5447ad47fe7afd23080be37961e0de16d09b9`.
- Complete original executed file blob identities: `reference.py::BLOBS`
  (21 complete original files, not hashes of extracted snippets).
- Read-only supporting oracle identities: `SUPPORT_BLOBS` pins the original
  commands, items, output, and workspaces `reference.py` files.

Before source compilation, the oracle checks the existing protected 259-file
Python/YAML tree digest, original and integration Git tree objects, the current
committed Python Git tree, every original executed Git blob, and every executed
working-tree file. Only checkout CRLF normalization is permitted. Rust-only
commits after the integration baseline do not invalidate the source pin.

`observe` executes the complete, unmodified original
`commands/fs/get/fab_fs_get_workspace.py::exec` definition, not a reimplementation
of its branches. It connects the actual four workspace API functions:

1. `get_workspace`
2. `ls_workspace_managed_private_endpoints`
3. `get_workspace_spark_settings`
4. `acl_list_from_workspace`

Each uses actual `do_request`, `_handle_successful_response`,
`ApiResponse.append_text`, private-link-disabled URL selection, status mapping,
`CommonErrors`, `FabricCLIError`/`FabricAPIError`, and actual query/output handling.
The query chain is original `process_nargs`, `query_and_export`,
`fab_jmespath.search`, and `_get_json_paths`, with development-only
`jmespath==1.0.1`. The bounded adapter supplies the same one-token nargs list to
the actual source processor; general registration/argument parsing remains owned
by the unchanged `query_arguments` oracle. Output uses the same
original definitions as the old command oracle: `dumps`, `OutputResult`,
`FabricCLIOutput`, JSON UI writers, exception decorator and main unexpected-error
handler. Original workspace/tenant/context classes supply `id` and `full_name`.

Only the outer boundaries are synthetic: an explicit current workspace,
in-memory auth, sequential scripted HTTP responses, fixed private-link-disabled
configuration, headless output writers, and frozen UTC time
`2000-01-01T00:00:00Z`. Every actual HTTP request must acquire exactly the Fabric
scope. The fake HTTP seam validates method, synthetic Authorization header,
User-Agent, content type, and 240-second timeout. It does not replace HTTP
status handling or paging. The actual SDK/probe receives no expected values.

The product execution guard rejects real imports/identity discovery, network,
processes, reads, writes, sleeps and ambient filesystem/environment changes.
A forbidden attempt invalidates the run **even if caught by source's broad
optional-enrichment exception handlers**. Extra and missing scripted responses
also fail. The oracle does not create temporary directories or files.

## Original frozen inventory: 61 cases

| Category | Count | Meaning |
| --- | ---: | --- |
| Exact | 56 | Full ordered requests/scopes, stdout/stderr bytes and exit must match source and handwritten expectations |
| Explicit native safety | 3 | Malformed/null primary metadata fail explicitly; non-list optional collections are omitted instead of copied literally |
| Explicit unsupported | 2 | Existing over-i64 sum and out-of-u64 literal query boundaries remain actual native InvalidInput failures |

All 61 execute the source. With `--rust-probe`, all selected cases execute native
and must match their own complete frozen contracts; none are skipped. Safety and
unsupported outcomes are counted separately and are **not full source parity**.
The native safety messages are independently authored required contracts, not
claimed observations of an unrun implementation. A mismatch requires review;
it is never permission to derive a new expectation from Rust.

Coverage includes:

- Unknown and nested ordered metadata, opaque non-GUID IDs, Personal workspace
  context, Unicode and duplicates.
- Preexisting enrichment keys replaced **without moving their dictionary slots**.
  A failed optional enrichment leaves any preexisting source value untouched.
- Empty, single, two-page and three-page collections and exact token encoding.
- Automatic primary and Spark-settings paging, including first-page scalar/key
  retention, list-only merges, discarded later-only keys, continuation removal
  and the source `total_pages == 1` recursion quirk.
- Optional 401/403/404 at **each** endpoint, malformed JSON at each endpoint and
  missing collection values, all while attempting later enrichments.
- Collection failure after a previously collected page: the **whole** optional
  enrichment is omitted, not returned as partial success. A primary later-page
  failure is fatal and does not render the partial first page.
- Optional first-page 201 promotion after a 200 continuation, and a continuation
  201 that is requested but **not merged** into the original 200 collection.
- Empty primary with all failed enrichments prints nothing; valid JSON null
  Spark settings are assigned literally rather than treated as a decode error.
- Primary 401/403/404/400, malformed service errors, 418, malformed success JSON,
  null, and source non-200/201 status quirks.
- Query None, empty, dot, quoted keys, false, integral float, zero, missing/null,
  empty string/list/object, syntax error, type error and numeric boundaries.
  Quoted-key coverage includes the actual source outer-quote stripping quirk:
  `("a.b")` preserves the literal quoted field, whereas `"a.b"` alone is stripped
  to nested access `a.b` before query execution.

### Source findings and current treatment

These are source observations inferred directly from the pinned definitions
and frozen as required expectations; the host must execute the source to verify
them before claiming observations passed.

1. **Only status 200 enriches and renders.** Plain primary 201 returns `{}` and
   emits **zero bytes**, despite the HTTP helper accepting it. Primary 204 and
   202 with a jobs-instance Location behave similarly. The test separately checks
   the actual command return value, not just the output stream. A 201 followed
   by continuation status 200 is promoted to 200 and therefore does enrich.
2. **Optional enrichment catches every `Exception`.** Authorization, not-found,
   malformed bodies, missing collection keys and later page failures are not
   fatal and do not prevent subsequent endpoint attempts. A failed enrichment
   does not delete an already present key.
3. **Generic paging is recursive and not collection-specific.** It may run on
   primary metadata or Spark settings. `append_text` extends only list-valued
   first-page keys also present on later pages, keeps first-page scalars, drops
   later-only keys and continuation fields, and writes `total_pages: 1` at each
   merge. This must not be simplified to a conventional pager.
4. **GET printing checks truthiness before constructing the output envelope.**
   Empty query string selects path discovery; `.` selects identity. Empty
   list/object/string projections emit zero bytes, unlike some LS behavior.
   Scalar false/zero/null are stringified first as `False`/`0`/`None` and print.
5. **Malformed primary success is an unexpected main error.** Its source JSON
   error is unscoped; the separate native safety contract is command-scoped
   `InvalidResponse`. JSON-null primary still attempts all source enrichments
   before printing nothing; native must reject the non-object before enrichment.

These are not redesign approvals. Relay substantive source oddities to migration
forum `tk:forum/12/4` through the authorized coordinator; this SDK lane does not
post externally or use Tachikoma lifecycle/reporting tools.

## Additive status follow-up: six more independently frozen cases

The implementation host subsequently reported a deliberate status-200-only
native metadata boundary and requested explicit status-transition evidence.
`status-cases.json` is additive: the original `cases.json`, its source
expectations, inventory, categories and original blob identity remain unchanged.
The six additional cases use complete primary metadata with string `id` and
`displayName`, so native's reported schema validation cannot mask the intended
status branch:

| Case | Original source behavior | Explicit native contract |
| --- | --- | --- |
| `status_primary_201` | Returns `{}`, zero bytes, exit 0, one request | Explicit unsupported-primary-status failure, one request |
| `status_primary_204` | Returns `{}`, zero bytes, exit 0, one request | Explicit unsupported-primary-status failure, one request |
| `status_optional_201` | Omits all three optional 201 responses, still attempts all four requests | Exact source behavior |
| `status_optional_200_then_201` | Requests the 201 continuation but renders only the first 200 collection page, then attempts later endpoints | Omits the entire optional collection instead of emitting truncated success; all five requests remain required |
| `status_primary_201_then_200` | Real generic pager promotes 201 to 200, merges list fields, then enriches; five requests | Rejects primary 201 without following continuation; one request |
| `status_optional_201_then_200` | Real generic pager promotes optional 201 to 200 and includes both collection pages; five requests | Omits optional 201 without continuation, still reads Spark/ACL; four requests |

Five are explicit **native safety**, not exact source parity; only
`status_optional_201` is exact. The source executes all six original command,
API, HTTP, paging and output chains. In particular, no simplified status model
is substituted for `_handle_successful_response` or `ApiResponse.append_text`.

Primary status failures have an independently hand-authored required native
protocol: exit 1, command `get`, error code `NotSupported`, message
`Workspace metadata GET requires HTTP 200`, exact request/scope transcript and
JSON bytes. This is a proposed precise safety contract to verify/review with the
host, **not** a claim that the unexecuted native implementation already uses
that message or error mapping. Do not regenerate it from native output.

Only `status_optional_201_then_200` needs a separately frozen native response
script. It removes the source continuation reply that native deliberately never
requests. Primary, first MPE response, Spark and ACL responses are otherwise
identical and retain order. This prevents a FIFO mock from incorrectly serving
the unused MPE continuation as Spark settings. Native requests/scopes remain
fully asserted; no fixture-specific Rust adapter or service logic is introduced.

The status follow-up brought the inventory to 67 source cases. `--suite original` selects the
unchanged 61-case inventory; `--suite status` selects the six additive status
cases. Selection is always reported and is never full-slice parity evidence.
Across both inventories there are 57 exact contracts, eight separate safety
contracts and two unsupported-query contracts; these are required assertions,
not claims of passing native behavior.

### Remaining native schema/status conflicts are not waived

The implementation host also reported that native primary metadata requires
string `id` **and** `displayName`, rejects any truthy primary continuation, and
accepts Spark settings only as objects. Source has none of those validation
requirements. Many original synthetic primary bodies deliberately exercise
arbitrary dictionaries without `displayName` (the replacement-key case lacks
both metadata identity fields); original `spark_null_payload` assigns null
literally, and the primary-paging cases run the actual generic source pager.

Those existing source-exact contracts remain frozen. The new status addendum
does **not** silently relabel them, add fields to their input/output, delete
them, substitute native outputs, or claim the current native implementation
matches them. A full native comparison must expose these conflicts until the
owner resolves native translation versus separately approved safety evidence.
The six-case focused status gate must not be presented as the full 67-case gate.

## Whole-command truthiness follow-up: three more exact cases

`truthiness-cases.json` adds valid-metadata queries selecting the actual
enrichments `roleAssignments` (empty list) and `sparkSettings` (empty object),
plus a scalar-false control. The expectations remain **source requirements**,
not native envelope behavior.

All three execute this actual original single-command source pipeline:

1. `main.main`
2. `main._execute_command`, including actual `Command.get_command_path`
3. Decorated `fab_fs.get_command`, including actual `handle_exceptions` and
   `set_command_context`
4. `FabricElement.check_command_support`
5. `fab_fs_get.exec_command`, including actual `process_nargs`
6. Complete workspace `exec`, four actual API methods, actual HTTP/paging,
   `query_and_export`, JMESPath and UI definitions

The parser supplies the bounded already-built Namespace rather than reading
ambient argv. Configuration initialization, disabled diagnostic logging,
autocompletion, the current `"."` workspace resolver and the single approved
workspace-GET command-support lookup are injected boundaries. No filesystem,
credentials or general command-support YAML discovery occurs. The actual
command-support check and all source dispatch bodies still run. Source exits
use a private `BaseException` subclass, so `main` cannot mistakenly catch the
substitute exit as an unexpected ordinary exception.

The pinned code has **no final single-command output serializer**:

- `query_and_export` does not call the UI when the projected list/object is empty.
- Workspace `exec` still returns its complete metadata object.
- `fab_fs_get.exec_command` discards that return value.
- `fab_fs.get_command` also does not render a return value.
- `main._execute_command` returns the command's result, and single-command
  `main` only selects the exit code and exits. Its multiple-command summary is
  not entered for this string-valued single `get`.

Consequently the two empty-container cases require exactly **stdout `""`,
stderr `""`, exit 0, four ordered full requests and four Fabric scope
acquisitions**. There is no `{result:{}}` or `{result:{data:[]}}` success
envelope. Scalar false becomes the nonempty string `"False"` and emits exactly
one ordinary success envelope. The guards reject substituting a native empty
envelope for either empty source stream.

This is pinned-source control-flow evidence plus frozen expectations; the
authoring lane still has not executed Python. If the host's native `ReadHost`
always renders an envelope, these exact cases must fail native comparison until
translation is corrected or a separate owner-approved difference is recorded.
They must not be redefined to native output.

The default now selects **all 70 source cases**: 60 exact, eight separate native
safety, and two unsupported. `--suite truthiness` selects only the three
whole-command cases and explicitly reports that selection.

## Existing native JSONL adapter

Use the existing `fabric-cli` example `read_probe`, not fixture-specific Rust
code. Only its protocol was inspected. `inputs_for` sends this existing shape:

```json
{"id":"case-id","command":["get",".","--output_format","json","--query","."],"current_workspace":{"displayName":"Offline","id":"w","type":"Workspace"},"responses":[{"status":200,"body":{"id":"w"}},{"status":200,"body":{"value":[]}},{"status":200,"body":{}},{"status":200,"body":{"value":[]}}]}
```

One input line must produce one output line:

```json
{"id":"case-id","exit_code":0,"stdout":"...exact JSON envelope bytes...","stderr":"","requests":["...ordered full URLs..."],"scopes":[["https://api.fabric.microsoft.com/.default"]]}
```

The source protocol has exactly these same fields. Command return values are
checked separately for designated source cases; they are not fabricated native
protocol fields. Literal request sets in the manifest expand only handwritten
full URLs; scopes expand one explicit Fabric scope per expected request.
All comparisons include ordering, Unicode JSON escaping, numeric spelling, LF
newlines, empty streams, exit codes and complete request/scope transcripts.

## Validation: host-only, under the shared OS mutex

**No Python, restore, or Cargo command was executed by the authoring lane.**
The parent host must place the following commands in its task-private script,
run that script through its existing shared OS mutex wrapper, and retain a
fresh receipt containing script identity, exit code, source/manifest identities,
and actual results. Do not invoke these commands outside that gate or infer a
lease from this document. The wrapper's exact interface is host-owned.

From repository root, inside that gated task-private script:

```powershell
python -B -m unittest discover -s rust\tests\compatibility\workspace_get -p test_reference.py -v
python -B rust\tests\compatibility\workspace_get\reference.py
```

If the selected validation fails specifically for missing `jmespath`, restore
only then, through the same gate:

```powershell
python -m pip install -r rust\tests\compatibility\workspace_get\requirements.txt
```

For native comparison, build the existing example from `rust`, inside the gate,
or reuse the host's separately verified build artifact:

```powershell
Push-Location rust
cargo build --locked -p fabric-cli --example read_probe
Pop-Location
python -B rust\tests\compatibility\workspace_get\reference.py --rust-probe rust\target\debug\examples\read_probe.exe
```

If the host uses a custom `CARGO_TARGET_DIR`, pass that actual verified absolute
probe path instead. The Cargo example's existence does not establish parity.
The source-only invocation explicitly reports native outcomes **NOT RUN**.
Optional focused diagnosis:

```powershell
python -B rust\tests\compatibility\workspace_get\reference.py --case primary_201
python -B rust\tests\compatibility\workspace_get\reference.py --suite status
python -B rust\tests\compatibility\workspace_get\reference.py --suite status --rust-probe rust\target\debug\examples\read_probe.exe
python -B rust\tests\compatibility\workspace_get\reference.py --suite truthiness
python -B rust\tests\compatibility\workspace_get\reference.py --suite truthiness --rust-probe rust\target\debug\examples\read_probe.exe
```

No native comparison, test pass, platform pass, or mutex receipt is claimed by
this authoring handoff. All original fixtures/source/hash pins/query goldens
remain immutable.
