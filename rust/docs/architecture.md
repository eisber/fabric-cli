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

One reviewed exception permits exactly `fabric-cli-output` (CLI) to depend on
`fabric-core-json` (core). The latter is a pure ordered JSON encoder shared with
SDK request-body and definition consumers: no CLI types, auth, transport, I/O,
runtime or global state. CLI public output APIs remain wrappers over this single
implementation. This is **not** blanket CLI-to-core permission: other CLI crates
cannot depend on this core crate under the exception, output cannot depend on
other core implementations, and the JSON core cannot acquire forbidden transitive
identity/deploy/CLI dependencies. Architecture tests exercise the exact names
and layers while retaining the unchanged generic five-by-five layer policy.

The exercised SDK edges are `fabric-sdk -> fabric-sdk-workspaces`,
`fabric-sdk -> fabric-core`, and `fabric-sdk-workspaces -> fabric-core`;
identity also depends on core, and `fabric-deploy` depends on `fabric-sdk`.
The CLI read-command library now consumes the SDK facade, CLI context, CLI
output and CLI query libraries. `PythonQuery` connects native evaluation and GET
path discovery to the actual read host; no-query LS preserves source columns.
Its offline consumers use `fabric-sdk-http`; the optional `native-read` binary
selects that same executor and `fabric-sdk-transport/hyper-rustls`.
An additive borrowed `FolderReadHost`, selected with `ReadHost::with_folders`,
composes the same SDK, context, query and output layers for folder-aware LS.
It preserves existing host struct literals and delegates other reads, including
source-routed GET, rather than duplicating their orchestration. Its bounded
folder graph and command-local snapshot boundaries are documented in the
[folder-command matrix](folder-command-compatibility.md).
These read paths use allowed CLI-to-SDK/CLI edges, not identity implementation.
The optional auth command separately selects `fabric-identity/windows-broker`;
it never adds an SDK-to-identity edge or installs a read host.
The default/broker-only binary recognizes reads but has no read host.
The explicit `native-read` feature composes the runtime-neutral `ReadSession`,
complete async metadata scopes, existing handlers and native broker/transport;
see [native host boundaries](native-read-host.md).
The same private native composition now dispatches the two read-only job leaves,
without a second runtime or credential/HTTP setup. `JobReadSession` borrows the
existing read metadata session and delegates to released `fabric-cli-jobs` and
`fabric-sdk-jobs` through allowed CLI-to-CLI/SDK edges. Its successful raw JSON
guard reuses the existing output parser before SDK decoding, never adding an
SDK-to-CLI dependency or changing default SDK consumers.
See [bounded job reads](native-job-reads.md) for the incomplete source branches.
The additive `JobSession` shares that metadata/credential composition for
`job start` and `job run`, injecting files, immediate UI and configuration into
the existing `JobHost`. The family retains all execution/polling/timeout logic;
the central CLI adds no SDK engine or runtime.
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

### Focused CLI query projection

`fabric-cli-query` is a CLI-layer leaf over `jpx-core` and ordered
`serde_json::Value`. Its public `search(&Value, &str) -> Result<Value, QueryError>`
translates Python's CLI JMESPath wrapper, not deployment JSONPath mutation.
It performs no I/O, SDK calls, credential discovery, or global CLI initialization.
Its public example composes projected values and typed errors with the existing
`fabric-cli-output` envelope through a same-layer development dependency.
Command dispatch remains separately owned. See
[query compatibility](query-compatibility.md) for the source pin, adapters,
independent oracle and explicit unsupported operations.

### Focused capacity SDK

`fabric-sdk-capacities` adds only bounded Fabric capacity listing in the SDK
layer. Its direct consumer has no facade/identity/CLI/deployment dependency.
It reuses the existing workspace-owned `Page`, `FabricServiceError`, response
and execution-error types without modifying workspace behavior. The additive
facade module delegates using the same provider and selection. The
`fabric-sdk-http` capacity module calls private shared HTTP execution; there is
no duplicate transport, retry, TLS, identity, or runtime implementation.
See the [capacity matrix](capacity-compatibility.md) for its source mapping,
failure/bounds/cancellation gates, and non-goals.

The SDK's normal dependency closure excludes the CLI and identity implementation.
SDK consumers supply an `Arc<dyn azure_core::credentials::TokenCredential>`.
Neither `az`, `azd`, nor Python is required by the Rust workspace.

## REST client implementation

The additive `fabric-sdk-folders` SDK-layer crate owns read-only folder LIST/GET.
It preserves opaque folder/parent data and Python query merge semantics, with
explicit safety bounds and cancellation. The facade delegates with the same
credential/selection; `fabric-sdk-http` adds only a thin adapter over private
shared execution. See [folder compatibility](folder-compatibility.md) for exact
source, consumers, independent evidence and non-goals.

