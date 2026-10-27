<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI performance baseline

**Measurement status: pending. No Rust-versus-Python startup or memory results
have been measured by this lane.** Harness contract tests use synthetic child
processes, not product benchmarks. There is no supported speedup claim or
performance acceptance threshold.

The initial requested product baseline is Rust source
`91e7523ea978c67ca183abaea04500e323c27eb3` and Python source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. These are **requested source pins,
not tested performance SHAs**. If the coordinator selects a corrected Linux
candidate, record its full SHA in both request source fields before execution.
Do not silently substitute a branch tip or treat newly registered commands as
working. Correctness and command delivery take priority over measurements;
optimization is a separate, evidence-led decision.

## First tier: offline process startup

The standard-library runner is
[`startup.py`](../tests/performance/startup.py). Its
[`measurement-request.json`](../tests/performance/measurement-request.json)
is a bounded, deliberately non-executable pending template, not an execution
grant. Version 1 measures Linux, including an explicitly recorded WSL host.
Windows and macOS measurements are deferred platform work; do not infer them
from Linux or local harness tests.

Run the optimized `fab-rs` executable directly against the installed Python
virtual environment's `fab` console entry point, with only `--help` and
`--version`. Do not use `cargo run`, debug Rust, Python imports, `pytest`,
an editable source checkout, a development probe, or build time as startup.
Include the Python interpreter and installed dependencies in the measured child.
No auth, service command, token discovery, workspace creation, mutation, or
automatic optimization is part of this harness.

For each target/flag pair:

1. Retain the first observed process separately. It is not necessarily the
   binary's first ever run:    the correctness check, executable hashing and build may warm files.
   `--version` also follows `--help`. **This is not a cold-cache experiment.**
2. Run three documented warmups, then at least 30 measured new processes.
   These use warm filesystem/package caches, not a persistent CLI process.
3. Run three more warmups and at least 30 separate memory processes through
   GNU `/usr/bin/time -v`. Do not include these instrumented processes in the
   startup latency distribution.

The runner executes serially, alternating Python/Rust order on successive
rounds within each phase; flags run in help/version order. It retains all
observations and outliers. Each latency is `perf_counter_ns` wall time from
before process creation through exit/reaping, converted to milliseconds.
This includes parent process-launch, output redirection, and scheduling overhead,
not just application CPU time. The watchdog is armed before the measured interval.
A blocking wait avoids
POSIX timeout-polling quantization. It is a user-facing process boundary, not
kernel `exec` time. Output hashing and watchdog cleanup happen afterward.

Children receive a minimal environment, fixed locale/terminal dimensions,
disabled color, and a fresh isolated home/config/cache/state directory per
target for the comparison. They do not inherit credentials, proxy settings,
`PYTHONPATH`, user packages, or real CLI state. The same per-target directory
is reused across rounds. Python may generate application state, but bytecode
writes are disabled; previously installed/precompiled package files and the
kernel's page cache remain warm. This is a controlled clean-home startup
scenario, not an existing authenticated user's configured startup.
The runner does not flush caches, reboot, use `sudo`, or alter CPU governors.
It is **not a network sandbox**: the authorized worker must first confirm that
the exact pinned help/version paths are offline.

### Output correctness and provenance

Before setting request `status` to `ready`, the authorized worker must confirm
both commands' expected exit 0, empty stderr, and semantically correct stdout.
For each target and flag, independently capture the exact stdout byte count
and SHA-256 using the same isolated environment, redirected streams, working
directory, and invocation as the runner. Review help content and the version
against the pinned source/package; do not bless bytes merely because a process
exited successfully. Put these reviewed contracts in the request. The benchmark
never learns or rewrites its own expectations. Every warmup and measured sample
must match; a nonzero exit, timeout, unexpected stderr, changed bytes, missing
RSS, or spawn failure fails the comparison and preserves the failed observation.
Failed or incomplete comparisons have **no aggregate summaries**.

Record a provenance receipt containing:

- Exact clean Rust source SHA, Rust/Cargo versions, target triple, complete
  build command, release profile/overrides, feature list, native backend,
  binary SHA-256, and native library/package mode.
- Exact Python source SHA, non-editable wheel/source installation receipt,
  Python version/architecture, distribution version, dependency freeze,
  entry-point shebang/interpreter binding, and installed-file/interpreter hashes.
  Hashing the small `fab` script alone does not pin the Python package/runtime.
- Actual OS/kernel/architecture, CPU model/allocation, governor (or explicitly
  unavailable), background load before/after, filesystem/mount/cache history,
  WSL version if applicable, and enforced CPU/RAM/task caps with readback.
  WSL caps, host contention, power policy and warm caches can materially
  influence the figures. Record unavailable evidence explicitly.

The runner verifies executable hashes and package-mode declarations, but cannot
prove compilation flags, wheel provenance, package immutability, governor state,
or an external execution lease. The worker must hold source/install files
immutable through the run and attach those receipts. Features and native
backend must be reported even for help/version. Default-feature startup does
not predict the footprint of a future service-capable/auth-enabled build.

### Peak resident memory

