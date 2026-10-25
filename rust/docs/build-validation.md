<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native build evidence and shared-host validation

This is evidence for the experimental offline scaffold, not a release,
installation, authentication, deployment, or Python compatibility claim.
Read the [architecture](architecture.md), [compatibility boundaries](compatibility.md),
and [migration guidance](migration-guidance.md) before extending the matrix.
The Python workflows and packaging are unchanged.

## Historical provider checkpoint

Re-read on 2026-10-22 using GitHub's run metadata, job logs, and artifact API:

- Repository: `eisber/fabric-cli`, branch `feature/rust-workspace-scaffold`.
- Exact source: `eccfca1d3b1cb4df71d0fa65a76221a588095290`.
- [Run 39843665213](https://github.com/eisber/fabric-cli/actions/runs/39843665213):
  push event, completed with `success`, created `2026-10-22T19:51:00Z`,
  updated `2026-10-22T19:52:50Z`.
- Cargo `1.99.1 (5f94df478 2026-08-27)`, rustc
  `1.99.1 (58cf8f9c3 2026-10-13)`, LLVM `23.1.1` on every job.
  `rustup show active-toolchain` confirms each used `rust/rust-toolchain.toml`.
- Artifact API: `total_count = 0`. These are executed debug/check/test jobs,
  not downloadable distribution artifacts.

| Configured runner | Actual rustc host | Actual runner image and version | Successful job | Job started / completed (UTC) |
| --- | --- | --- | --- | --- |
| `windows-2025` | `x86_64-pc-windows-msvc` | `windows-2025-vs2026`, `20261021.278.1` | [120051121926](https://github.com/eisber/fabric-cli/actions/runs/39843665213/job/120051121926) | `19:51:03` / `19:52:15` |
| `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | `ubuntu-24.04`, `20261012.334.1` | [120051122154](https://github.com/eisber/fabric-cli/actions/runs/39843665213/job/120051122154) | `19:51:03` / `19:51:41` |
| `macos-15` | `aarch64-apple-darwin` | `macos-15-arm64`, `20260907.0337.1` | [120051121913](https://github.com/eisber/fabric-cli/actions/runs/39843665213/job/120051121913) | `19:51:05` / `19:52:50` |

All job dates are 2026-10-22. Architectures come from `rustc --version --verbose`,
not inference from runner labels. Hosted image labels can move; these versions
describe this run, not pinned images or byte-for-byte reproducible builds.
The macOS evidence is ARM64 only, not Intel macOS.

### What passed

Every job successfully executed the following from the `rust/` directory:

```text
cargo --version
rustc --version --verbose
rustup show active-toolchain
cargo fetch --locked
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --offline --locked --workspace
cargo test --offline --locked --workspace --doc
cargo run --offline --locked -p fabric-sdk --example injected_credentials
cargo run --offline --locked -p fabric-cli -- --help
cargo run --offline --locked -p fabric-cli -- --version
cargo tree --locked -p fabric-sdk --edges normal
cargo tree --locked --edges features
cargo tree --locked --duplicates
```

The workspace test logs report 12 passing non-documentation tests per host:
three CLI subprocess contracts, one workspace boundary contract, three core
identity-selection/error tests, one unsupported identity-backend test, three
page serialization/error tests, and one independent injected-credential test.
No tests were failed, ignored, or filtered out. The separate documentation step
passed but ran **zero doctests**; it does not demonstrate documentation example
coverage. The CLI version output was `fab-rs 0.1.0`.

Dependency restoration uses registry access. Tests and executable examples use
Cargo's `--offline` flag after restoration; this is not an OS network sandbox.
Metadata, check, Clippy, and dependency-tree steps currently use `--locked`
without `--offline`. The tests/examples implement no live service or identity
calls. No live tenant, credential, broker, certificate, proxy, or TLS behavior
was validated.

The checked graph uses `azure_core` with default features disabled: no HTTP/TLS
transport is selected. The SDK's normal dependency closure excludes the CLI and
identity implementation. No installed Python, `az`, or `azd` is required by that
graph; passing on preprovisioned hosted runners is not a clean-machine
installation test proving their absence. Native linkers are still required.
Future Rustls HTTP transport does not rule out separate certificate-authentication
prerequisites such as OpenSSL.

## Owner-confirmed affected-crate goal

### Executable source-parity gate

When `fabric-cli` is selected, native CI also compares the built `read_probe`
and `projection_output` consumers against the independent pinned Python
references. This runs on each selected Windows, Linux and macOS runner, after
the existing Rust checks. Python 3.13 and development-only `jmespath==1.0.1`
and `cachetools==5.5.2`
are installed for that comparison; neither becomes a Rust product dependency.
The affected-crate selector and Python product workflows are unchanged.

[`run_read_parity.py`](../tests/compatibility/run_read_parity.py) requires all
current command comparisons, additive metadata cases, all specialized collection
observations, command guards, actual native query observations, query comparisons, query guards, and the actual
`folder_read_probe` with its independent folder-command comparisons and guards.
It also requires the independent raw query-argument comparisons through both
native probes, their real host/SDK/query/output consumers and oracle controls.
Those raw-argument observations remain distinct from the legacy handler-input
folder inventory; neither fixture set is rewritten to merge the boundaries.
Workspace GET adds the original 70 source contracts and controls separately from
its 58 typed-domain consumer cases and controls. Both native probes must assert
all 58 consumer outcomes, retaining exact, safety and unsupported categories.
The original 70-case native differences are documented evidence, not a passing
native-parity denominator or a waived source expectation.
The native checkout retains full Git history so the reviewed nine-ID folder
evidence can verify its original Python and Rust baseline blobs.
Both command comparators explicitly require
`--request-mode source-routed`; the historical generic-route mode is not a
fallback for a failing current probe. Every subprocess
has a finite timeout; nonzero exits, timeouts, missing probes and observation
write failures fail the native job and therefore the existing final gate.
There is no historical-mode fallback, fixture update, waived comparison or
success-shaped empty observation. Separate runner tests inject failures at
every one of the sixteen process stages. Each oracle retains its own
exact/different/unsupported accounting;
passing CI does not convert explicit safety exceptions into source equivalence.

From `rust/`, after building both examples and installing the pinned
development requirement in an isolated environment:

```powershell
cargo build --offline --locked -p fabric-cli -p fabric-cli-query --examples
python -B -m unittest discover -s tests\compatibility -p test_run_read_parity.py -v
python -B tests\compatibility\run_read_parity.py --read-probe target\debug\examples\read_probe.exe --query-probe target\debug\examples\projection_output.exe --folder-probe target\debug\examples\folder_read_probe.exe --artifacts $artifactDirectory
```

Use the actual private target and native executable suffix. The shared-host
execution grant and common mutex still apply locally. CI supplies its runner
temporary directory for observations; the runner never builds or installs
anything and never rewrites source expectations. Tests/oracles do not perform
live service or identity calls. Dependency restoration still uses the registry.
This gate does not configure production credentials/transport, measure coverage,
or claim parity for unported commands. Provider execution of the new workflow
must be verified separately after publication.

The owner confirmed crate-level selection including transitive consumers, rather
than attempting to infer individual relevant test functions from changed lines.
The outcome is to avoid compiling and testing unrelated crates while preserving
the contracts of affected consumers on all three native runners. This does not
establish release/support parity or an absolute latency, memory, or throughput
target. The measured baseline above is evidence, not a performance promise.

The selector reads Cargo's workspace metadata rather than maintaining a second
dependency graph in CI. It includes normal, build, development, optional, and
target-specific workspace dependency edges conservatively. Renamed dependencies
are matched by their package path, not their import alias. As consumers are
added to the manifests, selection follows those edges automatically.

| Changed input | Selected native validation |
| --- | --- |
| Identity source, with the integrated graph | `fabric-identity` only, including optional broker and safe-consumer checks |
| Windows native boundary source | `fabric-identity-windows` and its optional consumer `fabric-identity` |
| Identity source after the CLI declares an identity dependency | `fabric-identity` and `fabric-cli` |
| Core source, with the integrated graph | `fabric-core`, `fabric-identity`, `fabric-sdk-workspaces`, `fabric-sdk`, and `fabric-deploy` |
| Focused workspace SDK source | `fabric-sdk-workspaces`, `fabric-sdk`, and `fabric-deploy` |
| SDK facade source or its example | `fabric-sdk` and `fabric-deploy`, including the offline SDK example |
| CLI source or its contracts, with today's graph | `fabric-cli` only, including help/version checks |
| Several crates, including cross-crate moves | Union of changed crates and all their transitive consumers |
| Any Rust Cargo manifest/lockfile, toolchain, rustfmt configuration, workflow, shared fixture, or unclassified Rust build input | All workspace members, including the workspace architecture contract |
| Markdown under `rust/docs/` or `rust/skills/`, root Rust README/AGENTS, or the fixture README only | No native build; no Cargo invocation in selection |
| Changes outside Rust and this workflow only | No Rust native build |

Crate-local Markdown is deliberately treated as a crate input: it might be
included in rustdoc or source. Rust source containing documentation remains a
code change. Shared fixture contents are not exempted as documentation.
An unknown or deleted crate directory selects the whole current workspace.
Non-workspace path dependencies or workspace members outside `rust/` fail
selection explicitly and require review; external source trees are not silently
treated as unrelated.

### Check scope and correctness gates

For every selected crate, the existing formatting, all-target check, Clippy,
offline tests, and documentation tests still run on Windows, Linux, and macOS.
Package arguments are passed together in one Cargo invocation per check, not in
separate overlapping crate jobs. For an identity-only change, the core commands
are:

```text
cargo fmt --package fabric-identity -- --check
cargo check --locked --package fabric-identity --all-targets
cargo clippy --locked --package fabric-identity --all-targets -- -D warnings
cargo test --offline --locked --package fabric-identity
cargo test --offline --locked --package fabric-identity --doc
```

Larger selections repeat `--package <name>`. SDK/example/normal-closure checks
run only when the SDK is selected, and CLI help/version only when the CLI is
selected. Feature and duplicate dependency inspection is scoped to selected
packages. Shared changes select all members and therefore retain the full
workspace's checks, including the architecture test located in the CLI crate.
No test source, expected output, warning threshold, or independent oracle is
changed. Routine affected-crate CI does not waive any separately required full
integration check; full manual validation remains available subject to its own
execution authorization.

Cargo must still compile a selected crate's required dependencies on a cold
runner. Selecting identity does not imply avoiding compilation of its core
dependency, but it does avoid unrelated SDK/CLI/deployment builds and unit
tests. Registry restoration and metadata inspection are not package compilation;
the existing locked fetch can restore dependencies beyond the selected crates.
No cache system or persistence contract is introduced. Node is used only for
Actions orchestration, not as a shipped product dependency.

The lightweight selector runs for every PR and configured-branch push, including
documentation/Python-only changes; only relevant changes start native jobs.
There are no top-level path filters whose bounded provider diff could miss a
Rust change in a large mixed change set. A full local Git diff is used instead:

- PRs compare their event base SHA with the checked-out merge head, covering the
  cumulative PR rather than just its latest commit.
- Pushes compare the event's `before` commit with the checked-out head, including
  every commit in that push. Rename detection is disabled so both old and new
  paths participate, including deletions.
- Manual runs always select all members. A new branch, invalid base, or base
  unavailable after full-history checkout explicitly selects all members and
  logs why. A failed diff, Cargo metadata command, or graph parse fails the
  selector; it never returns a successful empty selection.

The job summary records the actual checked-out head, diff base, selection
reason, selected packages, and changed-path count. The `Rust validation` final
gate fails if selection fails or required native checks fail, cancel, or skip.
It passes without native execution only for an explicit successful empty
selection. The integration owner should use this stable gate for required Rust
validation; this lane does not modify repository branch protection.

### Measurable acceptance and current evidence limit

Local validation executes the actual inline selector with mocked Git/Cargo
results, without a local Cargo build. Its 38 scenarios cover identity-only,
core consumers, future/multi-hop consumers, a new workspace member, renamed
optional/target/build dependencies, combined/deleted/moved paths, shared inputs,
docs-only skipping without Cargo, manual/new-branch/force-push bases, and explicit
diff/metadata/graph failures. The same selector also runs its pure selection
regression assertions in CI. Eight final-gate outcome combinations, native Bash
syntax, package argument expansion, YAML structure, action pins, and conditional
SDK/CLI step wiring were checked locally. Native scripts were also executed
against tool spies, checking exact command/argument sequences for identity-only,
core-consumer, future CLI-consumer, and full-workspace selections without
invoking the real Cargo/compiler tools.

Acceptance requires the first authorized provider run at the integrated head
to pass all selected checks on the three recorded native targets, with the
actual selection/host/toolchain/result evidence retained. Shared/workflow changes
must select all members; an identity-only change must exclude unrelated crates
and still include any newly declared consumers. Documentation-only changes must
skip native execution. Selector errors must fail the final gate. These are
selection/output requirements, not assumed wall-clock savings.

The successful provider runs above predate change-aware selection and the
resource guards below. Neither the new selector nor the changed native commands
have been executed by this lane on GitHub-hosted runners. Local selector tests
are not provider execution or evidence that the hosted toolchain/metadata
prerequisites have succeeded. No local Cargo compilation/test, installation,
shared-host lease, workflow dispatch, or remote publication was performed.

## Earlier runs and resource controls

[Run 39843435944](https://github.com/eisber/fabric-cli/actions/runs/39843435944)
also completed successfully at exact source
`407c595095ca326ee4a7d8355232346f219b6912`. Its Windows job
[120050321726](https://github.com/eisber/fabric-cli/actions/runs/39843435944/job/120050321726)
ran from `19:49:29Z` to `19:51:06Z`; the next run's Windows job started at
`19:51:03Z`. This is concrete same-branch overlap, not evidence of a host failure.

The first submitted change added same-ref cancellation and a 15-minute job
timeout. Change-aware selection requires a narrower cancellation policy:

- Superseded PR runs can cancel because the replacement validates the cumulative
  PR diff. Groups include workflow, event type, and PR number.
- Push and manual runs use their unique run ID and do not cancel one another.
  Otherwise a docs-only push could cancel a pending code-push validation and
  then select no crates. This deliberately permits the same-branch push overlap
  observed above rather than dropping required evidence. Cross-push
  deduplication would need a separate validated-baseline protocol.
- `timeout-minutes: 15` bounds each native job if dependency restore or a future
  test stalls. The longest observed job above was 105 seconds. Fifteen minutes
  leaves substantial headroom but is an owner-confirmed resource budget, not a
  performance guarantee; revisit it with measured evidence as the port grows.
  The lightweight selection and final-gate jobs have the same upper bound.

The native matrix, dependencies, lockfile, toolchain, artifact formats, and pinned
direct Actions remain unchanged; no cache or installer is added.
`fail-fast: false` remains so one host's failure does not suppress the other
hosts' evidence. There is no approved startup, memory, or throughput threshold,
nor a measured cold/warm cache comparison.

## Proposed shared-host build lease

This is a small manual protocol, **not an active lease or a scheduler service**.
The integration owner remains the sole grantor. The build lane does not grant
slots, control peer sessions, or infer ownership from silence, historical task
state, file contents, an idle terminal, or an expired request.

1. A requester sends one task-scoped request to the integration owner. Name the
   exact holder session and grantor, host, worktree, branch/full HEAD, `rust/`
   working directory, purpose, complete ordered commands, dependency/network
   needs, expected duration, and requested UTC expiry. Keep private host/session
   identifiers in the coordination channel, not committed source. A request
   does not grant execution.
2. The integration owner explicitly grants one named holder a unique lease ID
   with the exact command list, process-local resource limits, start deadline,
   and hard UTC expiry. Default budget is one heavy build/test/coverage process
   tree across the host, not one per language or worktree. Coverage and Python
   suites consume that same slot. No delegation or implicit renewal.
3. The holder checks HEAD and working directory, starts only the granted
   commands, and records their shell/process identity. Use the worktree's own
   target directory; do not share mutable Cargo output with peer worktrees.
   Missing tools/dependencies, TLS failures, or broader checks require a new
   owner decision, not installs, certificate changes, or a network retry outside
   the grant. An offline miss remains an explicit failure.
4. On completion or failure, release once with lease ID, actual HEAD, commands,
   UTC start/end, exit status, log/artifact locations, and confirmation that the
   process tree has ended. Stop work at expiry using the recorded process-tree
   handle and report an incomplete result; never kill by process name or affect
   a peer. Expiry revokes permission but is not proof the process stopped. The
   owner verifies release/termination before regranting the slot.
5. Wait for a grant or continuation message without polling. Continue permitted
   reading, provider-log inspection, or documentation in the meantime. If
   messaging is unavailable, report that exact blocker to the integration owner;
   do not bypass it. A continuation message is not itself a lease unless it
   contains the required explicit grant.

For example, a requester changing a CLI subprocess contract could request
**only** this command, from its own `rust/` directory, after dependency restoration
has already been authorized and completed:

```text
cargo test --offline --locked -p fabric-cli --test cli_contract --jobs 1 -- --test-threads=1
```

Name the real holder and use concrete UTC timestamps in the private grant.
This example grants nothing and does not replace the full/native integration
gates. A workflow/documentation-only edit needs no Cargo build merely to consume
a slot. Shared-host scheduling does not serialize GitHub-hosted matrix runners;
the workflow's concurrency guard is a separate resource control.

## Unexecuted targets and next packaging decisions

No release-profile build, install/uninstall, distribution artifact, signature,
notarization, or clean-machine runtime test is evidenced by these runs. Linux
musl/static portability, Linux ARM64, Windows ARM64, Intel macOS, broker
integration, enterprise proxy/trust, and certificate authentication remain
separate gates, not implied support.

Before authorizing a packaging slice, the integration owner needs bounded answers:

| Question | Evidence required before claiming that capability |
| --- | --- |
| Which single already-executed native target should receive the first release-profile build experiment? | Exact head/toolchain, command, linker prerequisites, and resulting executable tested on that target; no installer or filename contract implied |
| Which clean-machine OS/runtime baseline must run that executable without Python, `az`, or `azd`? | Approved disposable environment, native-library inventory, and offline help/version/error smoke results |
| Are musl or additional architectures actually required for the next milestone? | Separate target/linker choice and native execution evidence; cross-compilation alone is insufficient |
| When network/identity dependencies arrive, which trust and certificate scenarios are required first? | Approved transport/features, proxy/trust fixtures, and native-prerequisite review, including possible certificate-related OpenSSL |

Installer/package formats, executable promotion from `fab-rs`, release filenames,
signing identities, notarization, publication destinations, and live auth/deploy
validation require their own owner decisions and authorization. Nothing here
freezes those contracts or authorizes release operations.
