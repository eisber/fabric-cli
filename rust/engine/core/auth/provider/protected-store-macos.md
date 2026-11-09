<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Isolated macOS device-code protected storage

The default-off `interactive` feature preserves the existing **user-default
legacy Keychain** store. The service remains
`Microsoft.Fabric.Cli.Rust.DeviceCode.v1`; the account remains the lowercase
SHA-256 of the explicit configuration-root bytes. This is not a migration to
the data-protection Keychain. Windows Credential Manager and Linux's no-Prompt
Secret Service path are unchanged.

## Caller-injected trust

On macOS, construct `OsSessionStore::new(root)` and explicitly attach
`interactive::macos::MacProtectedStoreHelper` with
`OsSessionStore::with_macos_helper`. The helper constructor accepts an absolute
installed executable path, an **independently trusted** SHA-256 digest and a
nonzero deadline of at most 120 seconds. Constructors do not inspect files,
access a store, change UI policy or launch a process. Missing helper
configuration fails closed for protected operations. The helper constructor and
attachment return `Unsupported` off macOS; neither chooses another provider.

The CLI captures two explicit, non-secret installation inputs on macOS:

- `FAB_RUST_PROTECTED_STORE_HELPER`: absolute installed helper path.
- `FAB_RUST_PROTECTED_STORE_HELPER_SHA256`: 64 hexadecimal digits from trusted
  installer/administrator policy, **not** a digest calculated from an arbitrary
  discovered executable. The CLI uses a 30-second operation deadline.

The SDK never reads these variables. Neither host searches PATH, HOME, a
developer output directory or the certificate helper. Each execution verifies
the existing root-custody, symlink, ownership, permissions, hard-link, file-size,
descriptor/path identity and SHA-256 checks shared with certificate signing.
Hashing and process ownership run on the caller's blocking pool.

## Native isolation and status

Only the separate `fabric-protected-store-helper` executable calls
`SecKeychain::disable_user_interaction()`. Its closed request is validated before
that process-global switch is changed. The SDK process never changes
Security.framework UI state. Every native read, upsert and delete occurs in that
one-shot child while its no-UI guard is alive.

Per-query `u_AuthUI`/`u_AuthUIF` flags are not a substitute: the legacy backend
does not honor them consistently for these operations. A read-only
`skip_authenticated_items(true)` search cannot implement writes/deletions.
After establishing the no-UI guard, the helper captures
`SecKeychain::default_for_domain(SecPreferencesDomain::User)` once and holds that
exact legacy keychain through the operation. With security-framework 3.7.0,
reads use its `find_generic_password` method; updates use the found item's
`set_password`. Only item-not-found permits `add_generic_password` on that same
held keychain. Deletion uses `ItemSearchOptions::keychains` with only the held
keychain, generic-password class and exact service/account, then its
status-returning `delete` method (`SecItemDelete`). No operation uses an ambient
search list or an unscoped password query.

Deletion preserves native status rather than keyring's status-discarding
deletion method. **Only** `errSecItemNotFound` (-25300) is an idempotent deletion.
Interaction-required, denied, locked, malformed and all other failures remain
failures, including after the existing durable logout tombstone is published.
Cancellation never claims native rollback.

## Protocol and lifecycle

The helper receives **no arguments**, an empty environment and `/` as its
working directory. One JSON request arrives on stdin followed by EOF; one JSON
reply is returned on stdout. Secrets never enter argv, environment, diagnostics
or logs. Stderr must be empty and is bounded/discarded by the parent.

The schema family is exactly `fabric-protected-store-v1`, separate from
certificate signing. `read`, `upsert` and `delete` variants reject unknown and
duplicate fields, trailing JSON and invalid account locators. Upsert/read secrets
are canonical unpadded base64url, with a decoded 256-KiB ceiling; complete encoded
requests/replies fit 352 KiB. Read replies and write/delete acknowledgements are
not interchangeable. Native errors return only static enumerated codes.

The private `macos_helper.rs` transport is shared with certificate signing, not
gated on headless/certificate support. It retains std-first child ownership,
pre-spawn I/O-driver checks, bounded concurrent pipes, exact-child termination,
actual reap, joined pipe workers and fail-closed permit quarantine. Protected
storage has two independent slots; the certificate-only two-slot budget is
unchanged. A session operation's blocking owner retains its file lock through
helper cleanup. Dropping its async waiter signals cancellation; timeout returns
no token while the blocking owner still performs bounded cleanup. Unconfirmed
cleanup quarantines capacity rather than asserting rollback or child exit.

## Parent-owned manifest wiring and validation

No shared manifest or lockfile is changed by this repair. In
`fabric-identity/Cargo.toml`, the integration owner must add:

```toml
[[bin]]
name = "fabric-protected-store-helper"
path = "src/bin/protected_store_helper.rs"
required-features = ["interactive"]
```

The current default-off `interactive` feature already selects every dependency
used here, including `base64`, `ring`, `serde`, `serde_json`, `tokio`,
`security-framework` and `thiserror`. Workspace Tokio already enables `net`,
`sync` and `time`; no `tokio/process`, new package, certificate/headless feature
edge, dependency or lockfile change is needed. The existing certificate helper
entry and feature remain unchanged. Production macOS packaging must separately
install this helper under root custody and provide its independently trusted
digest; building it alone does not establish installation trust.

Source-only repair: **no builds, tests, helper launches, stores, network or
authentication were executed by the repair author**. The parent owns formatting,
compilation, Clippy, tests, optional-feature impact and native validation.
Read-only `rustfmt --check --edition 2024 --config skip_children=true` passed
for the ten changed Rust implementation/binding/test files (exit 0), as did
`git diff --check` for the tracked repair changes. These establish source
formatting/parsing only, not type checking, native API availability, test
registration or runtime behavior.
From `rust/`, focused selectors after wiring are:

```text
cargo fmt --all -- --check
cargo check --locked --offline -p fabric-identity -p fabric-cli --all-targets --features interactive
cargo clippy --locked --offline -p fabric-identity -p fabric-cli --all-targets --features interactive -- -D warnings
cargo test --locked --offline -p fabric-identity --features interactive --lib interactive::macos::tests -- --test-threads=1
cargo test --locked --offline -p fabric-identity --features interactive --lib interactive::store::tests -- --test-threads=1
cargo test --locked --offline -p fabric-identity --features interactive --test protected_store_helper -- --test-threads=1
cargo test --locked --offline -p fabric-cli --features interactive --bin fab-rs interactive_binding::tests -- --test-threads=1
cargo test --locked --offline -p fabric-cli --features interactive --test interactive_arguments -- --test-threads=1
cargo test --locked --offline -p fabric-identity --features interactive,certificate-store-macos --lib headless::certificate_macos::tests -- --test-threads=1
```

Set `FAB_IDENTITY_TEST_ROOT` or `IMPACT_TEST_ROOT` to a caller-owned absolute
project-local test directory for CLI process tests. Obtain real macOS
interactive-only and combined certificate-feature execution, as well as existing
Windows/Linux persistence regressions. Offline fake-backend tests and real
fixture subprocesses never access a store: they cover status-preserving deletes,
closed/bounded protocol, empty child environment, secret-only pipes, deadlines,
cancellation, no-I/O preflight, actual reap and uncertain-spawn quarantine.
Executable tests send only help/version or malformed requests on every platform.
They do not establish live Keychain, packaging, policy or credential acceptance.
