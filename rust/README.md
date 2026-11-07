<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Microsoft Fabric CLI and SDK for Rust

This workspace provides the `fab-rs` command-line application, reusable Fabric
service clients, deployment orchestration, and optional Windows authentication.
Applications can use the SDK and deployment libraries without invoking the CLI.

## Get started

Run Cargo from this directory to use the repository-pinned Rust 1.99.1 toolchain:

```text
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-cli -- --version
cargo run --locked -p fabric-sdk --example injected_credentials
```

The SDK example uses synthetic credentials and performs no service requests.
Default builds support local commands and argument validation without connecting
to an account. Remote command execution requires an explicitly configured host.
See the [CLI guide](hosts/cli/fabric-cli/README.md) for command composition and
[native host configuration](docs/native-read-host.md) for service execution.

For the separate CLI v2 effort, follow the
[delivery and upstream synchronization process](docs/v2-delivery.md). Command
implementation starts only after the selected specification and first wave
are confirmed; this process does not change the existing CLI contract.

The additive [v2 local host](hosts/cli/fabric-cli-v2/README.md) provides the
six selected `cli` operations through a separate `fab-cli` executable.
It preserves `fab-rs` and its compatibility fixtures, distinguishes manual
handoffs from delivered actions, and fails explicitly when an optional
provider or trusted installation profile is unavailable.

The opt-in [`v2-cancel` slice](docs/v2-cancel.md) requests cancellation of one
existing Notebook scheduler run using an explicit ID-based path. Its acceptance
receipt does not claim terminal cancellation or rollback.

The owner-selected [nonrecursive delete preview](docs/nonrecursive-delete.md)
adds exact Notebook deletion with explicit permanent intent and host approval.
Its typed operation metadata propagates through SDK results and CLI receipts.
It does not replace `rm` or promise recoverable deletion or physical erasure.

The opt-in [v2 schema-discovery preview](docs/v2-schema-discovery.md) exposes
operation contracts through a separate `fab-cli-discovery` binary. It does not execute
mutations, replace `fab-rs`, or install a live data-schema host.
See the [shared tabular file output contract](docs/tabular-output.md) for the
export syntax, current consumers, and validation evidence limits.

## Workspace components

The opt-in [workspace v2 read slice](docs/workspace-v2-reads.md) uses the separate
`fab-cli-workspace` preview. It preserves both the six-leaf local `fab-cli`
executable and the native GET `fab-cli-v2` entry point; no preview overwrites
another's Cargo output or selects a provider implicitly.

The workspace groups service clients and command adapters by capability:

```text
capabilities\
  resources\       Workspace, item, folder, capacity, access and catalog APIs
  jobs\            Job APIs and command adapters
  storage\         OneLake APIs, HTTP adapter and file commands
  tables\          Table APIs and command adapters
  deployment\      Deployment workflows and definition transfer commands
  local\           Local configuration and navigation commands
hosts\
  cli\             Executable composition, context, query and presentation
  repl\            Optional Reedline editor and removable shell loop
  update\          Pinned-trust release verification and update orchestration
infrastructure\
  core\            Shared contracts and JSON encoding
  identity\        Credential selection and optional Windows broker
  http\            Shared executor and native transport
  execution\       Paging and operation reconciliation
  search\          Pure, bounded name-similarity suggestions
sdk\               Public SDK facade
```

Each leaf crate retains its package name and dependency boundaries. A capability
may contain both SDK and CLI crates; that proximity does not make CLI-specific
arguments, prompts or output contracts part of the SDK. Additional front-ends,
such as MCP servers or Azure DevOps tasks, can add host composition and
capability-specific adapters without invoking the CLI executable. Those
front-ends are not implemented by this layout.

