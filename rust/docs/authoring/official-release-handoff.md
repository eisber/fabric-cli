<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Official ADO release implementation handoff

Copy the entire prompt below into the trusted internal repository's
implementation session. This is an implementation specification, **not**
validated internal YAML. Public documentation cannot establish approved signing
tasks/profiles/service connections or certify compliance. Resolve missing inputs
with the responsible owners before implementing privileged stages.

---

Implement an official build, package, sign, and release pipeline for the minimal `fab-rs` CLI in the internal Microsoft Fabric CLI Azure DevOps repository. The product source lives under `rust/`, split between reusable `engine/` libraries and `clients/cli/`. It supports only Azure CLI session-selection auth commands and raw API with default LRO polling, `--async` to return the initial response, and JSON output. Do not alter Python packaging, add other authentication providers, or implement an updater.

Before writing pipeline configuration, inspect this repository's current official build/release templates and approved Microsoft engineering/signing policies. Resolve these required inputs from owners or existing verified configuration; do not invent them:

1. Internal repository/default branch, approved source-mirror mapping to the public GitHub commit, protected release triggers, and version/tag ownership.
2. Approved official-build template, native agent pools/images for Windows x64, Linux x64, and both macOS architectures; pinned Rust toolchain availability.
3. Approved ESRP integration/task/template version, Windows signing profile/certificate policy, timestamping, authentication and service connections, isolation/custody requirements, and retention.
4. Approved Apple Developer ID signing/notarization facility and credentials, supported packaging format and offline/online Gatekeeper expectations. Do not assume ESRP automatically supplies Apple signing or notarization.
5. Linux signing/attestation requirements, scanning/SBOM/notices gates, release approval owners, and GitHub publisher service connection with least privilege.

Implement protected official-release stages:

**Source and version.** Accept an approved immutable source revision and release version. Verify public/internal revision mapping, lockfile, toolchain, and source/tag consistency. Build official artifacts in trusted ADO jobs from that source, never from untrusted PR artifacts. PR validation must have no access to signing/publication credentials.

**Build and validate.** Use native controlled agents for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-apple-darwin`. Confirm actual architecture, toolchain, and OS compatibility baselines. Run the reduced workspace's locked fmt/check/clippy/test/doc gates and release build, plus help/version/offline executable smoke. Restore dependencies through approved feeds and emit logs, SBOM, notices, provenance, source revision, target triple, and unsigned artifact identities. Run required internal security/compliance scans. Use synthetic credentials/HTTP only.

**Immutable handoff.** Stage only explicit target artifacts in immutable build outputs with hashes and provenance. Separate build, signing, and publishing permissions. Signing failures, missing targets, source mismatches, and failed scans block release. Do not substitute unsigned binaries or silently skip a target.

**Platform signing.** Sign the final Windows PE executable using the approved ESRP-compliant integration and timestamp policy, then verify the expected Microsoft signer, chain, timestamp, and integrity. Use the approved internal Apple route to sign each Mach-O with the correct Developer ID and required options, submit the supported payload for notarization, verify acceptance, and satisfy the approved Gatekeeper/offline trust policy. A bare executable or ordinary ZIP cannot simply be assumed to support stapling; select and validate a supported container/package when required. For Linux, apply approved detached package/manifest signing or attestation if policy requires it; Authenticode does not apply to ELF. Report unsupported policy/platform combinations as release blockers, not as signed.

**Package and verify.** Package signed executables with license, third-party notices, and prerequisites/installation instructions. Start with Windows ZIP and Linux tar.gz; finalize macOS format according to the verified signing/notarization requirements. If a signed container or archive needs its own signing/notarization step, finish it before generating final checksums. No rebuild or binary modification after signing. Calculate SHA-256 over the final bytes, publish signed provenance/attestations as required, extract and execute the exact final bundles on clean target hosts, and verify signatures/trust plus help/version. Tests must not require live Azure/Fabric authentication.

**Publish.** Use a protected, approved publisher job/service connection to create a draft GitHub release in the explicitly authorized destination repository. Use a dedicated `rust-v<semver>` namespace, deterministic target-qualified assets, checksums, SBOM/notices, and provenance. `GitHubRelease@1` or an existing approved publisher may be used, but inspect how its update/upload behavior handles existing assets. Reject conflicts rather than overwriting silently. Download the uploaded assets, verify their final hashes/signatures and full expected set, then publish only after all four targets and approval gates pass. GitHub does not provide a multi-file upload transaction: keep the release draft until verification completes, serialize publication, and record a release manifest to detect partial uploads.

**Recovery and audit.** Make reruns safe with exact artifact/version matching; never rebuild under an already signed identity or clobber a published asset. A failed draft upload remains unpublished and actionable. Define controlled withdrawal/superseding-version procedures, approval evidence, access audits, signing/publishing logs, and retention. Keep credentials and internal infrastructure identifiers out of public release metadata.

Deliver the actual repository-appropriate pipeline/templates, minimal shared packaging changes, exact commands/results for each native target, verified signature/notarization evidence, an operator runbook, and a list of any unresolved compliance prerequisites. Do not describe the release as ESRP-compliant until the approved integration and evidence have been verified by the responsible owners.

Public task reference: https://learn.microsoft.com/en-us/azure/devops/pipelines/tasks/reference/github-release-v1?view=azure-pipelines

---
