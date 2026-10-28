<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Rust preview build artifacts

The dedicated `Rust preview binaries` workflow builds a selected immutable source
once and shares the resulting executables for offline CLI testing and controlled
preview. It is separate from `rust-ci.yml` and from product packaging, installers,
signing, releases, and deployment. A green compatibility run with zero artifacts
does **not** provide a downloadable executable.

At preparation time this workflow has not been published or dispatched by this
implementation. This guide is not evidence of a provider artifact or a
ready-to-download binary; consult the actual run receipts. The integration owner
owns publication, dispatch, and the completion gates below.

## Products and native hosts

The workflow has only `workflow_dispatch`: no push, pull-request, schedule, or
automatic release trigger. Dispatch the reviewed workflow at the exact source
commit to test; there is deliberately no independent source-ref input. Checkout
uses `github.sha`, and the job rejects a difference between the checkout,
`GITHUB_SHA`, and `GITHUB_WORKFLOW_SHA`. The manifest records the full source
commit and workflow ref/commit rather than resolving a moving branch later.
GitHub requires the workflow to exist on the repository's default branch before
it can receive dispatch events. Publishing only to a feature branch is not
sufficient; the integration owner must arrange that separate registration step
without changing the selected build commit.
Execution is restricted to the existing `eisber/fabric-cli` fork; copying the
workflow upstream or to another fork does not authorize preview uploads there.