| Component | Responsibility |
| --- | --- |
| [Fabric SDK](sdk/fabric-sdk/README.md) | Workspace, item, folder and capacity clients over caller-injected credentials and execution policies |
| [Artifact-first tables](docs/artifact-first-tables.md) | Parent-owned Lakehouse, KQLDatabase and Warehouse SDK adapters with distinct native loading contracts |
| [HTTP execution](infrastructure/http/fabric-sdk-http/README.md) | Shared authentication, request limits, retries, paging and operation adapters |
| [Native transport](infrastructure/http/fabric-sdk-transport/README.md) | Optional `hyper-rustls` backend and asynchronous timers |
| [Deployment](capabilities/deployment/fabric-deploy/README.md) | Configuration, parameters, repository planning, item publication and orphan removal |
| [CLI](hosts/cli/fabric-cli/README.md) | Argument parsing, command sessions and native host composition |
| [V2 local host](hosts/cli/fabric-cli-v2/README.md) | Separate six-leaf `cli` group, explicit effects and bounded optional providers |
| [Interactive shell](hosts/repl/fabric-cli-repl/README.md) | Optional terminal editor and shell loop, independent of SDK and command implementations |
| [Release updates](hosts/update/fabric-cli-update/README.md) | Explicit release trust, bounded downloads and installation-custody checks |
| [Path context](hosts/cli/fabric-cli-context/README.md) | Immutable hierarchy, path resolution and command context |
| [Name search](infrastructure/search/fabric-name-search/README.md) | Reusable, deterministic typo suggestions over caller-authorized names; never automatic correction |
| [Output](hosts/cli/fabric-cli-output/README.md) | Structured envelopes, text presentation and stream handling |
| [Workflow skills](skills/README.md) | Boundaries for workflows that compose CLI commands |

Focused SDK crates provide additional service APIs, including jobs, OneLake,
access, catalog, tables and raw requests. Each crate documents its own public
interface and supported operations.

Service and deployment libraries do not discover credentials, prompt users, create a runtime, initialize
global logging, or terminate the process. The caller supplies these capabilities
and owns cancellation and output delivery.

The CLI's optional `repl` feature enables a terminal-only interactive shell when
started without arguments. Its separate package and feature edge allow a future
major version to remove it without changing shared commands or SDK consumers.
The default CLI and SDK dependency graphs do not include Reedline.

## Unattended authentication

The optional `headless` feature composes the native service host with explicitly
selected unattended credentials. `FAB_RUST_AUTH_MODE` selects client-secret,
renewable assertion-file, workload-identity, or a supported managed-identity host
contract. It does not discover a credential chain, open login UI, launch another
authentication tool, or persist a local token cache.

Headless service commands acquire tokens when needed; they do not require a
desktop `auth login`. Identity inputs and service permissions must be configured
for the selected environment. Never supply secrets as command-line arguments.
See the [identity guide](infrastructure/identity/fabric-identity/README.md) for
provider inputs, assertion renewal and managed-host limits.

The SDK continues to accept caller-injected credentials independently of these
CLI features. Public SDK package publication is separate from CLI releases.

## Configure remote commands

The `native-read` feature enables the Windows service host. Despite the feature
name, this host also composes job commands, definition transfers and deployment.
It uses one credential selection, asynchronous runtime and shared HTTP executor.

Log in once, then run service commands using the saved non-secret selection:

```powershell
.\target\debug\fab-rs.exe auth login
.\target\debug\fab-rs.exe ls /
```

These commands require a `native-read` build. On first login WAM presents its
work-account picker; use `--tenant` to pin a specific guest or resource tenant.
Explicit non-secret environment selectors remain available for scripting:

```powershell
$env:FAB_RUST_TENANT = "<tenant-guid>"
$env:FAB_RUST_ACCOUNT = "<work-account-UPN>"
$env:FAB_RUST_CLIENT_ID = "<approved-public-client-guid>"
cargo run --locked -p fabric-cli --features native-read -- ls /
```

Service calls require an authorized account and an applicable client
registration. The host uses silent Windows credentials; it does not select an
arbitrary first account or fall back to another authentication tool.

Optional settings cover the configuration root, command context and Fabric
endpoint. Read the [native host guide](docs/native-read-host.md) before choosing
these settings. An accepted mutation is not necessarily complete, and stopping
local waiting does not roll back remote or filesystem effects.

