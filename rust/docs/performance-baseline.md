<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI performance baseline

**Measured on October 27, 2026:** one bounded WSL Linux startup and memory
comparison passed all 268 observations. This measures only `--help` and
`--version`, not service latency, full CLI parity, or a general speedup.
There is no performance acceptance threshold.

The measured sources are Rust
`b78d9654a37ca909036df54b8562e01fcfe8aa13` and Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. The optimized default-feature
`fab-rs 0.1.0` ran directly; Python `fab 1.8.0` was installed non-editably from
that source into a new private CPython 3.13.15 environment. All 276 installed
product source/resource files matched the pinned source. All installed
package/runtime files were hashed before and verified unchanged after the run.

## Measured startup and memory

| Target | Invocation | Wall median (ms) | Wall p95 (ms) | Peak RSS median (KiB) | Peak RSS p95 (KiB) | First observed (ms) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Rust | `--help` | 2.972 | 4.136 | 4,042 | 4,204 | 3.267 |
| Rust | `--version` | 3.013 | 3.665 | 4,024 | 4,184 | 3.077 |
| Python | `--help` | 1071.347 | 1369.381 | 46,728 | 46,800 | 1136.797 |
| Python | `--version` | 1038.556 | 1127.125 | 46,684 | 46,844 | 1023.552 |

Each target/invocation used three warmups and 30 new-process latency samples,
then three additional warmups and 30 separate GNU time memory samples.
Execution order alternated Python/Rust each round. p95 uses nearest rank.
First observations, all warmups, failures and outliers are retained separately;
there were zero measurement failures and no discarded outliers.

**This is a warm-filesystem, bytecode-disabled clean-install scenario.**
The newly installed Python environment contained zero `.pyc` files, and
`PYTHONDONTWRITEBYTECODE=1` remained set. Do not extrapolate these figures to a
precompiled Python installation, a cold cache, an authenticated user's
configuration, or service-capable Rust features. Help content and versions
differ between products and were reviewed independently, not forced to have
identical bytes. Python version includes its release-notes URL and CRLF endings.

The host was Ubuntu 24.04.4 LTS, WSL2 Linux 6.18.33.2, x86_64, glibc 2.39,
Intel Xeon Platinum 8370C at 2.80 GHz, on native ext4. Cgroup readback enforced
two CPU quota, 6 GiB RAM, zero swap and 128 tasks; 16 logical CPUs were visible,
without fixed CPU affinity or governor control. Linux load averages before/after
were `0.46 0.45 0.28` / `1.31 0.79 0.43`; Windows host contention was not fully
controlled. Builds, installation, correctness checks and hashing warmed files
and page caches. First observed does not mean cold-cache startup.

## Measured distribution and runtime footprint

| Artifact | Logical regular-file bytes | MiB | Convention |
| --- | ---: | ---: | --- |
| Rust default release binary | 2,619,344 | 2.498 | Default unstripped release profile; OS libraries excluded |
| Python source-built wheel | 358,997 | 0.342 | Compressed wheel, not installed footprint |
| Python product only, installed | 1,172,237 | 1.118 | Product package, distribution metadata and entry point only |
| Python private venv and dependencies | 31,662,871 | 30.196 | 28 installed distributions; interpreter symlink not followed |
| Managed CPython base runtime | 100,134,334 | 95.496 | Runtime and standard library, separately reported |
| Python venv plus base runtime | 131,797,205 | 125.692 | Sum of disjoint trees, not incremental disk cost |

Tree sizes count logical regular-file bytes with unique inodes within each tree,
exclude directory blocks, and do not follow symlinks. The Python product-only
row sums its installed files separately. Allocated-byte counts and per-file
hashes are retained in the JSON report. Hardlinks can share external cache
storage; no exclusive disk-saving claim is made. Neither tree includes OS
shared-library storage.

Rust used rustc/Cargo 1.99.1 and
`cargo build --offline --locked --release -p fabric-cli --bin fab-rs --jobs 1`,
without profile, LTO, strip or feature overrides. Its 165.404-second build is
provenance, not a comparative startup/build speed metric. Binary SHA-256:
`960eaba2dc1619a38619ab3d11a6b8bdd7ab24d1751920b8f6a627610605fd77`.
Python interpreter SHA-256:
`bb4d05c1f88e8568985e8704afe1822e82700a54bad9317c05c8eb96d3186701`.
The dependency freeze, wheel hash, installed-file manifests, output contracts
and raw measurements are retained in the integration owner's dated report.

The canonical milestone at the measured Rust source was 43/52 bounded offline
happy paths (82.69%) versus Python's 52-leaf source inventory. This is not full
behavior equivalence or timings for 43 commands. Existing correctness evidence
reported 770 workspace non-doc tests and nine doctests, with overlapping focused
default/native suites; those are not speed metrics or additional unique cases.
Offline LS/GET/job handler latency was **not measured**: a matched production
timing boundary was not established, and unlike Rust probes/Python source
comparators must not be compared as equivalent work. Live end-to-end,
authentication, refresh and native Windows/macOS measurements remain unmeasured.

The actual measurement receipt is run `609436b18a4b4c1cacdc510909b4a138`,
SHA-256 `2c4e8050464db0126698f7a16f22b86577dcbe8a37765aeb2fa1a02dafa14e25`.
The integration-owner handoff retains `performance-table.md`,
`performance-table.json`, `perf-startup-receipt.json`, provenance, dependency
freeze, independent correctness review, before/after file manifests and capped
wrapper/cleanup receipts. One preparation expectation error (missing Python's
second version line/CRLF) was corrected from pinned source before measurement;
its failed receipt is retained. No product/oracle change or measurement retry
was used to hide a failure.

The committed request below remains a non-executable template. Future runs
require fresh output/provenance checks and an explicit finite grant; these
measurements do not grant continued execution. Correctness and command delivery
take priority; optimization remains a separate evidence-led decision.

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
