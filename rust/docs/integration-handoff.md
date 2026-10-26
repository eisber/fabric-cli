<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared Rust integration handoff

The owner standardized on `feature/rust` in
[`eisber/fabric-cli`](https://github.com/eisber/fabric-cli/tree/feature/rust)
on October 23, 2026. Only the integration owner pushes the shared branch.
Lane owners continue on separate branches and adopt an exact validated checkpoint.
Upstream `microsoft/fabric-cli` is not the push destination. The former scaffold
branch remains intact; references to it in dated evidence are historical.

## Read-command, query and folder integration

### October 27 merge-tree G: immutable family source union

`validation/rust-merge-tree-g-ad43ba8d` starts at exact
`b3d7daac62068b73f2b34bc8d1e4295bc0bcdb27` and selects only released Rust
deltas from C `ce325bc2935f818623149b5a685e3607e6788739` and F
`26e433821ca684c72e36bceb136648490e58e34b`. No worker ancestry is merged.
The inherited `8ad4c703bdd6738189f59318a04cab297b6c4381` Python branch
remains separate and untouched; every non-Rust file matches the exact base.
The independent Python oracle remains
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, with old expectations unchanged.

The local/access and resource/transfer paths are disjoint. Their final crate
trees remain byte-identical to original-owner releases:

| Family | Original-owner release | Source selection |
| --- | --- | --- |
| Local | `27b01af956bf16aa4ece82ecb87eb0b322ce9411` | C local crate |
| Access | `2b48992607a78cb09ddf1fe904d3c6c7acd4d039` | C resources crate and access docs |
| Resource | `a7d043d4753ec23f86a9e1fdaf4d7a502c31adec` | F mutations crate |
| Definition transfer | `47715d9589e728aa2e586f36be4a05ec1d31b418` | F transfer crate |

Only the Cargo and documentation unions need integration-owned edits. The
manifest retains both safe default members and all three new local paths.
The lock retains the exact local and transfer package records, both family
Clap edges, and all 218 external registry blocks in their original order.
External versions and features, SDK/core/output/HTTP/auth, the single central
main/parser/Invocation/runtime/session/native-read implementation, existing
job/read regressions and protected oracles are unchanged.

The C and F API ledgers below remain authoritative descriptions of the
released interfaces. In particular, preserve explicit local state/effects and
`pwd` stderr; run label list-local before credentials; use
`AccessOutput::write_to`; keep resource mutation and OneLake write result/error
types distinct; and retain transfer completed paths, actual create IDs,
deferred import cache effects, original operation uncertainty and stream-write
errors. No new namespace, runtime, HTTP engine or credential fallback is added.

G is **source-ready only**, not Linux-validated, mounted coverage, or shared
publication readiness. The static source/blob/manifest/lock/whitespace checks
and exact immutable/provider head are recorded in
`rust/merge-tree/20261027/G.json`. No heavy build or second installer is run.
Merged-head Linux proof must use the existing capped validator through the
authorized parent/root relay and its preserved current budget. Component
receipts below do not prove this union. Access's three native failures remain
original-owner debt; corrected local and transfer native results are pending.
No registered-test count is represented as passing.

ACL export, access skill binding, full config-driven transfer deploy, REPL/batch,
Windows/macOS and actual identity/platform acceptance remain explicit gaps.
Credential or data-safety defects block the affected release, not merely a
platform-debt note. The final executable successor may take central custody
only after J explicitly releases an immutable source-ready result; idle is
not release. G launches at most the finite authorized H successor after the
join claim succeeds. Only the root publishes `feature/rust`; no PR, release or
live Fabric action is part of this source handoff.

### October 27 merge-tree F: resource and transfer source composition

`validation/rust-merge-tree-f-dff9ad93` composes only the released D resource
delta through `a7d043d4753ec23f86a9e1fdaf4d7a502c31adec` and E transfer delta
through `47715d9589e728aa2e586f36be4a05ec1d31b418` onto exact shared
`b3d7daac62068b73f2b34bc8d1e4295bc0bcdb27`. Both source branches fork at
`91e7523ea978c67ca183abaea04500e323c27eb3`; their unrelated ancestry is not
merged. The inherited newer Python branch remains separate. The exact
source-to-integration commit/blob ledger and provider head are recorded in
workstream document `rust/merge-tree/20261027/F.json`.

The resource API is
`fabric_cli_mutations::frontdoor::{resource_subcommands, command_from_matches,
ResourceCommand, ResourceContext, ResourceConfig, ResourceHost}`.
`ResourceHost::execute` returns existing mutation output/cache/operation
evidence. Select `ResourceTransferHost` and injected `ResourceFiles` for storage
`cp`/`mv`; preserve the separate `OneLakeWriteResult`/`OneLakeWriteError`
create/append/flush and partial-effect evidence. These are eleven bounded
family-adapter leaves, not eleven new mounted executable paths. See the
[resource host contract](../crates/fabric-cli-mutations/src/frontdoor/README.md)
for unsupported branches and source argument limitations.

The transfer API is
`fabric_cli_transfer::{transfer_subcommands, command_from_matches,
TransferCommand, TransferHost, TransferMetadata, TransferFiles, TransferUi}`.
It delegates export, recursive bulk export and import to existing
`fabric_deploy::definitions`. Preserve `TransferResult` file progress and
deferred import cache effects, original `TransferError` operation uncertainty,
and shared `render_result`/`render_error`/`write_result` stream failures.
`LocalTransferFiles` requires an explicit stable local root. Full config-driven
`deploy` remains an explicit `NotSupported` gap, not a fourth happy leaf.
See the [transfer contract](../crates/fabric-cli-transfer/README.md).

Cargo composition adds the safe transfer default member, its centralized local
path declarations and package block, and the resource crate's existing Clap
dependency. All 218 external registry blocks and external dependency features
must remain unchanged. No central main/parser/Invocation/runtime/session,
native-read, SDK/core/output/HTTP/auth implementation, Python product or old
oracle is changed. Existing job/read base history remains intact.

This is a **source-composed candidate, not merged-head validation or shared
publication readiness**. The D source has actual Linux format/check/strict
Clippy, 87 Rust tests and 37 resource plus six storage source/native comparisons
at its own SHA. E's recorded Linux format pass is followed by all-target
`E0599` failures at `tests/commands.rs:507,569,950` (`Body::as_bytes`) on
`1788f245d24927b32211094a4904153b641d0807`. The original owner's released
`47715d9589e728aa2e586f36be4a05ec1d31b418` changes only that test consumer to
match `Body::Bytes`, retaining exact request-byte assertions and explicitly
rejecting unexpected streaming bodies. The correction is included, but its
actual Linux result is pending; 19 registered tests are not a passing count.
Original owners supply immutable repairs. The existing capped validator must
check the exact merged head through
the root relay; no local heavy-build lease is implied. Neither family is mounted
in the executable here, and Windows/macOS, live authentication and full
deployment remain unproven. Only the root publishes `feature/rust`.

### October 27 merge-tree C: local and access source composition

The integration-only branch `validation/rust-merge-tree-c-local-access-1302934`
adopts exact `b3d7daac62068b73f2b34bc8d1e4295bc0bcdb27` by an additive merge,
preserving inherited Python history at
`8ad4c703bdd6738189f59318a04cab297b6c4381`. The independent Python oracle remains
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; newer Python is not a new oracle.
Only the following released deltas are cherry-picked, not either divergent
worker branch's whole tree:

| Released source | C integration commit | Scope |
| --- | --- | --- |
| `4652285f3555b10058a7e049b92f2d0a137970d5` | `67d0b76fdbe9c3f1ef29c6d5e48e65d4dfd59947` | Local configuration, cache and named version handlers |
| `bb842ed6886708232b078ae2aa86ca21dce92b0a` | `f5026f557d90e985009126df51246b9314c266b2` | Released local formatting |
| `1bfce8f9b8718df83ca67dbd95b81b09ecaed8ce` | `43de37310fb862078cc7ba863c4d9cf270fb2443` | Navigation, browser, terminal and source support interfaces |
| `6f06a63cff5114ce4f02bc60602798809f4d65f5` | `84824df4a110de8e4267fc3076214c626532cc87` | ACL, catalog and raw API typed frontdoor |
| `b2ef54aee4d78dff66072c1b511a8595fcd9f8a6` | `b56591523fd6880f8d35779a121be8a9738ccaa1` | ACL and label mutation frontdoors |
| `2b48992607a78cb09ddf1fe904d3c6c7acd4d039` | `55bd13315f4c9bcd01d97ba591fc8e40b9788d68` | Access composed-schema consumer and compatibility notes |
| `27b01af956bf16aa4ece82ecb87eb0b322ce9411` | `a4b44c8531d35bd041125aa4ae58685f3b919a45` | Original local owner's pinned-source table-header correction |

The local crate tree matches the released `27b01af` tree exactly, and the
resources crate tree matches `2b48992` exactly. The Cargo union adds only the
safe `fabric-cli-local` default member/workspace dependency, its local lock
record with six existing dependencies, and the resources-to-Clap lock edge.
All 218 external registry blocks, their order, and existing external dependency
versions/features are unchanged. Existing job/read, validator, engine, identity,
output and protected-oracle files remain intact. C does not edit central
main/parser/Invocation/native/session/runtime files.

#### Family API ledger for the eventual central assembler

| Surface | Released interface and caller responsibility |
| --- | --- |
| Local registration/conversion | `fabric_cli_local::{root_commands, from_matches}` returns `LocalInvocation`/`LocalCommand`; eight distinct root names, including the four-leaf `config` group; aliases are not extra leaves |
| Local execution/configuration | `execute`, `ConfigStore`, `LocalEffects`, `Capacity`; explicit file root, cache invalidation and preloaded capacity metadata; no identity or service discovery |
| Local navigation/platform | `NavigationState`, `NavigationHost`, `BrowserIdentity`, `SystemBrowserLauncher`, `AnsiTerminal`; caller-owned context/home/session, explicit authenticated identity metadata and actual opt-in effects |
| Local output/failure | `LocalOutput::render`, `LocalError::command_error`, `LocalCommand::command_path`; existing output pipeline and caller-owned streams; preserve `pwd` stderr and named-version behavior |
| Access registration/conversion | `fabric_cli_resources::frontdoor::{register, AccessCommand, AccessKind, RawApiCommand}`; distinct `acl`, `label`, `find`, `api` roots; conversion does not execute or acquire identity |
| Access local-only execution | `frontdoor::execute_local(command, definitions)` runs `label list-local` before account/remote-host setup, without dummy credentials |
| Access remote execution | `AccessHost`, `ApiClients`, `AccessTargets`; borrow existing runtime, context resolver, explicit audience-bound clients, confirmation, limits and request bounds |
| Access output/failure | `frontdoor::render`, `AccessOutput::write_to`; shared family renderer, explicit warnings, ordered streams and writer failures |

The two root-name sets are disjoint. Their common output-format/skill options
remain family-owned leaf arguments, not duplicated root globals. Local
navigation uses `HierarchyLookup`/`PathElement`; access borrows
`AccessTargets`/`Element`. The eventual central host must adapt its existing
metadata session to these released boundaries, not add another resolver,
runtime, HTTP engine or credential fallback. ACL export and access skill-header
binding remain explicitly unsupported; REPL/batch and platform debt are not
closed by source composition. See the [local host contract](../crates/fabric-cli-local/README.md)
and [access host contract](access-frontdoor.md).

#### Source readiness is not merged-head validation

C's static preservation checks pass: exact family blobs, 218 unchanged registry
blocks, unchanged external manifest features, protected central/oracle trees,
inherited Python content/history, and whitespace. No heavy local build or
second WSL setup is authorized. This Windows session has no Cargo, rustfmt or
Python on PATH; no installation or native pass is claimed.

The existing Linux builder's component receipts are not merged-head proof:
local `1bfce8f` passed formatting, all-target check, strict Clippy and two pinned
Python source references, but `config_consumer` had eight passes and one failure
at line 189 (`text.contains("Setting")`), receipt
`4302712e7386460886252f79bab45921`. Navigation tests were not subsequently run.
Access `2b48992` had 55 passes and three failures:
`frontdoor_contract/mutations.rs:263` (empty stdout), line 319
(`mutation_may_have_occurred`), and `frontdoor_contract/mod.rs:474`
(empty stdout). Original family owners retain these repairs; C does not change
their algorithms or weaken expectations.

The subsequent original-owner `27b01af` release replaces the new native test's
incorrect title-case assumption with the source's lowercase `setting`/`value`
headers, separator and all eighteen ordered rows. It adds execution of the
pinned Python renderer to establish that contract; production handlers and
existing frozen fixtures are unchanged. C includes this immutable correction,
but a passing native result for it is not yet available. The old failing receipt
remains historical evidence, not a result for the corrected C head.

The immutable C head, provider-head verification, source-to-integration ledger,
and any subsequently released corrections are recorded in workstream context
`rust/merge-tree/20261027/C.json`. `source_ready` permits the next source join
with explicit failures; it is not passing validation, mounted command coverage,
shared publication readiness, or a claim that eleven plus nine library leaves
are executable commands. Actual merged-head Linux validation must use the
existing capped builder through the authorized parent/root relay.

### October 26 released transfer and jobs library candidate

The isolated `dev/marcozo/released-library-c728-1302861` candidate starts at
exact `c72860db65e821ada2ec3cbac8f34c4bc7458042`, retaining the original newer
Python checkout separately. It adopts the released definition chain through
`230ab09375f4e741a74050a8fd88f21fa85e61dc`, skipping its duplicate shared
Unicode and cancellation adoptions. The deployment crate remains byte-identical
to that release, including the existing Notebook publisher.

Jobs require released prerequisites `e7dade25f1ac4f3d5d6b1b88f7711e41d62f225b`,
`44b2b068f56e3eb116af678c4c00d91cdf00fb84` and
`0543353ea3ca3bcc6820286ed8cd781e8fce51a3` before the explicit skill-header
delta `55701bfeda34e8132062207319a8ae3fc61c4910`. The duplicate borrowed
formatter adoption `809f6bcc2e9b333f1bc7d232209b1c7ff6eae03d` is not replayed:
the base already supplies the shared formatter and resource-error projection.
Only the family consumers and semantic union of eight internal lockfile edges
are adopted; all 218 external registry package blocks and package order remain
unchanged. The jobs crate trees and source consumers match the released header
checkpoint exactly. Native CI additionally requires the released branch,
operation-output and skill-header source suites, not just the original jobs suite.

Resource SET/copy integration consumes `fabric_deploy::definitions`, never
export-cleaned bytes. Copy uses `read_copy_definition` and `copy_payload` with
the source's absent format query and raw parts. SET uses `read_definition`,
`decode_set_definition`, `ensure_notebook_dependency`, the resource owner's
existing restricted path replacement, then `encode_set_definition`. Definition
POST and subsequent mutation use distinct caller-owned operation sessions;
uncertain requests are never reset or replayed. Discovery, support, aliases,
confirmation, collisions, mutation sequencing, deletion and cache reconciliation
remain owned by the resource caller.

This candidate changes no central main/parser/runtime/native-read/ReadSession,
shared HTTP/operation policy, core JSON, Python product, old expectations or
template-order repair. It does not install native command routing or increase
the five-of-52 partial executable-entrypoint count. Fresh validation receipts
and the exact source-to-integration map are retained in the lane handoff for
coordinator review; publication remains the coordinator's separate action.

### October 26 access and physical-storage candidate

The isolated branch `dev/marcozo/resource-storage-exact9c396-1302865` starts at
exact `9c396cef44b736733a635070431d871c8f18762c`. The inherited newer Python
checkout is preserved separately; the source oracle remains exact
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. No whole worker branch or newer
Python ancestry is merged.

| Released source | Integration commit | Adopted scope |
| --- | --- | --- |
| Access chain through `7e1bd53f314ffaa038d8ec05472072a7f2e97c3d` | `ba3383081cec4779cd52350d25f4ecc8ef09836a` | `fabric-sdk-access`, `fabric-sdk-api`, `fabric-sdk-catalog`, `fabric-cli-resources`, their source oracles and compatibility docs |
| Physical-copy chain through `f048914e72b200c63e46b1071a96ecfeaa7893f8`, docs `9f4e38bc5c084120c900ba6df5c6154fb2476938`, fix `437d2eaf6b91224dbe7bca0219a098d327b583a9` | `9bde307dd811f4ccb3db8e2ed85aa2b6e03dd3d4` | Only `fabric-cli-storage-files`, including the retained-source-handle and empty-native-path behavior |

All 92 adopted blobs match those immutable releases. The source-to-integration
ledger retains the complete original commit and per-file blob inventories.
The existing text renderer already matches the access release; its adoption-only
`8a6404c` prerequisite is not replayed. Existing resource/OneLake code already
contains the newer consolidated preflight and mutation behavior, so the storage
branch's older shared/resource files are not substituted. No shared HTTP,
operations, GET, JSON, native-value, identity, main, parser, session, jobs,
transfer or deployment implementation changes are included.

The workspace adds only five local default members/dependency entries and their
local lockfile blocks. All 218 registry blocks and all pre-existing local blocks
are byte-identical; existing dependency versions/features and architecture
policies remain unchanged. Actual normal SDK graphs exclude CLI, identity and
deployment implementations.

#### Central consumer contract

`ApiClient::new` receives an injected Azure credential, explicit selection and
executor. `ResourceHttpExecutor(Arc<HttpExecutor>)` delegates to the existing
bounded execution/timer APIs; `AccessClient` and `CatalogClient` wrap that client.
ACL handlers (`read_acls`, `read_named_acls`, `mutate_acl`), label handlers,
`find`/`find_interactive`, hidden-name/metadata helpers and raw `api` are callable
library consumers. The host owns resolved context, source support checks, finite
limits, cancellation, asynchronous confirmation and interactive output.
`export_acls` still requires a real caller-provided `AclExportSink`; this
integration does not install a writer or a successful no-op.

`render_family_output`/`render_family_error` compose the shared renderer through
`FamilySurface` and `FamilyPresentation`. Supply timestamps, format, terminal
width, accurate Unicode properties and caller-owned streams, preserving ordered
records, source-silent results, separate warnings and handled OneLake error
exit zero. `REGISTRATIONS` is an inventory, not executable registration.
See [access](access-compatibility.md), [catalog/raw API](catalog-api-compatibility.md)
and [query dispatch](query-dispatch-compatibility.md).

`StorageFilesHost::{upload,download}` borrow the existing OneLake client/executor,
`LocalFileSystem`, mutation notices and finite limits. `OsFileSystem::new` requires
an explicit existing root and existing Tokio handle. Select storage-audience
credentials explicitly; resolved `OneLakeWriteTarget` and native path/token
handling remain host-owned. Preserve `CopyError` stage, accepted/partial remote
steps and local-write evidence, not only its formatted message. The actual source
lease survives upload; cancellation is cooperative, not rollback. This crate
does not add recursive copies, local-to-local copies, physical moves, parent
directory creation or cleanup deletion. See its
[host contract](../crates/fabric-cli-storage-files/HOST-CONTRACT.md) and
[source branches](../crates/fabric-cli-storage-files/SOURCE-BRANCHES.md).

#### Actual validation and remaining gate

Windows x64 Rust 1.99.1 targeted validation passed **367 non-documentation tests
and one doctest**, including all 46 family-consumer tests, 35 physical-copy tests,
eight architecture contracts and existing CLI/native-read regressions. Formatting,
strict all-target Clippy, examples, source/blob/lock checks, every released
access/catalog/raw API source harness and **120 real-filesystem source-equal
cases** passed, including 79 uploads. Two SDK-status differences are asserted
separately, outside those 120 parity cases. Family rendering compares
64 original-handler/UI cases with stream order; these counts overlap other
inventories and are not whole-command coverage percentages.

The full old read/session/resource comparison continuation is **not complete**.
Receipt 01 failed before product validation on launcher quoting. Receipt 02
retains the passing checks above but failed after the first three read comparisons
and 64 source guards because the session launcher supplied the output example
instead of the existing `projection_output` query consumer. Only the artifact
launcher was corrected; no product, oracle, ordering or expectation changed.
Receipt 03 started no validation: the approved v3 gate exhausted its bounded
quiescence wait with a foreign Python blocker. All failed receipts are retained.
No automatic retry, foreign process termination or classifier change is allowed.
The coordinator must authorize a new gated continuation within the remaining
acquired-time budget before this candidate is considered integration-ready.

No push, PR, live operation, native Linux/macOS result, publication or completed
front-door registration is claimed. Central executable routing, concrete export
writers, fresh three-platform CI and eventual live acceptance remain separate
gates. Managed-resource, capacity and table releases are not included in this
first bounded access/storage candidate.

### October 26 resource-library integration candidate

The isolated integration branch
`dev/marcozo/resource-integration-exact61d-1302851` starts at exact
`61d77e330be8a13cbd5033713f1f2badcecfce60`. It preserves the inherited
`8ad4c703bdd6738189f59318a04cab297b6c4381` checkout separately; Python behavior
and template comparisons remain pinned to
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

The candidate applies the twelve released OneLake/resource extensions
`05d18f2` through `e47e74c`, followed by `6151041`, `c22dd98`, `7690755`,
`713501b`, `059ce49`, `c441271`, `de2739f`, `4badbaa`, `2269502` and `5596edb`.
The latter sequence's released evidence-only prerequisite `1545bbe` is retained
in the `c441271` ledger-context reconciliation. The resulting resource/OneLake
crates, shared-operation crate and workspace mutation implementation are
byte-identical to the released `5596edb` files. The existing `ef17b80` shared
HTTP prerequisite and `2c44a99` definition encoder are not applied twice.
The single core JSON encoder and its CLI wrapper remain unchanged.

Only local workspace dependencies are added; all 218 previous registry lockfile
blocks are unchanged. Main, parser, native-read, `ReadSession`, original
compatibility expectations and Python files are untouched. Newer Python drift
is excluded, not adopted as a new oracle. Raw embedded templates deliberately
retain the pinned source bytes, including trailing blank lines.

The exact candidate `3c81cadb46db1808ba9dd19700fc96b3a96e7582` passed the
resumed Windows x64 integration checks under the reviewed gate v2. All fifteen
original-source comparisons passed **2,228 cases**, including request ordering,
output/errors, partial effects and template bytes. Coupled Rust tests passed
333 cases including doctests; the separate native-read feature CLI run passed
98 cases. These overlapping run denominators are not unique branch totals.
Global formatting, focused all-target strict Clippy and actual public consumer
examples passed. The mutex was released normally after 807.69 acquired seconds.

Earlier heavy-process guard refusals remain preserved, not upgraded to success.
The session handoff retains the exact source-to-integration commit map, receipts
and bounded validation script. This library checkpoint is released for
coordinator review/adoption, not a publication, three-platform native CI,
new executable-routing or whole-family completion claim. CI must also exercise
the new OneLake, creation, removal, SET, preflight and continuation comparisons,
not only the three historical resource comparison commands.

The follow-up CI-only checkpoint requires all fifteen comparisons through
`tests/compatibility/run_resource_parity.py` on each selected native runner.
It validates every built probe before exporting only the pinned `cf67fd52`
payload directory into a private temporary directory. Export, missing probes,
failed comparisons and timeouts fail closed; five runner guards cover the
orchestration separately from unchanged source expectations. Resource-only
selections restore the existing pinned oracle dependencies, including changes
confined to `fabric-sdk-onelake-http`. Shared JSON checks also compare actual
Unicode-preserving bytes and run their independent guards. Product crates,
manifests, lockfile, existing expectations and native matrix are unchanged.

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
