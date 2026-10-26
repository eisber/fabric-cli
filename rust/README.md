<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental Rust Fabric workspace

This self-contained workspace stages the Fabric SDK, thin `fab-rs` CLI, optional
identity implementation, and native deployment library before a future explicit
cutover. The Python CLI remains authoritative. Default builds are offline;
the opt-in Windows authentication slice is experimental, not a completed port.

Change into this directory before running Cargo so Rustup uses the pinned
Rust 1.99.1 toolchain:

```text
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-sdk --example injected_credentials
```

An explicit `windows-broker`
feature adds experimental Windows `auth login`, `auth status`, and `auth logout`.
It does not configure service read execution or replace its current parser/host.
The CLI recognizes Python's `ls` (`dir`) and `get` names, but the binary returns
typed `NotConfigured` in default and broker-only builds. Opting into
`native-read` supplies an explicitly configured Windows read host; see
[native read setup and limits](docs/native-read-host.md). Its
[public async read-command library](crates/fabric-cli/README.md) composes real
SDK reads, immutable context and JSON output using caller-injected dependencies.
Default no-query LS works; GET/query output needs an injected projection provider.
Other invocations fail explicitly. The example uses an expired test-only dummy credential and makes
no service calls. Native workspace listing and a bounded
[single-Notebook deployment library](crates/fabric-deploy/README.md) now execute
through caller-injected SDK executors. The optional
[native transport](crates/fabric-sdk-transport/README.md) supplies an Azure
`HttpClient` and caller-runtime `TokioTimer`; its `hyper-rustls` backend must be
selected explicitly. The SDK facade has no default HTTP/TLS backend, and no
default read host or CLI deployment command is installed.
Azure CLI, Azure Developer CLI, Python and .NET are not runtime requirements.

Generic workspace item LIST and single-item GET are available through the
[focused item SDK](crates/fabric-sdk-items/README.md) and additive stable facade
methods. The existing shared HTTP executor implements its injected item adapter.
The [item compatibility matrix](docs/item-compatibility.md) distinguishes
source-backed behavior, explicit safety exceptions and unported CLI features.

The focused [capacity-list SDK](crates/fabric-sdk-capacities/README.md) adds bounded
Fabric `/v1/capacities` reads through standalone, facade and shared HTTP executor
interfaces. It does not add ARM capacity operations or CLI commands. See the
[capacity compatibility matrix](docs/capacity-compatibility.md) for source quirks
and explicit SDK safety differences.

The pure CLI-layer [JSON output](crates/fabric-cli-output/README.md) and
[path/context](crates/fabric-cli-context/README.md) libraries now translate
bounded Python behavior with independent source comparisons. The read-command
library now consumes both; `native-read` additionally composes the executable host.
See [read-command compatibility](docs/read-command-compatibility.md).
See their [output](docs/output-compatibility.md) and
[context](docs/context-compatibility.md) matrices for unported behavior.

