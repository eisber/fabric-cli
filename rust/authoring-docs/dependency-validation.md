<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Dependency-aware validation

The local validator and the Rust PR workflow use the same impact graph, planner,
command registry and executor. They select complete Cargo test targets, not
test-name substrings. Cargo remains responsible for compilation dependencies.
Capability declarations add the behavioral dependencies that Cargo cannot infer.

## Plan and run

Run these commands from the repository root. Replace the example paths with new
absolute paths outside the checkout. Use the repository-pinned Rust toolchain.
On a shared host, obtain the existing build-slot grant and run the executor
inside that slot; generating a plan does not grant build capacity.

```powershell
node rust\scripts\impact-plan.cjs --base <base-commit> --output C:\validation\impact-plan.json
node rust\scripts\validate-impact.cjs --plan C:\validation\impact-plan.json --artifacts C:\validation\run
node rust\scripts\validate-impact.cjs --plan C:\validation\impact-plan.json --artifacts C:\validation\run --execute
```

The second command prints the planned core commands without executing them.
Execution requires a new artifact directory. Restore locked Cargo dependencies
with `cargo fetch --locked` from `rust` if the offline commands report missing
dependencies. The executor injects that absolute artifact directory as
`FAB_CLI_V2_TEST_ROOT`, `FAB_IDENTITY_TEST_ROOT` and `FAB_UPDATE_TEST_ROOT`
for isolated offline fixtures; local and hosted runs use the same wiring.
Python is a development-time oracle dependency only. Missing
oracle modules produce an actionable error; `--restore-oracle-deps` permits
restoring their existing pinned requirement files after a missing-module probe.
Frozen-source Git invocations enable long paths without changing repository or
global Git configuration, so Windows artifact paths retain the full oracle tree.
Lexical source-path unit tests retain the same exact cases on every host, using
native fixture separators and absolute roots rather than interpreting Windows
drive paths as POSIX-relative filenames.

Omitting a base plans the local working-tree changes, including staged,
unstaged and untracked files. Use `--full` for full configured validation.
`--files-json <absolute-json-array>` is for inspecting a hypothetical change set;
it is not evidence that the actual PR or working tree was fully validated.
Do not use it to bypass the real diff in CI.

## Dependency model

| Input | Effect |
| --- | --- |
| Cargo metadata | Supplies every workspace package, target, feature and internal dependency, including optional, build, development and platform-specific edges. |
| Capability component | Associates implementation and non-code inputs with complete test targets and downstream behavioral consumers. |
| Component dependency | A changed dependency selects its transitive component consumers. |
| Uncovered Cargo consumer | Widens to that consumer's complete package tests instead of assuming it is unaffected. |
| Test target | Selects its actual Cargo target. Unclassified shared test helpers widen to package coverage. |
| Feature profile | Compiles and lints the selected composition, then tests selected targets with the corresponding package features. Explicit component obligations and affected feature-dependency paths determine relevance. |
| Contract check | Runs independent source comparisons, executable consumers, graph assertions or tooling checks from the shared command registry. |

Declarations live in `rust/validation/impact-graph.json`. Each component has a
package, input patterns, dependencies, target inventory and contract checks.
`depends_on` points from consumer to dependency. Patterns are repository-relative
paths using `/` in the JSON file, regardless of the native filesystem separator.
Non-Cargo dependencies such as templates, reference scripts, included fixtures
and generated data belong in the same graph.

Notebook and Spark/Environment have separate components and related downstream
deployment and CLI contracts. Shared definition/configuration code still selects
shared coverage. A test target containing multiple item families runs in full;
selection does not pretend those families have independent binaries.

All affected Cargo consumers still receive all-target compile/lint checks and
documentation tests. A Notebook-only change can therefore avoid unrelated
runtime suites without skipping compilation of CLI consumers.

Compile consumers, behavioral consumers and feature obligations are separate
sets. A component can declare `profiles` as an array of existing profile IDs.
An explicit array records its feature-composition obligations; omitting the
field retains conservative package-based profile selection. Changes to a
dependency selected by a feature path also select that profile. A check's
compile prerequisites do not by themselves imply that every behavioral suite
or profile for those prerequisites must execute.

