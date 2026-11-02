<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded WSL fast validation

This opt-in launcher runs one explicit, owner-authorized request in an existing
Ubuntu WSL2 distribution. It does not configure WSL, Docker, authentication, CI,
or the existing Windows validation guard. Linux results do not prove Windows
broker or macOS behavior and do not replace the required native CI matrix.

## Prerequisites and containment

Use PowerShell 7.4 or later and an existing Ubuntu installation with systemd,
cgroups v2, `systemd-run`, `sudo -n`, Python 3, Git, and a C compiler. Permission
to create a transient system service running as the default WSL user is required.
The launcher never requests an interactive password or changes a system service.

Create a **task-owned native Linux filesystem** root named
`/home/<user>/fabric-wsl-validator-<task>`. Do not build under `/mnt/c`.
Keep `cargo/`, `rustup/`, `python/`, `oracle/`, checkouts, and targets inside it.
Do not reuse the user's global Cargo, Python, profile, or authentication state.
The launcher clears the child environment and supplies an isolated runtime home.

The initial validated toolchain is Rust/Cargo **1.99.1**, rustfmt **1.10.0**,
CPython **3.13.15** (Unicode **15.1.0**), and GCC **13.3.0**. The private oracle
environment contains only `jmespath==1.0.1` and `cachetools==5.5.2`, matching
`rust/tests/compatibility/requirements.txt`. Python 3.12 is not interchangeable
with the pinned Unicode oracle. Rustup **1.29.2** and uv **0.13.5** were downloaded
from their official HTTPS release locations and checked against the corresponding
release SHA-256 files. uv verifies its pinned managed Python download. Retain the
download URLs, checksum files, binary hashes, timestamps, and actual version output
with the private setup receipts. Install Rust with `--no-modify-path`; never
replace system Python or edit a shell profile.

Each invocation holds `Local\FabricRustParityValidation` from Windows before
starting WSL and through transient-unit cleanup. A second Windows process proves
that the mutex is unavailable. Waiting is bounded to 1,800 seconds. The transient
unit enforces these **aggregate process-tree** limits, not just Cargo's own jobs:

| Limit | Setting |
| --- | --- |
| CPU | `CPUQuota=200%` |
| Memory | `MemoryMax=6G`, reduced to preserve 2 GiB available memory in both Windows and WSL |
| Swap | `MemorySwapMax=0` |
| Tasks/threads | `TasksMax=128` |
| Runtime | Request-specific, at most 600 seconds |
| Termination | `KillMode=control-group`, five-second stop timeout |
| Cargo/test concurrency | `CARGO_BUILD_JOBS=1`, `RUST_TEST_THREADS=1` |

The Linux runner reads the actual `cpu.max`, `memory.max`, `memory.swap.max`,
and `pids.max` before executing the request and rejects unbounded/excessive values.
It also records the systemd unit's actual runtime and termination properties.
A private nonblocking file lock prevents concurrent use of the same root.
The initial active budget is 3,600 seconds. A request reserves its full runtime
before execution; completed commands refund unused time, while interrupted
commands remain charged. Do not silently reset the budget to extend authorization.

Ordinary Windows Python processes do **not** automatically veto this explicitly
authorized, hard-capped WSL lane. This is not a change to the Windows v3 classifier,
a PID exemption, or permission to run other heavy validation concurrently.
Never kill or suspend foreign processes. Retain receipts even on failure.

## Request schema v1

Create one JSON file per command outside the repository. For example:

```json
{
  "schema_version": 1,
  "timeout_seconds": 600,
  "source_sha": "91e7523ea978c67ca183abaea04500e323c27eb3",
  "working_directory": "checkouts/91e7523ea978c67ca183abaea04500e323c27eb3/rust",
  "argv": ["cargo", "check", "--offline", "--locked", "-p", "fabric-cli", "--all-targets"]
}
```

The `source_sha` must be a full immutable commit. The runner verifies exact HEAD
and a clean checkout before invoking the explicit argument array. Import the
Python oracle commit `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` into the same
object database because source comparisons use Git objects. Public exact-commit
fetches or an isolated export of the authorized objects are sufficient; never
mount credential caches or modify a peer worktree.

`working_directory` is relative to the validator root and cannot escape it.
`argv` records the exact packages, test selectors, and feature graph. There is no
shell expansion. `source_sha` may be omitted only for explicitly authorized
setup/helper requests. Product requests get a private
`CARGO_TARGET_DIR=<root>/targets/<source_sha>`; registry and toolchain caches remain
warm within the task root. Never share writable targets with other lanes.
For a corrected immutable checkpoint in the same family, an explicitly recorded
`cp -a --reflink=auto` snapshot of a completed private target can seed the new
target before use. This retains the previous receipt's paths and avoids sharing
mutable output. Keep feature graphs explicit; a seeded cache is not evidence that
the new source compiled.

From Windows, invoke the checked-in wrapper:

```powershell
.\rust\scripts\Invoke-WslValidation.ps1 `
    -ValidatorRoot '/home/<user>/fabric-wsl-validator-<task>' `
    -RequestPath 'C:\validator-receipts\check.json' `
    -LogDirectory 'C:\validator-receipts'
```

Use the session's assigned absolute artifact directory for actual receipts.
The wrapper copies only the request and runner from Windows, then executes
inside native Linux storage. Windows stdout/stderr, request hash, unit/PID,
mutex proof, exit code, elapsed time, and cleanup outcome are retained under a
unique run ID. Linux command receipts are in `<root>/logs/<run-id>.json`.
Every failed command stops the caller's sequence; inspect its exact log before
requesting another check. PowerShell callers that sequence multiple invocations
must use `$ErrorActionPreference = 'Stop'`.

## Smallest useful sequence

Run `cargo fmt --all --check`, then
`cargo check --offline --locked -p fabric-cli --all-targets` **before** tests or a
full native CI batch. If a real offline cache miss occurs during initial setup,
the owner-authorized `cargo fetch --locked` restores dependencies once without
changing manifests or locks. Keep all later checks offline.

Run the family's actual integration-test targets in one Cargo invocation, with
`-- --test-threads=1`. For job reads, select `job_arguments`, `job_read_session`,
and `job_cli_contract`, plus the relevant parser/read-session targets. Check
`--features native-read` explicitly before testing that feature; default builds
alone do not compile its opt-in paths.

Build `job_read_session_probe` with the same feature graph and invoke the **actual**
pinned private Python executable:

```text
<root>/oracle/bin/python -B tests/compatibility/job_reads/compare.py --probe <root>/targets/<sha>/debug/examples/job_read_session_probe
```

Success must explicitly report 64 exact native comparisons and four declared API
gaps failing nonzero. A Python-only observation run without `--probe` is not
native command evidence. Also exercise the real `fab-rs` subprocess contracts;
injected synthetic credentials/HTTP must remain offline. Never introduce a fake
CLI fixture flag or regenerate oracle expectations to make a check pass.

Record cold dependency restore, first compile/test, and a harmless repeated warm
check separately. A failed all-target check is useful feedback, not readiness:
report the exact SHA, command, file/line, exit code, duration, and log to the
owning source author. Do not fix another author's product files in the validator
lane. Keep the same environment warm while awaiting their corrected SHA.
