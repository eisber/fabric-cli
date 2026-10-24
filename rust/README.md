<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental Rust Fabric workspace

This self-contained workspace stages the Fabric SDK, thin `fab-rs` CLI, optional
identity implementation, and native deployment library before a future explicit
cutover. The Python CLI remains authoritative. This is an unpublished offline
scaffold, not a completed port.

Change into this directory before running Cargo so Rustup uses the pinned
Rust 1.99.1 toolchain:

```text
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-sdk --example injected_credentials
```

Only help and version are implemented in the CLI. Other invocations fail
explicitly. The example uses an expired test-only dummy credential and makes
no service calls. Native workspace listing and a bounded
[single-Notebook deployment library](crates/fabric-deploy/README.md) now execute
through caller-injected SDK executors. No production HTTP transport, authentication
backend, or CLI deployment command is installed. Azure CLI, Azure Developer CLI,
and Python are not runtime requirements.

The focused [capacity-list SDK](crates/fabric-sdk-capacities/README.md) adds bounded
Fabric `/v1/capacities` reads through standalone, facade and shared HTTP executor
interfaces. It does not add ARM capacity operations or CLI commands. See the
[capacity compatibility matrix](docs/capacity-compatibility.md) for source quirks
and explicit SDK safety differences.

Windows desktop interop is isolated in optional `fabric-identity-windows`,
enabled through `fabric-identity/windows-broker`. The existing crates forbid
unsafe code; default consumer graphs exclude the native crate. Enabling it
provides safe Rust calls over reviewed unsafe interop, not a transitively
unsafe-free binary or a completed login provider. See the
[native boundary](docs/architecture.md#optional-native-unsafe-boundary).

Read before contributing:

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