Windows desktop interop is isolated in optional `fabric-identity-windows`,
enabled through `fabric-identity/windows-broker`. The existing crates forbid
unsafe code; default consumer graphs exclude the native crate. Enabling it
provides safe Rust calls over reviewed unsafe interop, not a transitively
unsafe-free binary or a production policy-support guarantee. See the
[native boundary](docs/architecture.md#optional-native-unsafe-boundary).

## Run an actual Windows broker login

From this `rust/` directory, on an interactive Windows desktop:

```powershell
$tenant = "<your-tenant-guid>"
$account = "<your-work-account-UPN>"
cargo run --locked -p fabric-cli --features windows-broker -- auth login --tenant $tenant --account $account
```

This is **live authentication**, not an offline test. It searches only for the
explicit account, attempts silent acquisition first, then opens broker sign-in/MFA
UI when required. It separately requests Fabric, OneLake and ARM, in Python login
order. Choosing another account is rejected, not treated as a successful switch.
All three acquisitions and response-identity/expiry checks must succeed before
the command reports `logged_in: true`. No Fabric, OneLake or ARM service request
is made, and successful acquisition does not independently prove MFA or service
authorization.

The default client ID matches Python Fabric CLI:
`5814bfb4-2705-4994-b8d6-39aabeb5eaeb`. Its applicability to this direct-WAM path
still needs live validation. If using a team-approved public client registration,
pass `--client-id <client-guid>` consistently to all three commands. The
registration must support the requested APIs and desktop broker redirect
`ms-appx-web://microsoft.aad.brokerplugin/<client-id>`. The CLI does not register
apps, change redirects, grant consent or change tenant policy on your behalf.
Stop and consult the registration owner if consent/configuration is unavailable;
do not disable security rules to make a login pass.

To validate silent reuse, then explicitly sign out:

```powershell
cargo run --locked -p fabric-cli --features windows-broker -- auth status --tenant $tenant --account $account
cargo run --locked -p fabric-cli --features windows-broker -- auth logout --tenant $tenant --account $account
```

`status` is a live **silent** token check and can refresh through WAM; it never
opens sign-in UI. Alternatively, use `auth login --no-interactive` with the same
selectors. No account/tenant is discovered from environment variables or a saved
first-account default. Repeat the explicit selectors on subsequent commands.

Tokens stay in memory or the broker-owned cache, never local plaintext files,
stdout, stderr or Debug output. JSON reports contain only account/tenant and
per-scope expiry metadata. The SDK's opt-in `WindowsCredential` implements Azure
`TokenCredential`; its subsequent requests are silent and pinned to the selected
native account. No raw access-token input or developer-tool fallback is supported.

Ctrl+C requests cancellation; `--timeout-seconds` bounds the CLI wait (default
120 seconds, valid range 1-600). Cancellation/timeout are not rollback: broker
cache updates or native cleanup can still be pending after a failed/partial
login. `logout` clears only this client's selected broker account tokens on this
device; it does not remove the Windows account or revoke sessions elsewhere.
No Python CLI configuration, navigation state or cache file is changed.
Using Python's default client ID can nevertheless affect broker tokens shared
under that registration, including Python CLI broker sessions, when signing out.

Current limits: public cloud, Windows desktop only, explicit UPN and tenant GUID;
no headless-service UI, browser fallback, device-code flow or general CAE/claims
challenge handling. Guest-account/provider alias behavior and all three live
audiences need representative tenant validation. This experimental code has not
yet been validated with a real account/MFA flow; passing offline tests is not that
proof. Non-Windows builds return a sanitized unsupported error.

Compile/help and offline validation do **not** sign in:

```text
cargo run --locked -p fabric-cli --features windows-broker -- auth login --help
cargo test --offline --locked -p fabric-identity-windows -p fabric-identity -p fabric-core -p fabric-cli --features fabric-cli/windows-broker
```

Read before contributing:

- [Complete Python command inventory, crate owners and gaps](docs/command-parity.md)
- [Shared `feature/rust` integration and safe synchronization](docs/integration-handoff.md)
- [Agent instructions](AGENTS.md)
- [Translation-first migration guidance](docs/migration-guidance.md)
- [Architecture and validation commands](docs/architecture.md)
- [Compatibility backlog and research baselines](docs/compatibility.md)
- [Unresolved identity backend](docs/decisions/identity-backend.md)
- [Integration validation baselines](docs/validation-baselines.md)
- [Native build evidence](docs/build-validation.md)
- [Workspace-list validation matrix](docs/validation-matrix.md)
- [Deployment compatibility](docs/deployment-compatibility.md)
- [Identity requirements and evidence](docs/identity-requirements.md)
- [Extension and workflow boundaries](docs/extensions-and-skills.md)

The repository's Microsoft copyright, MIT license, and applicable attribution
continue to apply. Moving this directory into a new product destination later
must retain those license and attribution files. GitHub Actions stays at the
repository root and runs Cargo from this directory; its location does not create
a product runtime dependency on Python.
