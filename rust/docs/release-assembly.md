<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline native release assembly, version 1

**Engineering tooling, not GA, installation, signing or publication authority.**
The implementation was prepared over source
`558d0dda718328b760059fa7a5bf55e7491c2006` plus the controller's explicitly
uncommitted engineering candidate. No production signing/public-key profile or
publisher approval was supplied. Existing preview schema 1, verifier, tests and
goldens remain unchanged. The production origin selected by the owner is
`microsoft/fabric-cli`, product `fab-rs`, release tag `rust-vSEMVER`.

[`assemble_native_release.py`](../scripts/assemble_native_release.py) is a
Python 3.10+ standard-library **development host** tool. It never ships as a
Python bridge in the CLI. It consumes already held native CLI **and separately
built installed helper**, independently held controller expectations, and an
exact native build record. It does not discover credentials, build, download,
extract, sign, install, upload, create keys, probe a signing provider or change a
workflow. It cannot infer approval from a GitHub checksum, hosting or provenance.
SDK publication and workspace `publish = false` are unaffected.

## Trust and outputs

All paths passed to the tool must be absolute, with plain ancestor directories.
The controller must hold inputs, expectations, signer policy and approved
verifier under exclusive custody outside the package. Obtain the expectation
file's SHA256 through an independently trusted controller channel; do not copy
it from a bundle. That pin transitively binds source, binaries, build record,
minima, compiled public key, policy and attestation. No asset-supplied key or
verifier establishes trust. The embedded key cannot be extracted/proved from
arbitrary binary headers: separately trusted build custody must establish that
the recorded compiler inputs actually produced the exact artifact digests.

An exclusive **new** output root contains an atomically exposed `bundle`
directory. The tool uses `mkdir`/exclusive file creation, flushes staged files,
compares bytes, and renames its private staging directory. Existing output
roots are rejected, including empty directories. Cleanup removes only recorded
owned files; unexpected entries prevent directory removal and must be retained
for diagnosis. There is no recursive removal, installation or overwrite mode.
Directory renaming does not promise power-loss durability across every
filesystem. Filesystem ownership, Windows ACLs and package installation are
separate platform gates; `0700` is not proof of Windows ACL protection.

The flat package inventory is exactly:

- Raw CLI asset: `fab-rs-TARGET-VARIANT[.exe]`.
- Installed helper: `fab-rs-update-helper[.exe]`.
- `native-build-v1.json`: exact independently pinned original build bytes.
- `.fab-rs-install.json` for standalone custody, **or**
  `manager-custody-v1.json` for MSI/winget/Homebrew/deb/rpm custody.
- `update-TARGET-VARIANT.json`: exact updater protocol-1 bytes.
- `native-assembly-v1.json`: separate provenance/inventory manifest.
- `dirty-source-v1.json` only for an explicitly pinned dirty engineering tree.
- After successful signature verification only: both `.json.sig` files and
  `os-attestation-v1.json`.

