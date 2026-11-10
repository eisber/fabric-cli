<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI update engine

`fabric-cli-update` is a separate CLI-layer Rust library for bounded, anonymous
GitHub Releases updates. Default features are empty. Its default graph has no SDK,
identity, HTTP client, runtime, global logger, settings store, archive extractor,
or process launcher. Construction performs no I/O. The optional
`bounded-helper` feature enables the dedicated installed `fab-rs-update-helper`
binary and its parent protocol. `running-replacement` is a dependency-free
compatibility alias; it no longer selects `self-replace`. The optional CLI
`self-update` feature composes anonymous HTTP and this helper without enabling
authentication. Building the CLI alone does not install a dependency's binary:
the standalone installer must separately ship and protect the helper.

## Status and deployment inputs

The engine, signature verifier, injected end-to-end contracts, and real
**non-running standalone file replacement** adapter are implemented here. An
optional `RunningInstaller` prepares an update and launches that installed
helper. Its parent receipt is **pending**, not proof of installed bytes.
Checking, downloading, and installation have different types. An HTTP fake does
not establish real HTTP interoperability.

The owner selected **`microsoft/fabric-cli` as the canonical production origin**.
The optional CLI pins that origin and the native product `fab-rs`; it never uses
the development fork or accepts an origin override. The reusable core still
requires an explicit `Trust` value. **Production signing identities, pinned
public-key distribution, and actual Rust release artifacts have not been
provided.** There is no built-in or generated production key.
`UpdateEngine::new(None, ...)` fails with `TrustRequired`; the CLI maps missing
build-time trust to actionable `NotConfigured` before HTTP or cadence I/O.
Release builds cannot apply updates without the trusted key and standalone
custody configuration.
Native package signing/notarization and an Ed25519 update-manifest signature are
different controls; this library does not sign packages or establish either
publisher identity. No release, live download, signing, or production trust is
authorized by these APIs.

The owning CLI must provide:

- Reviewed out-of-band repository/key configuration through `Trust`.
- Its actual current semantic version, channel, native target, and exact
  feature-set variant through `Identity`. Do not infer identity from downloaded
  metadata. Refresh current identity after installation.
- An anonymous `HttpTransport` implementing the strict contract below.
- A `Control` implementation supplying wall/monotonic clocks and cancellation.
- Explicit policy and invocation classification; this library never runs a
  startup hook or reads configuration.
- Proven standalone installation custody and either `FileInstaller` for a
  non-running file or the explicitly selected `RunningInstaller` boundary.
  Running updates require a revision-2 installer receipt that also pins the
  already installed helper's fixed sibling path and exact digest.

Native package publisher identities, Windows helper/platform acceptance, durable
health rollback, and real release acceptance remain **deployment blockers**.
This implementation is not production-enabled or a GA updater claim.
The owner's Windows signing direction is the existing Microsoft internal signing
service, not a newly created Azure account. This component creates no signing
accounts, changes no credentials, and assumes no signing identity or public key.

## Public flow

1. Construct `Repository`, `Trust`, `Identity`, `Limits`, and `UpdateEngine`.
2. Call `check` with `LatestRustStable` or one `ExactTag`, or supply bounded GitHub
   release JSON to `check_release_json`. A Rust tag is exactly `rust-vSEMVER`.
3. Inspect `CheckedUpdate::manifest()`. This has verified metadata, not binary
   bytes, and does not mean an update is installed.
4. Call `download`; this consumes the candidate, preserves the original overall
   network deadline, verifies exact length/SHA-256 and basic native executable
   type/architecture, and returns a non-cloneable `DownloadedUpdate`.
5. Only an explicit non-running install decision calls `install` with standalone custody and
   a caller-approved `Installer`. Installation consumes downloaded bytes and
   returns an `InstallReceipt` only after the adapter reports replacement.
   For a running image, call `RunningInstaller::queue` with an **outer-main-owned**
   `ParentLifetime`. It deliberately does not implement `Installer`: queuing must
   not become an installed receipt.

