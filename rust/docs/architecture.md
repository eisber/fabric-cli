<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust scaffold architecture

The Rust workspace is an experimental, additive bootstrap in the existing
`microsoft/fabric-cli` repository, staged in the self-contained `rust/` folder.
The Python package, `fab` executable, tests,
and build workflow remain unchanged. The temporary Rust executable is `fab-rs`.
It is not a full port, release contract, or replacement for the Python product.

## Crate boundaries

| Layer (initial crate) | Ownership | Allowed dependency layers |
| --- | --- | --- |
| `core` (`fabric-core`) | Credential interoperability, shared errors; future request context and HTTP policies | `core` |
| `identity` (`fabric-identity`) | Explicit identity selection and optional future adapters | `identity`, `core` |
| `sdk` (`fabric-sdk`) | Typed service models and future operations and reusable workflows | `sdk`, `core` |
| `deploy` (`fabric-deploy`) | Future native configuration, transforms, planning, publish/unpublish | `deploy`, `sdk`, `core` |
| `cli` (`fabric-cli`) | Arguments, login UX, streams, exit codes, future library composition | `cli`, `identity`, `sdk`, `deploy` |

The exercised SDK edges are `fabric-sdk -> fabric-sdk-workspaces`,
`fabric-sdk -> fabric-core`, and `fabric-sdk-workspaces -> fabric-core`;
identity also depends on core, and `fabric-deploy` depends on `fabric-sdk`.
The CLI has no service commands and does not need library-composition dependencies
yet. Add those allowed edges when implementing their first real consumers.
The initial five crates are not a fixed count or a maximum. Workspace membership
uses `crates/*`, so a new crate directory does not require editing a central
member list. Default members explicitly include safe crates and exclude the
optional `fabric-identity-windows` native boundary. Add new safe crates to
`default-members`. Each crate declares its architectural layer in its own manifest:

```toml
[package.metadata.fabric]
layer = "sdk"
```

The offline workspace contract test validates every member's declared layer,
internal dependency directions (including development and build dependencies),
publication settings, edition, and consistent declared Rust version. Missing or
unknown layers fail explicitly. Same-layer dependencies support extracting
focused crates without changing the layer rules; Cargo rejects dependency cycles.
SDK crates cannot depend on identity, deployment, or CLI implementations, even
indirectly through another SDK/core crate. Changing a layer or introducing a new
cross-layer direction requires architecture review, not relabeling a crate to
bypass the checks.

`fabric-identity-windows` belongs to the identity layer and exposes a safe API
over isolated desktop WAM interop. Only `fabric-identity/windows-broker` selects
that optional Windows-target dependency. Architecture checks also preserve its
exclusion from default SDK/CLI/identity graphs and its narrowly scoped unsafe
lint policy. The native boundary has no internal workspace dependencies; all
other workspace crates inherit `unsafe_code = "forbid"`.

### Crate-first parallel development

Prefer focused crates for independently owned porting areas rather than growing
the initial crates into monoliths. Establish a small public interface, a bounded
source-behavior contract, targeted tests, and an owner for each area. Service
families, deployment configuration/transforms/planning, and identity adapters
can become separate crates as their implementations are assigned. Do not require
them to grow into large modules first, and do not create empty placeholder crates
merely to increase the count.

More crates enable parallel implementation, review, targeted tests, and Cargo
compilation of independent dependency branches. They do not make tightly coupled
work independent: frequent shared-interface changes, high-fan-in core crates, and
long dependency chains still serialize work and increase integration/rebuild cost.
Keep shared contracts small, assign non-overlapping ownership, and use facades
where helpful to preserve consumer imports across extractions. Evaluate progress
by independently deliverable behavior, not crate count.

Adding a crate requires no change to the architecture test's crate-name list.
Use the shared package settings/lints and centralized dependencies, declare the
layer, document its responsibility and interface, and run its targeted checks plus
the workspace architecture contract. Workspace-wide CI already includes new
members. Shared-host build-slot rules still apply; additional crates are not a
grant to start concurrent heavy builds.

The eventual call direction is:

```text
Agents and skills -> CLI -> SDK -> Fabric / OneLake / required ARM
                      |      ^
                      +-> deployment library
Direct SDK consumer ---------+
```

### Focused workspace SDK

`fabric-sdk-workspaces` is the first extracted service crate in the `sdk` layer.
It owns the implemented workspace-list operation, wire models, pagination,
operation-specific executor and regression tests. Direct consumers use
`fabric_sdk_workspaces::WorkspaceClient`; it depends on core and shared external
types, never on the facade, identity implementation, CLI or deployment library.
It creates no transport or runtime.

`fabric-sdk` remains a facade. Its existing `Client` still owns its injected
credential and selection, preserving a place for other service methods without
making the workspace crate their owner. `Client::list_workspaces` delegates to a
focused `WorkspaceClient` using cloned selection metadata and the same credential
`Arc`; it does not reacquire or discover a provider during construction.
`fabric_sdk::Page`, `fabric_sdk::models::{Workspace, FabricServiceError}` and the
public `fabric_sdk::workspaces` types are reexports, not duplicate wire/error types.
Downstream integration tests exercise both paths without `cfg(test)` helpers.

