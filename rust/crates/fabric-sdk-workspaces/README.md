<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric workspace SDK

This focused service crate owns the offline workspace-list operation extracted
from SDK checkpoint `c07228b5ed5266b899ff3d2b37c7a3778cb855b8`.
The Python behavior baseline remains
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
See the facade's [source and compatibility mapping](../fabric-sdk/README.md)
for the original request/response/error behavior and intentional exceptions.

Direct consumers use `fabric_sdk_workspaces::WorkspaceClient`. The caller
provides an Azure `TokenCredential`, explicit selection, a `WorkspaceExecutor`,
finite limits and cancellation future. Construction acquires no token and does
no I/O. List execution uses the caller's runtime; no runtime, global logger,
production transport, identity backend or ambient credential discovery is added.

The implemented operation preserves ordered service fields/duplicates and
unknown workspace types, validates nonterminal continuation origins, bounds
page/item/response work, and returns typed incomplete errors with partial
progress. Retry, timeout and long-running-operation policy are not implemented
by this offline executor seam. It is not a full deployment API or Python CLI port.

## Ownership and dependencies

The crate belongs to the `sdk` layer. Its production dependencies are
`fabric-core` and the centralized Azure/serialization types. It has no dependency
on `fabric-sdk`, `fabric-identity`, `fabric-deploy`, or `fabric-cli`.
The facade depends on this crate and preserves existing consumer import paths
through reexports and client delegation. New service families must not be added
here merely to share a client; assign them their own implemented boundary.

All pre-extraction operation/model cases move with the implementation and read
the same unchanged independent reference fixtures. A standalone integration
test uses only public types from this crate. Facade tests separately check
wire/error type identity, identical execution/results, invalid audiences,
cancellation/drop behavior and a `Send` future.

From `rust/`, after obtaining a named shared-host slot:

```text
cargo test --offline --locked -p fabric-sdk-workspaces --all-targets -- --test-threads=1
cargo clippy --offline --locked -p fabric-sdk-workspaces --all-targets -- -D warnings
cargo test --offline --locked -p fabric-sdk-workspaces --doc
```

Changes to oracle expectations belong to the independent validation lane.
Passing these tests does not establish live-service, full CLI, retry/LRO, or
unexecuted-platform parity.

## Extraction validation

The owner-granted serial slot `sdk-workspaces-extraction-20261023-b` exercised
both SDK crates on Rust/Cargo 1.99.1, `x86_64-pc-windows-msvc`.
The combined all-target run passed 39 tests: all 30 pre-extraction unit cases
(27 focused-crate and 3 facade cases), 8 external consumer cases, and the existing
credential example test. Both compiled documentation examples, all-target Clippy
with warnings denied, formatting, and all 6 workspace architecture tests passed.
The normal dependency tree excludes the facade, identity, deployment and CLI
implementations. No third-party lockfile version or independent fixture changed.

The independent extraction/source-compatibility review found no significant
issues. Full-workspace integration and native macOS/Linux execution remain
separate gates; these results do not validate the deployment lane's new code.
