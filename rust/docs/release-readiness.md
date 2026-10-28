<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust release readiness and owner decisions

**Status: offline tooling, not publication approval or a release receipt.**
This lane starts from the published `eisber/fabric-cli` source
`b7d45d3cc1a1c80f17fe0cebccbdc397439b81fd`. The integration controller owns
independent review, combined-candidate validation, and any later promotion.
No SDK, crate, Cargo dependency, lockfile, identity registration, or production
configuration changes are part of this work.

## What can be established offline

[`release_verify.py`](../scripts/release_verify.py) consumes the existing
[schema-1 preview bundle](preview-builds.md#bundle-contract-schema-version-1).
It verifies an already extracted, exclusively held directory containing exactly
the named executable, `manifest.json`, `SHA256SUMS`, `LICENSE`, and `README.md`.
It rejects extra/missing entries, directories in place of files, symbolic links,
hard links, reparse points, duplicate JSON keys, unknown fields, unsupported
schemas, and oversized inputs. It does not download or extract archives.
Untrusted filenames from the manifest or checksum file never select disk paths.

The verifier requires a separate expectation file. Full source and workflow
commits, workflow ref, target, explicit features, disabled defaults, release
profile, Rust/Cargo identity, lockfile digest, binary hash/length, and run/attempt
must agree. All four canonical checksum entries must match. A separately
pinned manifest digest prevents changing metadata and merely recomputing the
bundle checksums. Compiler digests cover the UTF-8 bytes of the exact strings
the workflow records (trimmed command output, not a trailing shell newline).
Runner-image metadata remains informational, not a reproducibility guarantee.

Metadata is limited to 1 MiB per file, checksums to 4 KiB, and the executable
to 512 MiB. Size limits are input bounds, not product-size acceptance targets.
The verifier never calls Cargo, Python product code, `az`, or `azd`. Python
3.10+ and its standard library are required only on the **verification host**,
not by the shipped executable. No third-party Python package is required.

With explicit `--smoke`, the verifier checks the native OS/architecture, copies
the verified executable to a temporary directory, verifies the copy, and invokes
only that absolute path. It repeats the workflow's eight help/version/parser
cases from a separate empty working directory. Credentials, selectors, proxies,
toolchain variables, and developer-tool PATH entries are not inherited. Home,
configuration, cache, and temp directories are isolated; only Windows system
directory variables are retained. Each case has a ten-second timeout, null stdin,
no shell, a 1 MiB output acceptance limit per stream, and bounded-memory output
capture. Polling can briefly overshoot the disk-output limit; this is not a disk
quota. Exit codes and stream hashes are recorded, not raw process output.
The copied binary and original bundle are rechecked after execution.

This is **not an execution, network, OS broker, or hostile-code sandbox**.
It terminates its direct child, not an arbitrary descendant process tree.
Run only an authorized, independently trusted candidate under exclusive custody
on an isolated host. Do not execute arbitrary downloads as a verification step.
An attacker who can replace expectations or a running process is outside this
tool's trust boundary. Matching checksums are not publisher signatures.

See [clean-machine acceptance](clean-machine-acceptance.md) for executable
commands, receipt requirements, installation, update, rollback, and recovery.
Local synthetic tests establish verifier behavior only: they cannot establish
real candidate startup, clean-machine support, authentication, or publication.

## Approval and evidence blockers

| Gate | Current evidence / missing input | Required actor and continuation |
| --- | --- | --- |
| Workflow registration | Assignment reports provider 404: the preview workflow exists on a feature branch but default-branch dispatch registration is absent. This lane did not re-query or change provider state. | Repository owner chooses and explicitly authorizes a registration route. Review exact default-branch change separately; do not change defaults/settings, add triggers, or dispatch as a workaround. |
| Hosted build | No hosted build or dispatch executed by this lane. Existing workflow remains dispatch-only, fork-restricted, `contents: read`, with unchanged four-product feature scope. | Controller obtains explicit dispatch authority for one immutable source/workflow commit after registration; retain actual run/job/upload receipts. |
| Downloaded candidate | No release bundle, artifact ID, archive digest, or actual fab executable was provided to this lane. | Authorized validator downloads by exact artifact ID, checks provider archive digest, creates separately trusted pins, and runs verification on that exact bundle. |
| Clean-machine and minimum OS | Runner labels are not minimum-OS or clean-machine evidence. No clean-machine pass is claimed. | Owner selects targets and minimum OS; platform validators record actual OS/build, runtime inventory, tool absence, and copied-binary outcomes. |
| Signing and identity | No signing identity, key storage, notarization, production identity backend, or policy approval established. | Owner/security/platform teams select ownership and support; authorize signing/live acceptance separately. Do not create keys, registrations, consent, or tenant changes here. |
| Dependency notices | `cargo-sbom`, `cargo-cyclonedx`, and `cargo-license` were not available on this lane's PATH; no inventory/SBOM generated. A Cargo.lock checksum does not contain license evidence. | Build owner supplies an approved pinned inventory tool plus locally restored locked crate metadata/license texts, target/features and tool version. Run offline under the build-slot policy; legal owner reviews redistribution notices. No service/tool installed by this lane. |
| Release support | Product/channel/platform, retention, recovery, support owner and SDK publication decisions remain open below. | Owner records each decision with date, exact scope and approving identity; controller then plans separately authorized promotion. |

Preserve failures and partial matrices. An upload is not a verified download;
download verification is not clean-machine acceptance; offline smoke is not
live-service proof. Do not fill missing provider receipts with synthetic tests.

## Owner decision register

Every entry is **OPEN**. Recommendations are proposals, not consent. The
controller collects choices; this unattended lane does not pick for the owner.

| ID | Choice needed | Safe recommendation | Alternatives, costs, and tradeoffs |
| --- | --- | --- | --- |
| R1 | Windows-only experimental preview, three native previews, or full-platform GA? | Windows x64 opt-in preview first, distinguish default and native-read; retain all current workflow matrix rows until an approved change. | Three native previews need three independent clean hosts and per-platform runtime/security work; default Linux/macOS parser startup is not authenticated parity. GA requires complete supported-command, identity, installer and support evidence, not just matrix success. |
| R2 | Executable/package name and install channel? | Keep `fab-rs-default[.exe]` / `fab-rs-native-read.exe`, immutable source/target/variant archive identity, private preview access, side-by-side per-user install, absolute-path launch. Do not replace existing `fab`. | A common `fab` name can collide with installed products. MSI/winget, Homebrew or OS packages need publisher ownership, signing, uninstall/upgrade rules and native acceptance. Actions artifacts expire and are not a durable public release channel. |
| R3 | Update behavior/channel? | Explicit manual, verified side-by-side preview updates; no self-update, automatic PATH edit or automatic promotion. | Stable/preview package-manager channels improve UX but add feed custody, downgrade policy, signing, atomic replacement and compromise-recovery obligations. An automatic updater is a separate implementation and threat review. |
| R4 | Minimum targets and OS/runtime versions? | Begin acceptance on owner-selected supported Windows 11 x64 builds, document the exact MSVC CRT and WAM/system prerequisites; publish no minimum until tested. | Windows ARM64/older Windows, Linux glibc baseline, Intel/ARM macOS minima and musl need separate native artifacts and runtime inventories. Hosted labels `windows-2025`, `ubuntu-24.04`, `macos-15` do not establish minima. |
| R5 | Signing identity, storage and notarization? | Before public distribution, use an approved organization-owned Windows signing identity in managed/HSM-backed storage with access control, audit and timestamping; select Apple Developer ID/notarization only if macOS distribution is chosen. | An explicitly unsigned internal preview needs risk disclosure and enterprise policy approval. Local real keys, ad-hoc signatures, checksums or archive digests do not provide organization publisher assurance. Managed signing has cost, enrollment and service-availability implications. No provider is selected or configured. |
| R6 | Retention and rollback window? | Propose 90-day candidate evidence and one-year approved release evidence, keeping at least two accepted prior versions, subject to security/legal policy and storage approval. | Current Actions retention is seven days. Longer protected artifact/receipt storage costs money and requires authority; never promise retention based only on immutable filenames. Security-revoked versions must not remain rollback choices. |
| R7 | Support, incident and recovery ownership? | Name a release DRI, platform/identity DRI, security escalation route and rollback approver before widening preview; agree on support hours and response expectations. | Best-effort preview can be cheaper but must be explicit. GA without an accountable support owner and tested destructive-effects recovery is not ready. No person/team is assigned by this proposal. |
| R8 | SDK/package publication policy? | Preserve workspace `publish = false`; current SDK inherits this policy. SDK users build from a pinned source revision and lock graph. | Publishing SDK crates requires a separately reviewed public API/versioning/deprecation policy, crate naming/registry ownership, notices, credentials, dependency order and downstream acceptance. A CLI bundle is not a stable Rust ABI or SDK package. Do not change manifests as a release-tool shortcut. |

For each choice, record `decision ID`, `selected option`, `approved scope`,
`approver`, `UTC date`, `rationale`, `evidence refs`, and `remaining conditions`.
No entry may transition to approved solely because a script or test passes.
