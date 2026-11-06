<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline native engineering acceptance

`rust/scripts/native_acceptance.py` is development-only Python 3.10+ standard
library tooling. It executes **already built, explicitly trusted local
binaries**. It does not build, restore dependencies, download, sign, publish,
dispatch, discover credentials, or call a provider. Nothing here is installed
with the Rust CLI.

The newly authored `terminal-inputs.json` and `synthetic_child.py` are synthetic,
Microsoft-copyrighted MIT-licensed fixtures, not captured acceptance evidence.
Neither fixture contains an identity, provider configuration, or real secret.
The child is used only by the Python unit tests, never as a replacement CLI.

## Required controller inputs

Freeze the combined candidate first. Existing Windows results and Linux/macOS
cross-compilation results are not receipts for this matrix. A dirty engineering
candidate is supported only through its complete exact dirty-file manifest;
any source or binary drift rejects the run. A later commit needs new bindings.

The controller must provide:

1. A registered native Windows, macOS, or Linux host and independently reviewed
   host-evidence record. Confirm actual OS/build, architecture, toolchain,
   exclusive file custody, unprivileged identity, no provider credentials
   accessible to the execution identity, and enforced outbound isolation.
   Environment scrubbing is **not** a network or hostile-code sandbox.
2. Explicit authorization to execute the exact reviewed harness and binaries,
   plus a controller lease lasting longer than the whole-run budget plus five
   seconds. Use one shared approved artifact root. On Windows the harness
   acquires `Local\FabricRustParityValidation` without waiting; the controller
   must release its own ownership before handing over the slot. An occupied or
   abandoned mutex is rejected. POSIX uses nonblocking `flock` on
   `<artifact-root>/native-acceptance.lock`; the controller must coordinate
   other validators against the same lease/lock policy. No lease is inferred.
3. Absolute paths, lengths, SHA-256 digests, source-snapshot digest, exact
   requested and expanded package features, disabled defaults, target,
   Rust/Cargo identities, actual build argv and protected custody reference
   for each artifact. Build custody is external evidence, not something a
   matching hash or user-written JSON proves. Also pin the absolute Git
   executable and the resolved running Python executable independently.
   Both digests are checked before source inspection and after the matrix;
   neither tool is selected through PATH. These pins do not certify every
   loaded interpreter library or constitute a hostile-code sandbox.
4. The small projection described below from the existing offline locked Cargo
   metadata selection, not another hardcoded workspace/package inventory.
   Obtain exact binary filenames from Cargo's `compiler-artifact` messages
   retained by the authorized build controller. Never select `target` files
   by a wildcard or assume a previous output belongs to this candidate.
5. Controller-pinned SHA-256 digests for request, host and lease JSON, supplied
   independently of the files. Each JSON input remains limited to **64 KiB**,
   rejects duplicate keys, unknown request fields and nonfinite numbers.
   All nonrepository files, homes, test scratch and receipts stay beneath the
   approved artifact root. The root must exist, be outside the candidate,
   local, link/reparse-free and exclusively controlled; no default temp root is
   used. Each run ID creates a new directory and cannot overwrite a prior run.

The verifier itself must be obtained at a separately reviewed revision. Do not
execute contributor-controlled Python under provider credentials and then rely
on its environment allowlist to make that safe. Use an already isolated
credential-free execution context. Registered-host and isolation references
are externally checked attestations: this tool cannot query or authenticate
the native-host registry.

## Capture exact source and prepare records

The source-only command invokes only the absolute Git executable, with hooks/
fsmonitor execution avoided. It does not execute candidate code:

```powershell
& $TrustedPython -B $TrustedHarness --artifact-root $ApprovedRoot snapshot `
  --candidate-root $CandidateRoot --git $AbsoluteGit `
  --git-sha256 $GitSha256 --python-sha256 $PythonSha256 `
  > (Join-Path $ApprovedRoot 'source-snapshot.json')
