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

The explicit Linux `output-audit` and `orchestration-audit` lanes collect
independent baseline observations, not compatibility-gate passes. The output
lane runs both `fabric-cli-output/output_audit` and the native-read CLI
`output_contract_audit` targets even when the first fails. Each real exit and
its separate stdout/stderr are retained; any failed command leaves the job
failed. The orchestration lane captures all 18 Python cases, runs the source
guards, builds the existing `read_session_probe`, and attempts the actual
comparison. It records the probe hash separately from the claimed source pin.
Missing fields, failed captures and expected baseline divergences remain
failures, never passed coverage. Only these finite audit plans collect
multiple failures; ordinary preflight remains fail-fast.
Root-approved disjoint output, orchestration, wheelhouse and deploy hosted
families may run concurrently; overlapping families and all other fast
candidates retain the single-candidate guard. This grants no local build slot.

The dedicated `windows-wheelhouse` setup lane performs no Rust build,
authentication or service requests. It uses normal verified PyPI TLS on
hosted Windows x64, builds the pure Python product from pinned source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, verifies packaged files against
that source, and downloads the 28 exact dependency pins in
`scripts/rust-fast-windows-constraints.txt`, including `pymsalruntime` 0.20.7.
Each public wheel must match the non-yanked PyPI filename, size and SHA256.
The artifact contains 29 distribution wheels, an exact hash-required offline
requirements file, source/build provenance and preparation logs. A fresh
temporary hosted venv must install offline and pass `pip check`; partial
artifacts have a failed manifest and must not be installed. The newly built
product wheel hash is recorded independently, not assumed byte-identical to
an earlier Linux-built wheel. No trust bypass, feed change, product import or
credential discovery is performed. Local use still requires independent
resource authorization, artifact/provider verification and hash checks before
installation. Setup and audit lanes cannot authorize full-matrix dispatch.

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

SDK/value checks explicitly include the default transport library and strict
Clippy/tests for `fabric-sdk-transport/hyper-rustls`; the feature selection is
part of the cache fingerprint. The HTTP and operation `endpoint_context`
integration tests run first in the test stage, before the combined unit/doc
suite, to expose selected-host, paging, LRO, cancellation and audience failures
promptly. Family checks run the native transport and SDK injected-credential
examples, independent item and HTTP source references, and all four existing
output guard scripts, including text and native-value guards. These are finite
commands in the reviewed helper, never shell copied from a source manifest.

`scripts/rust-fast-requirements.txt` includes the shared oracle requirements and
pins Requests 2.34.2/urllib3 2.8.0 to the HTTP oracle's documented initial
execution. It does not modify the oracle, its expectations, shared requirements
or product dependencies.

The separate Linux-only `cli-auth` lane checks default CLI consumers and the
opt-in `windows-broker` compile/strict-Clippy graph. It runs only the finite
`auth_commands`, `auth_arguments`, `cli_contract` and `workspace_contract`
integration targets, the CLI binary unit target, default-feature-free CLI
regressions and broker-feature doctests. The command consumer substitutes a
synthetic broker at the existing boundary; no valid-account login/logout,
token acquisition, account discovery or native identity test is invoked.
An off-Windows unsupported response or a zero-test Windows-gated binary target
is not an authentication happy path. Native Windows adapter/STA contracts,
central root mounts and live platform acceptance remain explicit debt, not
three newly proven canonical commands.

The Linux `cli-95` lane adds the released table packages and explicit actual
root auth/table, family and local consumers. Default selected regressions,
complete native-read CLI regressions, doctests and the unchanged read/job source
comparators still run. Its name is a requested milestone, not proof of a
canonical command count, native Windows broker behavior or live acceptance.

All CLI lanes restore locked registry dependencies for the native Linux target
and the Windows x64/macOS ARM64 metadata targets before offline checks. The
unchanged architecture contract invokes `cargo tree --offline --locked` for
those targets even on Linux; fetching only native Linux dependencies caused
run `40517626784` to fail on missing `windows 0.62.2` after its six auth
command tests passed. This restoration does not cross-compile or execute native
platform code, install target toolchains, modify the lock, or weaken the oracle.
The finite target set is fingerprinted. Fetch runs after cache restoration even
on exact cache hits so a partial cache cannot become false offline coverage.

The bounded `windows-auth` lane uses the existing fast workflow on
`windows-2025`, selected only by the dedicated
`validation/rust-fast-windows-*` push namespace or that explicit dispatch choice.
The helper requires the matching lane, immutable candidate record, dedicated
ref and actual `x86_64-pc-windows-msvc` host; other lanes remain Linux-only.
Action pins, read-only permissions, private targets, command bounds and
source-versus-CI-head receipts are unchanged. The optional native identity crate
is selected explicitly for this lane, never added to ordinary default members.

