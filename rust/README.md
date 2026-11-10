<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Microsoft Fabric CLI and SDK for Rust

This workspace provides the `fab-rs` command-line application, reusable Fabric
service clients, deployment orchestration, and optional native authentication.
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
See the [CLI guide](clients/cli/app/README.md) for command composition and
[native host configuration](authoring-docs/native-read-host.md) for service execution.

## Workspace components

The source roots separate reusable functionality from CLI-specific interaction.
Paths below are relative to this directory; each package leaf retains its own
manifest, source, tests, examples and assets:

```text
engine/
  sdk/             Reusable convenience SDK entry point
  control-plane/
    resources/     contracts/, capacities/, workspaces/, folders/ and items/
      items/       Generic item client, with focused notebooks/ package
    jobs/          Job APIs and schedules
    acl/           Access APIs
    catalog/       Metadata discovery
    deployment/    Reusable deployment workflows
  data-plane/
    onelake/       client/, http/ adapter and tables/ workflows
  core/
    contracts/     Endpoint and credential contracts
    json/          Shared ordered JSON encoding
    auth/          provider/ credentials and isolated windows/ interop
    http/          requests/, executor/ and optional transport/
    pagination/    Shared paging
    lro/           Long-running-operation lifecycle
clients/cli/
  app/             fab-rs executable and central composition
  contracts/       Shared CLI values, errors and injected adapters
    import-export/ Shared import/export interfaces and file-policy adapters
  commands/
    resources/    Create, update, remove and resource lifecycle commands
    import/        Item-definition import and Environment publication
    export/        Item-definition export and bulk-export
    access/        ACL and label commands
    catalog/       Find and hidden-resource discovery
    api/           Explicit raw API commands
    onelake/       files/ and tables/ commands (separate packages)
    jobs/          Job execution and scheduling
    local/         Local settings and navigation
  compatibility/
    resource-commands/ Original aggregate's public APIs and adapters
    import-export/ Original fabric-cli-transfer APIs and delegation
  state/           Path resolution and command context
  query/           CLI query projection
  output/          Presentation and streams
  repl/            Optional terminal editor and shell loop
  self-updater/    Pinned-trust executable verification and replacement
authoring-docs/     Architecture, migration, compatibility and validation guides
```

Each leaf crate retains its package name and dependency boundaries. Grouping
directories are not additional packages. Physical placement under `engine/` does
not change logical layers or make authentication providers dependencies of SDK
service clients. CLI deployment handling stays in `clients/cli/app/src/deploy.rs`;
the reusable workflow library is `engine/control-plane/deployment/`. Additional front-ends,
such as MCP servers or Azure DevOps tasks, can add host composition and
capability-specific adapters without invoking the CLI executable. Those
front-ends are not implemented by this layout.

Control/data-plane groups describe capability ownership, not every network
request: table capabilities also orchestrate load and maintenance jobs.
`engine/sdk/` is a convenience facade, not the owner of Notebook service
implementation or a complete umbrella for every capability. Focused crates
remain directly usable.
See [architecture](authoring-docs/architecture.md#physical-organization) for directory
responsibilities and unchanged dependency-layer rules.

## Find the owner

Start with the capability, then its role. `parser.rs` handles argument schemas
and typed conversion, `host.rs` composes injected execution, `client.rs` implements
service calls, and `models.rs` defines wire/data contracts. Operation-named files
such as `import.rs` and `export.rs` own those workflows. Not every package needs
every role or another directory.

`app/` means the executable application: process startup, root command dispatch,
runtime/authentication composition and user-owned streams. It does not mean a
second service SDK. `compatibility` preserves older Rust imports, not a second
implementation or an implied API version. Cargo package names intentionally stay
stable when their physical directories change.

Resource definitions describe Fabric items; OneLake files/tables contain data.
`commands/import/` and `commands/export/` own the corresponding definition
workflows, not general table-data import/export. Their shared interfaces are in
`contracts/import-export/`; neither command depends on the other or the legacy
`compatibility/import-export/` adapter. That adapter preserves the original
`fabric-cli-transfer` host, parser composition and error rendering.
Their import/export or copy semantics are different, so they do not share a
generic transfer directory. Table loading can call Fabric Lakehouse REST while
remaining a data workflow. This layout adds no new command or data import/export
capability.

| Component | Responsibility |
| --- | --- |
| [Fabric SDK](engine/sdk/README.md) | Workspace, item, folder and capacity clients over caller-injected credentials and execution policies |
| [HTTP execution](engine/core/http/executor/README.md) | Shared authentication, request limits, retries, paging and operation adapters |
| [Native transport](engine/core/http/transport/README.md) | Optional `hyper-rustls` backend and asynchronous timers |
| [Deployment](engine/control-plane/deployment/README.md) | Configuration, parameters, repository planning, item publication and orphan removal |
| [CLI](clients/cli/app/README.md) | Argument parsing, command sessions and native host composition |
| [Interactive shell](clients/cli/repl/README.md) | Optional terminal editor and shell loop, independent of SDK and command implementations |
| [Release updates](clients/cli/self-updater/README.md) | Explicit release trust, bounded downloads and installation-custody checks |
| [Path context](clients/cli/state/README.md) | Immutable hierarchy, path resolution and command context |
| [Output](clients/cli/output/README.md) | Structured envelopes, text presentation and stream handling |
| [Workflow skills](skills/README.md) | Boundaries for workflows that compose CLI commands |

Focused SDK crates provide additional service APIs, including jobs, OneLake,
access, catalog, tables and raw requests. Each crate documents its own public
interface and supported operations.

For current implementation boundaries, use the
[command/resource acceptance inventory](authoring-docs/command-parity.md), not historical
scaffold tables. [Coverage reporting](authoring-docs/test-coverage.md) distinguishes actual
line/function/region measurements from registered commands and test counts.

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
See the [identity guide](engine/core/auth/provider/README.md) for
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
endpoint. Read the [native host guide](authoring-docs/native-read-host.md) before choosing
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

See [architecture and validation commands](authoring-docs/architecture.md) and
[build validation](authoring-docs/build-validation.md) for platform checks, dependency
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
- [Architecture](authoring-docs/architecture.md)
- [Delivery and review requirements](authoring-docs/agentic-delivery.md)
- [Integration handoff](authoring-docs/integration-handoff.md)
- [Authentication requirements](authoring-docs/identity-requirements.md)
- [Deployment behavior and limits](authoring-docs/deployment-compatibility.md)
- [Extension boundaries](authoring-docs/extensions-and-skills.md)

The repository's Microsoft copyright, MIT license and applicable attribution
apply to this workspace and must be retained in redistributed artifacts.