if ($LASTEXITCODE -ne 0) { throw 'Source capture failed' }
```

`source-snapshot.json` contains `snapshot` and its canonical `sha256`.
The snapshot contains `head` (full commit), `status_sha256` (raw Git status
bytes), and `dirty_files` with `path`, two-character `status`, and raw-file
`sha256` or null for a deletion. Tracked, staged and untracked nonignored files
are included. Unsupported dirty entries, linked files and status/output larger
than 64 KiB fail closed. It is not a full clean-source archive or signature.
The request's `source` is the snapshot object, not the surrounding wrapper.
`candidate_root` must be the exact Git worktree top level, never its `rust`
subdirectory. Git-root-relative names are hashed beneath that root. A missing
file is represented as null only for an actual porcelain deletion state;
missing modified/untracked entries reject the snapshot instead.

Generate the metadata projection in the existing **authorized build/selection
stage**, after `cargo metadata --offline --locked --no-deps --format-version 1`.
Keep `version`, `workspace_members`, and `packages`; retain only packages named
in the actual artifact records. Each retained package must keep at least `id`,
`name`, `features`, and `targets` from the original metadata. Preserve the
original metadata receipt/digest separately. The projection is bounded to
64 KiB rather than enlarging limits for the whole workspace. Only local feature
edges from the metadata graph are expanded by this harness; Cargo's artifact
message supplies `resolved_features`, which must agree. Dependency feature
closure and build correctness remain the build controller's checks.

### Request schema 1

The request has **exactly** these fields:

| Field | Value |
| --- | --- |
| `schema` | Integer `1` |
| `run_id` | New lowercase letters/digits/hyphens, 1–48 characters |
| `candidate_root`, `git` | Absolute existing candidate root and Git executable |
| `git_sha256`, `python_sha256` | Independent lowercase SHA-256 pins for Git and the resolved running interpreter |
| `source` | Exact snapshot object described above |
| `version` | Exact CLI version, for example `0.1.0` |
| `cargo_metadata`, `cargo_metadata_sha256` | Absolute projection path and independent lowercase SHA-256 |
| `per_command_seconds` | Integer 1–60; normally 10 for offline smoke; exactly 60 when bounded-helper tests are selected |
| `whole_run_seconds` | Integer 1–600, including preflight, inventory and final checks; at least 180 for bounded-helper tests |
| `binaries` | Up to five unique artifact records; an absent role remains `not_run` |

Each artifact record has **exactly**: `role`, absolute `path`, `sha256`,
`bytes`, `package`, `target_name`, `target`, `features`, `resolved_features`,
`default_features` (false), `source_snapshot_sha256`, `rustc`, `cargo`,
`build_command` (actual argv array, retained only, never executed),
`custody_ref`, and `auxiliary_binaries`. The latter is an array of
`{path, sha256, bytes}` records for explicitly pinned additional executables.
For `bounded-helper-tests`, each auxiliary also requires `target_name`, as
described below.
No field may contain credentials, private signing references or secret-bearing
build arguments.

| Role | Package / target name | Exact requested `features`, sorted |
| --- | --- | --- |
| `default` | `fabric-cli` / `fab-rs` | `[]` |
| `optional-combined` | `fabric-cli` / `fab-rs` | `["headless","native-read","repl","self-update"]` |
| `headless-tests` | `fabric-identity` / `headless` | `["headless"]` |
| `updater-tests` | `fabric-cli-update` / `contracts` | `["running-replacement"]` |
| `bounded-helper-tests` | `fabric-cli-update` / `native_fixture` | `["fixture-harness"]` |

`resolved_features` is the sorted complete **package-local** feature list in
the matching Cargo `compiler-artifact` message, not the workspace's global
feature union. Do not reuse a workspace-wide feature-unified test executable
for one of these exact isolated variants. This harness never locates auxiliary
executables through PATH.

The source-frozen bounded-helper native suite is selectable only through
`bounded-helper-tests`, with **both** `fabric-update-lifecycle-fixture` and
`fabric-update-helper-fixture` compiler artifacts pinned in
`auxiliary_binaries`. Their `target_name` fields must be those exact distinct
names; the controller verifies their embedded Cargo paths from the actual
build receipt. Metadata must contain both binary targets. The old
`fabric-update-fixture` self-replace target and old `updater-native-tests`
role are rejected as substitutes.

The harness selects nine exact public libtests, including two successive
upgrades, late-error lifetime protection, post-commit receipt failure,
rename/helper-death reconciliation, foreign-file/custody/unsigned rejection,
active-helper/major-upgrade gates, cancellation/tampering, independent signature
rejection and the real parent-lifetime timeout. The last case waits roughly
32 seconds: it requires the explicit 60-second command budget and at least
180 seconds overall. No sleep shortening or synthetic timeout success is used.
`update/bounded-helper-native` remains blocked unless all nine exact native
cases pass and final source/artifact rechecks succeed. A local fixture pass is
not production signing, installed-product support or durable health rollback.

Source readiness is **not execution evidence**. The build, host, review and
lease prerequisites still apply. The harness does not invoke helper `--apply`
or `--reconcile` itself and never invents request/trust/receipt JSON. The
feature-gated native tests own their synthetic protocol fixtures. The production
`fab-rs-update-helper` artifact (package `fabric-cli-update`, production feature
`bounded-helper`) is not substituted for either test fixture or certified by
these tests. In particular, fixture results do not prove production-helper
packaging, publisher signatures, live updates, or health rollback.

Targets are native x64/ARM64 Windows MSVC, GNU Linux, or macOS; other targets
fail closed. ELF/PE/Mach-O machine headers and native interpreter architecture
are checked. Universal Mach-O binaries are not accepted by this bounded
single-target harness. This is not a minimum-OS or libc dependency check.

### Host evidence schema 1

The record has exactly: `schema` (1), `registered_host_ref`, `system`
(`Windows`, `Linux` or `Darwin`), `architecture` (`x86_64` or `aarch64`),
`platform` (exact `platform.platform()` on that host), `native` (true),
`emulated` (false), `provider_credentials_absent` (true), `outbound_blocked`
(true), and `isolation_evidence_ref`. References must identify actual
controller-reviewed evidence, not these fixture files or a hosted runner label.
Interpreters running through Windows architecture emulation, Rosetta or a
mismatched Linux ELF architecture are rejected.

### Lease schema 1

The record has exactly: `schema` (1), `controller_ref`, matching `run_id`,
`artifact_root` (exact resolved approved root), `expires_utc` (timezone-aware
ISO timestamp), `policy` (`Local\FabricRustParityValidation`, JSON-escaped),
and `trusted_binary_execution` (true).

## Execute only after the controller grants the native slot

These are proposed controller hooks, **not dispatches or execution receipts**.
The controller must substitute independently trusted absolute paths and pins:

```powershell
& $TrustedPython -B $TrustedHarness --artifact-root $ApprovedRoot run `
  --request $Request --request-sha256 $RequestSha256 `
  --host $HostEvidence --host-sha256 $HostEvidenceSha256 `
  --lease $Lease --lease-sha256 $LeaseSha256 --execute-trusted-candidate
$AcceptanceExit = $LASTEXITCODE
if ($AcceptanceExit -notin @(1, 2)) { throw 'Unexpected harness status; inspect receipt' }
exit $AcceptanceExit
```

