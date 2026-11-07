<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI v2 delivery and upstream synchronization

CLI v2 develops separately from the existing Python CLI and Rust compatibility
work. Its source baseline is `feature/rust` in `eisber/fabric-cli`, initially
`a9df45501ec53d921c6fc765c726c32979b85931`. This process does not change the
existing command grammar, release support, or compatibility expectations.

### Round-two source and layout checkpoint

The first normal layout-adoption merge is
`79b92b213e16b4d9afcb4c29b979a4dd0553bf01`. Its parents are the completed
first-round v2 checkpoint `988730eb1a191174a1b0b765cd3b9dfcb1e16f16` and the
actually merged Rust checkpoint `19cf15ae3e5fdcd7b4f90d47cc21e1c2ead4980a`.
Both complete ancestries are preserved. The latter is the recorded accepted
upstream source, not a guessed moving `feature/rust` tip. This isolated source
merge is not common-branch publication or combined-candidate gate acceptance.
Notebook follow-up adoption and the coordinator's exact-SHA publication release
remain separate requirements.

The coordinator-released follow-up sources are Notebook's actual merge
`25b676dbaaed753e4e90122cc38bb5f9fc1dbfe2`, Schema
`ac566bff42d01cd55d5cae292fa90647b4a05f3f`, Copy
`e7cebc7f688799e3b51522ebc0e14a0ad3fee652`, and repaired Semantic
`f7094c096c96c540dcd79d1834a5cb2dfa891046`. These immutable sources may
compose an isolated validation branch; a staging merge does not mean that the
remaining source PRs have merged. Final common publication still requires the
actual complete merged checkpoint, current combined gates, and the coordinator's
exact-SHA release.

The complete capped round-two provider checkpoint is
`ccf9c240d53fd81a9d41be7ddf783d3eb1a196b8`, with tree
`6492cb12350340b0249431c2776dee2488a72c68`. PRs 16-19 are actually merged in
its ancestry. Layout adoption uses a normal merge of that immutable checkpoint,
including the isolated managed acceptance helper, the reviewed Windows profile
alignment, and strict source-inventory gates. Physical path reconciliation keeps
the managed helper bound to `sdk/semantic-model/dotnet/managed` and places
per-project outputs in external artifacts. Both the current native matrix and
the optional managed gate must pass on the adopted head; prior component results
or a source-identical ancestry change are not a new execution receipt.

Semantic analysis uses `sdk/semantic-model/client/`, the optional isolated
`sdk/semantic-model/dotnet/` native boundary, and
`frontends/cli/commands/semantic-model/`. The native boundary retains its
reviewed private unsafe exception and stays outside default members. Empty
default features keep SDK and CLI consumers .NET-free; explicit runtime builds
use the existing pinned managed gate. Preserve the original byte-pinned BIM
checkout attributes, source hashes, catalog and independent oracle when moving
the packages. Native matrix tooling provisions the pinned SDK on every host for
explicit all-feature compilation; this is not a default product dependency or
an additional platform-support claim.

New reusable v2 packages now live under `sdk/sql/`, `sdk/search/`,
`sdk/ontology/`, `sdk/dataagents/`, `sdk/dataflow/`, and `sdk/tabular/`.
Shared `sdk/contracts/` retains its core layer. CLI Ontology adapters live in
`frontends/cli/commands/ontology/`; the six-leaf local host has its own
`frontends/cli/v2/` package. It does not replace `frontends/cli/app/` or its
`fab-rs`, `fab-cli-v2`, `fab-cli-discovery`, and `fab-cli-workspace` targets.
Grouping directories are not packages, and physical SDK placement grants no
new dependency direction.

Round-two lanes start only from the coordinator's published checkpoint and
locate packages by Cargo manifest identity, not historical capability paths.
Register new leaves, safe defaults, actual test targets, shared helpers and
optional profiles in the existing Cargo-metadata impact plan. Preserve frozen
Python source, source-bound oracle overlays and historical evidence aliases;
do not regenerate expectations when paths move. A new composition needs
independent source/consumer and architecture/default/validation reviews plus
current complete Windows, Linux and macOS plans and the stable gate.

The common v2 integration branch is
`dev/marcozo/fab-cli-v2-integration-20261104` in `eisber/fabric-cli`.
All v2 lane PRs target this branch, not `feature/rust` or `main`.
Creating a local branch does not publish it or establish GitHub write access.
The original workspace branch is preserved. Neither `microsoft/fabric-cli` nor
the shared `feature/rust` branch is a push destination for this v2 effort.

