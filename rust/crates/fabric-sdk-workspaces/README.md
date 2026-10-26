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

## Ordered workspace metadata GET

The additive `WorkspaceClient::get_workspace_metadata` reads primary metadata,
managed private endpoints, Spark settings and role assignments through the same
executor, read policies and bounded pager. `WorkspaceMetadataResult` retains the
ordered object and typed per-step omission outcomes; it never reports a safety
failure as complete partial metadata. The legacy LIST models and public struct
literals remain unchanged. See the
[source/consumer matrix](../../docs/workspace-get-compatibility.md) for API shape,
source quirks, exact-versus-safety boundaries and independent offline evidence.

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

## Resource mutations

The additive `mutations` module describes validated workspace CREATE/PATCH/DELETE
and capacity assignment requests and executes them through caller-injected
`ResourceOperations`. This shared policy boundary is also consumed by the focused
item/folder SDKs; it does not implement another HTTP/retry/LRO stack or select
credentials. Accepted responses remain distinct from completed responses, and
errors retain operation-ID and uncertain-mutation evidence.
See the [mutation matrix](../../docs/mutation-compatibility.md) for the pinned
source, actual CLI consumers, omissions and integration gates.

`ResourceOperation::with_continuation_token(&str)` is an explicit opt-in query
attachment for a source command that reuses `args.request_params`. It neither
records read state nor inherits tokens into unrelated operations, accounts,
tenants, polls or future commands. The CLI command owner must provide actual
last-dispatched read evidence, not a collection's next unconsumed token.
`new`, `encoded`, ordinary `with_query`, bodies, completion and origin rules
are unchanged. Empty token means no token override/addition, while existing
query values still receive source dictionary/encoding treatment. Controls fail
before dispatch.
Existing query keys overwrite at their first insertion position; repeated token
attachment replaces, never appends a duplicate. `with_hard_delete` still requires
DELETE and defaults to the same exact route; query composition now retains other
keys and overwrites `hardDelete` instead of introducing a second question mark.

The opt-in source query path reuses Azure's existing URL query serializer and
adjusts its two safe-character differences to Python `quote_plus`: spaces use
`+`, star is `%2A`, tilde is `~`. Literal percent escapes stay opaque and are
encoded exactly once. No copied percent-encoding loop, pager dependency (which
would cycle back to this crate), runtime, or transport is added. Existing caller
operation/request/response/cancellation bounds remain unchanged; no unsupported
token-size guarantee is inferred. `ResourceOperation` debug omits its path.

`fabric-sdk-operations/examples/continuation_query.rs` exercises the existing
public `ResourceOperations`/`FabricOperation::from_resource` adapter with synthetic
credentials/execution. Its development-only source comparison reuses the original
Python source loaders and runs pinned `do_request` itself; this is not a shipped
SDK-to-CLI dependency. Managed command-owned state and read observers are separate
owners and remain incomplete until their paired consumers pass.

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
