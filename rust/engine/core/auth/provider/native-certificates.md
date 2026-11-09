<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native macOS certificate stores

The default-off `certificate-store-macos` backend uses an existing macOS Keychain
identity. It does not import a certificate bundle, read a PEM/PFX private key,
export a private key, discover the first identity, or fall back to another
provider. Linux PKCS#11 is a different explicitly approved backend, not a
replacement for this native macOS implementation.

## Explicit selection and trust

The SDK caller injects:

- `MacCertificateSelector::new(keychain_path: String, certificate_sha256: [u8; 32])`:
  an absolute macOS Keychain path with no lexical aliases, and SHA-256 of the
  **complete public certificate DER**, not a subject name or SHA-1 thumbprint.
- `MacKeychainHelper::new(executable: PathBuf, sha256: [u8; 32], deadline: Duration)`:
  an absolute installed helper path, an independently trusted installer or
  administrator SHA-256 digest, and a nonzero deadline of at most 120 seconds.
- `MacStoreSigner::new(selector, helper)`, implementing the existing
  `CertificateSigner` interface. Constructors and debug formatting are inert.
  `binding()` includes a digest of every selection and executable-trust component.

The helper and all path ancestors must be root-owned, nonsymlinked and not
group/other writable. The helper must be a regular, executable file with one hard
link, at most 64 MiB. Before each execution, a bounded blocking worker hashes an
open descriptor and compares device, inode, ownership, permissions, link count,
size and modification/change timestamps before and after the read against the
path. It rejects a mismatch. The administrator must secure the installation and
loader dependencies. Hash pinning is not a sandbox, code-signature verification
or protection against a malicious administrator changing a root-custodied path
between verification and execution.

No executable is found through PATH, HOME, a personal profile or a development
directory. The child receives an empty environment and `/` as its working
directory. The library does not read environment configuration.

## Why a one-shot helper is required

Security.framework's `SecKeychain::disable_user_interaction()` mutates
**process-global** state, and its guard re-enables interaction on drop. Calling it
inside an SDK process would interfere with unrelated Keychain consumers.

For certificate signing, only `src/bin/keychain_helper.rs` calls this API.
Device-code protected storage uses a separate
[one-shot helper and schema](protected-store-macos.md), sharing only the private
process transport; it does not depend on certificate/headless features.
Its `fabric-protected-store-helper` binary must declare
`required-features = ["interactive"]`, not `certificate-store-macos`.
The CLI captures `FAB_RUST_PROTECTED_STORE_HELPER` and the independently trusted
`FAB_RUST_PROTECTED_STORE_HELPER_SHA256`; neither input selects certificate signing
or replaces the certificate helper's separately injected trust.
The certificate helper validates its entire
request first, establishes the no-UI guard before opening the explicitly selected
Keychain, and retains it until every native identity/key handle has been dropped.
It never unlocks a keychain, requests authentication, changes an ACL, or prompts.
Locked, inaccessible or interaction-requiring keys fail closed. Help, version,
unknown arguments, malformed input and non-macOS execution do not access a store.
No new workspace unsafe boundary is introduced.

The identity search uses `ItemSearchOptions` with the explicit keychain list,
identity class, reference results, `skip_authenticated_items(true)`, and
`Limit::Max(257)`. More than 256 identities fails with `SearchLimit`; it never
accepts a prefix as a complete result. Each public certificate is bounded at
64 KiB, and exactly one identity must match the complete DER fingerprint.
Both its certificate and private key are obtained from that **same held identity**.

## Native policy and signatures

The native key must explicitly report:

- `kSecAttrIsExtractable` (`"extr"`) as **CFBoolean false**.
- `kSecAttrCanSign` (`"sign"`) as **CFBoolean true**.

Missing attributes and values of the wrong type return `PolicyUnavailable`.
String `"false"`, numeric zero, denied extraction, and absent attributes are not
evidence of non-exportability. The public low-level error preserves this
distinction; the existing assertion interface maps it to its sanitized `Policy`
category.

`SecKeyCopyAttributes` does not promise that every key exposes extractability.
This backend is therefore conditional: an otherwise usable native identity can
still return `PolicyUnavailable`. It never tries exporting the key to infer the
missing policy or substitutes another store/provider.

The safe `core-foundation` 0.10.1 untyped dictionary API has `find`, not the
sometimes assumed `get_as_CFType` method. The helper compares the returned opaque
pointer with the two **typed CFBoolean singleton** pointers. This proves both
type and value without an unsafe cast or a fabricated downcast API. The helper's
offline native test rejects CFString and CFNumber substitutes. These literal
attribute names are pinned to Apple's published constants, including `"extr"`
rather than the incorrect `"expr"`.