All errors remain typed. There are no hidden retries, release-history
pagination, fallback keys, caches, cached-error-to-success conversions, or
downloaded-code execution. One check verifies exactly one selected release. Equal
versions, semantic downgrades, and build-metadata-only changes are ineligible.
Preview identities require an exact prerelease tag and GitHub prerelease flag;
stable identities require stable versions and non-prerelease releases.
Drafts and cross-channel/target/variant candidates are rejected.

Installation defaults to the current **same major** only, including automatic
`install` policy. A check may report a signed newer major with
`requires_major_opt_in=true` without installing it. A major upgrade requires
explicit `update --apply --allow-major`; explicit apply without that flag is
still refused. `--allow-major` is invalid on `--check` or without `--apply`.
Automatic policy has no major-upgrade override and always passes
`allow_major=false`. Both the engine and real installer adapters enforce the
gate before creating locks/stages/rollback or entering native replacement.
The opt-in relaxes only the major boundary, not signature, expiry, custody,
target, feature variant, or downgrade checks. No extra version-zero minor
restriction is inferred: `0.1` to `0.2` remains same-major under this policy.

`LatestRustStable` requests one public releases page, bounded to 20 candidates by
default and at most 32. It ignores other products' tags, excludes drafts and
prereleases, rejects ambiguous semantic versions, and selects the highest
`rust-vSEMVER` by semantic precedence, not lexical or publication order. It does
not paginate or fall back to another release if the selected manifest fails.
The legacy `LatestStable` endpoint is not Rust product/channel proof: its result
still must have a Rust tag and the exact signed product/target/variant manifest.
The CLI never uses that mixed-product endpoint.

Selecting the official repository does not establish that it has usable Rust
releases or assets. A page containing only Python tags, a missing exact Rust tag,
or a selected Rust release without required manifest/signature/asset metadata
fails with `ReleaseUnavailable`, mapped by the CLI to actionable `NotConfigured`.
It does not infer a Rust tag, promote a numerically newer Python version, or fall
back to an older candidate when the selected Rust release is incomplete.

## Signed protocol v1

Each release must contain one pair per exact target and feature variant:

```text
update-{target-triple}-{variant}.json
update-{target-triple}-{variant}.json.sig
```

The signature file contains **exactly 64 raw Ed25519 bytes**. The manifest's exact
UTF-8 bytes are size-bounded and verified with the independently pinned public
key **before JSON parsing**. JSON is not canonicalized. All manifest fields are
required; duplicate and unknown fields are rejected, including nested schema
values. An illustrative manifest shape is:

```json
{
  "protocol": 1,
  "product": "fab-rs",
  "repository": "publisher/product",
  "tag": "rust-v1.10.0",
  "version": "1.10.0",
  "channel": "stable",
  "target": "x86_64-pc-windows-msvc",
  "variant": "default-no-features",
  "asset": "fab-rs-windows-x64.exe",
  "bytes": 123456,
  "sha256": "<64 lowercase hexadecimal characters>",
  "expires_at": 1800003600
}
```

The example is not a usable publisher configuration or a signed production
manifest. Keys in tests are generated from synthetic test-only seeds.

The manifest binds product, repository, exact Rust tag, canonical semantic version, channel,
native target, exact feature variant, asset leaf, byte count, SHA-256, exclusive
expiry, and protocol version. The default maximum remaining validity is seven
days. Unknown protocol versions fail. The caller must supply a trustworthy wall
clock; rollback/skew outside the allowed validity window fails closed.
Native executable header checks reject scripts, archives, PE DLLs, and mismatched
architectures; they are not a full OS-loader or native-signature validator.

The seven-day validity bound requires an explicit publisher refresh/re-signing
policy if an older release must remain eligible. The client never extends an
expiry, re-signs metadata, or assumes an immutable GitHub asset can refresh
itself. An expired selected release fails closed until an authorized publisher
provides fresh valid metadata through its reviewed publication policy.

This is not a TUF implementation. A valid Ed25519 signature plus SHA-256 does not
provide a trusted clock, persistent signed-generation high-water mark, revocation,
withdrawal detection, key rotation, or protection against withholding a newer
release. Replay protection here is limited to strict semantic upgrade checks
against the caller's current identity, expiry checks, and pinned installed-file
state during replacement. A stale but unexpired upgrade can still be presented.
Source-commit provenance, minimum OS/libc versions, and native signature acceptance need
publisher policy and, where required, an explicitly revised signed protocol.
GNU target names alone do not prove a binary's glibc or OS compatibility.