This extraction moves behavior already implemented at
`c07228b5ed5266b899ff3d2b37c7a3778cb855b8`; it does not add workspace lookup,
item operations, Notebook publication, or a metadata-resolver contract. The
deployment lane owns its separately scoped Notebook work and coordinates
facade/shared-manifest changes through exact commit handoffs, not shared writers.

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
continuation values. It does not follow URIs itself. The workspace service's
`list_workspaces` method separately implements bounded token-based traversal
through injected execution, with explicit partial failures and cancellation.
Missing or malformed `value` data fails rather than returning a success-shaped
empty page. The SDK README records source mapping and intentional differences;
workspace retry, timeout, production transport, and LRO policy remain unimplemented.

`fabric-deploy` implements a bounded local Notebook loader and supported transforms,
using the additive `fabric-sdk::notebooks` APIs for item discovery, create/update
and operation reconciliation. Both service modules use caller-injected credentials,
execution and cancellation. There is no production HTTP stack or CLI deployment
command. See [deployment compatibility](deployment-compatibility.md) for the
exact source, supported subset, safety bounds and unported behavior.

`fabric-identity::interactive_credential` always returns a typed `Unsupported`
error. No backend, unattended adapter, credential chain, interactive login,
cache, or plaintext persistence is implemented. See the
[identity decision](decisions/identity-backend.md).

### Optional native unsafe boundary

On October 23, 2026, the owner approved isolating Windows unsafe interop in a
separate crate. The five existing crates keep the workspace
`unsafe_code = "forbid"` lint, including when they consume the native crate's
safe Rust API. `fabric-identity-windows` uses `unsafe_code = "deny"`, with an
exception only for its private `native` module, `unsafe_op_in_unsafe_fn = "deny"`
and `clippy::undocumented_unsafe_blocks = "deny"`. No public unsafe API, raw HWND,
or handwritten COM ABI is exposed.

The native crate is excluded from `default-members` and from the identity
crate's default dependency graph. Its Microsoft `windows` and `windows-future`
dependencies are Windows-only. The SDK remains independent of identity.
These distinctions apply equally to executable and library consumers:

| Build | Unsafe-code boundary |
| --- | --- |
| Default SDK/CLI/identity | Excludes the optional broker crate and its Windows dependencies; existing crates forbid unsafe source |
| Identity with `windows-broker` on Windows | Safe public calls, but the linked dependency closure includes reviewed native unsafe code |
| Identity with `windows-broker` off Windows | No native edge or broker API; no fallback provider |

This is not a claim that the standard library or every other transitive
dependency is unsafe-free. A consumer's `forbid(unsafe_code)` applies to that
consumer, not its dependencies. An ABI-exported C DLL and a Rust library are also
different packaging contracts; no C ABI, dynamic loader or helper DLL is added.

The native boundary accepts an explicitly configured `WebTokenRequest` and
optional native account. It starts interactive desktop WAM only when called,
on a dedicated STA thread with its own system-class parent window and message
pump. The caller's async executor is not blocked or replaced. Dropping/cancelling
the returned future requests native cancellation; the worker retains the window
and apartment until native terminal completion, and suppresses late success.
Cancellation is cooperative, not guaranteed rollback or immediate termination.
Native-stage errors retain only numeric HRESULTs, not provider text or secrets.
If completion notification itself fails, the caller receives an explicit
`CompletionUnconfirmed` error while the worker cancels and retains its native
resources until terminal status can be observed. Later cleanup cannot overwrite
that failure with a successful result. This is not a bounded-cleanup guarantee
when the operating system/provider stops responding.

This is low-level interop, not the completed Fabric provider. The response is
opaque, is not printed by Debug, and must be explicitly interpreted by a future
adapter. Tenant/resource binding, authoritative token expiry, provider errors,
silent refresh, local selection metadata and logout integration remain separate
requirements. `interactive_credential` still returns `Unsupported`; no CLI login
command invokes this boundary. Completion/cancellation tests use synthetic
results. Windows-only lifecycle tests initialize an STA and create/destroy a
hidden system-class parent window, checking explicit close, repeated close and
scope-exit cleanup. They never acquire a broker factory, enumerate Windows
accounts, request tokens, access a credential store or display broker UI.

