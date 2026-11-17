<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fixed partial native bootstrap

`rust-native-posthoc.yml` registers `workflow_dispatch` on default `main`.
Push and pull request events run only offline Node contracts, never native builds.
The trusted driver checkout is the genuine workflow SHA; the DUT is a separate
immutable checkout of the current
`dev/marcozo/fab-cli-v2-integration-20261104` merge and its exact first parent.
Both histories must retain anchor `1c3c819c3c4162af13d23b4dd3aa475b06b16526`.
No event variables are rewritten. The Windows recovery workflow is unchanged.

## Frozen policy `bootstrap-v1`

This is **PARTIAL_FIXED_WORKLOAD**, not affected selection, full qualification,
or a native baseline. The initial reference is source
`61c8221914152e231e5e64f7bd38418f66264999`, base
`313271f4104a562d853318503f8e14f552ac6dc2`, tree
`fa28f064e9b6cb9eb165267c948c9dfc36688f82`.
The driver verifies pinned existing source APIs and all five `fabric-core`
manifest/source blobs before loading any DUT helper.
The separate `9ff586d8ca537880648df8284f1efc14590d1ba4` source-domain bridge
is not overlaid, copied, or assumed to be integrated.

Every selected platform must complete all these obligations:

1. Install Rust `1.99.1` with a minimal profile in a new private `RUSTUP_HOME`.
   Check actual `rustc --version --verbose` host and `cargo --version`.
1. Use source-exported `buildArgs` and `PROFILES` unchanged for locked,
   no-default-feature debug builds of headless `fab-cli-v2` and baseline `fab-rs`.
   The second build is offline.
1. Execute all three source-exported `smokeCases` for each actual executable:
   help, version, and sanitized invalid input. Use the original `verifyOutput`
   and `isolatedEnvironment`; no identity, network configuration, or ambient PATH.
   Record both executable hashes and all six results.
1. Execute the complete existing `fabric-core` library target:
   `cargo test --locked --offline -p fabric-core --lib --no-default-features -- --test-threads=2`.
   All 21 named assertions in `CORE_TESTS` must pass exactly once, with zero failed,
   ignored, measured, or filtered tests. These are local injected-core contracts,
   not protected identity/provider/session or live Fabric tests.

There is no list-only/no-run substitute and no trimming to fit the budget.
Other CLI integration targets, optional features, SDK suites, affected capability
closures, and accumulated Linux/macOS qualification remain explicit debt.
No whole-platform watermark is advanced.

## Finite execution and evidence

Only GitHub-hosted `ubuntu-24.04` and `macos-15` are admitted, at most two platform
jobs in parallel, 900 seconds/job, 720 seconds/native executor, 600 seconds/command.
Linux must be actual x64 GNU Linux; macOS must be actual x64 or arm64 Darwin with
the corresponding exact rustc host. Record actual image, CPU, architecture and
host triple rather than assuming an architecture from a label.
Targets, Cargo home, Rustup home and sandbox are private per run/attempt/platform,
removed after execution, and never cached across runs. Only hosted dependency
downloads are permitted, locked with one retry and a 30-second HTTP timeout.

Every command uses the DUT's existing `validation-process.executeOwned` supervisor.
A bounded, supervised setup creates private POSIX output FIFOs. Native commands
write to those pipes, not unbounded files. The driver drains into strictly capped
files and aborts through the existing supervisor on overflow; no process-tree
handling is copied. Such output is explicitly CENSORED, retained at no more than
16 MiB/stream, and can never pass. The artifact budget is 64 MiB with
8 MiB reserved for metadata. More than 2,048 reported cases is not admitted.
Timeout, cancellation, supervisor/cleanup failure, empty tests, source mutation,
stale final head, malformed or missing receipts fail explicitly.
Always-upload steps preserve partial failure evidence.

The read-only repository token exists only in provider-guard steps. Native
commands, source helper subprocesses and CLI cases receive explicit allowlists.
Source inventories are retained before/after, including declared CRLF checkout
representations, together with real workflow/run/attempt/driver/source pins,
descriptors, output byte counts/hashes, and cleanup outcomes.

## Existing event-first batch route

The existing controller `buoyant-pasteur` supplies one immutable merged-source
packet after its postmerge audit and writer STOP. The existing build lead
`silent-wright` consumes it through detector 29; no new timer or daemon is added.
The lead must actually adopt this policy and record durable source/platform/
workload/run-attempt dedupe in the existing debt ledger before ongoing lanes are
called restored. A stored message or ACK alone is not dispatch execution.

After default-main registration and initial actual receipts (or a precise hard
capacity classification), the existing lead may coalesce newer verified packets:
at most one new immutable source batch per six hours, two dispatches and four
platform jobs/60 allocated native runner minutes per rolling 24 hours, including
initial restoration attempts. Older queued batches are not replayed.
The driver conservatively reserves full 15-minute slots even for failed preflight
runs and retains a 15-minute boundary margin. It reads a complete bounded history
of at most 100 dispatches; exhausted/unknown history fails closed for explicit
policy maintenance. It does not silently forget duplicate sources after a day.

Inputs are `mode=bootstrap`, `workload_id=bootstrap-v1`, `platforms=both|linux|macos`,
exact `source_sha`/`base_sha`, `route=manual|batch`, and a bounded `batch_id` for a
batch (empty for manual). `repair_of` is empty initially. A GitHub rerun is refused.
One additional attempt per platform requires a new genuine dispatch referencing
an unsuccessful initial run, a different reviewed trusted driver SHA, and remaining
daily reservations. Product-source repairs require their own authorized path owner;
this driver must not modify Rust code or the common integration branch.

The dispatching actor records the actual returned run ID and attempt. Make at
most one acquisition checkpoint by 20 minutes, then a final provider query.
No acquired runner is capacity/acquisition debt, not a source failure. Cancel only
the exact owned run when doing so cannot interrupt an acquired peer platform;
otherwise retain the finite job outcome. Do not blindly rerun, push dummy commits,
change settings/protections, acquire compute, or switch identities.