The Dataflow execution components map the SDK implementation and tests to the
CLI execution library, native binding and complete downstream contract targets.
The default, native-read, headless and interactive execution paths remain
covered without selecting unrelated Notebook, deployment, updater or
certificate tests merely because they share the CLI package. The interactive
profile retains the Windows device-code provider composition; native-read
already includes the broker-backed native host. The unmapped CLI-v2 consumer
still receives conservative package coverage; this is not a claim that every
workspace capability has a narrow mapping.

Stage deployment is another isolated capability: the SDK `stage` contract
selects the CLI stage parser/entry targets and the explicit `v2-deploy` profile.
Default-mode entry tests still prove that the marker is unavailable without
the feature. Credential profiles are not stage-host implementations; the
current entry remains explicitly unconfigured. Unmapped Cargo consumers retain
their conservative package tests and profiles.

Notebook and Spark/Environment deployment components retain their distinct
complete target selections and shared deployment/CLI consumers. Broader
downstream profile selection remains conservative until those consumer
capabilities have an audited explicit mapping; component selection does not
claim that every workspace package is already equally narrow. OneLake source
and link-test changes include the complete shortcut `link` contract alongside
the existing library and consumer targets.

An unreleased changelog YAML entry is documentation, so adding the required
entry alongside a leaf implementation does not force the entire workspace.
Explicit component or check input declarations still take precedence over
that classification.

Feature profiles retain package-scoped `test_features` in the generated plan.
Each isolated test command uses its package's explicit profile features, plus
target-required features that the combined Cargo composition enabled through
another root's dependency forwarding. For example, a selected transport
`sdk_consumer` example receives its existing `hyper-rustls` requirement even when
the profile enabled it through a CLI native-read feature. Already satisfied
local/default requirements need no extra flag. Unrelated package features are
not passed to isolated tests; production defaults and target requirements remain
unchanged. Both planner command descriptors and the validation executor consume
this same metadata-derived selection.

## Conservative boundaries

Unclassified package inputs select the package and its transitive Cargo
consumers. Unknown shared Rust inputs widen to full validation. Workspace
manifest, lockfile, toolchain, graph, planner and workflow changes also require
full coverage. Missing or unusable CI diff bases require a full run.

A package-local manifest can use the union of old and new internal dependency
edges. Bounded Git reads and Python 3.11+ standard-library TOML parsing recover
the old declarations without executing an old build script or changing the
checkout. Dependency and default/feature changes can widen to affected
dependencies and their consumers because Cargo feature unification can change
their compilation. Missing Python/TOML support, unsupported declarations,
package removal/rename or changed workspace membership conservatively require
full validation. The plan retains the old metadata provenance and verifies it
again before execution; manifest narrowing must not ignore removed edges.

Invalid declarations, missing targets/features, dependency cycles, registry
mismatches, stale plans and command failures are errors, never clean skips.
New crates are discovered through Cargo; an undeclared behavioral consumer
receives conservative package tests until its capability dependency is modeled.
Changes that delete or rename files retain both sides of the diff as inputs.

To add a narrow component:

1. Identify all implementation, fixture/template and source-oracle inputs.
2. Declare every affected test target and downstream component dependency.
3. Declare relevant feature profiles and check all Cargo consumers; leave
   uncertain consumers on full-package coverage.
4. Add positive and negative selection tests, including deletion and shared-input cases.
5. Execute the selected contracts and inspect the retained plan and receipt.

Do not weaken compatibility fixtures, immutable source pins or required contract
inventory to achieve a smaller plan.

## CI and evidence

