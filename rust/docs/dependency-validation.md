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
dependencies. Python is a development-time oracle dependency only. Missing
oracle modules produce an actionable error; `--restore-oracle-deps` permits
restoring their existing pinned requirement files after a missing-module probe.

Formatting runs one `cargo fmt --package <name> -- --check` per selected package,
in sorted order, before compilation. This preserves the complete formatting
gate while avoiding one workspace-wide rustfmt argument list exceeding native
Windows process limits. Each package retains its own command receipt and any
failure stops validation; compile, lint and test package selection is unchanged.

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
| Feature profile | Compiles and lints its all-target composition, then tests selected targets with the corresponding package features. |
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

## Conservative boundaries

Unclassified package inputs select the package and its transitive Cargo
consumers. Unknown shared Rust inputs widen to full validation. Manifest,
lockfile, toolchain, graph, planner and workflow changes also require full
coverage. Missing or unusable CI diff bases require a full run.

Invalid declarations, missing targets/features, dependency cycles, registry
mismatches, stale plans and command failures are errors, never clean skips.
New crates are discovered through Cargo; an undeclared behavioral consumer
receives conservative package tests until its capability dependency is modeled.
Changes that delete or rename files retain both sides of the diff as inputs.

To add a narrow component:

1. Identify all implementation, fixture/template and source-oracle inputs.
2. Declare every affected test target and downstream component dependency.
3. Check all Cargo consumers; leave uncertain consumers on full-package coverage.
4. Add positive and negative selection tests, including deletion and shared-input cases.
5. Execute the selected contracts and inspect the retained plan and receipt.

Do not weaken compatibility fixtures, immutable source pins or required contract
inventory to achieve a smaller plan.

## CI and evidence

PRs, pushes and merge groups use their actual base/head pair. Every native job
recomputes the plan using that host's Cargo metadata and rejects a different
selection. Windows, Linux and macOS run selected checks. Manual and weekly runs
select full configured coverage. The stable `Rust validation` job rejects
failed selection and missing, failed or cancelled required native jobs; only an
intentional no-impact selection can skip them.

Artifacts retain the selection request, plan, source identity, actual command
arguments, exit status, output hashes and test-result counts. Execution checks
source identity before and after the run. `--stage core` or `--stage contracts`
is a partial run and is recorded as such; only the full `all` stage represents
the complete plan on that host.

The workflow does not configure branch protection or repository rulesets.
Repository maintainers must separately require the stable gate where appropriate.
An affected offline pass is not full-candidate acceptance, native installation
proof, signing evidence, live authentication approval or a GA declaration.
The historical bounded-candidate workflow retains its publication/source fences;
it is not an alternate general PR selector.
