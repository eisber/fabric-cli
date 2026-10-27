<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# TABLE family compatibility matrix

## October 27 central assembly candidate

The central assembly starts at exact shared
`b78d9654a37ca909036df54b8562e01fcfe8aa13`. It recovers only the TABLE-owned
tree at `4304083a8dada9b2c9c1e36ba84f6904a31a47c2` plus the preserved draft
whose SHA256 is
`80ba7b9d2b9c5d376758f4dbca70a7a5f892dc61b7590313f398ef934d395196`.
The closed original worktree is not modified and its unrelated ancestry and
Cargo lock are not merged. The graph adds two internal packages/edges only;
all 218 external registry records and external features are retained.

`fabric_cli::parser::Invocation::Table` connects all four root leaves to
`TableSession` through `FamilySession` and the existing native executable host.
The session delegates to the existing `CommandContext`/`PathResolver` snapshot,
real OneLake HTTP properties/list/read, `SharedTableExecutor`,
`FabricLoadCompletion`, `JobMaintenance`, checked generic job HTTP and
`SharedJobOperations`. Relative OneLake contexts and source tokens use the same
resolver rather than a second path implementation. Source progress precedes
final output; original table/operation/job evidence and warnings survive output
delivery failure. Bare 202 remains started, never completed.

The new `table_mounted` contracts exercise actual root argv through that
production path with only credentials, HTTP, time, configuration and output
injected. Four passing happy contracts can add four canonical leaves, not
fixture counts, parser/help counts or SDK-only consumers. This candidate is
**unvalidated until exact-SHA runner receipts are recorded**. Historical
component passes below are not central candidate proof. Auth mounting,
Windows/macOS execution, live Fabric, REPL/batch and full option/resource parity
remain separate; the canonical target is 50/52, not whole-product 95% parity.