PRs, pushes and merge groups use their actual base/head pair. Every native job
recomputes the plan using that host's Cargo metadata and rejects a different
selection. Windows, Linux and macOS run selected checks. Weekly runs and the
default manual request select full configured coverage. A manual `affected`
request requires an explicit full base SHA and uses its actual Git diff; an
invalid or unavailable base conservatively selects full validation.
The stable `Rust validation` job rejects
failed selection and missing, failed or cancelled required native jobs; only an
intentional no-impact selection can skip them.
The owner-approved [provisional integration smoke](build-validation.md#provisional-v2-integration-smoke)
is the exception for PRs targeting and pushes to the exact named v2 integration
branch. Its skipped full-validation jobs are not a full-suite success or a
performance improvement; scheduled/manual and other-branch qualification remain.

Artifacts retain the selection request, plan, source identity, actual command
arguments, exit status, output hashes and test-result counts. Execution checks
source identity before and after the run. `--stage core` or `--stage contracts`
is a partial run and is recorded as such; only the full `all` stage represents
the complete plan on that host. `--shard <id>` executes a trusted partition of
the original verified plan; its receipt always has `complete_plan: false`.
Only the aggregate of every selected successful shard represents the complete
configured validation across the selected native hosts.

Each shard receipt includes a bounded, hashed execution manifest. Aggregation
derives core coverage from successful command results and their original
aliases, checks the completed contract descriptors, and binds actual toolchain
outputs and normalized file hashes to the verified clean checkout. Missing
execution evidence, wrong host, contradictory success labels, timeouts,
cancellation or unconfirmed cleanup cannot satisfy an obligation. This is
execution and freshness evidence inside the existing unprivileged workflow,
not an independent signature or authorization to trust a modified workflow.

The workflow partitions default-package families, isolated feature compositions
and dependency-ordered contracts. Three independently admitted OS pools each
allow at most four native jobs, retaining the 12-job total ceiling. A queued
macOS pool cannot occupy the Linux or Windows admission slots. The reusable
native workflow verifies the selected pool, runner image and actual host.
Measured command descriptors are packed into bounded cohorts without merging
their Cargo feature flags or losing original obligation IDs. Unknown command
domains remain isolated until timings are available. Timing estimates include
cold-build and setup/cleanup allowances; they are not passing performance data.
Full-profile estimates use separate cold-run evidence: at most two complete
profiles share a measured bin, with an estimated 600-second ceiling including
180 seconds of cold-build allowance. Interrupted or unmeasured profiles remain
isolated rather than treating an incomplete timing as a cheap completed run.
This intentionally favors completing every required profile within its deadline
over minimizing the full-plan job count. The affected Dataflow plan retains
four jobs per native host.
Frozen-source preparation, its dependent comparisons and final integrity check
stay together. Full plans retain the all-feature Clippy and CLI preflights as
separate obligations; affected plans no longer run those unconditional full
preflights. Equivalent core Cargo formatting/check/Clippy descriptors and the
audited test suites below run once with all original obligation aliases retained;
contract side effects and arbitrary test packages are not deduplicated.
Fast planner/executor contracts remain in dependency selection and its native
checks. Real subprocess timeout, cancellation and ownership tests are a separate
`process-supervision` obligation: they run on all three native hosts for full
plans and when their validator inputs change, not repeatedly for an unrelated
Dataflow change or each profile's selector dependency.

Native CI uses an isolated Cargo home and fetches the locked dependency sources,
including those needed by offline cross-target graph assertions. This does not
cross-compile tests: execution still uses the recorded native host.
It caches registry archives/index and Git databases, not expanded
registry source trees, separately from `rust/target`.
Keys include runner OS, architecture, pinned toolchain, lockfile,
manifests and explicit build settings. Two immutable compilation cohorts per
host (`default` and `features`) replace per-revision or per-shard cache keys.
One designated producer per host/cohort saves at most 3 GiB of raw build output;
the shared source-archive producer saves at most 256 MiB. Completed packed leaf
cohorts measured 1,733-2,768 MiB raw, exceeding the earlier 1.5 GiB limit and
preventing a useful warm cache. These are uncompressed bounds, not a reservation
or an increase to the repository's provider cache quota. Oversize candidates skip
cache saving, not validation. Metrics report candidate size, restore time and
cache hit separately. Old manifest generations remain subject to provider
eviction; no quota or billing settings are changed.
Cargo still checks freshness and every selected test executes. Cached build
outputs are not validation receipts. The compilation cache now uses `v5`, while
dependency archives retain `v4`. Bump the corresponding generation when changing
the compilation settings or cohort policy.

The executor uses command-class limits: 120 seconds for tools, 300 seconds for
tests and 600 seconds for compilation, further bounded by explicit command,
remaining run and contract budgets. Test commands include any compilation
they require in their test-class budget. Timeout failures retain
partial logs, the applied `timeout_ms` and `timed_out` in `commands.json`, and
fail the validation gate. Output streams to artifact files while commands run;
start/end messages include elapsed time and exit status. Every native shard
has a 15-minute job ceiling. Its executor budget accounts for elapsed setup and
reserves time for cleanup and evidence retention. The timeouts are safeguards,
not a promise that an arbitrary full plan finishes in 15 minutes.
The full all-feature CLI preflight first compiles its exact test targets with
`--no-run` under the compilation budget, then executes the unchanged targets
under the test budget. Cold Windows compilation exceeded five minutes in the
first hosted measurement; it must not consume the runtime-test allowance.
Full Windows profiles that test every CLI target use the same compile-then-run
split. Their cold compilation alone consumed 4 minutes 28 seconds to 4 minutes
40 seconds in hosted measurements, leaving insufficient time for the complete
test execution under a combined five-minute limit. Affected-target invocations
remain unchanged.

### Slow-run investigation

[Run 43072057855](https://github.com/eisber/fabric-cli/actions/runs/43072057855),
at source `d7cac9a43a9a7dd10434c5f44aefcd81b090a227`, selected full validation:
the changes included Cargo manifests, the lockfile, the impact graph and an
unmapped shared script. The completed Linux job took 59 minutes 52 seconds.
Its shared executor took 54 minutes 47 seconds and recorded 262 commands,
including 202 Cargo commands across 42 selected feature profiles. Cargo
completion messages account for about 39 minutes of that executor time;
tests and other checks account for the remaining roughly 16 minutes.
The early all-feature Clippy and CLI preflights added another four minutes.
The Windows job completed in 94 minutes, with 321 executor commands taking
85 minutes 40 seconds. Its additional commands include bounded per-package
formatting and Windows-only optional runtime coverage. The repository had no
Actions caches when this investigation began.
macOS finished successfully after 105 minutes 7 seconds; the complete workflow
reached its successful final gate 105 minutes 42 seconds after creation. These
are provider-reported durations, not estimates from the configured timeouts.

This was repeated compilation and execution, not evidence of one hung test.
The run had no native Cargo cache. Caching can reduce compilation but cannot
alone bring the remaining serial execution down to a few minutes. The next
structural change is parallel execution of the selected feature profiles and
independent contracts, retaining native-platform coverage, source verification,
per-shard receipts and a stable gate that requires every selected shard.
Do not replace isolated profiles with `--all-features`, omit native platforms
or move required checks out of the PR gate merely to shorten the run.

### Performance acceptance

Measure feedback from workflow creation through completion of the stable gate,
including selection, runner queues, setup, cache restoration, execution,
artifact retention and aggregation. Report total runner time separately from
wall time: increased parallelism can improve feedback while using more compute.
Compare the same selected obligations, source and native platforms; do not
compare a skipped or reduced plan to a complete validation pass.

The initial target for an ordinary affected-package change is three to five
minutes with warm caches. Full/shared-input changes and cold caches have
separate measurements; five to ten minutes is an optimization target for the
full plan, not an unconditional guarantee. Job timeouts are failure safeguards,
not performance acceptance evidence.

Use actual base/head diffs for hosted measurements. A leaf-package run, a
package-manifest run, a documentation-only run and a full/shared-input run
exercise different selector boundaries. Rerun the same source and scope after
a successful cold run to measure a warm cache. Retain the run URL, commit,
selection digest, expected and completed shard obligations, queue/step
durations, cache hits and the final conclusion. Injected timeout, cancellation,
missing-receipt and stale-receipt tests must still fail closed.

### Hosted measurements and runner availability

The documentation-only probe
[43292281438](https://github.com/eisber/fabric-cli/actions/runs/43292281438)
completed successfully in 43 seconds, including selection and the stable
aggregate gate; native obligations were intentionally empty.

The real Dataflow-only probe
[43293849219](https://github.com/eisber/fabric-cli/actions/runs/43293849219)
selected five packages, six feature profiles and eight checks instead of the
previous 34 packages, 38 profiles and 24 checks. The package-description-only
probe
[43293973486](https://github.com/eisber/fabric-cli/actions/runs/43293973486)
also selected five/six/eight from its actual old/new manifest inputs. These
selection reductions are not complete native-pass or timing claims.

The Dataflow probe did not obtain a successful aggregate: GitHub cancelled
macOS ARM64 jobs before assigning runners. Provider annotations state
"The job was not acquired by Runner of type hosted even after multiple attempts"
and report macOS ARM64 capacity constraints. Do not count those jobs' synthetic
`started_at` timestamps as execution time when there are no runner or step
records. The failed run's 49-minute wall time includes acquisition waits;
completed native jobs took approximately one to six minutes with mixed cache
states. It is not a successful warm-cache benchmark or proof of the target.

Measured scheduling must account for both compilation and runner acquisition.
Reducing job fragmentation and independently admitting each native OS reduces
queue contention within the workflow, but cannot guarantee provider capacity.
Never replace missing macOS execution with a Linux pass or treat cancelled
obligations as successful to reach a duration target.

The final same-source warm retry
[43316167088, attempt 2](https://github.com/eisber/fabric-cli/actions/runs/43316167088/attempts/2)
used implementation `3d859943220444f55fd4f3949aee693ceac778b8` plus the isolated
Dataflow test probe. All four Linux jobs and all four Windows jobs passed with
compilation and dependency cache hits:

| Native pool | Longest executor sequence | Longest allocated job, including setup and artifacts | Pool span, including staggered runner admission |
| --- | ---: | ---: | ---: |
| Linux | 156.0 seconds | About 3 minutes 9 seconds | 5 minutes 17 seconds |
| Windows | 268.1 seconds | About 5 minutes 36 seconds | 5 minutes 36 seconds |

These are component-validation measurements, not an apples-to-apples speedup
of the original full 42-profile run. The whole warm workflow still failed:
three macOS ARM64 jobs were cancelled before runner assignment. One macOS
feature cohort executed successfully in about 5 minutes 20 seconds, but that
does not establish the missing macOS obligations. A universal three-to-five
minute complete gate has therefore not been demonstrated.

Cold full-profile qualification remains more expensive. Tight packing based on
historical warm timings exhausted the run budget; the corrected conservative
plan isolates those configurations rather than dropping tests or increasing
the 15-minute job ceiling. Source selection and warm caching deliver the
ordinary-component improvement; a few-minute full qualification would require
additional reliable native capacity or a separately approved coverage policy.
The published provisional integration-smoke policy is distinct and must not be
reported as a complete native qualification result.

For unsharded local execution, the total budget defaults to 7,200 seconds and
explicit `--budget-seconds` values retain the existing 8,100-second maximum.
The configurable per-command ceiling defaults to 600 seconds and accepts values
up to 7,200 seconds; the shorter command-class and contract budgets still apply.
Receipts retain configured budgets and each command's actual timeout. Native
CI uses the shorter setup-adjusted shard budget and 15-minute job ceiling above,
not the historical two-hour serial-job allowance.
Within one CORE phase, exact repeated single-package all-target test invocations
for `fabric-sdk-transport`, `fabric-identity`, `fabric-identity-windows`,
`fabric-cli-repl` and `fabric-cli-update` reuse their first successful execution.
These offline suites use injected/memory fixtures, ephemeral loopback servers or
owned per-test artifact directories; they do not prepare later profiles. Reuse
requires identical ordered arguments (including features), effective working
directory and full effective environment under the same recorded source,
toolchain and native platform. Environment comparison is private in-memory
hashing, not a secret-bearing receipt. The map exists only inside that CORE
invocation; it is never shared with another run, head, platform or contract phase.
Setup, arbitrary test packages and stateful registry contracts still execute
normally. Source integrity is checked after CORE and again after all contracts;
any mismatch fails the entire receipt, including its aliases.

Every required profile/check label remains in `commands.json`. Each actual
execution has an `aliases` array containing its original obligation IDs and its
actual exit, log paths, output hashes, elapsed time and test counts. Aliases do
not add synthetic result rows or count tests twice. Receipt `commands` and
`commands_executed` count actual executions; `commands_reused` counts additional
aliases. Their sum records satisfied or attempted command obligations. The
execution manifest binds every alias to the real command, and aggregation
requires all owned obligations. Cross-shard co-location uses the same audited
eligibility rule and exact descriptor equality, never broader feature unions.
CLI runtime tests and distinct feature compositions remain separate.
Fewer processes alone do not prove that the full native plan fits its deadline;
only the complete source-bound native receipt establishes that result.

The workflow does not configure branch protection or repository rulesets.
Repository maintainers must separately require the stable gate where appropriate.
An affected offline pass is not full-candidate acceptance, native installation
proof, signing evidence, live authentication approval or a GA declaration.
The historical bounded-candidate workflow retains its publication/source fences;
it is not an alternate general PR selector.