The additive `fabric-sdk-items` service owns bounded generic item LIST/GET.
Its exercised SDK-layer edges are `fabric-sdk -> fabric-sdk-items`,
`fabric-sdk-http -> fabric-sdk-items`, and `fabric-sdk-items -> fabric-core`
and `fabric-sdk-workspaces` (existing `Page`/`FabricServiceError` wire types).
It depends on neither the facade nor identity/deployment/CLI implementations.
The workspace pager is private and remains unchanged; item-specific traversal
reuses one local set of read/query/cancellation/error helpers. The stable facade
delegates both new methods; the HTTP adapter uses its private shared execution
engine. See [item compatibility](item-compatibility.md) for source and limits.

The owner selected handwritten typed endpoint methods over shared HTTP
infrastructure and behavioral policies on October 24, 2026. Keep credentials,
bounded reads, cancellation, errors, pagination, and LRO behavior reusable rather
than duplicating them per endpoint. Do not adopt the isolated generator PoCs as
production dependencies. Swagger remains a contract/reference input, not the
production SDK generator. See the
[REST client decision](decisions/rest-client-implementation.md) for rationale,
evidence, and limits; production transport and identity decisions remain separate.

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
the facade has no default production transport. Retry/deadline behavior is
available through the optional caller-composed `fabric-sdk-http` adapter;
workspace listing does not acquire LRO semantics from that adapter.

`fabric-deploy` implements a bounded local Notebook loader and supported transforms,
using the additive `fabric-sdk::notebooks` APIs for item discovery, create/update
and operation reconciliation. Both service modules use caller-injected credentials,
execution and cancellation. A caller can explicitly compose the optional native
transport; no production credential host or CLI deployment command is installed.
See [deployment compatibility](deployment-compatibility.md) for the
exact source, supported subset, safety bounds and unported behavior.

The original synchronous `fabric-identity::interactive_credential` returns
`Unsupported`; its metadata-only input cannot authorize async UI/client setup.
The owner-approved opt-in Windows slice instead provides
`WindowsCredential::login`, a configured Entra WAM acquisition path and Azure
`TokenCredential` with silent refresh. The default build has no backend or ambient
credential discovery. No unattended adapter, chain or local credential file is
implemented. See the [identity decision](decisions/identity-backend.md).

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

The low-level raw request API remains separate from `entra`, the native crate's
safe-Rust provider adapter. That adapter validates explicit account/tenant/client
configuration, selects only the requested UPN/native account ID, sets the
source-backed Entra request fields, and checks returned authority/account/expiry.
Tokens are opaque and never shown by Debug. Windows broker metadata, not a JWT
parse or guessed lifetime, supplies expiry. WAM owns token persistence. Login
acquires all three Python audiences before publishing a credential; SDK refresh
is silent and account pinned. Logout is explicit/client scoped. No provider
response text, local plaintext cache or developer-tool fallback is introduced.

Building the CLI with `windows-broker` adds actual user-run login/status/logout;
without it the existing bounded read parser/host contract remains. These commands
can contact Entra and alter the broker cache, but are never run with valid
credentials by tests/CI. Completion/cancellation and provider-field tests use
synthetic results. Windows lifecycle tests create/destroy only an STA and hidden
system-class parent window. Subprocess tests exercise help/invalid configuration,
not broker factories, account enumeration, tokens, stores or sign-in UI.

The forward-port onto `a23b4de` registers auth additively in the public parser.
`Invocation::Auth` and its non-secret typed arguments exist only with the broker
feature; the executable dispatches them to auth without replacing read parsing.
Read-host configuration/execution still belongs to callers, not login. Tests and
CI compare existing source read/query behavior with broker enabled and disabled.