The selected public certificate must have valid dates, permit digital signatures
if key usage is present, and contain a complete RSA SPKI with a 2048–4096-bit
modulus. `SecIdentity::private_key()` guarantees private-key class. Native
`RSASignatureMessagePSSSHA256` accepts the **original JWS bytes**, not a digest.
Successful native RSA/PSS signing and independent verification against the
selected RSA certificate establish key type and association. Neither public nor
private key external-representation APIs are called.

The helper verifies the signature before returning it. The SDK host independently
parses the returned public certificate, checks the exact pinned fingerprint and
policy again, and verifies the exact original bytes using ring's
`RSA_PSS_2048_8192_SHA256`. Verification enforces SHA-256/MGF1-SHA-256 and a
32-byte PSS salt. A helper success flag alone cannot publish a signature.
Certificate pinning identifies the caller's enrolled certificate; this is not a
general CA-chain/revocation or OAuth enrollment validation service.

## Finite protocol and lifecycle

The helper runs with **no arguments** for signing. Stdin contains one schema-1
JSON request, then EOF:

```json
{
  "schema": 1,
  "selector": {
    "keychainPath": "/Library/Keychains/explicit.keychain-db",
    "certificateSha256": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                          0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  },
  "signingInput": "aGVhZGVyLnBheWxvYWQ"
}
```

The fingerprint above illustrates the exact 32-byte schema, not a usable
certificate. Requests and replies reject duplicate or unknown keys and trailing
JSON. Binary fields use canonical unpadded base64url. The decoded signing input
and public DER each have a 64 KiB ceiling; **the complete encoded request and
reply must also fit 64 KiB**, so base64 overhead can reject a smaller decoded
value. Existing assertion-engine JWS input is independently limited to 8 KiB.

Stdout contains exactly one bounded object:

```json
{"status":"ok","schema":1,"certificate":"<public DER base64url>","signature":"<signature base64url>"}
```

Failures use `{"status":"error","schema":1,"code":"policy_unavailable"}` or another
static enumerated code. No native OS description, key handle, path, input or
private key is returned. Stderr must be empty; the host bounds it at 4096 bytes
and never publishes it. Stdin writing, bounded stdout/stderr reads and process
waiting run concurrently, avoiding a full-pipe deadlock.

Two independent certificate-only slots bound helper/validation work across SDK
instances. The caller's Tokio runtime owns the async wait; its `spawn_blocking`
pool performs hashing and process ownership. No new runtime is created. The
process owner starts at most three bounded pipe threads (stdin, stdout and
stderr), so the two slots permit at most six pipe threads, including any threads
left behind by quarantined operations.

Before any process can be spawned, the worker verifies the caller runtime has an
I/O driver by registering and dropping a nonblocking loopback UDP socket; no
datagrams are sent. Tokio has no safe non-panicking I/O-enabled query.
Registration panic is caught without replacing the host's global panic hook.
A time-only runtime is rejected before spawn and returns ordinary capacity.

The worker reserves a fail-closed RAII capacity guard **before**
`std::process::Command::spawn`. It obtains the owned std child/PID before doing
any fallible pipe-thread setup; there is no Tokio spawn-then-pipe-registration
interval. Spawn error/panic uncertainty returns `CleanupPending` and quarantines
capacity rather than claiming that an unseen child is dead.

Dropping the caller's future closes a result receiver, **not** the blocking
owner. An atomic cancellation guard also covers unexpected async task drop.
Within its 20-millisecond polling interval, the owner requests termination of
only its owned child, observes actual exit with `try_wait`, and joins completed
pipe threads. It kills/reaps before waiting for a blocked reader/writer, and
releases capacity only after actual exit **and all pipe workers have joined**.
Deadline, setup/write/read failure and oversized output use the same cleanup.
The async wait may return a pending-cleanup failure before this owner finishes;
that does not release its slot or claim native rollback.

A failed or five-second cleanup timeout permanently quarantines the slot rather
than claiming that a possibly live child or pipe worker has stopped. An owned
child's drop path also attempts exact-child cleanup. A cleanup attempt is not
repeated indefinitely on drop. Runtime shutdown cannot silently release an
unconfirmed child slot, and cannot forcibly interrupt a synchronous OS spawn or
filesystem call.

## Parent-owned integration

The integration owner must add, without enabling it by default:

