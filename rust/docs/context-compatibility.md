<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded Fabric path and context resolution

`fabric-cli-context` is a pure CLI-layer library, not complete Python
`get_command_context` parity and not a service command or REPL. Python remains
authoritative. Source pin: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

This page records the original `Resolver` contract and its unchanged oracle.
The additive `PathResolver` follow-up covers virtual, OneLake and explicitly
injected local-fallback branches without reclassifying the original six
unsupported fixtures. See the [extended source branch ledger](context-branch-ledger.md)
for those new APIs, source branch mappings and evidence boundaries.

## Source and consumers

The translation follows `src/fabric_cli/core/fab_handle_context.py`:
`get_command_context`, `process_absolute_path`, `process_relative_path`,
`process_path_part`, the tenant/workspace/folder handlers, and their `_get_*_id`
boundaries. Metadata follows `hiearchy/fab_element.py`, `fab_tenant.py`,
`fab_workspace.py`, `fab_folder.py`, `fab_base_item.py`, `fab_item.py`, and
`fab_types.py`. Status/message mappings come from `fab_constant.py`,
`errors/common.py`, and `errors/hierarchy.py`.

Python `commands/fs/fab_fs.py` resolves paths for `ls`, `cd`, `mkdir`, `exists`,
copy/move and other commands. Creation-target consumers pass `raise_error=False`;
`fab_fs_cd` separately performs the explicit context change. Commands also check
command support after resolution. This library does not claim those command
checks, service operations, output or persistence are ported.

## Interface and ownership

`Element` holds typed metadata, opaque optional IDs and immutable `Arc` parent
links. Construction enforces tenant/workspace/folder/item parent relationships.
Callers provide current context and `HomeContext` explicitly; home may carry a
caller-resolved error. No discovery, global state, filesystem, identity, network,
SDK cache, runtime, blocking I/O or persisted context is implemented.
An unspecified home returns typed `HomeDiscovery` unsupported only if a tilde
path actually requests ambient discovery.

`Lookup` accepts a typed operation, scope and canonical full name. It is an
already-resolved metadata boundary, implementable by a closure or trait object.
Service loading, cache refresh and API error acquisition belong outside this
algorithm. An optional ordered immutable `Catalog` applies the pinned mem-store's
name/scope/duplicate matching to a caller-owned graph; it is not an SDK or mock
production service. Lookup failures retain their exact code and message.
Python callers pass the existing exception's normalized `status_code`/`.message`,
not raw constructor strings (Python applies defaults and removes trailing dots).

`Resolver::resolve` never mutates current context. `change_context` replaces it
only after complete successful resolution; a partial traversal error leaves the
original `Arc` intact. Consumer command-support validation remains separate.
`Element::{kind,name,id,path,path_id}` expose the complete resolved result.
The crate's `examples/resolve.rs` is an independent, offline consumer.

## Preserved source behavior

- Scalar arguments preserve quotes. Token lists join with single spaces and
  strip all leading/trailing single/double quote characters, not balanced quotes.
- `/` and `/..` return the selected tenant without lookup. Relative paths start
  at explicit current context; `~` and `~/` replace only the initial tilde using
  the supplied full home-workspace name.
- Trailing slashes are removed before splitting, including escaped final slashes.
  Slashes immediately preceded by a backslash do not split; all backslashes are
  then removed and each token is Python-whitespace-stripped.
- Empty and dot tokens are skipped; parents clamp at tenant root. Every token
  beginning with at least three dots is invalid before lookup.
- Type suffixes are canonicalized using Python-style Unicode lowercasing, while
  display-name matching is case-sensitive with no Unicode normalization.
  Dotted display names retain all but the final suffix.
- Folder ID paths omit folder IDs. Missing IDs are rendered as `None` in ID
  paths. Workspace display paths escape embedded slashes; item and folder names
  do not introduce an OS-path abstraction.
- Item lookup is workspace-wide even beneath a folder. Initial folder lookup
  likewise scans all workspace folders; nested-folder lookup checks parentage.
  Duplicate names select the first match, not an invented ambiguity error.
  The resolved path uses the requested parent, not the catalog match's parent.
