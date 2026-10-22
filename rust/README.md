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
no service calls. Authentication, service operations, and deployment are not
implemented. Azure CLI, Azure Developer CLI, and Python are not runtime
requirements for this scaffold.

Read before contributing:

- [Agent instructions](AGENTS.md)
- [Translation-first migration guidance](docs/migration-guidance.md)
- [Architecture and validation commands](docs/architecture.md)
- [Compatibility backlog and research baselines](docs/compatibility.md)
- [Unresolved identity backend](docs/decisions/identity-backend.md)

The repository's Microsoft copyright, MIT license, and applicable attribution
continue to apply. Moving this directory into a new product destination later
must retain those license and attribution files. GitHub Actions stays at the
repository root and runs Cargo from this directory; its location does not create
a product runtime dependency on Python.