This v2-specific destination overrides the v1 Rust-trunk destination in the
general contributor and delivery guides for v2 assignments only. It does not
retarget existing v1 work.

## First implementation gate

Before changing command behavior, the coordinator must obtain the current
selected command specification, including its owned subcommand pages, and
confirm the first implementation wave with the owner. Historical design notes,
existing source command names, and alternative proposals are not substitutes.
An inaccessible specification blocks affected implementation, not independent
source inventory or development tooling.

The proposed first wave is shared typed-path and output contracts followed by a
read-only get/list vertical slice. This is a recommendation, not an approved
grammar change. Preserve the existing executable and tests until the owner
selects the compatibility boundary and the slice has its own acceptance tests.

## Track the Rust upstream

Keep the read-only Rust source separate from publication remotes:

```text
git remote add rust-upstream https://github.com/eisber/fabric-cli.git
git fetch --no-tags rust-upstream feature/rust
```

Add the remote only when it does not exist. If it already exists, verify its
fetch URL instead of replacing its configuration. Fetching does not merge
changes or validate their behavior. A cached tracking ref is not proof of the
current provider head.

Check upstream drift at the beginning of each integration cycle and before
publishing or merging a lane change. The coordinator records the fetched full
SHA, the last accepted upstream SHA, the v2 candidate SHA, ahead/behind counts,
and any affected command contracts. Keep the approved baseline immutable;
advancing the accepted upstream checkpoint requires a reviewed synchronization.

From a clean checkout, use the developer-only tracking helper:

```text
python -B rust/scripts/v2_upstream.py
python -B rust/scripts/v2_upstream.py --fetch
```

The default JSON report is offline and labels its upstream SHA as the cached
tracking ref, not the live provider head. `--fetch` explicitly observes only the
trusted `feature/rust` branch and downloads its exact commit objects. It does
not update tracking refs or `FETCH_HEAD`; subsequent offline reports still use
the cached ref. Retain the fetched report and reviewed SHA for integration.
The helper never merges, commits, pushes, or validates product code.

The original baseline pin remains immutable. Default-baseline reports also
prove that the recorded accepted upstream SHA is an ancestor of the candidate.
They label whether the cached and observed upstream contain that checkpoint
separately: a stale cache is not a current-provider or rollback claim.
Custom `--base-sha` reports do not silently inherit another checkpoint ledger.
Git invocations enable native long-path support without changing repository or
global configuration.

Native validation retains `source-before.json`, `source-after.json`, and exact
per-file differences alongside the existing fail-closed source digest. Each
entry records its tracked state, mode, byte count, and SHA-256, not file content.
Preflight inventories also catch generated files before the full plan starts.
The manual **Diagnose managed source integrity** workflow runs only the smallest
optional managed build and retains its before/after manifests; it is diagnostic
evidence, not a replacement for the complete native matrix. Fix the actual
writer when it changes a source or creates an untracked file. Do not exclude the
file, reset its bytes after validation, or regenerate a checked-in lock or oracle
to make a guard pass.

The helper exits 0 for a complete safe report, including ordinary ahead/behind
divergence; 1 for an operational, lineage, or safety failure with JSON on stderr;
and 2 for invalid CLI syntax. A dirty checkout, in-progress Git operation,
unexpected remote URL, missing pin/ref, incomplete history, or rewritten upstream
lineage blocks a report. A detached HEAD must be placed on an authorized v2
branch before manual integration. Use `--base-sha <reviewed-full-sha>` only when
adopting an explicitly reviewed baseline.

Run its offline synthetic-Git tests without Rust or service credentials:

```text
python -B -m unittest discover -s rust/scripts -p test_v2_upstream.py -v
```

The existing impact-validation registry also runs these tests when the helper
or its tests change, and in full validation. Python remains development tooling,
not a dependency of the shipped Rust executable.

## Merge a reviewed upstream checkpoint

Only the v2 coordinator integrates upstream changes into the v2 branch:

1. Stop other writers to the candidate. Verify a clean worktree, the expected
   branch, no merge/rebase in progress, and the trusted upstream URL.
2. Fetch `feature/rust` explicitly. Record its full SHA and verify that the last
   accepted upstream checkpoint remains its ancestor. A rewritten or missing
   history requires investigation; do not reset or force-push to accommodate it.
3. Review the incoming commits and changes against the last accepted checkpoint,
   including shared contracts, manifests, optional features, and test oracles.
   Record conflicts between intentional v2 behavior and incoming v1 parity
   changes rather than silently accepting either side.