- `raise_error=False` suppresses only lookup `NotFound`. Missing workspace/folder
  nodes can be returned or traversed back out of, but attempting to descend
  beneath one still fails. Other lookup errors are never suppressed.
- Strict ASCII display names apply only to Lakehouse, MLExperiment, MLModel
  and Eventstream path tokens. Other supported item types retain Python's broad
  names, including empty item names in folder context. Hidden-token dispatch
  differs between workspace and folder contexts.

## Explicit unported branches, not parity waivers

Tenant virtual collections, workspace virtual item containers (including external
data shares), and traversal beneath items into OneLake return typed
`ErrorKind::Unsupported` with a specific `UnsupportedBranch`. A Fabric item itself,
including a Lakehouse, is supported; its data-plane children are not.
Invalid hidden collections still follow source `InvalidPath` dispatch.

`PathInput::Local` and the entire `supports_local_path=True` fallback mode return
typed `LocalFilesystem` unsupported, without consulting the catalog or filesystem.
The latter is an explicit narrower interface: Python first tries Fabric and only
then conditionally accesses the filesystem. This library does not silently treat
that requested mixed-resolution mode as Fabric-only success. Callers must tag
known local paths instead of passing them as untyped Fabric scalars.

Home identity selection is injected, not discovered. Cache invalidation,
duplicate API requests on misses, authentication-dependent home discovery,
command capability checks, warnings for external sharing, all virtual hierarchy,
OneLake metadata, local fallback and context persistence remain outside this
slice. No broad validity exception or general `get_command_context` claim is made.

## Independent evidence

The source-pinned Python oracle and hand-authored expectations live under
`tests/compatibility/context`. It executes real source handlers and hierarchy
against an offline metadata boundary rather than reimplementing path resolution
or generating expectations from Rust. Exact supported comparisons and deliberately
unsupported branches have separate denominators. Existing workspace, item, HTTP
and deployment fixtures are unchanged.

Run targeted validation from `rust/` only with the shared-host parent's named
grant, using a private target directory and one build job/test thread:

```text
cargo test --offline --locked -p fabric-cli-context -- --test-threads=1
cargo test --offline --locked -p fabric-cli --test workspace_contract -- --test-threads=1
cargo clippy --offline --locked -p fabric-cli-context --all-targets -- -D warnings
cargo fmt -p fabric-cli-context -- --check
python tests/compatibility/context/reference.py
```

Native Linux/macOS evidence, command integration and full Fabric path coverage
remain separate gates.

### Executed bounded validation on October 24, 2026

Under the named `context-parity-20261024-a` parent grant, Rust 1.99.1
(`58cf8f9c3`, LLVM 23.1.1) ran on `x86_64-pc-windows-msvc` with a private
artifact target directory, `CARGO_BUILD_JOBS=1`, and one test thread. Python
3.13 executed the standard-library-only reference harness.

| Check | Observed result | Boundary |
| --- | --- | --- |
| Public consumer contracts | 13 passed | Includes lookup failures, atomic change, metadata scopes, every pinned item suffix and explicit unsupported modes |
| Rust independent-fixture harness | 1 passed, comparing all 81/81 exact cases and 6/6 typed unsupported classifications | No expectation generated from Rust |
| Compiled documentation consumer | 1 passed | No SDK/runtime/authentication dependency |
| Actual Python source oracle | 81/81 exact cases passed; 6 separately classified exclusions | Executes source AST/hierarchy/mem-store matching over offline metadata, not live API behavior |
| Oracle side-effect denial probes | 14/14 passed | Filesystem/environment/network/process/sleep/product-auth boundaries denied |
| Existing workspace architecture contract | 8 passed | Additive manifest/default member/CLI-layer boundaries retained |
| Standalone `resolve` example | Built and exited successfully | Pure public API consumer |
| Targeted strict all-target Clippy and formatting | Passed | No full-workspace or cross-platform claim |

The first Cargo invocation could not find `cargo` on the session's `PATH`;
using the existing user Rust toolchain directory resolved it without installation.
No dependency restoration or lockfile regeneration was needed. Existing Python
product sources and all pre-existing compatibility fixtures remained unchanged.