| Hosted runner | Required actual rustc host | Product / Cargo features | Executable |
| --- | --- | --- | --- |
| `windows-2025` | `x86_64-pc-windows-msvc` | Default parser preview / none | `fab-rs-default.exe` |
| `windows-2025` | `x86_64-pc-windows-msvc` | Windows bounded host / `native-read` | `fab-rs-native-read.exe` |
| `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | Default parser preview / none | `fab-rs-default` |
| `macos-15` | `aarch64-apple-darwin` | Default parser preview / none | `fab-rs-default` |

There are **four products on three native targets**, not four supported operating
systems. Runner labels are not architecture proof. The existing
[native build evidence](build-validation.md#historical-provider-checkpoint)
records these three actual hosts, including ARM64 macOS. Every new job checks
the actual compiler host and runner architecture before building and fails if
an image changes architecture. Intel macOS, Windows ARM64, Linux ARM64, and
Linux musl are outside this preview.

All products use Rust **1.99.1** from `rust/rust-toolchain.toml`, the existing
`Cargo.lock`, `--release`, `--no-default-features`, and an explicit native target.
After `cargo fetch --locked --target <target>`, compilation uses
`cargo build --offline --locked --release --target <target> --package fabric-cli
--bin fab-rs --no-default-features`, adding `--features native-read` only for the
Windows host product. No Cargo dependency, toolchain, or feature definition is
changed. Cargo offline mode prevents dependency restoration during compilation;
it is not an operating-system network sandbox.

Default products expose help, version, and the bounded parser; recognized reads
have no configured host. Compiling all the library handlers does not wire them
into executable commands. `native-read` composes the existing Windows broker
boundary and Hyper/Rustls transport with the bounded read host. It is **not**
all 52 Python command leaves, a complete live CLI, or live authentication proof.
See [compatibility](compatibility.md) and
[native host capabilities and omissions](native-read-host.md).
The workflow never supplies account/tenant/client selectors, acquires tokens,
invokes WAM login, or calls Fabric.

## Bundle contract (schema version 1)

The artifact name is:

```text
fab-rs-<full-source-sha>-<target>-<default|native-read>-rust-1.99.1-run-<run-id>-<attempt>
```

The run/attempt suffix prevents reruns from replacing an earlier bundle.
`overwrite: false` preserves immutable upload identity within that run; it does
not make Actions storage permanent. Retention is **7 days**, subject to repository
policy, deletion, and Actions access permissions. The owner can share the artifact
URL with authorized repository readers; this is not an anonymous public download,
GitHub Release, signature, notarization, or durable distribution channel.

Each bundle contains exactly five files:

| File | Contract |
| --- | --- |
| Named executable above | One native release-profile executable; default and native-read are distinct products |
| `manifest.json` | Schema version, full source/workflow identity, build identity, binary SHA256/length, capabilities, limits, smoke receipts, and run URL |
| `SHA256SUMS` | Lowercase SHA256, two spaces, filename, newline for the other four files; no self-referential checksum |
| `LICENSE` | Repository MIT license |
| `README.md` | Product-specific identity/limitations followed by this guide |

`manifest.json` has these required top-level fields:

| Field | Contents |
| --- | --- |
| `schema_version` | Integer `1`; consumers must reject unsupported schema versions |
| `artifact_name` | Exact Actions upload name |
| `source` | `repository`, 40-hex `commit`, `workflow_ref`, matching `workflow_commit` |
| `build` | `target`, explicit `features` array (`[]` or `["native-read"]`), `default_features: false`, `profile: "release"`, toolchain, full `rustc`/`cargo` versions, `lockfile_sha256`, runner/OS/architecture, image OS/version when provided |
| `binary` | `filename`, lowercase 64-hex `sha256`, positive `bytes` length |
| `capabilities` | `variant`, bounded Windows read-host and compiled broker-command booleans, `live_service_verified: false`, `full_python_cli_parity: false` |
| `smoke` | `environment: "isolated-allowlist"` and cases with argv, exit, stdout/stderr SHA256 |
| `limitations` | Explicit preview, native, ABI, installation, and reproducibility exclusions |
| `ci` | Provider `run_url`, decimal-string `run_id` and `run_attempt` |

Feature names record explicit Cargo selections; `native-read` transitively selects
the existing broker and transport features. This does not enumerate or alter
dependency features. `lockfile_sha256` hashes the checked-out bytes, not a
reformatted lockfile. No account IDs, tenant/client selectors, identity codes,
environment dump, token, auth cache, oracle input, transcript, debug log, PDB,
whole `target` directory, or library output belongs in an artifact.

The artifact archive digest in the job summary is distinct from the executable
SHA256 in `manifest.json`. Neither is a publisher signature or proof against an
attacker who can replace both executable and manifest. Verify the expected
source, workflow, and provider run through the trusted repository before use.
Hosted image versions, linkers, and system libraries can change; pinned Rust and
Cargo inputs support traceability, not a bit-for-bit reproducible-build claim.

## Copied-executable smoke boundary

Before upload, the job copies the executable out of Cargo output into the
allowlisted bundle and invokes that exact absolute path from an empty temporary
working directory. It does **not** use `cargo run`. The child environment is
constructed afresh: only Windows `SystemRoot`/`WINDIR` when present, isolated
home/config/cache/temp directories, and `NO_COLOR=1`. No inherited PATH,
developer tools, proxy variables, Fabric selectors, or credential variables are
forwarded. PATH and Node coverage output are explicitly empty to prevent
runtime defaulting or instrumentation inheritance. Each subprocess has a
ten-second bound.

Both help flags must succeed on stdout with empty stderr, both version flags must
match the workspace version exactly, and no arguments, unknown command, unknown
flag, and a synthetic rejected secret option must fail with exit 2, empty stdout,
and the existing sanitized JSON error on stderr. The rejected sentinel must not
be echoed. No auth or service command is used. Stream hashes and exits are
recorded without retaining logs or inherited environment.

Passing this smoke proves the copied binary starts on its build runner for those
offline cases. It does not prove clean-machine installation, successful read
configuration, broker policy, login/MFA, enterprise trust/proxies, live Fabric,
all commands, or source compatibility. Native preview builds complement, never
replace, the exact-source consumer/differential gates in `rust-ci.yml`.

## Runtime prerequisites and download verification

An executable avoids a Rust/Python/az/azd build dependency for its user; it does
not eliminate platform libraries. Windows MSVC binaries may require the matching
Microsoft Visual C++ runtime/system CRT and Windows SDK-era system APIs. The
native-read variant also depends on Windows WinRT/COM/WAM system components and
the existing isolated native interop boundary; no redistributable broker DLL is
bundled. Linux GNU needs compatible glibc and platform libraries; macOS ARM64
needs a compatible macOS system runtime. The optional TLS composition uses
Rustls/ring with platform trust/verification services and trust stores, not a
promise that all TLS-related OS libraries disappear. This does not add an
OpenSSL backend. A clean-machine library inventory/minimum-OS acceptance test
remains outstanding.

After an authorized download, extract into an otherwise empty directory. Verify
all four `SHA256SUMS` entries, the executable length/hash against `manifest.json`,
and the expected full source SHA, target, feature selection, toolchain and
provider run. On Windows, `Get-FileHash -Algorithm SHA256` can verify each file;
on Linux use `sha256sum -c SHA256SUMS`, and on macOS use
`shasum -a 256 -c SHA256SUMS`. ZIP extraction may lose executable permissions on
Unix; after verifying the hash, apply `chmod +x` to the named binary.

Repeat the credential-cleared temporary-directory help/version/invalid-argument
smoke on the downloaded executable, not the Cargo output. Record the actual
download/verification commands, target/architecture, exits, checksums, run/job
URLs, and artifact ID/URL/archive digest. The workflow writes upload receipts,
but an upload receipt alone is not a successful download or execution receipt.

The offline [release verifier](../scripts/release_verify.py) implements this
five-file schema without changing the workflow. It requires separately trusted
identity/digest expectations, rejects links and unexpected files, and can repeat
the copied-executable smoke on a matching native host. See
[release readiness and pending owner decisions](release-readiness.md) and
[clean-machine acceptance, installation and recovery](clean-machine-acceptance.md)
for commands and evidence boundaries. Its Python standard-library dependency is
verification-host tooling, not a shipped executable requirement. Synthetic tests
are not downloaded-binary, clean-machine or live-service receipts.

## Resource and security policy

Only `contents: read` is granted to `GITHUB_TOKEN`; upload uses the job-scoped
Actions artifact service, not a PAT or an `actions: write` provider grant.
Checkout does not persist credentials. Direct Actions use existing repository
full-commit pins. There are no new third-party actions, secrets, external
storage, package publication, or release credentials.

Matrix jobs use separate hosted runners, private temporary Cargo target
directories, one build job, no incremental compilation, and a 20-minute job
timeout. No local shared-host slot or peer target directory is used. The
existing approved cache action caches only Cargo registry/git dependency sources
under an exact toolchain/target/variant/Cargo.lock key, without broad fallback
restore keys. Compiled outputs are deliberately not cached or uploaded.
No sccache or additional tool installation is introduced.

Cold/warm build times have **not been measured** for this workflow. Measure them
from real authorized CI step timings and cache-hit receipts; do not infer savings
from the presence of a cache.

## Completion gates and remaining scope

1. The integration owner reviews and publishes this dedicated workflow and guide
   without changing existing compatibility gates, ensures default-branch
   workflow registration, then separately authorizes dispatch at one selected
   exact source/workflow commit.
2. All three actual native hosts pass identity checks and produce their default
   bundles; Windows also produces the separate native-read bundle. Record real
   run/job URLs, full commit, compiler hosts, features, and toolchain.
3. Verify actual provider artifact uploads and download every bundle by artifact
   ID. Check all file hashes/lengths/metadata and rerun the isolated smoke on the
   downloaded executables on their matching hosts. Preserve receipt URLs and
   failures; do not upgrade a partial matrix to complete.
4. Only after those receipts may the owner call these exact previews
   **ready to download**. Live use, clean-machine support, installer/release
   publication, signing/notarization, and product rollout remain separate gates.

A possible later slice is to freeze small per-family JSON-line probe contracts
around **existing development executables** for offline subprocess consumers.
No probes, test credentials, fake/test product mode, new RPC framework, loader,
FFI boundary, or command namespace is included here. Sharing a CLI executable is
not sharing a stable Rust ABI: rlibs/dylibs are not drop-in plugins across
compiler/features/source graphs, and direct SDK dependents still rebuild from
source when APIs change.