1. An identity feature `certificate-store-macos` selecting `headless`, `serde`
   with derive, `serde_json`, `base64`, `ring`, `x509-parser = "=0.18.1"` with
   default features disabled, and macOS-only `security-framework = "=3.7.0"` and
   `core-foundation = "=0.10.1"` dependencies.
2. The existing centralized Tokio `rt`, `sync`, `time`, `macros` and `net`
   features. The std-first ownership repair no longer requires `tokio/process`;
   this source lane does not alter the parent's existing feature wiring.
3. `#[cfg(feature = "certificate-store-macos")] pub mod certificate_macos;`
   beside the other modules in `headless.rs`. Keep the host/protocol module
   available cross-platform for offline tests; native Keychain code exists
   only in the macOS binary branch.
4. A binary manifest entry named `fabric-keychain-helper`, path
   `src/bin/keychain_helper.rs`, `required-features = ["certificate-store-macos"]`.
   The required feature prevents automatic binary discovery from affecting
   default builds.
5. Explicit caller/CLI selection and installer hash policy. Do not invent a
   development-build digest, ambient helper path or automatic fallback.

This source slice does not edit manifests, shared certificate/headless modules,
CLI composition or the integration owner's lockfile.

## Evidence and remaining acceptance

Baseline: `a9df45501ec53d921c6fc765c726c32979b85931`, with preserved concurrent
lane edits. This is the owner-approved native-store architecture, not a claim of
Python certificate-file parity.

Source-only tests cover selection validation, strict finite request/reply
schemas, unavailable/exportable/unsignable policy, existing licensed public
RSA/PSS vectors, an explicitly synthetic public DER envelope, wrong
fingerprint/message/signature and bounded pipes. Lifecycle regression tests use
real std-owned subprocesses of the Rust unit-test binary, never the native
helper: time-only runtime rejection before spawn, injected setup failure after
actual spawn, dropped waiters, deadlines and oversized output. Their actual OS
exit observations establish reap and capacity retention, rather than a fake
child reporting success. Additional tests cover uncertain spawn and stalled-pipe
quarantine. Native helper tests create only in-memory CFDictionary values and
never open a Keychain.

The assigned source-only lane did not build, run tests, launch a helper, load a
provider, access Keychains, request UI, use a real key or run live signatures.
`rustfmt --edition 2024 --check` over all three new Rust files passed with exit 0.
Source-only assertions for isolated UI suppression, bounded search/protocol,
explicit boolean attributes, no export/unsafe code, cleared child environment,
owned-child cancellation and retained-capacity guards also passed with exit 0.
These checks do not prove Rust type checking or registered tests.
Independent review cycle 1 identified the Tokio spawn-before-pipe-registration
ownership interval. The repair uses pre-spawn I/O preflight, a pre-spawn
fail-closed reservation, std-first PID ownership, and kill/reap before bounded
pipe joins. The added real-process regression tests remain unexecuted in this
source-only lane and require the parent's build slot and independent re-review.
The parent must run all-target focused checks/tests for the optional feature,
and independent implementation/lifecycle review under its build slot.
Native macOS CI must establish compilation and offline tests. Separately
authorized manual macOS acceptance with a suitably provisioned nonexportable RSA
identity remains required for actual native-store signing and no-UI behavior;
Windows fake tests do not establish that evidence or GAP4/GA acceptance.

### Pinned API references

- [security-framework v3.7.0 Keychain](https://github.com/kornelski/rust-security-framework/blob/v3.7.0/security-framework/src/os/macos/keychain.rs)
- [security-framework v3.7.0 identity](https://github.com/kornelski/rust-security-framework/blob/v3.7.0/security-framework/src/identity.rs)
- [security-framework v3.7.0 item search](https://github.com/kornelski/rust-security-framework/blob/v3.7.0/security-framework/src/item.rs)
- [security-framework v3.7.0 key signing](https://github.com/kornelski/rust-security-framework/blob/v3.7.0/security-framework/src/key.rs)
- [Apple Security-61901.40.77 attribute constants](https://github.com/apple-oss-distributions/Security/blob/Security-61901.40.77/OSX/sec/Security/SecItemConstants.c)
- [core-foundation 0.10.1 dictionary](https://docs.rs/core-foundation/0.10.1/src/core_foundation/dictionary.rs.html)
- [core-foundation 0.10.1 CFBoolean](https://docs.rs/core-foundation/0.10.1/src/core_foundation/boolean.rs.html)
- [x509-parser 0.18.1 public keys](https://docs.rs/x509-parser/0.18.1/src/x509_parser/public_key.rs.html)