Defaults bound release JSON to 64 KiB, manifest to 16 KiB, each raw executable to
64 MiB in memory and on disk, release assets to 64, redirects to three per
request, and the complete network operation to 30 seconds. Limits themselves
have hard upper bounds. Release metadata and candidate lists are parsed only
inside their byte bound. Executable assets are raw files; no ZIP/TAR extraction
is supported.

## HTTP injection contract

GitHub's public REST release schema must bind `url`, numeric release `id`, and
`html_url` to the exact pinned repository/tag. Extra GitHub API fields are
allowed; manifest schema fields are not. Duplicate asset names are rejected.
`browser_download_url`, API-provided digest values, author information, and
release text never establish trust.

Initial URLs are generated only from validated repository/tag/asset leaves:

```text
https://api.github.com/repos/{owner}/{repo}/releases/latest
https://api.github.com/repos/{owner}/{repo}/releases?per_page={bounded-count}
https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}
https://github.com/{owner}/{repo}/releases/download/{tag}/{asset}
```

No API redirects are followed. Asset redirects must be absolute HTTPS URLs on
these exact origins, without user information, alternate ports, fragments,
backslashes, whitespace, or control characters:

- `https://release-assets.githubusercontent.com`
- `https://objects.githubusercontent.com`
- `https://github-releases.githubusercontent.com`

The engine validates every redirect and bounds the chain. Signed CDN query
strings remain request-local and are never included in errors. Repository
renames/transfers and new CDN origins fail closed pending reviewed configuration.

The transport must enforce TLS certificate validation, use anonymous GET only,
disable automatic redirects/retries/decompression, send no Authorization,
cookies, SDK credentials, or ambient credentials on **any** request, bound
read-ahead, and stream all bytes through `ResponseSink`. Stop on sink errors.
It must enforce the supplied overall monotonic deadline while connecting and
reading, and observe cancellation. A synchronous callback that blocks forever
cannot be preempted by this library and is not an acceptable production adapter.
The engine independently checks signals and bounds at all returned/chunk
boundaries, including a transport that accidentally suppresses sink errors.
The core has no network implementation. The optional CLI's private
`update_host.rs` provides a **real adapter** over `NativeHttpClient` and
`azure_core::http::HttpClient`, on a dedicated caller-owned runtime. It sets only
anonymous headers, explicitly requests identity encoding, rejects encoded
responses, bounds content length and streamed bytes, and enforces the overall
deadline across request/body reads. The shared native client does not retry or
follow redirects; only this engine decides whether another anonymous request is
allowed. Fabric service-origin policy and credential policies are not reused.
Injected-wire tests cover the adapter independently of the native client's
existing TLS/wire contracts; no live GitHub request or actual signed release
acceptance is implied.

