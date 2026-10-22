<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust scaffold architecture

The Rust workspace is an experimental, additive bootstrap in the existing
`microsoft/fabric-cli` repository, staged in the self-contained `rust/` folder.
The Python package, `fab` executable, tests,
and build workflow remain unchanged. The temporary Rust executable is `fab-rs`.
It is not a full port, release contract, or replacement for the Python product.

## Crate boundaries

| Crate | Ownership | Allowed internal dependencies |
| --- | --- | --- |
| `fabric-core` | Credential interoperability, shared errors; future request context and HTTP policies | None |
| `fabric-identity` | Explicit identity selection and optional future adapters | `fabric-core` |
| `fabric-sdk` | Typed service models and future operations and reusable workflows | `fabric-core` |
| `fabric-deploy` | Future native configuration, transforms, planning, publish/unpublish | `fabric-sdk`, `fabric-core` where used |
| `fabric-cli` | Arguments, login UX, streams, exit codes, future library composition | `fabric-identity`, `fabric-sdk`, `fabric-deploy` |

Only exercised edges are declared today: `fabric-identity -> fabric-core` and
`fabric-sdk -> fabric-core`. The deployment crate has no APIs or dependencies;
the CLI has no service commands and does not need library-composition dependencies
yet. Add those allowed edges when implementing their first real consumers.
The offline workspace contract test checks the exact five members, allowed edges,
publication settings, edition, and consistent declared Rust version. Cargo
rejects dependency cycles.

The eventual call direction is:

```text
Agents and skills -> CLI -> SDK -> Fabric / OneLake / required ARM
                      |      ^
                      +-> deployment library
Direct SDK consumer ---------+
```

Skills describe workflows using the CLI. They do not contain service logic.
The SDK's normal dependency closure excludes the CLI and identity implementation.
SDK consumers supply an `Arc<dyn azure_core::credentials::TokenCredential>`.
Neither `az`, `azd`, nor Python is required by the Rust workspace.

## Current offline slice

`fabric-core` re-exports Azure's credential types and provides explicit, non-secret
account, tenant, and public-cloud audience selection. Blank account or tenant
selectors fail with a typed `NotConfigured` error. Selection metadata does not
verify the injected provider: the caller must bind that provider to the selected
account and tenant. Sovereign-cloud configuration remains a separate decision.

`fabric-sdk::Client` stores that provider without acquiring credentials or
performing I/O. It deliberately does not format the provider in its `Debug`
implementation. The credential accessor proves interoperability, not authenticated
service behavior. The example implements a clearly test-only, in-memory Azure
credential outside the identity crate. Its dummy token is expired, is never sent
to a service, and is not a production static-token mechanism.

`Page<T>` round-trips a single Fabric-style JSON page, including opaque
continuation values. It does not follow URIs or provide a pagination loop.
Missing or malformed `value` data fails deserialization rather than returning a
success-shaped empty page.

`fabric-identity::interactive_credential` always returns a typed `Unsupported`
error. No backend, unattended adapter, credential chain, interactive login,
cache, or plaintext persistence is implemented. See the
[identity decision](decisions/identity-backend.md).

`fab-rs` supports only help and version. Other invocations fail with sanitized
JSON and do not echo arguments. The subprocess tests clear inherited environment
variables except native Windows runtime and temporary-directory settings.
See [CLI contracts](compatibility.md#bootstrap-cli-contract).

## Runtime and transport policy

Libraries must not prompt, exit the process, initialize a global logger, create
their own async runtime, or hold global mutable CLI context. Async work reuses
the caller's runtime. Tokio's `macros` and single-thread `rt` features are used
only by the SDK example and its test.

`azure_core` default features are disabled. The locked scaffold includes no
Reqwest, Rustls, OpenSSL, or native-TLS dependency, so there is no HTTP transport
or duplicate HTTP implementation to configure yet. Azure's shared types still
bring transitive URL, time, serialization, and diagnostic dependencies.
Duplicate `syn` major versions are proc-macro dependencies, not HTTP transports.

A future network slice must select one shared service pipeline, provisionally
using Rustls, and validate enterprise proxy and certificate trust behavior.
Rustls for HTTP does not rule out separate native prerequisites for certificate
authentication, including OpenSSL. Resolve those prerequisites before adding an
authentication adapter.

The current graph needs only the standard Rust target's native linker: MSVC C++
build tools and the Windows SDK on Windows, Xcode command-line tools on macOS,
or a system C toolchain/linker on Linux. No additional OpenSSL installation or
certificate-authentication native library is required by this offline slice.

## Build and contribution loop

All packages use edition 2024, resolver 3, version `0.1.0`, and `publish = false`.
Dependency versions and features are centralized in `workspace.dependencies`;
`Cargo.lock` records the exact resolution. The repository MIT license applies.
New source files retain Microsoft copyright notices; JSON fixture attribution
is recorded in its directory README.

Rust 1.99.1 is both the pinned stable toolchain and the conservative declared
minimum supported Rust version for this bootstrap. That is the version actually
checked, not a claim about the oldest compiler that might compile the dependency
graph. Lowering the minimum requires a separate locked-graph check.

From the `rust/` workspace directory (change into it first so Rustup selects
the pinned toolchain):

```text
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked --workspace --doc
cargo run --locked -p fabric-sdk --example injected_credentials
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-cli -- --version
```

Dependency restoration needs registry access. After `cargo fetch --locked`,
`--offline` can additionally prevent Cargo registry access during checks and
tests. The examples and tests themselves never call Fabric or an identity
service and require no credentials.

The separate Rust CI workflow configures native Windows, macOS, and Linux jobs,
uses `rust-toolchain.toml` automatically, and logs the actual compiler host.
Configuration is not proof of successful execution on those runners. Packaging,
signing, notarization, Windows ARM64, Linux musl, and broker support are not
covered by this scaffold.

## Next bounded slice

Implement one read-only Fabric workspace-list operation with an injected mock
transport, typed response/error fixtures, explicit audience validation, and
bounded continuation handling. Establish the shared HTTP policy boundary in that
slice before exposing a corresponding CLI command. Do not couple it to an
interactive identity decision.

Follow the [migration guidance](migration-guidance.md): translate first, improve
separately, preserve independent test oracles, and integrate small slices from
agreed commits. Repository contribution and team-review gates still apply to
publishing or merging changes. A local build or this document does not authorize
external comments, pushes, pull requests, releases, tenant configuration, or live
Fabric mutations.
