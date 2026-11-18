<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal native packaging

There is one product variant per target: `fab-rs`. No installer/updater, native
login variant, migration manifest, or release orchestration framework.
`scripts/package_native.py` uses Python 3.11+ standard library only and never
builds, signs, or publishes. It packages an already-built native binary from a
clean immutable source checkout. Do not use it to imply signed-official status.

## Producer

From `rust/`, on the matching native host, with the pinned toolchain:

```sh
# Replace this value with the native target. Use PowerShell's $env:TARGET on Windows
# or run these commands in Git Bash, as the workflows do.
TARGET=x86_64-unknown-linux-gnu
cargo fetch --locked
python3 scripts/native_host.py --target "$TARGET" --output target/native-evidence.json
cargo build --offline --locked --release -p fabric-cli --bin fab-rs --target "$TARGET"
python3 scripts/package_native.py build --target "$TARGET" --output "target/preview-$TARGET"
python3 scripts/package_native.py verify --target "$TARGET" --directory "target/preview-$TARGET" --smoke
```

Run the full validation commands in the README first. A local heavy-build lease
is mandatory on the shared host. `build` refuses a dirty checkout or existing
output directory, so an old/mixed artifact set cannot silently become a preview.
It reads locked offline Cargo metadata and the existing target-directory binary.
The output directory must remain ignored (normally under `rust/target/`).

Version comes from the `fabric-cli` package. Source comes from full Git HEAD;
target and toolchain must match actual host evidence. Architecture is checked
again from PE/ELF/Mach-O headers. Linux needs `readelf`; macOS needs `otool`.

For version `<version>` and target `<target>`, the complete per-target set is:

```text
fab-rs-<version>-<target>.zip              # Windows only
fab-rs-<version>-<target>.tar.gz           # Linux/macOS instead of ZIP
fab-rs-<version>-<target>.cdx.json
fab-rs-<version>-<target>.provenance.json
SHA256SUMS
```

Each archive contains **only** `fab-rs` (`fab-rs.exe` on Windows), `LICENSE`,
`THIRD-PARTY-NOTICES.txt`, and `README.txt`. The full MIT product license is copied
from the repository, not a license identifier alone. Notices include declared
licenses and complete shipped license/copyright/notice files, including nested
native dependency texts, for the conservative full resolved dependency graph.
Missing declared licenses, unreviewed absent license texts, empty shipped legal
files, escaping license paths, and invalid text encoding block packaging instead
of producing placeholders.
Review the resolved inventory for additional obligations before official release.
No claim of automated legal approval is made.

### Reviewed package-specific license inputs

Some published crate archives omit their repository-level license files.
[`packaging/license-inputs.toml`](../packaging/license-inputs.toml) explicitly
maps **exact package versions** to source-reviewed license inputs, not a generic
SPDX-to-license fallback. Each entry pins the crates.io archive checksum, declared
license expression, public repository, crate VCS commit/path, chosen license,
and each input's SHA-256. Authoritative public URLs use the full immutable source
commit. No network access is used during packaging.

The initial exceptions cover `azure_core` 1.1.0, `azure_core_macros` 1.0.0,
`azure_identity` 1.0.0, `typespec` 1.1.0, `typespec_client_core` 1.1.0,
`typespec_macros` 1.0.0, `jni` 0.22.4, `jni-macros` 0.22.4,
`jni-sys-macros` 0.4.1, `jpx-core` 0.4.0,
`rustls-platform-verifier-android` 0.2.0, and `r-efi` 6.0.0.
The first eleven omit recognized shipped legal files; `r-efi` instead ships
the full MIT grant and copyright notices in `AUTHORS`. Only that exact reviewed
`AUTHORS` input is recognized; arbitrary authors files are not treated as licenses.

MIT is explicitly selected for these packages, including the packages offering
`OR` alternatives. The original declared expression remains in the SBOM.
Vendored source bytes preserve original copyright and permission notices;
Azure SDK inputs include **both** `LICENSE.txt` and `NOTICE.txt`, including its
embedded third-party attribution. `r-efi` uses the hash-checked shipped file.
Notices and SBOM license references record each source URL and text checksum.
These are source-review records, not legal certification or official-release
approval.

When dependencies change, audit the **full** locked graph. Do not copy a generic
MIT/Apache text or relax validation. For an omitted legal file, inspect the crate's
`.cargo_vcs_info.json` and the authoritative repository at that exact commit,
review the applicable license choice and required notices, preserve the complete
input bytes, and add an exact package/version entry with archive/text checksums.
Verify vendored bytes against the immutable public URL and add regressions.
An unmatched version with no shipped license still fails; a reviewed entry with
a changed checksum, declared license, repository, crate VCS identity, or text
also fails. Malformed declared paths never trigger fallback.

Source-only validation against the restored lock/metadata generated notices for
all **213 external packages**, with **225 SBOM components** (12 workspace
packages). This checked the full dependency references and absence of local
registry paths; it is not native build, archive, signing, or legal approval.

The release-level CycloneDX 1.5 SBOM lists Cargo packages, versions, source
identity, registry archive checksums where present, and resolved dependency
edges. It deliberately includes build/dev and target-specific packages; this
is a conservative source inventory, not a claim that every entry is linked.
The verifier requires it as part of the complete set.

Provenance records version, full source revision, target, toolchain, build
command, UTC time, lock hash, per-content hashes, unsigned/unofficial status,
host observations, and loader evidence. No credentials, account identifiers,
local registry paths, or internal infrastructure links are intended for public
metadata. Review actual native-tool output before official publication.

Checksums cover the **final bytes** of the archive, SBOM, and provenance.
Each per-target directory has its own `SHA256SUMS`; when assembling all four
targets for a release, generate one aggregate checksum file over the exact
target-qualified files. Never overwrite one target's checksums with another's.
This script does not implement a release-level publisher or signature policy.

## Consumer verification

Download from a trusted source; the workflow artifact is an outer transport ZIP.
Extract that wrapper into a dedicated directory before using the inner archive.
From a checkout at the **exact source revision** in provenance, run:

```sh
python3 scripts/package_native.py verify \
  --target x86_64-unknown-linux-gnu \
  --directory /path/to/extracted-workflow-artifact --smoke
```

`verify` checks the exact file set, checksum inventory, provenance identities,
SBOM consistency, flat archive entries, executable format, and archived-content
hashes. It also checks the source revision against checkout HEAD and the compiler
pin against `rust-toolchain.toml`, plus the version against the workspace manifest.
It refuses unexpected files, duplicate entries, archive links and path
traversal. With `--smoke`, it verifies the actual native host, writes **only the
verified executable bytes** to a temporary directory, and runs help/version and
structured missing-binding failures. PATH is empty, home/config is temporary,
and Azure-related environment overrides are removed: ambient Azure CLI/config
cannot satisfy smoke checks. This is a basic packaged-executable smoke, not
replacement for integration's injected executable/LRO tests.

Run smoke only on trusted artifacts. Unsigned hashes prove integrity against the
provided inventory, **not** identity or authenticity: someone who replaces the
entire set can replace its hashes too. Official consumers must also verify the
approved platform signatures/attestations and expected publisher.

## Official-release boundary

Proposed tag namespace: `rust-v<semver>`, independent of Python versioning.
Official trusted builds, scans, signing, notarization, final packaging,
post-signing hashes, exact clean-host verification, draft upload and approved
publication are separately implemented in ADO. Windows starts with ZIP and Linux
with tar.gz; official macOS format depends on verified signing/notarization and
offline Gatekeeper policy. An ordinary archive/bare executable is not assumed
staplable. No arbitrary PR preview may be promoted as an official build.