4. Merge the recorded immutable SHA, not a moving branch name. Use
   `git merge --no-ff --no-commit <reviewed-full-sha>` to inspect the combined
   candidate before committing. Resolve conflicts in the coordinator's own
   worktree; never replace peer branches or copy unfinished peer work.
5. Run the existing Cargo-metadata-based impact validation for the changed
   packages and transitive consumers, including applicable native/optional
   feature profiles. Use one explicitly allocated heavy-build slot per shared
   host. Missing tooling or platform evidence remains a blocker, not a pass.
6. Obtain the independent outcome/compatibility and architecture/counterexample
   reviews required by [agent-operated delivery](agentic-delivery.md). Record
   the exact combined revision, actual checks, findings, and accepted upstream
   SHA before publishing the synchronization commit.

Do not rebase or force-push the shared v2 integration branch. Use ordinary
reviewed merges so its Rust-upstream ancestry remains visible. Revalidate each
new combined candidate; component results and a conflict-free merge do not
establish integration correctness.

## Command dependency and ownership ledger

The additive [`fabric-v2-contracts`](../sdk/contracts/README.md)
core library supplies deterministic operation descriptors, enumerated
generation/facet/representation bindings and type-constrained facade discovery.
It performs no execution or authorization. Conditional and unsupported
capabilities remain explicit in discovery; an operation registration is not
proof that its provider, CLI handler or MCP host is available. The library takes
already-resolved targets and does not change the existing v1 path grammar,
executable or independent oracle.

The coordinator maintains a command-level ledger from the selected live
specification. Keep private specification exports and coordination artifacts
outside the public repository unless their publication is explicitly approved.
For every command and executable subcommand, record:

- Stable task ID, specification document ID/revision, and supported typed targets.
- Dependencies on shared contracts and other tasks, with an acyclic ordering.
- Existing adapter/SDK entry points, changed files, and downstream consumers.
- One implementation owner, separate reviewer, and permitted file area.
- Exact integration base SHA, branch, acceptance criteria, and validation scope.
- State, blockers, immutable handoff commit, and integrated/validated revision.

Distinguish executable subcommands from representation paths, artifact types,
documentation headings, and historical source aliases. Never treat a registered
parser command as proof that its host or remote operation is implemented.

Use the following dependency stages to organize the ledger, not to invent
commands or endpoint support:

```text
selected specification + first-wave confirmation
    -> shared path, capability, result/error and host contracts
    -> read-only metadata and local-host slices
    -> target-specific data, lifecycle, access and authoring slices
    -> combined executable and compatibility validation
```

Within those stages, reuse the existing SDK, credential injection, HTTP,
paging, and long-running-operation policies. Do not duplicate those mechanisms
per command. Command adapters stay above the SDK; SDK consumers must not acquire
CLI dependencies. A workflow dependency is not automatically a compile-time
crate dependency.

## Dispatch and integration

Launch one bounded implementation assignment per independently deliverable
command/subcommand once its prerequisites are accepted. Combine tightly coupled
producer/consumer changes into one atomic slice rather than creating permanent
family-merger branches. Each lane starts from an exact committed v2 checkpoint
and targets the v2 integration branch, not `feature/rust`.

The coordinator owns shared interfaces, manifests, lockfiles, executable
registration, upstream synchronization, and combined-candidate validation.
Workers own nonoverlapping capability areas and return immutable commits.
Only the coordinator integrates agreed commits. No worker may weaken existing
tests, add credential fallbacks, publish releases, or perform live Fabric
mutations as part of offline implementation.

Require actual execution evidence before marking a command implemented. Keep
unsupported targets, denied access, timeouts, partial results, and
accepted-but-incomplete operations explicit. Authentication, platform, and
native acceptance claims need their own evidence.

Missing specification access, unresolved owner choices, unavailable required
validation, and absent repository write permission must be reported with the
exact action needed. Do not claim that a queued worker, local branch, test plan,
or proposed synchronization is a published or completed feature.

## Central merge pipeline

### Combined candidate checkpoint

The common branch now composes the committed CLI local host, name hints,
Notebook cancellation, create policy, definition copy, typed delete, relationship
link, schema discovery, Notebook/Livy protocol, Dataflow execution/result
formats, SQL database and native get candidates. MLModel SDK contracts were
already present. Each lane's immutable source head is preserved in merge
ancestry; uncommitted drafts and unrelated Python preparation history are not
part of this composition.