Both binaries are mandatory even when the CLI build includes the updater as a
library dependency. No fixture helper, missing helper, alternate name, directory,
hard link, symlink, reparse point, extra entry, case-colliding name, traversal or
alternate stream is accepted. Metadata is capped at 1 MiB, updater metadata at
16 KiB, and each binary at 64 MiB (the current CLI updater's default asset limit).
Native executable header checks match the current bounded PE/ELF/Mach-O checks;
they are not execution, compatibility or OS-signature proof.

The output is a directory, **not an archive/MSI/PKG/deb/rpm builder**. A later
approved packager must safely archive/install its exact inventory, rename the
raw CLI to installed `fab-rs[.exe]`, preserve the helper's fixed sibling name,
reverify bytes and create/protect the receipt at the exact installed path.
Publishing six identically named raw helpers would collide: helpers travel
inside target/variant-specific packages, not as same-name GitHub release assets.
The raw CLI and matching `update-TARGET-VARIANT.json[.sig]` are distinct assets
within that version's official GitHub release.

### Results and exit codes

| Mode | Result and authority |
| --- | --- |
| `assemble` / `verify` | `unsigned-engineering-candidate`, exit 0 only for successful offline assembly/integrity verification. There is no signature or release claim, even when a policy hash is pinned. |
| `promote` | Requires an unsigned assembly, externally supplied detached signatures for both manifests, independently pinned OS-signature attestation, complete approved signer policy and explicit verifier execution consent. Produces a separate new output; never edits the unsigned candidate. |
| `verify-signed` | Requires the signed package inventory and repeats both cryptographic verifications with the same external pins. It does not trust a previous success report. |
| Missing signing/governance prerequisite | `blocked`, exit 2, no promoted output and no verifier execution. |
| Invalid, expired, changed, oversized or malformed input; invalid signature; absent execution consent | `rejected`, exit 1, static diagnostic only. Preserve failures. |

A successful signature operation reports `verified-signed-assembly`, **not**
GA or permission to publish. Every mode reports `publishable: false`. Platform
installation/protection, real native acceptance, command/identity readiness,
legal/notices and separately authorized publication remain external gates.
The tool cannot certify that a free-text approval reference represents an
authorized decision: its provenance and authenticity belong to the controller.
Synthetic references in unit tests are never production approvals.

## Closed controller expectation contract

The expectation file has **exactly** these fields. All `schema` fields in this
document are integer 1 unless expressly identified as receipt revision 2;
booleans are not integer substitutes. Unknown fields, duplicate JSON keys,
floating/nonfinite numbers, oversized inputs and unknown schemas are rejected.
Digests are 64 lowercase hex characters; source is a full 40-character SHA.

| Field | Required value |
| --- | --- |
| `schema`, `product`, `repository` | `1`, `"fab-rs"`, `"microsoft/fabric-cli"` |
| `source_commit` | Independently held full source/base SHA |
| `dirty_manifest_sha256` | Null for clean source; otherwise digest of the complete separately held engineering dirty-file manifest |
| `target` | One of the six current updater target triples listed below |
| `platform` | Exact object: `os`, `architecture`, `minimum_os`, `runtime_floor`, `evidence_ref` |
| `version`, `tag`, `channel` | Canonical SemVer, exact `"rust-v" + version`, `"stable"` or `"preview"` consistent with prerelease status |
| `explicit_features`, `variant` | Sorted unique explicit CLI feature list and exact compiled variant described below |
| `public_key_hex` | Independently pinned nonzero 32-byte Ed25519 public key, hex; null permits only blocked engineering assembly |
| `build_record_sha256` | Digest of exact native build-record bytes |
| `artifacts` | Exact object with `cli` and `helper`, each `{role, filename, sha256, bytes}`; roles must match object keys, byte lengths positive |
| `installation` | Exact object `{custody, root, windows_system_root}` |
| `issued_at`, `expires_at` | Positive integer UTC Unix seconds; expiry exclusive, at most seven days after issue. Verification requires explicit `--now` within that window. |
| `signing_policy_sha256` | Digest of separately approved signer policy, or null engineering blocker |
| `os_attestation_sha256` | Digest of independently authenticated native signature verification attestation, or null engineering blocker |

Supported protocol targets are `x86_64-pc-windows-msvc`,
`aarch64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` and
`aarch64-apple-darwin`. They bind respectively to OS `windows`, `linux`,
`macos` and architecture `x86_64` or `aarch64`.
This recognizes protocol identities, **not six platform acceptance passes**.
`minimum_os` and `runtime_floor` are mandatory owner declarations backed by
`evidence_ref`; no hosted CI label is interpreted as an OS minimum. GNU targets
do not establish glibc independence.

Explicit CLI features may be `headless`, `interactive`, `native-host`,
`native-read`, `repl`, `self-update`, `windows-broker`,
`certificate-store-windows`, `certificate-store-pkcs11` or
`certificate-store-macos`, with defaults disabled.
`headless` resolves `native-host`; `native-read` resolves `native-host` and
`windows-broker`. Broker/read builds require Windows. The record's
`resolved_features` means these package-local host identity features, **not**
the complete transitive dependency feature graph. For updater builds the exact
current `feature_variant()` order is `self-update`, then enabled `headless`,
`native-host`, `native-read`, `repl`, `windows-broker`, joined with `+`.
For non-updater engineering builds the same order omits `self-update`, or is
`default` when empty. These builds cannot pass the production update-feature
gate. Variant identities are limited to the updater's 64-byte bound.

Feature sets containing `interactive`, `certificate-store-pkcs11` or
`certificate-store-macos` use the version-2 identity: `f2-` followed by the
first 60 lowercase hexadecimal characters of SHA-256 over
`fabric-cli-feature-v2`, a NUL byte, and the sorted resolved package-local
feature names joined by NUL bytes. No trailing NUL is added. Forwarded
`native-host`, `headless` and `windows-broker` features participate when enabled;
all-feature builds therefore remain distinct within the bound. The Rust host
and assembler use the same encoding. Existing feature sets retain their legacy
identities, including the compact Windows-certificate identity. Native
certificate helper installation and independent path/digest trust remain
separate requirements; a CLI feature bit does not deploy or authorize a helper.

`custody` is one of `standalone`, `msi`, `winget`, `homebrew`, `deb`, `rpm`,
appropriate to the target OS. `root` is an explicit absolute planned native
installation directory. Windows requires a local DOS-drive
`windows_system_root`; it is never read from the ambient environment.
Non-Windows requires null. Runtime validation of Windows `System32\mswsock.dll`
and receipt protection still belongs to the actual trusted installer and
bounded helper. The assembly tool validates path shape, not an uninstalled
target system.

### Native build record

`native-build-v1.json` has exactly:

```text
schema, source_commit, dirty_manifest_sha256, toolchain,
rustc_sha256, cargo_sha256, lockfile_sha256, build_command_ref, cli, helper
```

`toolchain` is currently `"1.99.1"`. The compiler, Cargo and lock digests and
`build_command_ref` must be controller/build-custody evidence, not copied from
the uploaded bundle. Each of `cli` and `helper` has exactly:

```text
package, bin, target, host_target, profile, default_features,
explicit_features, resolved_features, compiled_pins, artifact
```

CLI package/bin is `fabric-cli` / `fab-rs`; helper is
`fabric-cli-update` / `fab-rs-update-helper`. Both target and host target equal
the explicit native target, profile is `release`, defaults are false. Helper
explicit/resolved features are exactly `["bounded-helper"]`, never
`fixture-harness`. `artifact` is exactly the corresponding expectation pin.
Both `compiled_pins` objects have exactly these build-time environment names:

```text
FAB_RELEASE_PUBLIC_KEY
FAB_RELEASE_CHANNEL
FAB_RELEASE_VARIANT
FAB_RELEASE_INSTALL_CUSTODY
```

They must match each other and the controller key/channel/variant. Custody is
`"standalone"` only for standalone packages, otherwise `"package-manager"`.
The helper does not consume the custody marker, but its build environment is
recorded identically; the CLI independently refuses apply without standalone
custody and a valid protected receipt. A release dependency does **not** build
another package's `[[bin]]`. Independent explicit helper compilation is a gate.

Record artifact digests **after approved native code signing**, since native
signing changes bytes. Retain the protected build/signing chain connecting
compiler inputs to those exact signed bytes. Signing inputs never go into
these JSON records.

### Engineering dirty manifest

Dirty source requires exact separately pinned bytes with
`{schema, base_commit, entries}`. Every sorted, case-unique entry has exactly
`{path, status, sha256, bytes, mode}`. Paths are relative repository paths with
`/` separators as data, no traversal/root/stream names.
`status` is `added`, `modified` or `deleted`. Deleted entries have null digest,
length and mode; other entries have exact digest, nonnegative length and
`"100644"` or `"100755"` mode. Include untracked source/build inputs and
deletions; do not report only tracked diffs. The trusted builder/controller
must establish completeness and before/after source stability. This tool
binds that evidence but does not rescan or mutate the source tree.
Dirty candidates cannot be promoted. Rebuild reviewed clean source before
production signature verification.

## Receipt and protocol boundaries

The generated standalone `.fab-rs-install.json` is the current **revision-2**
shape, exactly:

```text
schema, product, repository, custody, executable, version, target, variant,
sha256, helper, helper_sha256, windows_system_root, backups
```

It records `schema: 2`, `custody: "standalone"`, empty `backups`, explicit
installed absolute CLI/helper sibling paths and both exact artifact digests.
This is a **planned installer record**, not an assertion that those paths now
exist or have protected ACLs. Never drop it next to arbitrary writable binaries
and infer protected installation. The installed helper independently validates
the actual receipt, files and Windows runtime root. Package managers instead
receive assembly-only `manager-custody-v1.json`, with the common product/path/
digest fields, `schema: 1`, their exact custody and `auto_update: false`.
It is not a Rust `CustodyReceipt`, and is deliberately not named as one.
MSI/winget/Homebrew/deb/rpm upgrades remain manager-owned.

Protocol 1 is unchanged. The exact fields, and only those fields, are:
`protocol`, `product`, `repository`, `tag`, `version`, `channel`, `target`,
`variant`, `asset`, `bytes`, `sha256`, `expires_at`.
`asset` selects the raw CLI, not an archive, helper or installer.
Its signature is exactly 64 raw Ed25519 bytes. The current CLI/helper enforce
strictly newer eligible releases, same-installed-major automatic installation,
and explicit major-upgrade opt-in. The assembly tool does not authorize an
upgrade or replay a command.

Protocol 1 cannot bind source, minimum OS, helper digest or package receipt.
Those are in the separate `native-assembly-v1.json`, whose **exact bytes also
require an external signature**. It records source/dirty manifest, target,
variant, declared platform floors, external expectation/policy/attestation
digests, planned installation, existing major policy and exact file inventory.
The updater does **not** fetch or verify that assembly manifest today.
Consequently assembly provenance/minimum-OS policy is an installer/release gate,
not an extra runtime updater guarantee. A changed helper requires a separately
verified installer/package upgrade; protocol 1 only replaces the raw CLI and
retains the installer-pinned helper. Do not silently add wire fields or pretend
the updater updates/protects its helper.

## Separate signature verification

Signer policy is independently pinned outside the package. Its exact fields:

```text
schema, public_key_hex, public_key_policy_ref,
native_signer_policy_ref, native_verification_policy_ref,
windows_signing_profile_ref, apple_developer_id_ref,
apple_notarization_policy_ref, linux_signer_ref,
production_workflow_ref, release_authorization_ref, verifier
```

Public references may be null to retain actionable blockers. Key matches the
independently held compiled public key. Windows requires the approved Microsoft
internal service/profile reference; macOS requires Developer ID and notarization
policy; Linux requires its approved native/package signer policy. Every signed
operation also requires pinned-key distribution/rotation/revocation policy,
protected production workflow and exact release authorization references.
`native_signer_policy_ref` must identify the approved public certificate/key
identity and validation policy; `native_verification_policy_ref` identifies the
approved native verification tools/versions and evidence-authentication policy.
These are public policy/identity references, **not credentials or key exports**.
No missing identity/profile is substituted with a local key, ad-hoc signing,
GitHub provenance, an archive checksum or another product's profile.

The current approved-external-verifier adapter accepts only this exact shape:

```text
kind: "openssl-ed25519-v1"
path: absolute, independently held approved executable path outside the bundle
sha256, bytes: exact approved executable custody pins
version: exact single-line "OpenSSL 3...." output
approval_ref: independently authenticated approval and runtime/loader custody
windows_system_root: pinned verifier-host Windows root, otherwise null
```

This adapter is **not approval to install/use an arbitrary OpenSSL binary**.
Owner approval must cover its exact version, executable and exclusively held
runtime/library/loader environment. The verifier-host runtime root is separate
from the target installer receipt's runtime root. No tool/dependency is
installed by this lane. If no approved verifier is available, retain the blocker
or request an exact separately reviewed Rust verifier interface; do not execute
the bounded installation helper as a signature-only verifier.

With `--allow-approved-verifier`, the fixed no-shell commands are:

```text
ABSOLUTE_APPROVED_OPENSSL version
ABSOLUTE_APPROVED_OPENSSL pkeyutl -verify -pubin -inkey PUBLIC_DER -keyform DER -rawin -in EXACT_JSON -sigfile RAW_SIGNATURE
```

The second command runs separately for the assembly manifest and updater
manifest. The DER file is RFC 8410 public-key encoding of the **external**
controller pin; no private key exists. Inputs are exact held bytes, never
reformatted before verification. Version and verifier digest are checked,
with a new exclusive workspace, empty PATH, scrubbed environment, disabled
ambient OpenSSL configuration/modules, null stdin, 15-second per-process
deadline and bounded 64 KiB streams. Only fixed diagnostics/receipt metadata
are emitted. Polling output bounds are not disk quotas. Direct-child cleanup
is not a hostile-code process-tree sandbox: execute only an approved trusted
verifier on an isolated verification host. Package and external trust pins are
rechecked after verification.

The independently authenticated OS attestation bytes must match the controller
digest. Exact fields:

```text
schema, target, version, tag, policy_sha256,
verification_tool_ref, evidence_ref, files
```

`files` has ordered CLI and helper records, each exactly
`{role, sha256, bytes, signature_kind, signer_ref, timestamp_ref,
notarization_ref, verified}`. Both must match the final pinned artifact bytes,
`verified: true`, signer policy and authenticated native verification evidence.
Windows kind is `authenticode`; macOS is `developer-id-notarized`, requiring
both timestamp and notarization evidence; Linux is `gpg-detached`, with
timestamp evidence and null notarization. Absence or a merely self-asserted
attestation is not a pass. The controller must establish authenticity outside
the asset; this tool does not invoke SignTool/codesign/GPG or contact a service.
GPG/native-binary evidence does not establish deb/rpm repository metadata,
Homebrew distribution, PKG signing/notarization or MSI installer signatures.
Those final package formats need their own approved verification gates.

## Proposed CI gates and exact commands

These are proposed commands for a separately authorized controller, **not CI
edits, execution receipts, dispatch or production workflow configuration**.
Run native builds with the pinned toolchain from `rust`, separately for every
approved target/feature variant. `FEATURES`, `TARGET`, `PUBLIC_KEY`,
`CHANNEL` and `VARIANT` must come from owner-approved immutable records.
For example, the full Windows root feature selection is
`headless,native-read,repl,self-update`, yielding
`self-update+headless+native-host+native-read+repl+windows-broker`.
Set build pins in a scoped child build process, not the parent/global environment:

```text
FAB_RELEASE_PUBLIC_KEY=PUBLIC_KEY
FAB_RELEASE_CHANNEL=CHANNEL
FAB_RELEASE_VARIANT=VARIANT
FAB_RELEASE_INSTALL_CUSTODY=standalone OR package-manager
```

```powershell
# Proposal only; a fresh numbered build slot and native host approval are required.
cargo +1.99.1 build --offline --locked --release --target $TARGET `
  --package fabric-cli --bin fab-rs --no-default-features --features $FEATURES
cargo +1.99.1 build --offline --locked --release --target $TARGET `
  --package fabric-cli-update --bin fab-rs-update-helper `
  --no-default-features --features bounded-helper
```

1. Freeze/review exact clean source, lockfile/toolchain, features, declared
   native OS minima and artifact custody. Record the two commands and actual
   native compiler host, not a matrix label. No fetch is implicit in these
   commands; separately authorize/restored dependencies first if missing.
2. Obtain approved native code signatures from the existing protected
   Microsoft service for Windows; approved Developer ID/notarization on Apple;
   approved Linux signing for the selected channel. No service endpoint,
   credential wiring or fake signing/upload script is supplied here.
3. Independently validate final native signatures and timestamp/notarization,
   pin **post-signing** CLI/helper bytes, and record build custody, approved
   signer policy and externally authenticated OS attestation. Produce the
   independent expectation file and its externally transported digest.
4. Run offline assembly, preserving its unsigned status:

   ```powershell
   python -B C:\CHECKOUT\rust\scripts\assemble_native_release.py assemble `
     --expect C:\HELD\expected.json --expect-sha256 $EXPECTED_SHA `
     --build-record C:\HELD\native-build.json `
     --cli C:\HELD\fab-rs.exe --helper C:\HELD\fab-rs-update-helper.exe `
     --output C:\HELD\new-candidate --now $UTC_UNIX_SECONDS
   ```

   Engineering-only dirty builds additionally require
   `--dirty-manifest C:\HELD\dirty-source.json`. They cannot be promoted.
   The command intentionally fails if the output root already exists.
5. Submit the exact generated updater and assembly JSON byte digests to the
   separately approved protected Ed25519 signing service. Obtain 64-byte
   detached signatures without rewriting metadata, changing expiry, changing
   binaries, generating private keys or signing within this tool.
6. Perform the separate signature gate:

   ```powershell
   python -B C:\CHECKOUT\rust\scripts\assemble_native_release.py promote `
     --bundle C:\HELD\new-candidate\bundle `
     --expect C:\HELD\expected.json --expect-sha256 $EXPECTED_SHA `
     --policy C:\HELD\approved-policy.json --now $UTC_UNIX_SECONDS `
     --update-signature C:\HELD\update.json.sig `
     --assembly-signature C:\HELD\native-assembly-v1.json.sig `
     --os-attestation C:\HELD\native-os-attestation.json `
     --workspace C:\HELD\new-verifier-work --allow-approved-verifier `
     --output C:\HELD\new-signed-assembly
   ```

   Later validators use `verify-signed` with the signed bundle, same external
   expectation/policy/time and a new verifier workspace; external signature/
   attestation/output flags are omitted. `verify` verifies only the unsigned
   inventory and intentionally rejects the signed inventory.
7. Approved packaging/installers consume that exact signed inventory and
   separately establish archive extraction safety, receipt placement/protection,
   platform package/feed signatures and protected helper installation. Native
   clean-machine, terminal, update, rollback, recovery and major-boundary
   acceptance remain required. Do not auto-overwrite manager-owned installs.
8. Independent reviewers verify exact final artifacts, notices, acceptance and
   publisher authorization before any separately approved official publication.
   The existing fork-only unsigned preview workflow is not a production route.

No unit/build/native execution grant was supplied to this implementation lane.
Its initial validation is read/AST-only; the controller must run the
[33-method synthetic unit selector](../tests/acceptance/release_assembly/README.md)
under a fresh numbered grant, preserve actual results and obtain independent
review. Additive tests never substitute for the existing preview verifier tests,
actual Ed25519 interoperability or native platform/installer acceptance.

## Consolidated publisher inputs for the accountable maintainers

Supply one authenticated, revision-bound packet, not secrets:

1. **Reviewed candidate and platform scope:** full clean source SHA, locked
   toolchain/compiler/Cargo pins, exact CLI/helper feature/build records,
   native targets, declared minimum OS/runtime floors and independent evidence.
2. **Update trust/channel:** approved independently distributed Ed25519 public
   key, rotation/revocation/recovery policy, stable/preview selection, variant,
   expiry policy and same-major/explicit-major-upgrade acceptance.
3. **Windows:** existing Microsoft internal signing service/profile and
   protected workflow references, approved signer/public certificate policy,
   timestamp/verification evidence, installed helper/receipt ACL policy,
   explicit installer-pinned Windows runtime root, and MSI/winget policy if used.
4. **Apple:** Developer ID and installer-signing references where applicable,
   notarization/stapling and verification policy, package format/channel,
   receipts and native acceptance. No Apple identity has been supplied.
5. **Linux:** approved binary/package/feed signing public references and native
   runtime/installation evidence for each chosen standalone/deb/rpm channel.
6. **Verification:** approved pinned signature-verifier executable/version and
   runtime custody, independently authenticated post-signing OS attestation
   digest for both binaries, and exact outside-package expectation pins.
7. **Production route and release decision:** protected official
   `microsoft/fabric-cli` workflow/reference, exact release authorization,
   accountable repository maintainer approval, signed package inventory,
   installer/manager policies, clean-machine/update/rollback evidence, notices,
   retention/support/recovery commitments. No publication or live authority
   was supplied; the tool never upgrades an absent decision into approval.

Private keys, service tokens, certificate exports, Apple credentials and signing
service credentials belong only in the already approved protected service, never
in these records, repository files, test fixtures or chat.
