<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared Rust integration handoff

The owner standardized on `feature/rust` in
[`eisber/fabric-cli`](https://github.com/eisber/fabric-cli/tree/feature/rust)
on October 23, 2026. Only the integration owner pushes the shared branch.
Lane owners continue on separate branches and adopt an exact validated checkpoint.
Upstream `microsoft/fabric-cli` is not the push destination. The former scaffold
branch remains intact; references to it in dated evidence are historical.

## Read-command, query and folder integration

### Resource consumers and one shared operation engine

The coordinator reviewed and applied seven released Rust-only commits, in order:
`2d2d770`, `bc450f5`, `b3a527a`, `e7155cc`, `ba25faa`, `4e02e3b` and `128398d`,
onto `ef17b8096fae51ca94d701cdec11f101902868a1`. The initial manifest/lock conflict
was resolved as the union of existing jobs/paging and new local resource packages,
not by replacing either lane or changing registry versions. Unrelated upstream
Python ancestry and newer unreleased resource increments were not merged.

`fabric-cli-mutations` composes source-backed workspace/item/folder create,
metadata update, delete and existence branches, SQL creation modes, mirrored
resource lifecycle and capacity linkage. `fabric-sdk-onelake` supplies bounded
list/read/HEAD over an injected storage executor. `fabric-sdk-operations` owns
the shared bounded request/poll/result engine; the existing Notebook public API
now delegates through a compatibility adapter. Python source-synchronous and
strict Notebook completion policies remain distinct. Already-accepted responses
can be reconciled without replaying mutations, retaining operation IDs, raw
service failures, cancellation and uncertain-side-effect evidence.

Independent combined Windows x64 validation passed **388 non-documentation Rust
tests and seven doctests**, strict all-target workspace Clippy, formatting and
the executed job consumer. Actual public resource examples passed **95 original
Python command cases, 12 poller cases and 23 HTTP/exception/decorator error/exit
cases**. All 155 independent job contracts and sixteen pre-existing read/query/
folder/workspace comparison stages passed unchanged. These denominators are
separate scenarios, not a complete command or migration coverage percentage.

All previous registry package versions, sources, checksums and features were
preserved. Only local package/edge additions were required; Python product
source and protected expectations remain unchanged. Native CI now requires the
three resource source-to-native comparisons on every selected OS, including
resource-only selections. The shared local validation mutex released normally.
The exact published checkpoint and fresh provider result are supplied separately.

This is not complete generic filesystem verbs, a live CLI host, broader
templates/dependencies/bulk selection, copy/move, shortcuts, ARM/domain
administration or online parity. The later concrete OneLake adapter and storage
mutation increments remain separate handoffs. See the
[resource compatibility matrix](mutation-compatibility.md).

### Job handlers and shared token paging integration

The coordinator reviewed and applied only the released Rust commits
`f75b7fe6ac492a61b203691926ffb17df9e151a1`,
`e4faed444a0fa9c212a477692e7d30149ad0457b` and
`4cf9d71f19ae693a2a6193096f77265f670037ed` onto `869263b`.
The worker's unrelated upstream Python ancestry was not merged.
`fabric-sdk-jobs` has real run/status/cancel/wait and schedule CRUD;
`fabric-cli-jobs` supplies all eight typed handlers, registration and an actual
shared `HttpExecutor` adapter. `fabric-sdk-paging::TokenPager` is a
transport-neutral collector first used by job lists. Direct TableMaintenance
tests exercise submission, status and cancellation through shared execution.

Independent combined Windows x64 validation passed **343 non-documentation Rust
tests and seven doctests**, strict all-target workspace Clippy, formatting, the
executed offline job consumer and **155 unchanged pinned Python job contracts**.
All sixteen pre-existing read/query/folder/workspace source stages passed
unchanged. The new job/pager source reference and executed consumer are required
on every selected native runner, including changes that select only these new
families rather than the central CLI.

Only three local workspace packages were added; existing registry versions,
sources, checksums and dependency features were preserved. Normal SDK job/pager
graphs exclude HTTP implementations, facade, identity, CLI and deployment.
Existing public service pagers, Python product source and protected expectations
remain unchanged. The common local validation mutex released normally.
The exact published checkpoint and fresh provider result are supplied separately.

This is partial library/handler integration, not a completed production
executable or online job validation. The existing front-door owner must compose
the handlers with host configuration, text rendering, batch/REPL and help.
Generic-operation reconciliation is an injected shared capability and still
needs its adapter; skill headers, bounded numeric/conversion-error differences
and main-level unexpected-error envelopes remain explicit gates. Account
approval alone does not authorize live job mutations. See the
[job compatibility matrix](job-compatibility.md).

### Offline Windows credential forward-port

The identity lane deliberately merged exact
`a23b4deda07011ea36bb3a635f75266b14b35c80` into its existing
`bf4479f926d491393657ca923b656e1beec456d9` credential implementation, without
resetting either history or adopting the moving shared head. The current read
parser/entry point remains authoritative. Feature-gated auth registration now
extends that parser and returns typed `Invocation::Auth` arguments to the
separate auth execution module. Existing `ReadCommand`, `ReadHost`,
`FolderReadHost`, LS/dir/GET option/query rules and read-host `NotConfigured`
results are preserved; successful login does not install a live read host.

The broker and transport features retain empty defaults and remain independently
selected. The SDK has no identity edge; unsafe native calls remain in
`fabric-identity-windows`. Only the optional auth CLI dependency adds Tokio signal
support; the shared transport feature set and all 216 existing registry lock
entries are retained. The lock adds only the credential slice's `errno` and
`signal-hook-registry` entries. No protected reference input or Python product
source/test is changed.

On October 25, 2026, focused offline Rust 1.99.1 Windows x64 checks passed
**72 default CLI tests** and **99 broker-feature CLI/identity/native tests**,
with no skipped tests in those targeted runs. These counts are separate run
denominators, not 171 distinct contracts. They cover default CLI reads, auth arguments named
like read commands, read paths named `auth`, invalid auth selectors, all existing
CLI read-host tests, native/credential tests and both unsafe-forbidden consumer
examples. The original sixteen-stage source-comparison runner also passed with
default **and** broker-enabled native probes; classifications and expectations
were not changed. CI retains the default comparison and additionally requires the
feature-enabled comparison. The common OS mutex, private short Cargo target and
serial jobs/tests were used.

Original classifications were retained in both source runs: read dispatch
22 exact/three safety cases, query 108 exact/ten unsupported, folder commands
32 exact/21 safety/three snapshot differences/three unsupported, all 60 raw
argument cases exact, and each workspace-GET probe's 58 consumer cases classified
44 exact/12 safety/two unsupported. The independent 70-case workspace source
inventory and its guards also passed; it is not relabeled native parity.

No live sign-in, credential/cache extraction, logout, app-registration change or
Fabric call was performed. Offline integration does not depend on choosing a new
backend or repeating the platform decision. Live acceptance still needs the
registration owner's confirmation of tenant/client/redirect/permissions and
separately authorized service targets; real MFA/CA proof is not established by
these checks. Full native three-OS CI remains an integration-owner gate, not a
local Windows result.

### Explicit native transport integration

The coordinator reviewed and merged exact lane commit
`274f9975ff5cbac3f67f375da615dc45fa586ed3` onto the enriched workspace-GET
checkpoint `b57ed2a0a98cf8e34d1c26770408b7098c6bcc4b`.
`fabric-sdk-transport` is a separate SDK-layer crate with empty defaults and an
explicit `hyper-rustls` backend. Its client and asynchronous `TokioTimer`
compose with the existing `HttpExecutor`; they do not replace its retry,
deadline, paging, LRO, credential or decoded-byte policies. The facade and
focused service SDKs do not acquire a transport dependency.

Independent combined Windows x64 validation passed **298 non-documentation Rust
tests and seven doctests**, all-target strict workspace Clippy, formatting, the
public offline SDK consumer and all sixteen existing source-comparison stages.
All 125 pre-existing registry package/version/source/checksum entries were
preserved; 91 registry entries were added. Default SDK/default transport normal
graphs exclude HTTP/TLS backends and higher layers; the opt-in graph selects
Hyper/Rustls without an identity/CLI/deployment dependency.

Nineteen native wire contracts and the independent consumer are included in
that total, not additional tests to add again. Wire fixtures exercise the actual
SDK/shared executor against synthetic TLS peers, including public CONNECT proxy
composition, no backend replay/redirects, exact/+1 decoded 4 MiB gzip limits,
pull-driven 8 KiB chunks, cancellation/drop/deadline, trust and hostname
rejection, pooling, repeated headers and Latin-1 values. The public consumer
cancels before credentials or network execution; it is not live-service proof.

Native CI explicitly checks, lints and tests the opt-in backend and asserts the
normal dependency graphs on all three runners. Default-feature checks alone
would not exercise this implementation. The exact published checkpoint and
fresh provider result are supplied separately. The common local validation
mutex released normally; Python product code and protected source fixtures
remain unchanged.

This is not a production credential/CLI-host integration or general enterprise
certification. PAC/system/authenticated proxies, client certificates,
enterprise revocation certification, HTTP/2/3, additional codecs and live Fabric
proof remain outside this slice. Unsupported encoding chains, raw deflate and
response trailers fail explicitly. See the
[complete transport contract](../crates/fabric-sdk-transport/README.md).

### Enriched workspace metadata GET integration

The integration owner reviewed and adopted exact lane commit
`36e3a7596af614ab413fba337461355140f4a269` from the published `fad3ea7` base.
Workspace GET now composes primary metadata, managed private endpoints, Spark
settings and role assignments through shared SDK read/paging policies and the
actual parser/context/query/output consumer. Optional source omissions have
typed, redacted SDK outcomes; cancellation, credential, deadline, origin and
limit failures are not converted into complete partial success. Source-empty
workspace projections emit zero bytes rather than an empty success envelope.

The coordinator independently validated the combined Windows x64 offline
workspace: **278 non-documentation Rust tests and seven doctests**, strict
all-target workspace Clippy and formatting passed. All original read, query,
folder and raw-argument comparisons and guards passed unchanged.

New workspace evidence has two deliberately separate denominators. All **70
original source contracts and 20 controls** passed with their original content
pins. Those original inputs are not a passing native-equivalence gate: the lane
observed all 70 through both native probes, with **13 source-equal and 57
different per probe**, mostly around typed metadata identity, status,
continuation and shape boundaries. No blanket overlay or source expectation
rewrite was approved.

The additional **58 typed-domain consumer cases** were independently
hand-authored and source-verified before native comparison. The parent asserted
all 58 through both real probes: **44 exact, 12 explicit safety and two
unsupported query outcomes per probe**, plus 14 controls. These are 116 native
observations, not 116 distinct source cases. The native CI runner requires
both source inventories, their guards and both consumer comparisons, with
fail-closed checks covering all sixteen subprocess stages.

Source pins, older fixtures, Python product code, Cargo manifests, lockfile and
dependency features remain unchanged. Validation used the common OS mutex and
private serial output; it completed and released normally. The exact published
checkpoint and its fresh native CI result are supplied separately. This is
bounded metadata composition, not production credential/transport support,
unqualified workspace parity, export, or complete CLI migration.

### Raw query-argument integration

The integration owner reviewed and fast-forwarded exact lane commit
`7076815be5a1aa340d7bbdb943d10b99685c2291` from
`d3d42eca672b387cabc0626901bb3719bab76f74`. The bounded lexical adapter preserves
source scalar LS versus variadic GET query semantics, quoting, repeated-option
precedence and demonstrated option/path boundaries over the existing Clap
schema. Ordinary variadic GET already worked; this is not a new GET feature.
Public `ReadCommand` and `ReadHost` fields/layout remain unchanged.

`FolderReadHost::with_scalar_query()` explicitly selects the actual LS scalar
truthiness boundary: an empty string uses default columns. The default wrapper
retains the existing handler-list empty-query inventory contract. Raw consumers
pass argv directly; the legacy folder handler protocol and frozen 59 cases
remain separately identified and unchanged.

The coordinator independently reran the integrated Windows x64 offline
workspace: **257 non-documentation Rust tests and seven doctests**, strict
all-target workspace Clippy and formatting passed. All **60 raw argument cases
(42 accepted, 18 rejected)** matched original Python in both native probes.
The **16 original-source consumer cases produced 23 exact native observations**
because seven GET cases run in both probes. All 16 new oracle controls passed.
These are distinct denominators, not 120 source argument cases or 23 distinct
source consumers.

The existing read, metadata, collection, query and folder comparisons and guards
also passed unchanged through the integrated runner. Native CI now requires the
new raw-argument reference and guards, with fail-closed controls for all eleven
process stages. New argument/consumer fixture blobs `8ecdbf2` and `d4a3d20`,
and the original folder case/evidence blobs `dab9f96` and `c2a058e`, remain
unchanged. Source pins, old expectations, Python product code, dependencies and
Cargo manifests/lockfile are preserved. The common validation mutex released
normally; the exact published checkpoint and fresh provider result are supplied
separately.

This is bounded read-argument and metadata-helper fidelity, not full argparse,
Unicode decimal-token, Python global-output-default, complete GET, or production
host/authentication/transport parity.

### Folder-aware read-host integration

The implementation delta `340cf49207007369ef3708eeee1e90a8bb93e8cb` is applied
as `137e798` without replaying its local helper cherry-pick `443cb0a`. The
integration branch already contains the equivalent parent prerequisite
`cc74d0244919d2aee4fa037ce7a0e5bf63c43141`. Source-routed GET and the original
`ReadHost` layout remain intact; `with_folders` is an explicit borrowed wrapper,
not a production binary host or a second SDK.

The coordinator independently reran the combined Windows x64 offline workspace:
**248 non-documentation Rust tests and seven doctests**, strict all-target
Clippy and formatting passed. The real public folder consumer asserted all
**59 outcomes: 32 source-exact, 21 safety, three snapshot differences and three
unsupported-query outcomes**. The original Python reference executed 54
terminating observations and separately proved five nonprogress cases without
running their infinite source loop. All 36 folder oracle guards passed.
The original folder-command case blob and reviewed nine-ID native-evidence
overlay remain unchanged, including their original Python expectations.

The earlier source-routed read evidence remains 22 exact plus three safety
outcomes out of 25; four raw-metadata cases, 176 Python helper observations,
124 supported native observations and 60 command guards passed. Query evidence
remains 108 exact out of 118 with ten explicit unsupported cases and 15 controls.
The raw folder SDK reference retained 26 comparable plus 13 Python-only
observations and six guards.

Native CI now requires the actual `folder_read_probe`, folder comparisons and
guards in addition to the existing read/query checks. Full checkout history
permits verification of the pinned historical source and Rust evidence blobs.
`cachetools==5.5.2` joins `jmespath==1.0.1` only as a development-oracle dependency;
neither becomes a Rust product dependency. Fail-closed orchestration tests cover
all nine process stages, including missing folder probes. Validation used the
shared OS mutex, private outputs and serial jobs, with normal release.
The exact published checkpoint and fresh provider result are supplied separately.

The command-local folder snapshot and unchanged flat-workspace request gap are
not full cache parity. Folder paths still require caller-loaded context;
multi-token query arguments are joined by the offline adapter, not yet by the
production parser. Null/orphan/cyclic graphs fail explicitly without silently
dropping or reparenting rows. These boundaries remain distinct from source-exact
cases, and the standalone executable still lacks production host/auth/transport.

### Source-routed GET integration

Exact lane commit `93b67dee3c1e8ecce9f11bd14bb2b9db470ad645` is merged without
replaying or removing the four crate-local helper visibility declarations from
`cc74d0244919d2aee4fa037ce7a0e5bf63c43141`. The finite SDK collection mapping
reuses the existing GET policies and preserves generic SDK methods. The actual
CLI selects source-specific metadata routes while retaining all 13 original
command rejections. Definitions, augmentation and export remain out of scope.

The coordinator independently validated the merged Windows x64 offline tree:
**229 non-documentation Rust tests and seven doctests**, strict all-target
workspace Clippy and formatting passed. All 25 actual command observations now
have **22 fully source-exact outcomes and three unchanged SDK safety
differences**, with no URI-only, metadata-order or projection-provider gaps.
Four additional raw-metadata outcomes match including their requests. All 176
Python collection/status observations and 124 supported native command outcomes
passed, along with 60 command source/guard tests. Original item evidence remains
56 observations plus seven guards; query evidence remains 108 exact of 118 with
ten explicit unsupported cases and all 15 controls passing.

The native CI runner explicitly selects `--request-mode source-routed` for both
command comparators and executes the additional collection reference. Historical
generic-route and typed/no-projection evidence remains unchanged and is never a
fallback for the current probe. Runner controls cover every one of its seven
process stages. Original protected expectations, source pins, Python product
code, Cargo manifests and lockfile are unchanged. The shared validation mutex
was released normally; the exact published checkpoint and fresh native CI
receipt are supplied separately.

### Metadata fidelity follow-on

The integration owner reviewed and fast-forwarded exact lane commit
`db1c0c24a6db9e1ed91f09d42b4092d9cd164345` from the published
`b9a69ab443040955056a113cd75fce8e9cd28d97` base. Additive `ItemMetadata` and
`get_item_metadata` preserve parsed HTTP object order through the existing GET
execution policies and actual `ReadHost` query/output consumer. Existing typed
`Item` fields, struct literals, imports, LIST and `get_item` remain unchanged.
No manifest, dependency feature, lockfile or protected expectation changed.

The coordinator independently reran the integrated Windows x64 offline
workspace: **223 non-documentation tests and seven doctests**, strict
all-target Clippy and formatting passed. All 25 actual command outcomes and
four additive raw-metadata comparisons passed against pinned Python. Both
successful GET inventories now match source output bytes and order. The
correct exclusive classification is **16 fully exact, six generic-URI
differences, three SDK safety differences, zero metadata-order differences
and zero unsupported-projection failures**. The two successful GETs also
retain specialized Python request URIs; fixing their output does not make
their requests exact. Original expectations and historical native boundaries
remain intact, with historical comparator modes explicitly selected.

All 56 command source/guard tests, 56 original item observations and seven
item guards passed. Query evidence remains 108 exact out of 118 with ten
explicitly unsupported and all 15 controls passing; this change does not close
those query gaps. Worker validation separately covered seven original HTTP
retry traces. The parent validation gate completed and released normally.
The exact published checkpoint and its fresh native CI result are supplied
separately; no production host, specialized GET or full CLI parity is implied.

### Earlier combined read wave

The next wave combines folder SDK commit
`83eb23611f0a21fb966437f2194481c8327e33a0`, native query commit
`08839d778daaf3563977d3c9ba594c0c8e68d923`, and the read-command delta
`9094bde9d2b8c654b0d12622f4e304707f068d68`. The command delta is applied without
replaying its local cherry-pick of the already integrated SDK `Audience`
prerequisite `fad81bf5f6b60835597b7627f97ac107d88966b2`.

Integration also connects `PythonQuery` and
`ReadHost::with_default_projection()` to real SDK-backed reads. This is not a
fixture callback: no-query GET performs native path discovery, explicit queries
run the native evaluator, and no-query LS retains source default columns.
`NoQuery` remains an explicit opt-out with regression coverage. The binary
recognizes `ls`/`dir`/`get` but still returns `NotConfigured` without a production
credential/transport/context/clock host.

The combined tree passed Windows x64 offline workspace validation: **215
non-documentation tests and seven doctests**, strict all-target Clippy and
formatting. Actual Python command comparisons asserted all 25 outcomes: 16
source-exact, four generic GET URI differences, three existing SDK safety
differences, and two successful GET path inventories with separately recorded
typed-metadata ordering differences. All 50 command source/guard tests passed.
Query comparison retained **108 exact out of 118** cases, with ten explicitly
unsupported; all 15 query guard/caller controls passed. Folder comparison
retained 26 comparable plus 13 Python-only observations, with all six guards
passing. No source expectations or pinned Python source were rewritten.

All validation used the shared execution gate with one job and private output.
The initial reference runner incorrectly requested Python isolated mode for
harnesses importing sibling modules; using their documented script/discovery
import modes resolved those runner failures. Failed receipts remain available;
the correction did not change product behavior, fixtures or guard assertions.

These are bounded offline command/SDK results, not complete Python CLI parity.
Specialized GET augmentation/export, folder CLI/tree assembly, query
numeric/runtime gaps, text output, production authentication/transport and
deployment breadth remain open. See the individual compatibility matrices and
forum oddity discussions. The final published checkpoint and native CI result
are supplied separately; local Windows results are not other-platform evidence.

## October 24 earlier parallel parity integration

The next implementation wave combines these exact committed handoffs. Original
lane commits remain in the integration ancestry; no unfinished peer worktree
was copied.

| Slice | Exact source commit | Implemented boundary |
| --- | --- | --- |
| Shared HTTP execution and handwritten REST decision | `4e822a3b760214f37e789d8a016e8cd7635f2f7f` | Caller-owned Azure HTTP client/timer, bounds, deadlines, read retries and no mutation replay |
| Item SDK | `ceeafaaee1f12bebca5a01513603451035a0afc7` | Generic workspace item LIST/GET, facade and shared HTTP adapter |
| JSON output | `fb325f6f298afa88f5af4ff9330f060cd0ad257c` | Pure CLI-layer envelopes, formatting, stream/exit plans and caller-owned writers |
| Paths and context | `ef93be856edaab360825ad7220e91dae82116faf` | Pure CLI-layer tenant/workspace/folder/item resolution over injected metadata |
| Capacity SDK | `bb96bc11abbf0706cfac1789f3b0bdb88327fc77` | Fabric capacity listing, facade and shared HTTP adapter; no ARM management |

Integration resolves additive module, dependency, default-member and lockfile
conflicts by preserving every slice. The central `serde_json` `preserve_order`
and `float_roundtrip` features intentionally unify across the workspace for
Python output fidelity. Existing item, workspace and Notebook contracts were
rerun with those features, not inferred from the output lane's older base.

The combined tree passed local Windows x64 MSVC validation on October 24 using
Rust 1.99.1, one Cargo job and single-threaded tests:

- Offline locked full-workspace tests: **159 non-documentation tests and five
  doctests**, none failed, ignored or filtered out; strict all-target workspace
  Clippy and formatting checks passed.
- The real Python output reference compared **42 cases**, including exact
  bytes from the merged Rust public-API probe, semantic values, streams and
  exits; all five oracle guard tests passed.
- The real Python context reference passed **81 exact cases and 14 guard
  checks**. Six additional cases remain explicitly unsupported, not parity
  successes.
- The capacity reference passed **35 source observations and nine guard tests**;
  the item reference passed **56 observations and seven guard tests**. Some
  source observations are documented Python-only quirks, not Rust parity claims.
- Seven source-pinned Python HTTPS retry traces passed. Existing Python
  source/tests and earlier workspace, item, HTTP and deployment expectations
  were unchanged.

Full Python CLI parity, dispatcher composition, production authentication,
transport/TLS configuration and the wider deployment surface remain open.
The CLI executable still exposes only help/version. This wave does not grant
live operations, select an identity backend or adopt generator experiments.
The integration owner supplies the final published merge SHA and fresh native
CI result separately; the local results above are not Linux/macOS evidence.

## October 23 captured committed progress

This consolidation starts at scaffold checkpoint
`4d740138ae738ffdca64415638ca491593da7c86` and records these exact heads:

| Lane | Captured head | Integration treatment |
| --- | --- | --- |
| SDK and CLI | `cbb6b6a554854d8a1d10ef9535f225a2e819922c` | Non-fast-forward merge; includes focused workspace SDK and Notebook deployment |
| Native deployment | `79654cf3d08e0276d6aec9b55c59edd1db6d8ec4` | Already an ancestor of the captured SDK head |
| Identity | `ea858c660579d47fb8a47d0a888e279e96dc3be4` | Non-fast-forward merge; optional native boundary reconciled with layer-based crate checks |
| Build and distribution | `997c1447087a987433d69d227a6653b49d7a5d1c` | Non-fast-forward merge; affected consumers, native matrix, resource controls and optional identity checks retained |
| Independent compatibility | `e3366ccccef486579867cd43cd6754c56b8e7439` | Non-fast-forward merge; owner-confirmed gate retained alongside later Windows oracle fixes |
| Plugins and skills | `a43f2b97dcc6ffa63a85d3439f56a8450b536519` | Non-fast-forward ancestry merge; equivalent content already cherry-picked |
| Python unit-test coverage | `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` | Baseline ancestor; no committed implementation delta |

All seven active lane worktrees were clean at capture. Uncommitted peer work is
never copied or included. A later lane head is a new handoff, not implicit
permission to expand this snapshot.

The merge preserves immutable fixtures and source digests, the synthetic Windows
hostname, exactly-once controlled headless fallback, and CRLF-drift rejection.
The production Python source/tests are unchanged. Native Notebook support and
Windows interop remain bounded library slices, not complete deployment or
authentication parity. The identity backend decision remains unresolved.
See [architecture](architecture.md), [deployment compatibility](deployment-compatibility.md)
and [independent validation](validation-matrix.md) for their limits.

## October 23 local consolidation validation

The merged graph was validated on Windows 11 x64, with actual compiler host
`x86_64-pc-windows-msvc`, Rust 1.99.1 (`58cf8f9c3`), Cargo 1.99.1 and LLVM 23.1.1.
Builds used one job, a private target directory, and single-threaded tests:

- Locked metadata, formatting, all-target workspace check and Clippy with
  `-D warnings` passed.
- Offline locked workspace tests passed: 89 non-documentation tests and three
  doctests, with none failed, ignored or filtered out. The explicit documentation
  test command also passed all three doctests.
- Optional broker-feature Clippy and the safe-consumer example with the feature
  both disabled and enabled passed without authentication. The offline injected
  SDK example and CLI help/version passed.
- Independent Python reference validation passed on Python 3.13.15: 13 workspace
  harness tests, all eight standalone cases, and all 44 pinned deployment leaf
  cases (PyYAML 6.0.3, jsonpath-ng 1.9.1). Expectations and source digest were
  unchanged. No full Python suite or coverage repeat was run.
- The actual CI selector passed its embedded assertions and manual/new-branch/
  scaffold-base executions against real merged Cargo metadata, selecting all
  seven members. YAML checks retained the three native runners, timeouts and
  failure gates.

These local results are not Linux/macOS execution evidence. Native provider
results must be read at the exact published head, not inferred from historical
scaffold runs. The coverage percentages and the existing failed headless UI test
in [validation baselines](validation-baselines.md) remain historical evidence,
not an improved or newly green Python baseline.

## Safe synchronization

The integration owner supplies the full validated checkpoint SHA and actual
provider results separately. Do not substitute a presumed latest branch tip.
In **your own lane worktree**, inspect status and checkpoint any authorized
local work first. If unrelated edits are present, preserve them and coordinate
before merging; do not automatically stash or discard them.

For an existing remote named `eisber` pointing to the fork above:

```powershell
git --no-pager status --short --branch
git remote get-url eisber
git fetch eisber
git show --no-patch --format=fuller <agreed-checkpoint>
git merge-base --is-ancestor <agreed-checkpoint> eisber/feature/rust
git merge --no-ff <agreed-checkpoint>
```

Run the merge only after checking that the agreed commit exists on the published
integration branch and that your current branch is your lane branch, not the
shared branch. An ancestor merge is a harmless no-op. Resolve any divergence
semantically in your own lane; do not reset, rebase published work, or force-push
as a synchronization shortcut. New merge commits must retain the repository's
required co-author trailer.

Run Cargo from `rust/`, with a private target directory and the shared-host
validation grant. Use targeted tests first; the full integration commands remain
in [architecture](architecture.md#build-and-contribution-loop).
Do not infer cross-platform success from local Windows results, CI configuration,
or historical scaffold runs. No PR, release, upstream push, live login or service
operation is part of this branch handoff.