This is candidate integration, not feature-completion or production acceptance.
The local host keeps `fab-cli`, schema discovery uses `fab-cli-discovery`, and
the explicit get/runtime preview keeps `fab-cli-v2`. Distinct preview executable
names prevent one candidate from overwriting another Cargo build output.
Shared feature profiles and acceptance checks are retained. Integration repairs
reject legacy shortcut normalization in schema selectors, reconcile Cargo
dependency edges, and preserve every composed adapter and SDK export.

The combined candidate must still pass its recorded dependency-impact,
native/optional-feature and independent integration-review gates. No lane is
complete merely because its source head is merged; close a session only after
its agreed finished scope and publication are verified.

The v2 coordinator is the single writer and merger of the common branch. Lane
owners continue in their own worktrees and submit small PRs or explicitly
released immutable commits. Never switch, clean, reset, or merge into a peer's
worktree. A branch containing new commits, a completed preparation packet, or a
task status alone is not an integration handoff.

The coordinator maintains one queue in the workstream context, separate from
private specification exports and public repository code. Each entry records:

- Scope, task, current discussion session, lane branch, exact integration base,
  released full commit SHA, changed files, and explicit release evidence.
- Prerequisite interface/producer commits and overlapping ownership. Shared
  paths, manifests, lockfiles, parser/dispatcher registration, and SDK facades
  are coordinated centrally; dependent consumers wait for accepted producers.
- Actual tests and native/feature coverage at the released SHA, both independent
  review perspectives, outstanding findings, and unexecuted requirements.
- Queue state: awaiting handoff, blocked on prerequisite, ready for review,
  validating combined candidate, integrated locally, or published and verified.
  Keep the resulting integration SHA, PR/check links, and blocker explicit.

For each ready entry, serialize this sequence:

1. Verify the clean common-branch worktree, recorded local and remote heads, the
   exact source/base commits, and the trusted fork URL. Inspect outstanding
   upstream drift with the existing helper. Do not silently include all new
   `feature/rust` work or all commits from a divergent worker branch.
2. Review the released diff and its prerequisite/ownership records. Obtain the
   independent reviews required by the delivery contract. Apply only the agreed
   immutable slice; use a normal merge when its ancestry is intended, or an
   explicitly agreed cherry-pick when unrelated ancestry must remain separate.
   Record the source-to-integration SHA mapping. Never copy dirty worker files.
3. Generate and execute the existing dependency-impact plan for the complete
   candidate against the last validated checkpoint. Validate affected consumers,
   optional features and native platforms. A source-only local integration
   remains unvalidated until this exact combined revision has the required
   evidence. Do not start a competing heavy build on a shared host.
4. Publish only to the common v2 branch with a normal non-force push. Verify the
   provider ref equals the exact intended commit, then check the existing
   **Rust validation** gate for that revision. A push receipt, queued CI run or
   an old green result is not a successful combined-candidate check. If the
   remote advanced unexpectedly, stop and reconcile custody; never force-push.
5. Mark the entry published only after the actual remote SHA is verified. Record
   validation separately and notify dependent owners of the exact accepted
   checkpoint. Resume the next entry only with explicit source and gate state.

### Publication and CI

Inspect the configured remote and actual provider write access before
publication. The following explicit destination cannot accidentally push to
the checkout's `origin` if it still points at `microsoft/fabric-cli`:

```text
git push https://github.com/eisber/fabric-cli.git HEAD:refs/heads/dev/marcozo/fab-cli-v2-integration-20261104
git ls-remote --heads https://github.com/eisber/fabric-cli.git refs/heads/dev/marcozo/fab-cli-v2-integration-20261104
```

Compare the returned full SHA with `git rev-parse HEAD`. A missing ref, denied
push or mismatched head leaves publication blocked. The owner must grant Write
access to the signed-in GitHub account or renew an authorized owner login; do
not borrow another session's credentials or publish to a different fork as a
success-shaped fallback.

The existing Rust scaffold workflow runs on lane PRs, pushes to the common v2
branch, merge-group candidates, and explicit full-validation dispatch. It keeps
the dependency-impact selector, frozen source checks, native Windows/Linux/macOS
matrix, and stable fail-closed **Rust validation** job. Push validation for this
branch shares a concurrency group and never cancels the running check. GitHub
can replace a pending run with a newer pending head, so only a successful result
for the exact current candidate counts; this is not a per-commit FIFO queue.

This setup does not install a privileged automatic merger, enable branch
protections or GitHub's merge queue, waive reviews, or change the workflow's
read-only token permissions. Until an independently authorized controller exists,
the coordinator operates the serial queue above and honors all provider gates.