For CI, a controller can add this exact step **only in an approved
credential-free native runner job**, after frozen-artifact transfer and lease
handoff. Environment values below are supplied by the controller, not PR text:

```yaml
- name: Collect exact-candidate offline native evidence
  shell: pwsh
  run: |
    & $env:TRUSTED_PYTHON -B $env:TRUSTED_NATIVE_HARNESS `
      --artifact-root $env:APPROVED_NATIVE_ROOT run `
      --request $env:NATIVE_REQUEST --request-sha256 $env:NATIVE_REQUEST_SHA256 `
      --host $env:NATIVE_HOST --host-sha256 $env:NATIVE_HOST_SHA256 `
      --lease $env:NATIVE_LEASE --lease-sha256 $env:NATIVE_LEASE_SHA256 `
      --execute-trusted-candidate
    exit $LASTEXITCODE
```

Exit **1** means failure/rejected input. Exit **2** means retained offline
evidence with unresolved acceptance gates, **not a passing release check**;
there is deliberately no green exit-0 `run` fallback. These hooks propagate the
nonzero status: retain/upload receipts in an independently configured
always-run evidence step, not by converting the command into a passing
acceptance gate. A separate controller must inspect every required case and
keep native/manual/live/signing gates unsatisfied. It must not translate exit
2, report existence or `offline_result: partial` into green acceptance.
The read-only `snapshot` command alone uses exit 0.

## What actually runs

- Both exact CLI variants: long/short help and version, redaction-safe invalid
  arguments, and redirected scripted stdin. Scripted `quit` must **not** turn
  redirected streams into a terminal or run a command. Error output is checked
  against the known complete JSON envelope; unknown output schemas fail.
- Native Unix headless integration tests: FIFO shutdown, AKS leaf/`..data`
  rotation, and pinned-parent retarget rejection. Windows leaves these rows
  unrun; a Windows pass cannot satisfy a Unix runtime requirement.
- Updater contracts: actual nonrunning local-file replacement and retained
  rollback bytes, failure retention, foreign lock/stage/rollback protection,
  and cancellation.
  Existing tests use synthetic signature material and injected local bytes,
  not trusted production signing keys, release downloads or installed `fab`.
  This is not production installation or durable health rollback acceptance.
  The separately selected `bounded-helper-tests` exercises the nine source-frozen
  native fixture contracts, with both dedicated auxiliaries pinned. The old
  self-replace fixture is not selected. Any implementation/helper lifecycle
  changes require a reviewed protocol and fresh exact-binary tests.
- POSIX only: actual kernel PTY startup, observed raw/noncanonical/no-echo mode,
  quit, EOF, lexical error and Ctrl+C followed by quit, and exact terminal
  attribute restoration. Source-controlled input drives the real CLI/editor,
  not a fake `Terminal`/`ShellInput`. The driver answers cursor-position
  protocol queries at a fixed initial position; this checks lifecycle, **not**
  layout, editing/rendering correctness or real human terminal UX.
  Windows standard-library PTY is unavailable: all corresponding cases remain
  `requires_native_PTY_or_manual_terminal`, with no dependency installation or
  dummy-editor substitute.

Before each exact libtest case the harness verifies registration using `--list`,
then requires a named test result and exactly one passed, zero failed/ignored
tests. A filtered/empty/ignored success is not a passing case. A missing test
target/feature is not replaced by a synthetic success.
Duplicate named results, multiple summaries, measured tests and unknown summary
formats also fail. Cargo metadata must identify the CLI target as a binary and
the selected contract target as a test, not merely contain a matching name.

Output capture remains bounded to 64 KiB per stream; only counts/hashes, fixed
diagnostic categories, argv, exit, duration, timeout and cleanup metadata are
retained. Raw child output is not written to receipts. Deadlines are enforced
per command and across the run, with bounded cleanup overhead (up to approximately
three seconds). A full stdin pipe cannot block the controller deadline.
PTY cursor queries enqueue bounded replies and use nonblocking writes only;
a raw child flooding `ESC[6n` without reading input cannot block that thread.
Receipts distinguish observed cursor queries from bytes actually written and
report queue overflow explicitly, rather than counting queued replies as sent.
Windows children start suspended, join an owned kill-on-close Job, then resume;
cleanup terminates that Job and observes zero active processes. POSIX commands
own a new session/process group, which is killed and checked on completion,
including on normal direct-child exit. Unconfirmed cleanup fails the case.
After terminal capture ends, cleanup drains queued PTY bytes while waiting for
the owned child to be reaped. This avoids a full Darwin PTY blocking the killed
session leader's final close. Drained cleanup bytes do not extend captured output
or its 64-KiB limit; the existing one-second wait and half-second fallback budgets
remain unchanged. Child reaping and process-group absence are still required.
Terminal-state observation is separate: a hung-up terminal may no longer expose
its settings after the child has been reaped. Its observation error is retained,
and `termios_restored` remains false. This does not invalidate confirmed process
and descriptor cleanup, but it cannot satisfy terminal-restoration acceptance.
Spawn failures retain a static execution diagnostic and `no_child_created`;
failed cleanup retains fixed failure codes, never a successful cleanup claim.
This is a trusted-process lifecycle boundary, not containment against malicious
code that deliberately escapes a POSIX session or changes filesystem custody.

Each case is saved immediately as `case-NNN.json`; the final `report.json`
records real host, Python/harness identity, source manifest, toolchain/artifact
records, commands, exits, failures, timeouts, cleanup and explicitly blocked/
unrun gates. Source and all named binaries are rehashed after the matrix.
Git, Python, the harness script and terminal fixture are rechecked as well.
Preserve partial receipts on failure; never reuse their run directory.
The harness does not recursively delete product-test artifacts: retained
fixture effects require operator review, and cleanup is not data rollback.

## Small development validation

Ask the shared-host controller for a grant before spawning these runner tests:

```powershell
# The controller checks $TrustedPython against $PythonSha256 first and grants
# this exact command. Run from an approved artifact directory, not the source.
Set-Location $ApprovedUnitRoot
$UnitRunner = @'
import os
import sys
import unittest

suite = unittest.defaultTestLoader.discover(sys.argv[1], pattern="test_native_acceptance.py")
if suite.countTestCases() != 32:
    raise SystemExit("unit_inventory_mismatch")
result = unittest.TextTestRunner(verbosity=2).run(suite)
expected_skips = [
    ("test_native_acceptance.FilesystemTests.test_raw_pty_cursor_flood_without_input_reads_is_bounded_and_reaped",
     "requires_native_POSIX_PTY")
] if os.name == "nt" else []
actual_skips = [(test.id(), reason) for test, reason in result.skipped]
raise SystemExit(0 if result.wasSuccessful() and result.testsRun == 32
                 and not result.expectedFailures and actual_skips == expected_skips else 1)
'@
& $TrustedPython -I -B -c $UnitRunner (Join-Path $CandidateRoot 'rust\scripts')
exit $LASTEXITCODE
```

The exact 32-case count is the source inventory for this freeze, not a result.
The controller must retain actual discovered/run counts and exact skip reasons;
zero tests, unexpected selection, expected failures or a POSIX PTY skip fail
this command. A later source freeze requires a freshly reviewed inventory.

The unit suite uses current-directory-relative owned scratch, mocked host/build records
and short, real synthetic interpreter children. It exercises timeouts, full-pipe
stdin, output limits, redaction receipts, unknown schemas, missing features,
wrong platform/binary, partial coverage, candidate drift, dedicated helper
artifact/budget gates and zero-test success rejection. It does not build or
run Rust, acquire the shared native lease,
contact providers or certify a registered host. An actual native matrix still
requires all controller prerequisites above and independent evidence review.
The synthetic raw-child PTY flood must actually run on POSIX. Its explicit
Windows-only skip is not a runtime pass and cannot satisfy Linux/macOS coverage.
The separate five-case `test_native_acceptance_pty_cleanup.py` unit suite checks
draining, the unchanged wait deadline, fail-closed descriptor/selector errors, and
explicit unavailable terminal-state observation with
synthetic handles. The existing `oracle-tools` gate also runs it; these unit
checks do not replace any case in the frozen 32-case suite or native PTY proof.
The existing three-platform Rust CI also runs both exact engineering suites
before native compilation, retaining their run counts, skip reasons, and logs.
For the synthetic PTY cases it also retains the subprocess's bounded receipt
without changing its inputs, return value, or captured bytes.
The complete dependency-impact plan still runs afterward; an early unit pass
does not establish Rust, full-profile, or complete native acceptance.
These commands have not been executed for the current source freeze; a fresh
numbered controller grant is required before any unit/native process starts.
