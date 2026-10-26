<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource front-door adapter

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`parsers/fab_fs_parser.py`, `parsers/fab_global_params.py`,
`commands/fs/fab_fs.py`, the existing workspace/item/folder handlers, and
`commands/fs/fab_fs_ln.py`. Shared implementation base:
`91e7523ea978c67ca183abaea04500e323c27eb3`.

Mount `resource_subcommands()` directly on the existing root Clap command.
Pass the root matches to `command_from_matches`, then execute the resulting
`ResourceCommand` through `ResourceHost`. This crate does not edit or install
the central `Invocation`, executable, runtime, session, or writer.

The host supplies `ResourceContext` over the existing command-local metadata
snapshot and resolver, `MutationHost` with the **same** `ResourceOperations`
session, UI and enriched metadata provider, and `ResourceConfig`. The context
boundary must preserve lookup errors and source diagnostics. `exists`, `mkdir`
and `ln` resolve with `raise_error=false`; `rm` and `set` require an existing
resource. No new cache, HTTP engine, credential provider, retry or poller exists.

The returned `MutationResult` retains `OutputInput`, creation table-header
selection, ordered cache effects, declined prompts, accepted/completed outcomes
and dependency evidence. The central host applies effects and context changes,
uses `ResourceCommand::args().output_format` with its existing output-format
resolver, and passes the inputs to its existing JSON/text renderer. It must not
drop earlier creation evidence when rendering an error. JSON command errors
use `MutationError::output()` and the existing envelope's stream/exit rules.
The host owns notice rendering and propagates writer failures; this adapter
never opens a terminal or swallows an output error.

## First microbatch

| Canonical leaf | Working branch | Explicit remaining adapter branches |
| --- | --- | --- |
| `exists` | Resolved workspace, item and folder; truthy ID, no redundant GET | Virtual and OneLake targets |
| `mkdir` (`create`) | Workspace capacity selection/default/none; item and folder creation through released handlers | Virtual and OneLake creation |
| `rm` (`del`) | Item/folder confirmation, force, item hard delete, accepted/cache evidence | Workspace/tenant catalog and selection composition; virtual and OneLake removal |
| `set` | Workspace/item/folder metadata, source query/input cardinality and read/confirmation ordering | Definition codec composition, virtual and shortcut SET |
| `ln` (`mklink`) | Lakehouse shortcut from inline JSON, all registered target types and force conflict policy | `--target` needs lazy resolution after source prompt/validation |
| `start`, `stop` | MirroredDatabase status/action requests through the existing lifecycle host; force retains state validation | Capacity ARM lifecycle and other item types |
| `assign`, `unassign` | Selected Capacity and target workspace; source unassignment metadata/linkage check before confirmation | Domain assignment and non-workspace destinations |

`--skill` is parsed but returns an explicit `NotSupported` error until shared
resource request-header plumbing is available. It is never ignored. `--type`
belongs to `ln`; workspace capacity is `mkdir -P capacityName=...`, not an
invented `--capacity` flag. Only the original command's flags are registered.
An unsupported branch is not a successful no-op or a generic unimplemented verb.

Mkdir parameters preserve the default `run=true`, explicit empty `-P` help,
comma-before-key boundaries, quote/backslash handling, string scalar values,
two-level dotted keys and the source's array-of-objects JSON exception. This
preview rejects malformed/Unicode keys rather than emulating Python regex
substring recovery. This stricter lexical boundary is not full argparse/regex
parity. No protected oracle, source pin, or source expectations are rewritten.

The first immutable microbatch contains five leaves. A separate additive
increment registers the four lifecycle/assignment leaves above. `cp` and `mv`
remain unregistered while their released folder-move and OneLake helpers are
composed into a subsequent bounded adapter.

## Offline evidence

`examples/offline_resource_frontdoor.rs` is an actual external-crate consumer:
argv -> root registration -> typed conversion -> context resolver -> handler ->
SDK mutations -> shared operation policy -> shared HTTP with injected credentials,
responses, time, metadata and UI. Its tests cover all five canonical leaves,
request bodies, accepted deletion/cache effects, confirmation/read ordering,
explicit gaps, parser defaults/aliases/cardinality, and existing JSON stream/exits.

`tests/frontdoor_reference.py` executes the pinned original registrations and
source handlers with independent synthetic inputs and compares actual Rust
requests and outputs. Python is development-only; no ambient settings, token
cache, certificate, live network, or Fabric resource is accessed.

Run on the coordinator's authorized Linux worker:

```text
cargo test -p fabric-cli-mutations --example offline_resource_frontdoor --locked
cargo build -p fabric-cli-mutations --example offline_resource_frontdoor --locked
python crates/fabric-cli-mutations/tests/frontdoor_reference.py --probe target/debug/examples/offline_resource_frontdoor
cargo clippy -p fabric-cli-mutations --all-targets --locked -- -D warnings
```

Linux execution is the microbatch integration gate. Native Windows/macOS
execution and complete front-door/branch parity remain explicitly unverified.