Each memory observation starts a **new GNU time process**, obtaining that
invocation's Linux `Maximum resident set size (kbytes)` as **KiB**; divide by
1024 only when explicitly displaying MiB. This is not VSZ, current RSS, the
Python benchmark parent's memory, or cumulative `RUSAGE_CHILDREN` across rounds.
GNU time uses child resource accounting: waited descendants can affect its
reported high-water mark, but it does not sum simultaneous process-tree RSS
and cannot guarantee coverage of detached/unwaited descendants. The intended
CLI entry points do not require helper processes. Any observed descendants
must be disclosed; a separate authorized process-tree study is required before
claiming a tree-total peak. Hard aggregate resource caps remain external.
The timing wrapper's own wall time is retained diagnostically, not compared
as CLI latency.

### Request and result schemas (version 1)

The request template is the canonical field inventory. Unknown/missing fields,
unsupported flags/targets, unpinned digests, fewer than 30 samples or three warmups,
or bounds above 600 seconds are rejected. Only `rust` and `python`, each with
exactly `--help` and `--version` output contracts, are accepted.
`endpoint_host` is always null; no identity or secret is accepted. `source_sha`
matches the Rust target source, while the Python target pins its own source.
`status: pending` permits schema validation but prohibits execution; `ready`
also requires replacement of pending metadata/digests. Readiness is an
attestation, **not authority**. No request file grants a lease.

The JSON receipt contains schema/status, UTC start/finish, harness/request
hashes, harness runtime/platform/architecture, the validated request, units,
every ordered observation, and only on complete success, four case summaries.
Each observation records target, flag, phase, index, `wall_ms`, exit code,
stdout/stderr byte counts, stdout SHA-256, validity/failure, and optional
`peak_rss_kib`. No raw child output or inherited environment is published.
Each summary has `wall_ms` and `peak_rss_kib` objects containing sample count,
median, nearest-rank p95 (`ceil(0.95 * n)`), min, and max. Keep first-observed
and warmup observations separate. A failed receipt exits 1; success exits 0.
`validated_only` exits 0 but is explicitly **not a measurement receipt**.
Existing receipt files are never overwritten.

## Bounded worker handoff

**No benchmark lease is active or implied.** The coordinator assigns the
existing sole WSL validator a low-priority slot after CLI correctness. Use its
existing shared Windows validation mutex and private native-Linux checkout,
target/cache and venv, with actual aggregate limits no greater than 2 CPU,
6 GiB and 128 tasks. Do not start another installer, builder, WSL instance
controller, or nested worker. Stop if caps/mutex/provenance cannot be verified.

The maximum comparison budget is 30 minutes: one bounded preparation/correctness
job, one release build (once), and one harness job, each no longer than 600
seconds including its process tree. The runner has its own <=600-second deadline
and <=30-second child timeouts (template: 5 seconds). External bounds must cover
builds, preparation, scheduler delays and cleanup as well. A timeout is a failed
run, not permission to increase the budget or discard a sample. Linux timeout
cleanup kills only the launched process group; the existing worker's cgroup
must bound and clean the entire job, including any escaped descendants.

Commands below use Linux paths because they execute **inside** the already
authorized Linux worker. Substitute the worker's actual immutable source,
private directories and artifact locations in its grant. They do not authorize
execution or dependency restoration.

```sh
# Lightweight contracts; no product builds, package installation, or service I/O.
python3 -B -m unittest discover -s rust/tests/performance -p 'test_*.py' -v

# From the exact pinned Rust source's rust/ directory; one authorized build only.
# CARGO_TARGET_DIR must be the worker's private native-Linux target.
cargo build --offline --locked --release -p fabric-cli --bin fab-rs --jobs 2

# From repository root, after independent correctness/provenance review.
# Prepare a COPY of the pending template in the worker's artifact directory.
python3 -B rust/tests/performance/startup.py --request /ARTIFACTS/request.json \
  --output /ARTIFACTS/request-validation.json --validate-only
python3 -B rust/tests/performance/startup.py --request /ARTIFACTS/request.json \
  --output /ARTIFACTS/startup-receipt.json
```

The coordinator must supply the actual release feature selection and its
correctness receipt. The initial proposed build above uses default features;
it makes no claim that Linux native authentication/service execution works.
Python should use the existing pinned installation if proven non-editable and
source-correct. Missing package/runtime/dependencies require explicit worker
authorization; the harness never builds or installs either implementation.
Return the actual receipt, provenance/output checks, cap/mutex evidence,
commands/exit codes, full tested source SHAs and confirmation of cleanup.
Publish numeric results only after verifying that evidence.

## Later tiers (not implemented or authorized here)

**Handler-boundary CPU/latency:** compare deterministic offline LS/GET/job
fixtures only through the actual production handler paths and injected HTTP
and identity seams. Match payload, pagination/page count, serialized bytes,
requests and time policy. Report different request counts or internal cache
snapshots as behavior differences, not quiet latency wins. Existing development
probe executables are not shipped CLI flags, full-process service performance,
or a reason to add static-token/fallback authentication.

**Live read-only end-to-end:** requires a separate explicit coordinator grant
for an existing provider bridge, same endpoint/tenant/dataset/identity purpose,
and finite request/rate budget. Record user wall time including refresh, plus
separate token-acquisition/refresh, SDK CPU, HTTP/server/queue and cache-warming
evidence where observable. Do not infer native Rust authentication readiness
from a Python service success or introduce a Python auth wrapper as a Rust
product fallback. Never export certificates/tokens or publish private tenant,
identity, host, or organization details in this page.
