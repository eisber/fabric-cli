<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded candidate preflight

`rust-fast-check.yml` adds one Ubuntu 24.04 job, bounded to 15 minutes, before
the existing three-OS `rust-ci.yml`. It does not replace Windows broker or macOS
filesystem evidence. It does not publish releases, open PRs, authenticate to
Fabric, modify product expectations, or automatically retry failing code.

Pushes trigger only on `validation/rust-fast-*`. New workflows cannot receive
manual dispatch until registered on the repository default branch; the initial
validation uses the push trigger without editing that branch. Checkout uses the
event's exact SHA, not a moving branch. The committed candidate record names the
original product SHA separately from the combined CI-only head.

The standard-library Node script uses the existing Cargo metadata pattern to
select safe default members and transitive consumers. `baseline` checks all
default members; `cli-jobs`, `endpoint`, and `native-values` have finite committed
seed sets. Unknown lanes and missing family comparators fail. Dependencies still
compile even when not selected as test packages. This is crate-level preflight,
not inferred per-function coverage or the complete native feature matrix.

The Linux-first `sdk-values` lane seeds `fabric-core`, `fabric-sdk`,
`fabric-sdk-http`, `fabric-core-json` and `fabric-cli-output`. It retains transitive
consumers within default core/SDK layers and CLI output, using declared Cargo
layer metadata. It explicitly defers central CLI, identity, deployment and their
native platform gates; the receipt records this boundary. This lets endpoint
and native-value candidates receive real checks independently of a known central
CLI compile failure, without claiming that their CLI consumers passed. Existing
lanes retain their broader consumer selection. The SDK/value family stage retains
the independent job source comparisons but does not build CLI job or mutation
probes; SDK unit/doctests and the facade's injected-credential example remain
in scope.

Each run executes workspace formatting, selected all-target offline checks and
Clippy, single-thread tests and doctests, then actual family examples and pinned
source comparisons. Selected CLI default and `native-read` all-target compilation
both run before Clippy, unit tests, doctests and family comparisons, so native
compilation failures stop the candidate before longer correctness work.
CLI selections include native-read compilation and its
offline consumer, not a live login. The repository-pinned Rust toolchain is
selected by Cargo/rustup as in native CI. Existing pinned Python oracle
requirements are development-only. No package or lockfile is changed.

Node automation contracts and the synthetic Python performance-harness contracts
run before compilation. These are not product performance measurements. The
finite CI composition allowlist also includes the four performance harness,
request and documentation files; product code, locks and independent compatibility
oracles remain forbidden deltas from the named source.

Cargo uses one build job, one test thread, and a private runner-temporary target.
The existing approved cache action keys toolchain/host, exact package/feature
fingerprint and lock hash, with an immutable head suffix. Restoration never
crosses those fingerprints. Registry access is limited to locked dependency
restoration; subsequent Cargo checks use `--offline`. Hosted runners are not a
network sandbox. The bounded artifact contains JSON identity/command receipts
and synthetic observations only, never environment dumps, credentials or binaries.
Provider step timestamps distinguish queue, cache, compile and test time; no
cold/warm latency is promised.

## Publish one explicitly released candidate

Run from a checkout containing the automation commit. Source and automation
must be full 40-character SHAs, already available locally. Fetch only an
explicitly released SHA if necessary. Worktree and receipt paths must be new,
absolute, caller-owned paths outside peer worktrees.

```powershell
node rust\scripts\rust-fast-check.cjs publish --source <released-product-40hex> --automation <ci-only-40hex> --lane cli-jobs --branch validation/rust-fast-<task>-<source-short> --worktree C:\owned\new-candidate --receipt C:\owned\candidate.json
```

The command verifies the existing `eisber` push URL is exactly the owner fork,
creates a new branch/worktree at the source, replays the CI-only
automation commit series, records source/lane, verifies the entire product-to-CI delta,
commits and performs a non-force push. It replays the patch rather than merging
the automation's historical base: released candidates may diverge before later
product fixes, which must not leak into a CI-only composition. If the source
already contains that series, only the candidate record changes. Each commit is
checked against the finite CI file allowlist; merge commits are rejected.
A patch conflict stops for human resolution;
there is no reset, deletion, automatic conflict resolution or branch overwrite.
A receipt is written before pushing. If interrupted or uncertain, inspect the
remote exact head and existing receipt; do not replay with another candidate.

Read the push-triggered provider run at the receipt's **combined head**, then
use a bounded `gh run watch <id> --repo eisber/fabric-cli --exit-status`.
Failure requires an owner fix and a new explicitly released SHA, not a retry.

## Optional full native dispatch after fast success

```powershell
node rust\scripts\rust-fast-check.cjs full --receipt C:\owned\candidate.json --fast-run <actual-fast-run-id>
```

This checks provider fast success at the exact combined head and verifies the
branch has not moved. Existing native runs at either the original source or
combined head are recorded and never duplicated, including failed/running runs.
Otherwise it requests the already-registered `rust-ci.yml` at the immutable
combined SHA. The receipt is marked before dispatch; an uncertain dispatch is
not replayable. This checks only this candidate, not other owners' ongoing work.
Another ongoing `validation/rust-fast-*` batch blocks a second dispatch.
Publication likewise blocks another ongoing fast candidate.

Run success is not a whole-workspace claim. Read affected-crate selection and
every actual native job/step, including failures and skipped comparisons.
An original-source run is not proof of the later combined CI head; keep both
identities and their exact coverage separate in the root handoff.

Local automation tests require only existing Node, not Rust or Python:
`node --test rust\scripts\rust-fast-check.test.cjs`.