It compiles and strictly lints the default, `windows-broker` and `native-read`
CLI graphs, then runs the existing native identity library tests, safe identity
library tests, pure Windows CLI adapter binary tests, synthetic auth/parser/CLI/
architecture contracts for both features, default CLI contracts and broker
doctests. Reviewed native tests cover metadata/authority/expiry/status checks,
synthetic request lifecycle and hidden parent-window creation/cleanup on an STA;
they do not call account enumeration, valid-account login, sign-out or token
acquisition. No `cargo run` or live CLI auth invocation is in the finite plan.
This supplies direct native identity unit coverage that a CLI-only push may omit
from the separate unchanged three-OS workflow. It does not prove WAM/MFA/CA,
service access, all Windows behavior or canonical command acceptance. Read
existing exact-source provider jobs before dispatching any additional backstop.

On October 27, 2026, the actual Windows x64 backstop
[run 40525206815](https://github.com/eisber/fabric-cli/actions/runs/40525206815)
completed successfully on product source
`03fab20577a1016178de227eac0ee178921bb8d0`, combined CI head
`801ee480e03f49fadcac55606c5e3dfc09648010` and automation
`94a6a2fb01d53edba6fa6ddf10887bea0c1ca3f1`. The downloaded receipts match all
17 finite commands, all exit zero. Native identity library tests passed 19,
including both hidden-window/STA lifecycle cases; safe identity tests passed two
and pure broker adapter binary tests passed two. Broker-feature integration
tests passed 27, native-read binary/integration tests passed 32, and default
CLI contracts passed 15. These 97 test invocations overlap across features;
they are not 97 unique cases. The doc target executed zero doctests, which adds
no coverage. The final source/lock check passed. Native WAM authentication,
MFA/Conditional Access, service access and full three-platform acceptance remain
separate gates.

The initial Windows manifest's supplemental metadata-target labels reused the
Linux labels, while the actual locked fetch correctly restored all three
declared targets. Subsequent metadata generation shares the same finite target
helper as execution so labels match the recorded commands on both hosts.
This receipt-label correction does not change validation commands or product
behavior; retain the original successful run's metadata and actual command
receipts separately.

The bounded Linux `deploy-mount` lane checks and strictly lints the selected
default and native CLI graph, then tests `fabric-deploy` before the actual
`fabric-cli` `native-read` integration targets `deploy_mount` and
`deploy_parameters`. Before execution, it lists the four deployment library
targets (`config`, `repository`, `parameters`, `yaml`) and both CLI targets.
Compare actual registered tests with the source inventory; listing a test
does not prove it ran, and Cargo can stop before later binaries when one fails.
The family stage checks nine unchanged source comparators against the pinned
public CICD/CLI source, then native-feature CLI doctests. Development-only
PyYAML 6.0.3/jsonpath-ng 1.9.1 match the protected deployment oracle; the
separate installed Python performance baseline is not modified.
Its consumer injects offline HTTP and uses
temporary files; the finite plan never invokes a real `deploy` command or live
publication. The receipt explicitly limits evidence to the library/root
consumer, not broad configuration parity, whole-workspace regression, canonical
acceptance or native Windows/macOS deployment proof. Source-owner fixes require
a newly selected immutable source, never oracle weakening or unchanged retries.

The `native-endpoint` lane uses `validation/rust-fast-msit-*` and runs the same
immutable candidate on Linux x64 and Windows x64, with fail-fast disabled between
hosts and separate host-labeled artifacts. Native endpoint configuration/read,
ReadSession and source-operation contracts run first, followed by affected
default/native CLI/SDK regressions, doctests and existing source comparisons.
This tests endpoint routing with synthetic caller-injected HTTP, not live MSIT
credentials, access or workspace creation. Existing lanes still select only
their documented single runner. Windows probe names retain their `.exe` suffix.

Ordinary preflight runs execute workspace formatting, selected all-target offline checks and
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
network sandbox. Ordinary bounded artifacts contain JSON identity/command receipts
and synthetic observations only, never environment dumps or credentials.
Independent audits additionally retain command stdout/stderr. The explicitly
approved wheelhouse artifact contains public package wheels (including Windows
dependency binaries), their hashes and preparation provenance; it contains no
native CLI build or credentials.
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
The setup-only Windows wheelhouse lane is a narrow exception to patch replay:
its pinned Python source predates `rust/`, so it copies only allowlisted
automation-file snapshots from the validated automation commit. Product files
and historical product ancestry are never copied. Other lanes still replay the
checked CI-only commit series and stop on conflicts.
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