`fab-rs` supports only help and version. Other invocations fail with sanitized
JSON and do not echo arguments. The subprocess tests clear inherited environment
variables except native Windows runtime and temporary-directory settings.
See [CLI contracts](compatibility.md#bootstrap-cli-contract).

## Runtime and transport policy

Libraries must not prompt, exit the process, initialize a global logger, create
their own async runtime, or hold global mutable CLI context. Async work reuses
the caller's runtime. Tokio's `macros` and single-thread `rt` features are used
only by offline tests and the independent credential example, not library code.

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

The optional boundary and a `forbid(unsafe_code)` consumer can be checked without
starting authentication:

```text
cargo test --offline --locked -p fabric-identity-windows -p fabric-identity
cargo test --offline --locked -p fabric-cli --test workspace_contract
cargo clippy --offline --locked -p fabric-identity --all-targets --features windows-broker -- -D warnings
cargo run --offline --locked -p fabric-identity --example safe_consumer --no-default-features
cargo run --offline --locked -p fabric-identity --example safe_consumer --features windows-broker
```

The example only checks the API signature when the feature is enabled; it never
constructs a native request or opens the broker. The CI feature checks complement
the default graph checks, not replace native policy acceptance.

### Native isolation validation on October 23, 2026

The identity lane ran the following offline checks on
`x86_64-pc-windows-msvc`, Rust 1.99.1 (`58cf8f9c3`), against the boundary from
`5f3294e58e8e054c8ade0a4e3de7d6cc3c4685de` plus the native lifecycle tests:

| Check | Observed result | Limit |
| --- | --- | --- |
| Formatting and targeted Clippy with `-D warnings` | Passed for native/identity/CLI targets with broker feature enabled | Not a full workspace or cross-platform build |
| Core, identity, CLI and architecture tests | 10 passed: 3 core, 1 identity, 3 CLI and 3 architecture | CLI still intentionally rejects login; graph checks for Linux/macOS are not native execution on those OSes |
| Native-boundary tests | 11 passed: 9 synthetic lifecycle/error tests and 2 real STA/hidden-window lifecycle tests | No broker/account/token/store operation or live MFA |
| `forbid(unsafe_code)` executable example | Built and ran with broker disabled and enabled | Example does not invoke the broker; the opt-in dependency contains unsafe interop |
| Identity library as Windows `cdylib` | Compiled/linked with broker disabled and enabled, retaining workspace unsafe prohibition in the consumer | Compile/link probe only; no exported C ABI, DLL-loading or broker invocation proof |

The DLL compile/link probes, from `rust/`, were:

```text
cargo rustc --offline --locked -p fabric-identity --lib --crate-type cdylib --no-default-features
cargo rustc --offline --locked -p fabric-identity --lib --crate-type cdylib --features windows-broker
```

A successful build cannot certify soundness of all native lifetimes or prove
the provider's MFA, tenant, token-expiry or service-audience contract. Those
remain explicit integration and live-acceptance gates.

Dependency restoration needs registry access. After `cargo fetch --locked`,
`--offline` can additionally prevent Cargo registry access during checks and
tests. The examples and tests themselves never call Fabric or an identity
service and require no credentials.

The separate Rust CI workflow configures native Windows, macOS, and Linux jobs,
uses `rust-toolchain.toml` automatically, and logs the actual compiler host.
Configuration is not proof of successful execution on those runners. Packaging,
signing, notarization, Windows ARM64, Linux musl, and broker support are not
covered by this scaffold.

## Bounded implementation lanes

Two implementation lanes may proceed independently:

| Lane | Owned implementation | Boundary |
| --- | --- | --- |
| SDK and CLI porting | Offline workspace-list SDK operation, typed models/errors and targeted tests | Injected credential and operation executor only; no CLI service command or production HTTP transport yet |
| Fabric content deployment | Parsed configuration helpers, local Notebook files, explicit logical/literal/workspace transforms, SDK create/update and bounded completion | Single Notebook; injected executor, no production HTTP/auth, broad deployment YAML/JSONPath/regex, folder moves, planner, unpublish or live operations |

The SDK's existing `azure_core` and `serde_json` dependencies are available at
runtime for token interoperation and response decoding. Deployment uses the
workspace's existing `serde_json` for parsed configuration and metadata. Notebook
definition parts use already-locked base64 0.22.1. No new third-party versions or
transport features are selected by the native Notebook slice.

An SDK-local executor is an offline testing seam, not a second general HTTP
pipeline. Shared retry, timeout and operation policies still need the core
boundary before a service CLI command is exposed. Preserve Python behavior or
record an unsupported/incomplete capability explicitly; do not silently claim
retry, long-running operation, or cross-platform parity. Concrete safety bounds
and compatibility exceptions remain review gates, not implied defaults from
this workspace change.

Deployment consumes the existing immutable configuration fixtures and the pinned
Python behavior. It does not implement another SDK or duplicate credential
acquisition. The Notebook path consumes the SDK's operation-local service APIs.
Both lanes reuse the caller's runtime and remain independent
of production interactive authentication.

Follow the [migration guidance](migration-guidance.md): translate first, improve
separately, preserve independent test oracles, and integrate small slices from
agreed commits. Repository contribution and team-review gates still apply to
publishing or merging changes. A local build or this document does not authorize
external comments, pushes, pull requests, releases, tenant configuration, or live
Fabric mutations.
