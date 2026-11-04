<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI v2 delivery and upstream synchronization

CLI v2 develops separately from the existing Python CLI and Rust compatibility
work. Its source baseline is `feature/rust` in `eisber/fabric-cli`, initially
`a9df45501ec53d921c6fc765c726c32979b85931`. This process does not change the
existing command grammar, release support, or compatibility expectations.

The initial local integration branch is
`dev/marcozo/fab-cli-v2-integration-20261104`. Creating a local branch does not
publish it or establish GitHub write access. The original workspace branch is
preserved. Neither `microsoft/fabric-cli` nor the shared `feature/rust` branch is
a push destination for this v2 effort.

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
