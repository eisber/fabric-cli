<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native build evidence and shared-host validation

This is evidence for the experimental offline scaffold, not a release,
installation, authentication, deployment, or Python compatibility claim.
Read the [architecture](architecture.md), [compatibility boundaries](compatibility.md),
and [migration guidance](migration-guidance.md) before extending the matrix.
The Python workflows and packaging are unchanged.

## Verified provider checkpoint

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

### Earlier run and bounded workflow changes

[Run 39843435944](https://github.com/eisber/fabric-cli/actions/runs/39843435944)
also completed successfully at exact source
`407c595095ca326ee4a7d8355232346f219b6912`. Its Windows job
[120050321726](https://github.com/eisber/fabric-cli/actions/runs/39843435944/job/120050321726)
ran from `19:49:29Z` to `19:51:06Z`; the next run's Windows job started at
`19:51:03Z`. This is concrete same-branch overlap, not evidence of a host failure.

Two resource guards are added to the otherwise unchanged workflow:

- Concurrency group `${{ github.workflow }}-${{ github.ref }}` cancels superseded
  runs of this workflow for the same ref, including all three matrix jobs.
  Other refs and Python workflows are independent. A PR merge ref and its source
  branch ref are different groups; this is not cross-event SHA deduplication.
  A same-ref manual run can also supersede, or be superseded by, a push run.
- `timeout-minutes: 15` bounds each native job if dependency restore or a future
  test stalls. The longest observed job above was 105 seconds. Fifteen minutes
  leaves substantial headroom but is a budget, not a performance guarantee;
  revisit it with measured evidence as the port grows.

No matrix entry, check, test, dependency, lockfile, toolchain, cache, artifact
format, action pin, or failure gate is changed. `fail-fast: false` remains so a
failure on one host does not suppress evidence on the other hosts. Normal
supersession can still cancel an obsolete run.

The successful provider runs predate these two guards. The new workflow has
not been executed by this lane; neither dispatch nor rerun is needed to inspect
the existing evidence. The integration owner must verify the first authorized
provider run of the integrated head, including unchanged native checks.
No local Cargo compilation or test was run for this workflow/documentation-only
change, and no shared-host build slot was acquired.

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
