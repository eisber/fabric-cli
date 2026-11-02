<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Clean-machine acceptance, install and recovery

**Runbook only: no clean-machine, live-service, signing or release pass is
recorded here.** Owner approval and the [decision register](release-readiness.md)
gate actual installation, hosted workflow setup, live authentication and
promotion. These instructions are not permission to perform those operations.

## Establish immutable inputs

1. Select the full reviewed source/workflow commit, target, variant and
   toolchain. Obtain the actual provider run/job URL, run attempt, artifact ID,
   artifact URL, archive length and archive SHA256 from authenticated provider
   receipts. Keep these receipts outside the candidate bundle. The artifact
   archive digest is distinct from the executable hash and the individual file
   hashes. Neither is a publisher signature.
2. Obtain the verifier at a separately reviewed full commit. Preserve its script
   hash and the verification-host Python version. Download only after the owner
   authorizes provider access. Verify the archive bytes against the trusted
   provider digest before extraction. No downloader or installer is added here.
3. On a disposable unprivileged host, inspect archive member paths before
   extraction using an approved archive tool. Reject rooted paths, parent
   traversal, links, device/stream names and unexpected members; do not extract
   over an existing directory. Extract to a new local directory under exclusive
   custody, not a network share or shared writable location. The verifier
   verifies an extracted directory, not archive extraction safety.
4. Create `expected.json` **outside** that directory using authenticated build
   receipts and reviewed source inputs. Never bootstrap trust by copying all
   expectations from the downloaded bundle. If independent pins are missing,
   stop and obtain the build/provider evidence. Required fields follow; all
   `REPLACE` values and zero length are deliberately invalid placeholders.

```json
{
  "source_commit": "REPLACE_WITH_FULL_40_HEX_SOURCE_COMMIT",
  "workflow_ref": "eisber/fabric-cli/.github/workflows/rust-preview-build.yml@refs/heads/feature/rust",
  "target": "x86_64-pc-windows-msvc",
  "variant": "default",
  "toolchain": "1.99.1",
  "rustc_sha256": "REPLACE_WITH_64_LOWERCASE_HEX",
  "cargo_sha256": "REPLACE_WITH_64_LOWERCASE_HEX",
  "lockfile_sha256": "REPLACE_WITH_64_LOWERCASE_HEX",
  "manifest_sha256": "REPLACE_WITH_64_LOWERCASE_HEX",
  "binary_sha256": "REPLACE_WITH_64_LOWERCASE_HEX",
  "binary_bytes": 0,
  "run_id": "REPLACE_WITH_PROVIDER_RUN_ID",
  "run_attempt": "REPLACE_WITH_PROVIDER_ATTEMPT",
  "version": "0.1.0"
}
```

Compiler pins hash exact `manifest.build.rustc` / `cargo` UTF-8 string bytes from
the independently trusted build record, including internal newlines, excluding
any trailing newline trimmed by the workflow. Hash the raw original manifest and
lockfile bytes, not reserialized JSON or line-ending-normalized copies. Current
workflow upload summaries do not provide every independent pin: obtain the
original metadata through an authenticated build record or separately reviewed
artifact provenance. Missing pins are an actionable evidence blocker, not a
reason to trust a replacement manifest.

## Offline verification and copied-binary smoke

The following PowerShell examples are commands for a later authorized
validator, not a record that they ran against a product bundle:

```powershell
python .\rust\scripts\release_verify.py `
  --bundle 'C:\Acceptance\candidate' `
  --expect 'C:\Acceptance\expected.json' `
  1> 'C:\Acceptance\verify.json' 2> 'C:\Acceptance\verify-error.json'
if ($LASTEXITCODE -ne 0) { throw 'Bundle verification failed; preserve the diagnostic' }

# Execute only an authorized independently trusted candidate on its native host.
python .\rust\scripts\release_verify.py `
  --bundle 'C:\Acceptance\candidate' `
  --expect 'C:\Acceptance\expected.json' --smoke `
  1> 'C:\Acceptance\smoke.json' 2> 'C:\Acceptance\smoke-error.json'
if ($LASTEXITCODE -ne 0) { throw 'Copied executable smoke failed; do not promote' }
```

For the other supported native targets use absolute native input paths with
the same arguments on their matching hosts. The tool does not cross-execute
artifacts. Smoke copies to a temporary path and sets executable permission on
the Unix copy only, after verifying the original bytes. Success emits stream
hashes/exits for eight actual cases; failure emits only a static diagnostic
or numeric OS error, never child stdout/stderr, rejected values, or environment.
Preserve command, exit, UTC timestamp and receipt hash. Do not infer success
from a created redirection file, blank stderr or a planned command.

To exercise only the helper on a development machine:

```powershell
python -m unittest discover -s rust\scripts -p test_release_verify.py -v
```

Those synthetic tests, including their real test-interpreter subprocess checks,
are **not** a fab executable, downloaded-binary or clean-machine receipt.

## Clean-machine matrix and acceptance checklist

Use a fresh disposable image, not a developer profile with cleared environment.
The verifier host can have Python, but the **product execution host** must have
no Python, Rust/Cargo, Azure CLI or Azure Developer CLI. For a strict no-Python
machine, run verification elsewhere, transfer the exact verified bytes and
rehash them with approved native OS tooling; execute the eight cases below
using an absolute product path, null stdin, scrubbed environment and explicit
ten-second per-case timeouts in the approved native harness. Record that
harness's full revision and environment allowlist. Such a native clean-host
harness/receipt is still required; this Python tool alone cannot prove absence
of Python. Do not install Python on the clean host to make that gate pass.