References:
[GitHub releases REST API](https://docs.github.com/en/rest/releases/releases),
[release asset responses and redirects](https://docs.github.com/en/rest/releases/assets#get-a-release-asset).

## Replacement and rollback boundary

`FileInstall` pins an absolute path, the SHA-256 of the current installed file,
and a fresh host-supplied lock nonce. The host must prove standalone custody and
exclusively control the parent directory; all cooperative writers must use the
same lock. Unknown/package-manager custody is never overwritten. This boundary
does not defend against an adversarial same-user writer or elevated process
racing path names.

`FileInstaller` rejects symlink/junction paths, directories, read-only files, and
the current process executable on all platforms. It performs:

1. An exclusive `create_new` lock with nonce ownership checks.
2. A bounded current-file hash check against the pinned installed digest.
3. Exclusive same-directory staging in 64 KiB chunks, signal checks, retained
   permissions, file sync, and staged hash verification.
4. A retained same-directory hard link to the previous file, never overwriting an
   existing rollback. Filesystems without hard-link support fail without replace.
5. A final lock/current digest/expiry check and a single native `fs::rename`
   replacement. Rename failure leaves the old file and rollback intact.

Only exact operation-owned staging/lock paths are cleanup candidates. A lost
nonce is not cleaned up as though still owned. Existing files are never deleted.
Pre-commit errors can leave a retained rollback or cleanup debt, available at the
documented deterministic paths; no broad startup cleanup is performed.
After the replacement linearization point, a cancellation or lock-cleanup error
does not claim the old file is still installed. A receipt explicitly reports
`cleanup_pending` when the committed operation's lock cannot be removed.
The previous file remains at `rollback_path` for host-managed recovery; the
adapter does not silently launch it, remove it, or select a downgrade.
Power-loss durability of directory metadata and cross-platform running-file
replacement are not certified.

### Dedicated bounded helper

`RunningInstaller` requires the actual current executable, its digest pin, a
fresh nonzero nonce, standalone custody, and explicit immediate-exit agreement.
It creates an exclusive nonce-named transaction directory in the installation
root, stages bounded exact bytes, and saves the signed manifest/signature and
original installer receipt. A strict cooperative lock binds the nonce,
transaction path and request digest. Pre-spawn cancellation cleans only its own
known files. Once spawn is ambiguous, recovery evidence is retained.

The only launched program is the fixed installed sibling
`fab-rs-update-helper.exe` on Windows or `fab-rs-update-helper` elsewhere, whose
digest is pinned in the protected installer receipt. Launch uses its absolute
path, the installation directory as explicit child CWD, an explicitly cleared
environment, and no shell or process lookup. There is no `cmd.exe`, script,
raw OS FFI, elevation, PATH editing, global environment/CWD mutation, downloaded
helper, executable replay, or helper self-deletion.

On Windows, Winsock provider initialization requires `SystemRoot` even for
loopback IPC. Clearing it causes `WSAEPROVIDERFAILEDINIT` (10106) before helper
readiness. The protected installer receipt therefore has an optional
`windows_system_root` path: null/absent on non-Windows, required for Windows
helper launch. The installer pins a local-drive OS runtime directory using either
an ordinary DOS spelling such as `C:\Windows` or its canonical verbatim-drive
spelling. The host rejects UNC/device namespaces, relative and parent-traversing
roots, checks all original ancestors for reparse points, then validates the
canonical drive-rooted directory and `System32/mswsock.dll` component.
Only that validated canonical value becomes **SystemRoot** in the otherwise
cleared child environment. This is normalization at the validated-use boundary:
the protected receipt retains its original pin unchanged across upgrades.
Missing input fails closed. No ambient environment/PATH fallback,
runtime argument override, signing identity or credential is introduced.
The installer is accountable for that OS-root pin; these filesystem checks are
not independent proof of drive-mapping locality, native DLL signatures or OS
authenticity. The offline fixture
explicitly supplies its local Windows runtime root through its test installer
record, not through release metadata or a production test-key fallback.

The production helper obtains publisher trust **only at build time**:
`microsoft/fabric-cli`, product `fab-rs`, `FAB_RELEASE_PUBLIC_KEY`, required
`FAB_RELEASE_VARIANT`, the compiled native target and `FAB_RELEASE_CHANNEL`.
No request/argument/runtime environment supplies a key, origin, program or
variant override. Missing signing/key inputs fail closed. It independently
verifies the exact detached signature and product/origin/tag/channel/target/
variant/version/expiry, the old custody snapshot/current image/helper digest,
and exact candidate bytes before mutation. The helper is an installed trusted
component, not OS-signature or signing-service acceptance in itself.

The actual lifecycle is:

1. `prepared_verified`: same-directory staged bytes and signed evidence.
2. `pending_parent_exit`: helper independently verified the transaction and
   returned the nonce over bounded loopback TCP. Parent reports
   `installed=false`, `exit_required=true`.
3. `old_displaced`: verified rollback is retained and old image was moved into
   the exact transaction directory. A normal new-image rename failure restores
   the old path; abrupt helper death requires the next bounded reconciliation.
4. `applied`: new bytes are present, but the old receipt can remain. Failure here
   is recovery-required, never "unchanged" or completed installation.
5. `installer_receipt_committed`: receipt version/digest advance to the signed
   candidate; its original publisher/custody/helper pins remain unchanged.
6. `reconciled`: installed/receipt/rollback digests agree, exact-owned transient
   state is removed, and versioned verified rollback remains.

`ParentLifetime` **must be owned by the outermost `main` frame** and borrowed
through startup/explicit execution. It retains the connection on every
post-ready error and output failure, without a mutable global or leaked socket.
The host must finish diagnostics and exit, never resume business dispatch.
Socket EOF releases a lifetime capability; it is **not proof of OS process
death**. Windows sharing failures therefore receive finite post-EOF retries.
No PID or stale-PID guess is used. A safe standard-library kernel file lock
distinguishes an active helper from an exact transaction eligible for
reconciliation. Unknown, old-format and foreign locks are never reclaimed.
The readable JSON nonce record and the exclusive kernel lease are separate
files. Windows byte-range locks deny reopening a locked file through another
handle, even in the owning process. A nonce-named sibling lease contains exactly
32 nonce bytes, validated through the handle holding its kernel lock; the JSON
record remains readable for repeated ownership checks. The lease stays held
through journal cleanup and JSON-lock removal, then only its exact matching
nonce file is removed. Missing or unknown leases are never synthesized or
reclaimed as if owned.
An explicit cancellation byte prevents apply before lifetime release; failed
cancellation delivery is ambiguous and requires reconciliation.

Readiness has a five-second deadline; lifetime release has a 30-second deadline;
Windows sharing retry has a 30-second deadline with 20-ms intervals. The helper
also checks an independent overall 90-second monotonic cap at filesystem/hash
boundaries. Parent reconciliation waits at most 35 seconds, and unready owned
children have bounded kill/`try_wait` cleanup. No blocking `wait()` or child
respawn loop exists. Safe filesystem APIs cannot interrupt an OS syscall that
itself hangs; this is not a hard-real-time OS guarantee.

An explicit apply inspects an exact existing transaction before HTTP. An active
helper reports pending. After binary commit it can finish receipt commit; after
old-image displacement it restores the original runnable path and reports
`installed=false`. A completed reconciliation reports actual bytes and receipt
facts. It does not replay the failed replacement or the business command.
An expired/invalid recovery manifest fails closed and needs installer/operator
intervention, not relaxed trust.

Rollback names bind **version + SHA-256 + nonce**, never a one-shot fixed name.
This implementation uses bounded **operator-managed retention**: at most two
verified prior images are named in the receipt. A third upgrade fails with
`Retention` before lock/stage creation. The owning installer must reconcile
retention and its protected receipt; do not delete foreign, quarantined or
pending rollback files to bypass the cap. A reconciled aborted update removes
only its exact verified unused rollback. Two successive upgrades do not require
manual lock/stage reclamation. The stable shipped helper is never deleted or
silently replaced by these transactions.

The exclusive installer-owned directory contract remains mandatory.
Canonicalization and repeated symlink/**all Windows reparse-point** guards do
not defend against an adversarial same-user/elevated writer racing names.
Directory-entry power-loss durability, general crash recovery, OS code-signature/
notarization enforcement, key rotation/revocation, real-platform acceptance and
health-verified rollback are **not established**. This is not a GA claim.

## Explicit automatic policy

`Policy` defaults to `Off`. `CheckSchedule` is a pure, memory-only 24-hour
monotonic cadence. The host must explicitly select `Check` or `Install`;
construction/configuration does not initiate network work. Failed attempts also
consume the interval. Backwards monotonic time resets the baseline and skips,
avoiding a tight retry loop. Wall-clock time does not control periodic cadence.
The core schedule does not persist across process restarts. The optional CLI
adds a bounded, strict-schema cadence file under an explicitly configured
directory, with an exclusive lock and atomic state replacement. Attempts,
including failed HTTP checks, consume the interval. Cross-process timestamps
necessarily use wall time; backwards skew resets the baseline and skips, and
corrupt/unavailable state produces a visible diagnostic with no check. This
does not claim monotonic time or cryptographic freshness across reboots.
An owned RAII lock guard is active immediately after exclusive creation, before
the first content write. Post-create write failures close the owned file and
remove that exact lock before returning an error; explicit cleanup errors remain
visible and a pre-existing foreign lock is never removed or written.

Help/version, CI/headless/batch automation, and nonterminal/redirected invocations
always return `Skip`, even with `Install`. An explicit update invocation may use
the engine in automation, but no ordinary command or library call implicitly
replaces its binary. The startup decision does not bypass trust, custody,
integrity, expiry, cancellation, or installer requirements. Pre-commit automatic failures leave
the ordinary command usable offline and do not change its stdout or exit status.
A queued automatic replacement is terminal: the process exits **75**, not
success, instead of dispatching the original command. Its stderr notice states
`requested_command_executed=false` and `rerun_required=true`; the user must rerun
the requested command explicitly. There is no replay/re-exec or successful
business-operation claim. Its notice never calls a pending update installed.
Explicit `update --apply` may exit 0 for successfully queued work, but JSON
distinguishes `pending_parent_exit`/`installed=false` from a reconciled completed
result. A recovery-required native failure must also
stop rather than pretending ordinary dispatch is safe. Help/version,
batch/REPL recursive dispatch and automation never enter this startup hook.
Automatic startup never registers a process-global signal handler; only an
explicit apply command owns its Ctrl+C signal source until immediate exit.

## Optional CLI configuration

`fabric-cli/self-update` is off by default. It registers `update --check`
(also bare `update`) and the distinct `update --apply`; `--tag rust-vSEMVER`
selects exactly one release. No URL, token, repository override, or runtime key
argument is accepted.

| Input | Boundary | Meaning |
| --- | --- | --- |
| `FAB_RELEASE_PUBLIC_KEY` | Build time only | Exactly 64 hexadecimal characters, one independently trusted Ed25519 public key; missing/invalid means `NotConfigured` before HTTP |
| `FAB_RELEASE_CHANNEL` | Build time only | `stable` (default) or `preview`; preview requires an exact tag and a prerelease build identity |
| `FAB_RELEASE_INSTALL_CUSTODY` | Build time only | Required `standalone` build gate; not sufficient without a matching installer receipt. Absent, unknown, and package-manager builds are never overwritten |
| `FAB_RELEASE_VARIANT` | CLI build-time assertion, helper build-time requirement | Must equal the CLI's actual compiled variant and helper's independent pin; cannot override it |
| `FAB_RUST_UPDATE_POLICY` | Explicit runtime policy | `off` (default), `check`, or `install`; never inferred from another setting |
| `FAB_RUST_UPDATE_STATE_ROOT` | Runtime, automatic policy only | Existing absolute, caller-owned directory for bounded cadence state; no default path or credential/configuration migration |
| `FAB_RUST_UPDATE_INSTALL_RECEIPT` | Runtime, apply only | Explicit absolute revision-2 `.fab-rs-install.json` beside the executable, written/protected by the trusted standalone installer and bound to the installed CLI/helper |

The variant is `self-update` followed, in order, by enabled `headless`,
`native-host`, `native-read`, `repl`, `windows-broker`, joined with `+`.
It includes transitive feature activation. Publishers must produce an exact
manifest/asset pair for that identity. Build metadata and a public key are not
proof that a distribution still has standalone custody if an external installer
repackages the same binary: that installer must select the correct build custody
and issue/protect the correct local receipt. This is a release/installation gate, not
permission to overwrite a package-manager installation.

An apply receipt has the following strict shape (example values are not an
installation authorization):

```json
{
  "schema": 2,
  "product": "fab-rs",
  "repository": "microsoft/fabric-cli",
  "custody": "standalone",
  "executable": "C:\\Tools\\fab-rs.exe",
  "version": "0.1.0",
  "target": "x86_64-pc-windows-msvc",
  "variant": "self-update",
  "sha256": "<64 lowercase hexadecimal characters of the installed executable>",
  "helper": "C:\\Tools\\fab-rs-update-helper.exe",
  "helper_sha256": "<64 lowercase hexadecimal characters of the installed helper>",
  "windows_system_root": "C:\\Windows",
  "backups": []
}
```

The host bounds receipt/request bytes to 16 KiB, rejects duplicate/unknown fields and
symlink/junction paths, requires that exact sidecar beside the actual executable,
and verifies product/origin/version/target/compiled variant/current digest before
HTTP. Unix receipts writable by group/others are rejected. Unknown or
`package-manager` custody is refused even when the binary is writable. The updater
does not create or promote an ownership receipt itself. This local protected
installer record establishes custody, not publisher authenticity or OS signing;
the independently pinned manifest key is still mandatory. The helper advances
the version/digest receipt only after exact binary commit. The installer remains
accountable for retention maintenance and protecting helper/custody records.

The parent binary must call the private `update_host::startup` only once after a
successful eligible outer parse and before authentication/dispatch; honor its
optional stop result. Recursive batch/REPL execution must not call it.
The host's pure invocation guard excludes `Display`, `Batch`, explicit `Update`,
broker `Auth`, and `LocalCommand::Version` (the named `version` command), before
reading policy, trust, receipts, cadence, terminals, or creating components.

The binary integration hook contract is:

1. Under `self-update`, declare parser `mod update`, re-export
   `UpdateAction`/`UpdateCommand`, add `Invocation::Update(UpdateCommand)`, register
   `update::register(command)`, and map successful root `update` matches through
   `update::parse(&matches).map(Invocation::Update)` before generic family routing.
2. Declare binary `mod update_host` under the same feature. After successful
   outer parsing, route explicit `Invocation::Update` to `update_host::run`
   before ordinary state/authentication construction.
3. For other valid outer invocations, call `update_host::startup(&invocation)`
   before state capture/dispatch. Honor its `Option<CommandOutcome>`, including
   automatic rerun status 75; otherwise dispatch the original invocation once.
   Both hooks borrow the same `ParentLifetime` created in outermost `main`, not
   in `run` or inside a temporary expression. Retain it through all error/output
   handling before returning from `main`.
4. Reject explicit `Update` during batch/REPL recursive dispatch with structured
   `update_context`, without calling the host, constructing credentials, or
   replacing a raw-mode/mid-batch process. Never replay the original invocation.

## Offline validation

Default contracts use only synthetic local keys, fake HTTP, injected installers,
and non-running native-header fixture files; those assets are never executed.
The separate opt-in native harness executes only its locally built fixture.
Filesystem tests prefer `FAB_UPDATE_TEST_ROOT` when supplied. Ordinary
`cargo test` users otherwise use the OS temporary root, create uniquely named
owned children, and remove only their own known files; no test is silently
skipped. Agent validation must explicitly set `FAB_UPDATE_TEST_ROOT` to its
already-created, authorized absolute session artifact directory and must not
use that fallback. This selection exists only in test helpers: production trust,
custody, cadence, and install paths do not acquire a fallback or default.

From `rust`, after obtaining the shared-host validation lease:

```powershell
# Agent runs: set FAB_UPDATE_TEST_ROOT to the granted absolute artifact root first.
cargo test --offline --locked -p fabric-cli-update -- --test-threads=1
cargo clippy --offline --locked -p fabric-cli-update --all-targets -- -D warnings
# Separately authorized local native-helper proof, using copied fixture binaries:
cargo test --offline --locked -p fabric-cli-update --features fixture-harness --test native_fixture -- --test-threads=1
# CLI composition and injected HTTP/cadence contracts:
cargo test --offline --locked -p fabric-cli --features self-update update -- --test-threads=1
```

The contract suite covers signature/JSON ordering, tampering, strict schema,
identity bindings, semantic version order/replay, expiry, bounds, unsafe
redirects, service errors without retries, cancellation/deadlines, policy,
package-manager custody, injected write/replacement failures, real fixture
replacement/rollback retention, lock loss, permission refusal, and actual
replacement failure. The feature-gated native harness copies only a locally built
parent executable and a separate test-only pinned helper into the artifact root,
clears child environments, signs locally modified native fixture bytes with a
synthetic test key, exercises two upgrades, outer-frame late errors, retention
caps, finite parent waits and both rename/receipt recovery windows. Its seed and
fault hooks exist only in separate fixture targets; the production helper never
calls them, including all-feature builds. The old self-replace fixture was
retired and is not new-helper evidence. A passing run proves that exact local
platform trace, not universal crash/health rollback. Passing any of
these tests is not production-key, real-network, or native package-signing acceptance.
