<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Existing-backend authentication command evidence

This bounded extraction starts at Rust
`b78d9654a37ca909036df54b8562e01fcfe8aa13`. It changes CLI testability, not the
identity implementation, backend decision, feature defaults or native support.
The Python source remains pinned to
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, specifically
`commands/auth/fab_auth.py::_acquire_default_access_tokens`, `status`, `logout`
and `core/fab_auth.py`. The [identity requirements](identity-requirements.md)
and [backend decision](decisions/identity-backend.md) remain authoritative.

## One production command engine

The existing root parser still dispatches `Invocation::Auth` to the private
binary `auth::run`. On Windows that adapter validates `LoginOptions`, creates
the same current-thread runtime, and invokes `auth_commands::run`.
`WindowsBroker` delegates directly to the existing
`WindowsCredential::login(options, allow_interaction)` and `sign_out(options)`.
There is no second token-acquisition engine or fallback provider.

The runtime-neutral command module accepts a configured `Broker`, cancellation
and deadline futures. It owns action dispatch, status's silent constraint,
cancellation/timeout selection, non-secret JSON projection, stream separation
and exit selection. It creates no runtime, discovers no accounts, reads no
environment or cache, and calls no service. The broker interface carries only
successful scope/expiry metadata, never token values. Broker failures must
already be classified and sanitized; production keeps the existing
`auth::broker_error` mapper and numeric native/provider diagnostics.

`tests/auth_commands.rs` parses actual root arguments and calls this same source
module with a synthetic implementation of that credential boundary. Its fixture
returns three metadata records; it does **not** implement or prove WAM's
per-audience acquisition algorithm. Tests cover exact success/error streams and
exits, explicit account/tenant/client effect selection, silent status and
`--no-interactive`, app/device-scoped logout, no command-level retry/fallback,
broker failure propagation, pending-future drop on cancellation/timeout, signal
failure sanitization and output failure. Windows binary unit tests additionally
exercise the real adapter's pure selection and existing error mapping without
calling login, logout, account enumeration or token acquisition.

The production identity crate remains responsible for Fabric, OneLake, then ARM
acquisition; silent-first interaction policy; authoritative expiry; explicit
account/tenant/client and native-account pinning; silent SDK refresh; and native
sign-out scope. Its existing pure response/selection/status/expiry and
cancellation/lifecycle tests remain separate evidence. Real hidden-window/STA
tests do not authenticate and are not MFA, Conditional Access or service proof.

## Preserved source differences

| Surface | Python source | Existing Rust constraint, unchanged here |
| --- | --- | --- |
| Login | Eager Fabric, OneLake, ARM acquisition before success | Same sequence remains inside `WindowsCredential::login`; not replaced with lazy acquisition |
| Selection | Saved/discovered tenant and some first-account behavior | Explicit account, tenant and client; no alternate account/backend fallback (E1) |
| Modes and persistence | Additional unattended/az/raw-token modes and optional plaintext persistence | No new provider, external-tool dependency, raw-token fallback or cache format (E2-E4) |
| Status | Noninteractive acquisition/refresh and masked token fragments | Same three-audience login path, forced silent; no token or token fragment is printed |
| Logout | Local auth/cache/configuration/navigation resets | Existing explicit client/app-scoped broker sign-out on this device only; no OS-account removal or global revocation |
| Failure | Some errors collapse into `AuthenticationFailed` | Existing distinct sanitized Rust errors and nonzero exits (E6); cancellation is not cache rollback |
| Support | Python authentication has a broader platform/mode surface | The real adapter remains opt-in and Windows-only; off-Windows `unsupported` is not a happy contract |

This is not a source-oracle waiver. Full Python auth-mode/output/local-state
parity is not asserted. Live MFA/CA, native provider acquisition, registration,
guest/alias handling, macOS/Linux backends and service access remain unverified.

## Root assembler handoff

The microbatch does not edit `main.rs`, `lib.rs`, the parser, families, sessions,
shared manifests, the identity crates or CI workflows. The existing binary route
is already wired to the extracted engine. For a public central host mount, the
assembler can add this export to `fabric-cli/src/lib.rs`:

```rust
#[cfg(feature = "windows-broker")]
pub mod auth_commands;
```

Then replace the private path declaration and its `parser` import in `auth.rs`
with this Windows-only import, so the binary and central consumer share one
library module:

```rust
#[cfg(windows)]
use fabric_cli::auth_commands as commands;
```

The standalone microbatch keeps the private source mount so it compiles without
commandeering the assembler's `lib.rs`. The module resolves `AuthAction` through
its parent's `parser`; both the binary adapter and its integration-test consumer
import the real root parser. A library export resolves the existing root parser
directly. The assembler should replace the test's path mount with the public
library import when adding that export.

A central offline host must route actual root `Invocation::Auth` to `run` and
`write_result` with its injected, explicit selection and effects. Registering
arguments, executing only this isolated consumer, or observing an unsupported
Linux exit is not enough to add an auth leaf to the canonical mounted numerator.
The assembler owns whole-head acceptance and preservation of the old 43 paths and
218 source pins; no `43 + 4 + 3` coverage claim follows from this microbatch.

## Required execution receipts

Run from `rust/` on the authorized existing CI worker or assembler slot. These
are requested checks, not claims that they ran:

```text
cargo fmt --all -- --check
cargo test --locked -p fabric-cli --features windows-broker --test auth_commands --test auth_arguments --test cli_contract --test workspace_contract
cargo test --locked -p fabric-cli --features windows-broker --bin fab-rs
cargo clippy --locked -p fabric-cli --features windows-broker --all-targets -- -D warnings
cargo test --locked -p fabric-cli --no-default-features
```

On the existing approved native Windows worker, also run the existing safe
contracts, without invoking a valid-account auth command:

```text
cargo test --locked -p fabric-identity-windows -p fabric-identity --features fabric-identity/windows-broker
cargo test --locked -p fabric-cli --features native-read
```

The assembler's actual root-mount and whole-head regression gates are additional.
The shared performance worker's finite lease is not permission for another local
Cargo build, another WSL toolchain or an independent benchmark.

Until those receipts and the central mounted contracts exist, **all three auth
leaves remain unproved**. The baseline's other exclusions remain table load,
table optimize, table schema, table vacuum, deploy and extension, unless the
assembler supplies their independent whole-head evidence. Full-config deployment
is broad unfinished technical work, not an inherent Rust impossibility.
`extension` already has warning-only unsupported Python behavior; its exclusion
must not be described as a missing Rust plugin loader. Neither provisional
exclusion justifies claiming 95 percent.