| Check | Required evidence and pass condition |
| --- | --- |
| Host identity | Actual OS name/build, architecture, VM/image ID, update state, non-admin test identity, minimum-version candidate, UTC timestamp and platform validator. Match artifact target; no architecture emulation as native proof. |
| Runtime inventory | Windows CRT/system components and native-read WinRT/COM/WAM availability, Linux glibc/system library requirements, or macOS version/runtime. Use approved OS inspection tools; record actual linked dependencies. Do not install a missing runtime silently or infer it from hosted CI. |
| Tool independence | Approved software inventory proves Python/Cargo/az/azd absent on the product host. Use an isolated empty PATH plus approved test-only trap tooling to prove no tool invocation; preserve trap counters/process observations, not credentials. Traps must not be production fallbacks. |
| Offline execution | Block outbound traffic at the disposable host boundary; no login/tenant/selectors. Record actual firewall/network isolation evidence. Environment scrubbing is not network isolation. |
| Help | `--help`, `-h`: exit 0, stdout contains `Usage: fab-rs`, `--help` and `--version`, empty stderr. |
| Version | `--version`, `-V`: exit 0, exact `fab-rs <pinned-version>` followed by newline, empty stderr. Preserve captured stream bytes/hashes; do not normalize away unexpected output. |
| Invalid arguments | No args, `unknown-command`, `--not-a-flag`, and `--client-secret must-not-be-echoed`: exit 2, empty stdout, newline-terminated exact parsed JSON envelope documented in compatibility; sentinel never echoed. No auth or service invocation. |
| Bounds and effects | Ten-second timeout per case, 1 MiB output acceptance limit per stream, no child environment/log dump; copied binary hash/length unchanged. Record unexpected files/processes and cleanup. Snapshot before/after profile/config locations; no real user profile or OS broker mutation should occur in these cases. |
| Evidence and approval | Verify archive/provider pins and executable hashes at every transfer; retain actual commands, exit codes, stream hashes, per-host outcomes and failures. Missing/failed/skipped matrix rows remain explicit. Reviewer and owner approve exact digest and scope separately. |

Default previews are help/version/parser products, not complete live CLIs.
Windows native-read additionally compiles a bounded host and broker commands;
offline acceptance must not invoke them. MFA, Conditional Access, refresh,
secure-store failure, tenant/account isolation, proxies/trust and authorized
service behavior remain separate team-reviewed live gates under
[identity requirements](identity-requirements.md). No signing-store changes,
consent, app registration or tenant operation is authorized by this checklist.

## Controlled per-user install, update and rollback

These are a proposed manual preview procedure, pending R1-R8 approval; no
installer, updater, PATH mutation or privileged package service is implemented.

1. Retain the approved old executable, hash, receipt and configuration snapshot
   before changing anything. Confirm it is not security-revoked and that the
   owner-selected rollback version supports the current state/schema. If
   migration compatibility is unknown, stop rather than assume downgrade safety.
2. Verify and smoke the new candidate away from the active installation.
   Stage its five files under a new per-user immutable
   `<source>-<target>-<variant>` directory. Hash staged bytes again. Do not
   overwrite a running executable, change global PATH, replace `fab`, or mix
   default/native-read files. Do not remove quarantine/SmartScreen restrictions
   or weaken enterprise execution policy; escalate trust failures.
3. With explicit owner approval, switch only the caller's documented absolute
   executable path/launcher to the accepted new directory. Record old/new paths,
   binary hashes, timestamp and approver. Re-run the accepted offline checks on
   the installed path, preserving its actual runtime failure if any.
4. On failure, stop new invocations and, with approval, restore the previous
   recorded launcher/path to the still-retained verified version. Rehash and
   smoke that version. Do not restore a revoked binary. Restore configuration
   only from an approved snapshot with known schema compatibility. Preserve
   failed-version evidence before targeted removal; do not delete broad roots.
5. Uninstall removes only specifically recorded installed files and launchers
   after confirming no process uses them. Do not clear existing `fab` settings,
   OS accounts, broker caches, trust stores or unrelated tools. Retain receipts
   and prior versions according to the owner-approved policy, not Actions'
   seven-day artifact expiry.

Installation rollback restores software selection, **not remote data or broker
effects**. No real signing keys, credentials or profile contents belong in
release bundles or acceptance logs.

## Destructive-effects recovery template

Fill this in before any separately authorized mutation/live acceptance. A blank
template is a missing readiness gate, not evidence that recovery works.

| Field | Required record |
| --- | --- |
| Authorization and scope | Approver, UTC window, exact executable hash/source/variant, command scope, isolated environment, target resource IDs in access-controlled evidence; never secrets. |
| Ownership | Operator, release/platform/identity DRI, service/data owner, security escalation route and rollback approver. |
| Precondition and backup | Exact local paths/config schema, service state/version/ETag, backup/snapshot IDs, restore procedure, retention and proof backup is readable. No ambient account discovery. |
| Expected effects | Local files/settings, broker/session effects, service creates/updates/deletes, irreversible operations, data-loss boundaries and explicitly excluded effects. |
| Detection and stop | Expected outputs, failure/time limits, observation channel, stop conditions, and how to prevent new work. Accepted/queued is not completed; cancellation is not rollback. |
| Recovery steps | Local launcher rollback versus configuration restore versus service/data restoration; authorization and idempotency/reconciliation keys per step. Do not blindly replay a timed-out/uncertain mutation. |
| Credential/session recovery | App-scoped broker actions only if separately authorized; do not delete OS accounts or assume local logout revokes remote sessions. Secure-store failure must not trigger plaintext or developer-tool fallback. |
| Verification | Actual restore commands/exits, resulting resource versions/data checks, executable hashes and independent owner confirmation. Preserve failures and residual effects. |
| Closeout | UTC timeline, protected evidence refs, remaining damage/manual work, support owner and explicit acceptance decision. No fabricated live or recovery receipts. |
