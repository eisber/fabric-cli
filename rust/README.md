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

## Workspace components

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
infrastructure\
  core\            Shared contracts and JSON encoding
  identity\        Credential selection and optional Windows broker
  http\            Shared executor and native transport
  execution\       Paging and operation reconciliation
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
| [HTTP execution](infrastructure/http/fabric-sdk-http/README.md) | Shared authentication, request limits, retries, paging and operation adapters |
| [Native transport](infrastructure/http/fabric-sdk-transport/README.md) | Optional `hyper-rustls` backend and asynchronous timers |
| [Deployment](capabilities/deployment/fabric-deploy/README.md) | Configuration, parameters, repository planning, item publication and orphan removal |
| [CLI](hosts/cli/fabric-cli/README.md) | Argument parsing, command sessions and native host composition |
| [Path context](hosts/cli/fabric-cli-context/README.md) | Immutable hierarchy, path resolution and command context |
| [Output](hosts/cli/fabric-cli-output/README.md) | Structured envelopes, text presentation and stream handling |
| [Workflow skills](skills/README.md) | Boundaries for workflows that compose CLI commands |

Focused SDK crates provide additional service APIs, including jobs, OneLake,
access, catalog, tables and raw requests. Each crate documents its own public
interface and supported operations.

Libraries do not discover credentials, prompt users, create a runtime, initialize
global logging, or terminate the process. The caller supplies these capabilities
and owns cancellation and output delivery.

## Configure remote commands

The `native-read` feature enables the Windows service host. Despite the feature
name, this host also composes job commands, definition transfers and deployment.
It uses one credential selection, asynchronous runtime and shared HTTP executor.

Set the required nonsecret selectors before running a service command:

```powershell
$env:FAB_RUST_TENANT = "<tenant-guid>"
$env:FAB_RUST_ACCOUNT = "<work-account-UPN>"
$env:FAB_RUST_CLIENT_ID = "<approved-public-client-guid>"
cargo run --locked -p fabric-cli --features native-read -- ls /
```

Service calls require an authorized account and an applicable client
registration. The host uses silent Windows credentials; it does not select an
ambient account or fall back to another authentication tool.

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

These are live authentication commands. Login attempts silent acquisition first
and can open broker sign-in or MFA UI when interaction is required. Status is
silent and never opens sign-in UI. Login requests Fabric, OneLake and ARM tokens;
all identity and expiry checks must succeed before it reports success.

The default public client ID is `5814bfb4-2705-4994-b8d6-39aabeb5eaeb`. An
explicit registration must support the requested APIs and the desktop broker
redirect `ms-appx-web://microsoft.aad.brokerplugin/<client-id>`. These commands
do not register applications, grant consent or change tenant policy.

Tokens remain in memory or the broker-managed cache. Diagnostics do not include
tokens, and no plaintext token input or developer-tool fallback is supported.
Sign-out affects the selected account's tokens for that client registration on
this device, including other applications using the same registration. It does
not remove the Windows account or revoke sessions on other devices.

Ctrl+C stops local waiting. `--timeout-seconds` accepts 1-600 seconds and
defaults to 120. Cancellation and timeout do not guarantee that broker cache
updates or native cleanup have stopped.

Current authentication constraints include public-cloud Windows desktop use,
explicit account and tenant selection, and no browser or device-code fallback.
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