Python authority is `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
The lane safely merged exact `cc908ce08617f92601aa2dd68622797ccbcde9e7`,
then shared execution prerequisite `68e15d38bcc3df751b893414265876304a16a2f8`.
Its original `8ad4c703bdd6738189f59318a04cab297b6c4381` upstream ancestry was
preserved, not reset or discarded. That upstream change is not a new oracle.
Parent integration should adopt only the scoped Rust implementation commit.

## Source provenance

Paths below are relative to `src/fabric_cli`; these are exact Git blob identities.

| Source | Blob | Behavior |
| --- | --- | --- |
| `commands/tables/fab_tables.py` | `5a4134688776923bbaa3632d00129510fd9c4527` | Resource checks, wait flag, configuration, progress |
| `commands/tables/fab_tables_load.py` | `4da52548171a177eefbb4ff1e8283f439017ec87` | Properties, formats, request shape, started/completed output |
| `commands/tables/fab_tables_schema.py` | `74f3b39ddfba1616bc041bf0d84dc6109332a6d5` | Delta-log listing/read order, metadata, fields/errors |
| `commands/tables/fab_tables_opt.py` | `c101c581f6ee560e0a7e56b98f9087cdbe17324c` | TableMaintenance execution data and tempfile cleanup |
| `utils/fab_cmd_table_utils.py` | `3b00fb6af4e5e1037e288a84cc2ac361970f596a` | Names/schema, retention floor division, comma parsing |
| `client/fab_api_tables.py` | `eaec86fff74f2ea424c5bc3138cdbd2e3a5e2c0f` | Actual Lakehouse load endpoint |
| `client/fab_api_client.py` | `0e896e27ff9fe0e6446448d721679db422f283dd` | Conditional LRO, result, continuation and status policies |
| `client/fab_api_onelake.py` | `73508f6d00811dd0240727348bc3f62dac8b0b51` | Storage GET/HEAD/list routes |
| `client/fab_api_jobs.py` | `a333bf53db369c0c54cc99db67943b44587b3ba9` | Job submission/status/cancel routes |
| `parsers/fab_tables_parser.py` | `81fbec50db0129575e1bba0318b85994ca7a59a6` | Four leaves and exact argument shapes |

## Family and behavior denominator

| Surface | Implemented TABLE-owned behavior | Remaining dependency / evidence |
| --- | --- | --- |
| Group registration | Four leaves, group-help signal; no aliases | Front-door attachment/help formatting remains its owner's work |
| `schema` | Reverse-lexicographic JSON logs, first metadata action, raw fields, malformed/missing schema errors; released OneLakeHttpExecutor and generic SDK all-page reads | Shared context/text; generic typed safety exceptions remain classified |
| `load` | Real source HEAD before options, File/Folder, recursive, mode, format, extension, Lakehouse endpoint; actual received202 shared PythonSynchronous engine and shared error formatter | Shared context/text and remaining raw-storage diagnostic extension |
| `vacuum` | Signed retention floor division, default 168 hours, schema only at three components, exact execution data; actual JobClient submission/status/timeout/cancel via `JobMaintenance` | Generic cancellation LRO only when selected by response; remaining presentation/context integration |
| `optimize` | Empty optimize settings, optional true V-Order, trimmed Z-Order preserving empty columns; same actual generic job lifecycle | Same conditional cancellation LRO and presentation integration |
| Output | Existing JSON envelope, source root/leaf command distinctions, table progress on stderr, no-output terminal outcomes, propagated writer errors | Shared text renderer and generic job progress integration |
| Paths/resources | Resolved OneLake element support and distinct nested item `job run` check/type selection, verified from original hierarchy functions | Shared absolute/relative/home/shortcut/existence/FS_LS resolution |
| Lifecycle | Actual SDK/shared-executor POST, conditional shared LRO/result, actual generic jobs, immediate 200/201, bare 202, 204/no output, mutation non-replay, local cancel/drop/deadline/byte bounds | Lossless generic cancellation-LRO jobs bridge and native cross-platform TABLE execution |
| Online | No account/cache/token discovery or live request executed | Disposable workspace/table, cleanup scope, tenant/cloud/client registration and explicit activation absent |

## Preserved source details and known boundaries

- The load help advertises CSV header/delimiter defaults that the handler does
  not actually add. Native defaults likewise contain only `format: Csv`.
- Format parsing is literal comma splitting, not CSV quoting. Quoted commas
  can fail. Keys are case-sensitive, duplicates use the last value and boolean
  conversion applies even to header/delimiter. Parquet discards CSV options.
- Invalid format values use source `UnknownError`; boolean format values
  trigger the source unexpected-error branch. Invalid-key set display order
  varies with Python hash randomization; native uses deterministic membership
  order, not an invented semantic key set.
- Retention accepts zero and negative values; no new seven-day minimum or
  dry-run behavior is added. The parser has no `--dry-run` flag. Native decimal
  string division avoids narrowing integer spellings. Unicode15.1 decimal
  spellings and the default4300-digit conversion limit now have source/native
  evidence; other Python Unicode database versions remain an explicit boundary.
- A schema-qualified load still uses only the final table name in its endpoint.
  A cross-item file HEAD still yields only its local path in the payload.
- Python reads all OneLake listing pages in its shared client; sorting a raw
  first page is not sufficient. Checkpoint-only schemas remain unsupported.
- A load sets `wait=True`: a header-less 202 stays started; a header-bearing
  Fabric operation is polled, except job-instance Locations. Source
  Location-without-operation-ID polls `operations/`, a captured quirk rather
  than permission to infer an ID or invent completion.
- Maintenance no longer needs a temporary-file bridge. No file is created,
  so failure/drop cleanup has no local tempfile to leak. Generic job lifecycle
  is consumed from the separately owned implementation, not a table-specific clone.

## Load request-byte consumer after shared checkpoint61d

The lane safely merged exact shared checkpoint
`61d77e330be8a13cbd5033713f1f2badcecfce60` while retaining the TABLE-only
implementation history and explicitly released OneLake/error helpers. This adds
the shared core-layer Python JSON encoder and shared text/context libraries; it
is not a replacement oracle or proof that every TABLE host branch is integrated.

The load request now delegates to `fabric-core-json::json_dumps`. Parsed JSON
equality was insufficient: Python's original load handler sends default
comma-space/colon-space separators, ordered keys and ASCII escaping, whereas
the earlier native compact serializer emitted different bytes. Five additive
literal-wire fixtures independently specify default CSV, recursive append,
Unicode source/delimiter/extension, supplementary paths and quotes/controls.
Both the unchanged source handler and actual native
HEAD/POST202/shared-poll/result/output consumer must match those literal bytes.
No protected expected bytes are regenerated and no second encoder is added.

The new `wire.json` cases bring the additive source inventory to88 distinct rows;
the source-branch ledger now contains57 rows. Validation of this increment is
tracked separately from the previously passed49-test checkpoint below. A
refused shared-host gate is not evidence that this new consumer passed.

## Released concrete adapters and source-branch correction

The branch consumes the explicitly released minimal deltas
`05d18f20ce5438b65413703df537d01441b52541` (OneLakeHttpExecutor) and
`d7bb0678e6f49c314699dc4ecf84677272bb364a` (borrowed shared operation error
projection). The latter required only a README context reconciliation; unrelated
OneLake writes/shortcuts/moves were not adopted. No retry, TLS, paging, LRO or
Python representation implementation was duplicated in TABLE.

The public consumer now executes actual OneLake HEAD, Lakehouse POST202 and two
shared GET polls to completion. Paged schema and all option fixtures likewise
use the released OneLake HTTP adapter, not a synthetic preflight seam. Generic
operation failures use the shared formatter's already-normalized `OutputInput`
directly, including ordered dictionary/list/null/string details and the
period-only empty-message case. `TableFailure::envelope` and `execute_detailed`
retain original SDK evidence; legacy `execute` remains a lossy CommandError view.
The optional text renderer receives normalized errors through its existing
shared rendering boundary.

Full source tracing corrected an earlier Rust-only assumption: a OneLakeItem
inherits `FabricElement.check_command_support`, not `_BaseItem`'s owning-item
TABLE allowlist check. The source therefore permits schema/load processing on
resolved OneLake children beyond the six advertised schema types, and still
uses the Lakehouse load URL for other owning IDs. Maintenance's later `job run`
checks the owning item after table progress; it rejects Warehouse-like owners
there, and overwrites the job type to `sparkjob` for SparkJobDefinition.
`resource_reference.py` executes the original class methods/lookup functions and
job-type overwrite. The native host now preserves this sequencing through
`run_observed_for_job`. The Notebook mock context rejects traversal at the
context boundary rather than masquerading as a valid OneLake child; original
Python expectations were not changed.

[`table-branch-ledger.json`](table-branch-ledger.json) records56 source branch
rows with functions, line ranges, concrete tests and remaining gates. The
independent source denominator is83 case rows:31 initial,8 lifecycle,4 additional
full-client result branches,2 paged schema,20 additional options/schema/status
branches,5 full-client error cases,5 Unicode retention cases and8 hierarchy
resource rows. Eight original lifecycle cases are additionally observed through
the full Python client; those are not eight new cases. The hierarchy rows
perform32 leaf and8 nested-job checks, plus source job-type overwrite controls.
The original integer parser also passes exact4300/+1 digit-limit controls.

Windows x64 offline validation passed **49 TABLE tests**, eight released
OneLake HTTP adapter tests, three borrowed error-helper tests and eight existing
architecture tests. Targeted all-target locked check/strict Clippy, formatting,
both standalone examples and both original-source probes passed under the
required common mutex. Receipts: `released-final-receipt-03.json`,
`released-final-03-steps.json` and `released-final-03.log`. Earlier queue refusals and
failed attempts are retained. This is not whole-family/front-door/native3OS or
live proof.

Remaining branches are named, not waived: actual context/traversal, inherited
`-help`/`--skill` and mode/header-context plumbing, and shared
text/job-row/warning/banner rendering; interpreter-specific malformed-JSON diagnostics; raw storage-body diagnostics in the newer separately owned
OneLakeError extension; the generic jobs cancellation-LRO error/immediate-status
bridge; invalid-format Unicode capitalization/set ordering; native safety and
Python-runtime-version boundaries; and the shared SDK strict-JSON decoder for
wide schema metadata integers. The actual CLI uses the existing strict output
decoder and rejects unsupported wide integers instead of silently rounding;
direct SDK consumers can retain `schema_string` and supply their own decoder
before `schema_fields_value` until the shared SDK/core guard is delivered.
Current owner scope forbids live
load/vacuum/optimize/job execution, and none occurred.

## Released jobs adapter increment

The TABLE branch adopted the exact released Rust-only jobs/pager commits
`f75b7fe6ac492a61b203691926ffb17df9e151a1`,
`e4faed444a0fa9c212a477692e7d30149ad0457b` and
`4cf9d71f19ae693a2a6193096f77265f670037ed` by scoped cherry-pick. Shared
manifest/lock conflicts were reconciled additively, retaining TABLE and all
existing registry versions/checksums. No peer drafts or Python ancestry were
copied.

## Released operation and generic OneLake consumers

The lane adopted the seven explicitly released Rust-only resource prerequisites
in their given order, starting at
`2d2d770673ed435b4521ff380632ed0c85d839fa` and ending at
`128398d228f1005d2ded0a2ef60ba07794614b2b`, followed by bound HTTP checkpoint
`ef17b8096fae51ca94d701cdec11f101902868a1`. These were scoped cherry-picks with
additive local manifest/lock reconciliation, not moving branch or Python merges.

`FabricLoadCompletion` consumes the actual accepted response through
`OperationSession::reconcile_accepted(..., PythonSynchronous)`. Only the shared
engine performs polls/results, and no second POST can be issued by this path.
Original credential/selection, external request/response bounds, operation ID,
warnings and raw service errors are retained. Cancellation, drop, wait timeout,
request limits and HTTP errors never become proof of completion or rollback.

`TableWithOneLake` selects the released generic SDK for complete list/read/HEAD.
The TABLE layer still owns log ordering/metadata selection; it neither duplicates
paging nor acquires a second preflight token. Pages count against the command's
request budget. Two independently hand-authored paging cases execute unchanged
Python schema/OneLake/shared-success/response-append functions and the native
generic pager against the same expected fields and log read order.

Windows x64 offline evidence passed under the renewed common mutex: **36 TABLE
tests**, 33 selected shared SDK/facade regressions, eight architecture checks and
one existing facade doctest. Targeted all-target check/strict Clippy, formatting,
both public examples, 31 original TABLE cases, eight lifecycle cases (also eight
full Python client observations) and two paged-schema cases passed. The lifecycle
native test substitutes one canonical GUID for the opaque `op` ID and normalizes
only that ID for comparison; this is not an expectation rewrite or a claim that
the existing native ID safety rule matches every Python input.
An additional regression step passed all 34 unchanged shared-HTTP library tests
and 24 unchanged Notebook deployment tests after adopting the shared-engine
prerequisite; these are recorded separately in
`operation-regressions-receipt.json` and `operation-regressions.log`.

The standalone load example now runs real handler/SDK/shared-HTTP/shared-LRO
composition through202, Running and Succeeded, with one POST and two GETs.
Its source HEAD remains synthetic. Actual generic storage SDK tests use a
test-only bound-HTTP executor; the resource owner still owns the production
OneLake executor handoff. No production generic storage adapter was copied here.

Final receipts are `operation-receipt-final-02.json`,
`operation-final-02-steps.json` and `operation-final-02.log`. Earlier failed
attempts retain unused-helper/test-placement and example-header diagnostics.
They were fixed and revalidated, not skipped or waived.

At that earlier operation checkpoint, non-equivalent branches were: generic OneLake path/shape/2xx
safety rules, accumulated-list byte bounds, exact source context/presentation,
and the lossless generic-cancellation jobs bridge. The source operation error
formatter was private in the resource CLI, leaving structured CLI error details
explicitly unsupported while retaining SDK data. The newer released helper
integration above supersedes that limitation without a duplicate Python repr
formatter. There is still no complete-family claim.

### Earlier maintenance checkpoint evidence

The increment passed 26 TABLE tests (the original 18 plus six SDK maintenance
contracts and two CLI-to-real-adapter contracts), eight existing architecture
contracts, all-target targeted check/strict Clippy, formatting, both offline
examples and the unchanged 31+8 source cases with eight full-client observations.
This was Windows x64 offline evidence under the common mutex, not a new native
three-platform or live proof. Final artifact evidence is
`maintenance-receipt-final-02.json`, `maintenance-final-steps.json` and
`maintenance-final.log`. Earlier queue refusals and the initial Clippy
large-error failure are retained; the error now boxes only its optional service
row, preserving fields without suppressing the lint.

`JobMaintenance` uses `JobTarget::new(..., TableMaintenance)`, `submit`, `wait`
and `cancel_for_wait` directly. It does not add another HTTP, timer, paging or
polling engine. Wait options and timeout-cancel configuration are caller-owned.
Accepted submission is followed to actual terminal state; accepted cancellation
never becomes claimed terminal completion. Unknown/Deduped/Cancelled states and
no-cancel timeouts retain source no-completion-output behavior. Service failure
rows, creation/status/timeout events, run IDs and mutation uncertainty remain
typed and observable.

At that earlier checkpoint, full family completion still required the
OneLake/context/pager and generic Fabric-operation adapters. The newer consumers
above supersede their SDK/LRO gaps. Shared row/table presentation for job
Failed/unknown states, log warnings and the item-display-name job banner remains
an explicit presentation/context gap, not hidden by passing JSON contracts.
Existing protected fixtures and the source pin are unchanged.

The tests and standalone example are offline and synthetic. Passing TABLE
domain contracts is not full offline integration, live parity, native CI,
production authentication or front-door command availability.

## Initial checkpoint evidence (historical)

On Windows `x86_64-pc-windows-msvc`, Rust 1.99.1, the coordinator's common
validation mutex guarded the following successful checks:

- 31 independent TABLE source cases and eight independent lifecycle cases.
  Those same eight lifecycle cases also passed through the complete unchanged
  Python handler/client stack; these are observations, not eight extra cases.
- 18 native TABLE tests: six handler contracts, three option/registration
  contracts, three actual shared-HTTP contracts and six focused SDK contracts.
- Eight existing workspace architecture contracts, all-target targeted
  `cargo check`, strict targeted Clippy and formatting.
- The standalone public offline consumer built and ran the real
  handler/SDK/shared-executor path against its scripted immediate response.

The shared-HTTP controls include immediate completion versus bare acceptance,
header-bearing 202 delegation without fabricated completion, job-Location
bypass, 204/no output, non-replayed failed POST, decoded byte limit, cancellation,
future drop and timeout resource release. Their OneLake and pending LRO
boundaries are synthetic and explicitly do not count as actual adapters.

Receipts and command transcripts are session artifacts, including
`full-receipt-03.json`, `full-03-steps.json` and `full-03.log`. Earlier failed or
queue-refused attempts remain recorded. No Linux/macOS TABLE run, live request,
credential/cache access, push, PR or release was performed by this lane.