<a id="run-an-actual-windows-broker-login"></a>

## Windows authentication

The optional `windows-broker` feature provides `auth login`, `auth status` and
`auth logout`. This authentication backend is experimental and requires an
interactive Windows desktop for sign-in UI.

```powershell
$tenant = "<tenant-guid>"
$account = "<work-account-UPN>"
$client = "<approved-public-client-guid>"
cargo run --locked -p fabric-cli --features windows-broker -- auth login --tenant $tenant --account $account --client-id $client
cargo run --locked -p fabric-cli --features windows-broker -- auth status --tenant $tenant --account $account --client-id $client
cargo run --locked -p fabric-cli --features windows-broker -- auth logout --tenant $tenant --account $account --client-id $client
```

These are live authentication commands. Account and tenant arguments are optional:
login reuses a saved selection or opens the WAM picker when selection is incomplete.
`--client-id` explicitly overrides the saved/environment client registration.
Complete selections attempt silent acquisition first and can open broker sign-in
or MFA UI when interaction is required. Status is
silent and never opens sign-in UI. Login requests Fabric, OneLake and ARM tokens;
all identity and expiry checks must succeed before it reports success.

The default public client ID is `5814bfb4-2705-4994-b8d6-39aabeb5eaeb`. An
explicit registration must support the requested APIs and the desktop broker
redirect `ms-appx-web://microsoft.aad.brokerplugin/<client-id>`. These commands
do not register applications, grant consent or change tenant policy.

Tokens remain in memory or the broker-managed cache. Diagnostics do not include
tokens, and no plaintext token input or developer-tool fallback is supported.
Only the selected tenant, UPN and client ID are saved in `broker-session.json`
under `FAB_RUST_CONFIG_ROOT` or `%LOCALAPPDATA%\Microsoft\FabricCLI`. This is not
a token cache. Later commands, status and logout reuse that binding without
prompting; a complete explicit selection takes precedence.
Sign-out affects the selected account's tokens for that client registration on
this device, including other applications using the same registration. It does
not remove the Windows account or revoke sessions on other devices.

Ctrl+C stops local waiting. `--timeout-seconds` accepts 1-600 seconds and
defaults to 120. Cancellation and timeout do not guarantee that broker cache
updates or native cleanup have stopped.

Current authentication constraints include public-cloud Windows desktop use,
validated broker-resolved account/tenant selection, and no browser or device-code fallback.
Guest-account behavior, tenant policies and service authorization require
environment-specific validation. Non-Windows native authentication fails
explicitly. Offline checks do not establish live MFA or tenant-policy support.

## Build and validate

```text
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --offline --locked
cargo test --offline --locked --doc
```

Optional host features require their own checks. Help is available without
authenticating:

```text
cargo run --locked -p fabric-cli --features windows-broker -- auth login --help
cargo run --locked -p fabric-cli --features native-read -- deploy --help
```

See [architecture and validation commands](docs/architecture.md) and
[build validation](docs/build-validation.md) for platform checks, dependency
boundaries and shared-host execution limits.

## Support boundaries

The executable is named `fab-rs`. This workspace is not an installation or
general-availability support commitment. Default and broker-only builds do not
install a remote command host. Native credential execution is Windows-only;
library consumers on other platforms must inject their own supported
credentials and execution capabilities.

Interactive shell mode, complete terminal styling and several command-specific
features remain outside the current host. Advanced deployment regex and JSONPath
expressions have explicit supported subsets and resource limits. Unsupported
operations fail rather than returning successful-looking results.

## Contributing

- [Agent and contributor instructions](AGENTS.md)
- [Architecture](docs/architecture.md)
- [Delivery and review requirements](docs/agentic-delivery.md)
- [Integration handoff](docs/integration-handoff.md)
- [Authentication requirements](docs/identity-requirements.md)
- [Deployment behavior and limits](docs/deployment-compatibility.md)
- [Extension boundaries](docs/extensions-and-skills.md)

The repository's Microsoft copyright, MIT license and applicable attribution
apply to this workspace and must be retained in redistributed artifacts.