Default and broker-only `fab-rs` recognize Python LS/dir/GET and return typed
`NotConfigured`. The optional native host explicitly supplies credentials,
transport, context and time. The public
`fabric_cli::ReadHost` runs real SDK reads on the caller's runtime, with pure
lookup snapshots and caller-owned JSON writers. No-query LS works without query
dependencies; generic metadata GET projects through the injected query boundary.
See [read-command compatibility](read-command-compatibility.md) for gaps.
Help/version remain available. Other invocations fail with sanitized
JSON and do not echo arguments. The subprocess tests clear inherited environment
variables except native Windows runtime and temporary-directory settings.
See [CLI contracts](compatibility.md#bootstrap-cli-contract).

## Runtime and transport policy

`fabric-sdk-operations` owns the shared bounded Fabric request/operation
reconciliation engine extracted from the original Notebook implementation.
It depends on SDK/core types, never the facade, HTTP implementation, identity,
CLI or deployment. Notebook's public API remains a compatibility consumer of
that engine, with its original strict status/result policy. Resource endpoint
operations select separately documented source-synchronous policies. An already
received 202 can be reconciled without replaying the original mutation, retaining
operation IDs, raw service failures and uncertainty through cancellation.

`fabric-cli-mutations::FabricMutationBackend` is thin CLI composition over that
SDK policy and `fabric-sdk-http`, not a second SDK or operation loop.
`fabric-sdk-onelake` owns bounded storage list/read/HEAD with explicit storage
selection and an injected executor; its first released checkpoint adds no
facade, CLI, credential backend or concrete HTTP dependency. Complete filesystem
verbs, storage adapters, broader mutations and the main CLI host remain separate
implementation/consumer gates. See the [resource matrix](mutation-compatibility.md).

`fabric-sdk-http` is the first reusable executor adapter for workspace and
Notebook service requests. It accepts a caller-owned Azure `HttpClient` and
asynchronous timer, with bounded streaming, deadlines, read-status retries and
no mutation replay. It depends on the SDK facade, not the reverse, so focused
SDK consumers remain free to inject other executors. See its
[source mapping and compatibility limits](../crates/fabric-sdk-http/README.md).
It does not install a production HTTP/TLS backend or create a runtime.

Libraries must not prompt implicitly, exit the process, initialize a global logger, create
their own async runtime, or hold global mutable CLI context. Async work reuses
the caller's runtime. The explicitly selected `fabric-sdk-transport` uses
Tokio time and I/O; it never creates a runtime. Tokio's `macros` and
single-thread `rt` support remains available for offline consumers and tests.
The CLI owns its single-thread Tokio runtime, signal handler and timeout only
when the auth or native-read feature is enabled. Tokio's signal feature is selected by that
optional CLI dependency, not by default read/SDK consumers. A dedicated native
STA thread is a WinRT/UI requirement, not an independent async runtime.

`azure_core` default features remain disabled. Direct SDK consumers still
exclude HTTP/TLS implementations. The optional
[`fabric-sdk-transport`](../crates/fabric-sdk-transport/README.md) SDK-layer crate
has no default backend; its `hyper-rustls` feature selects one pooled Hyper
client, Rustls with a per-client ring provider, explicit platform/additional
trust, a direct/static-HTTP-proxy policy and bounded decoded streaming.
It depends on the shared executor, not the reverse, and installs no new REST,
retry, paging, authentication or overall-deadline engine.

The owner approved Hyper/Rustls instead of the provisional Reqwest direction
because the adapter must disable Hyper's implicit pooled cancelled-request
retry explicitly. Redirects and all backend retries remain disabled. Offline
trust/static-proxy fixtures are not general enterprise certification.
Rustls for HTTP does not rule out separate certificate-authentication native
prerequisites, including OpenSSL; those remain an identity decision.

The optional ring backend needs a C build toolchain: MSVC C++ build tools and
the Windows SDK on Windows, Xcode command-line tools on macOS, or a system
C toolchain/linker on Linux. It adds no OpenSSL or native-TLS service backend.

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

These are isolation-slice results, before the later actual-login implementation.
The login slice additionally runs feature-enabled command parsing/invalid input,
strict provider identity/expiry/status and SDK scope/redaction tests, without real
authentication. It preserves the same unsafe isolation; manual MFA/provider and
service acceptance remain unexecuted.

The final targeted actual-login run on the same Rust 1.99.1 Windows x64 host
passed **32 tests** with `fabric-cli/windows-broker`: 19 native/Entra-contract,
2 identity, 3 core, 5 CLI and 3 architecture. A separate default-build CLI run
passed its original 3 CLI and 3 architecture tests. Targeted Clippy with
`-D warnings`, formatting, feature-enabled auth help, safe-consumer example runs
with broker off/on and the feature-enabled identity `cdylib` compile/link probe
also passed. No valid-account login/status/logout or service request was run.

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
| SDK and CLI porting | Offline workspace-list SDK operation, typed models/errors and targeted tests | Injected credential and operation executor; native transport is opt-in, and the binary still lacks a production read host |
| Fabric content deployment | Parsed configuration helpers, local Notebook files, explicit logical/literal/workspace transforms, SDK create/update and bounded completion | Single Notebook; caller-composed executor/optional transport, no production credential host, broad deployment YAML/JSONPath/regex, folder moves, planner, unpublish or live operations |

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
