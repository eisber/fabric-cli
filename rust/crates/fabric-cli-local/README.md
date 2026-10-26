<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Local command family

This runtime-neutral CLI-layer crate translates local behavior from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, without a shipped Python bridge.
The first slice implements **five library handlers**: `config set`, `config get`,
`config ls` (`config dir`), `config clear-cache`, and named `version`.
They are not counted as production executable commands until the central owner
mounts them and proves actual binary invocations.

## Host contract

Mount `root_commands()` into the existing front door. Pass the root matches to
`from_matches()` to obtain `Option<LocalInvocation>`; the invocation contains a
typed `LocalCommand`, optional `output_format`, and source `skill` argument.
Use `LocalCommand::command_path()` for the shared output command field.
Parsing performs no I/O. Group-only `config` produces Clap help.

Create `ConfigStore::new(explicit_root)` and call
`execute(&invocation.command, &store, &mut effects)`. The store uses only
`explicit_root/config.json`. It does not discover HOME or credentials.
`LocalEffects::clear_cli_cache` must invalidate the host's seven CLI metadata
cache families, not broker/credential/token storage. `capacities()` supplies
already-fetched non-secret capacity metadata from the existing SDK; an async
host fetches that snapshot before executing a default-capacity change.
No new HTTP, auth, runtime, registry, plugin loader, or executable is added.

Render `LocalOutput` with the existing output-format selection, explicit
timestamp, character-width provider, and presentation. Write its `TextPlan`
through the caller's existing writers. Failures use
`LocalError::command_error()` in the existing error pipeline. Writer failures
propagate; libraries never exit or print to process-global streams.
The `config_consumer` example takes an explicit disposable directory and
persists/reads a real setting:

```text
cargo run --locked -p fabric-cli-local --example config_consumer -- <temporary-directory>
```

## Source behavior

The eighteen configuration keys, twelve string defaults, source migration from
`fab_<key>`, removal of deprecated persisted `mode`, and source listing order
are preserved. Writes use four-space ASCII JSON, insertion order, and no final
newline. Unix directory/file modes are `0700`/`0600`. Existing unrelated
settings are retained. Empty/unset `config get` emits nothing.
Keys are lowercased; values are whitespace-trimmed, then edge-single-quote and
edge-double-quote stripped. Enumerated values remain case sensitive.
Local label paths must exist. Default capacity selection matches display names
case insensitively and writes both the requested name and source internal ID.

`version` follows Python's two text lines, even for JSON selection. The native
preview reports **0.1.0**, not Python source product **1.8.0**; the constants
`PREVIEW_VERSION` and `SOURCE_PRODUCT_VERSION` make that difference explicit.
`config get mode` warns and returns `command_line`; noninteractive mode sets
warn without persisting. Entering `interactive` fails explicitly because this
slice does not implement a REPL. Batch/REPL lifecycle is not implemented here.

Intentional safety difference: malformed/non-object config fails with
`InvalidJson`, rather than Python's silent malformed-JSON reset.
Progress is returned with successful results; a failed write/capacity lookup
returns a structured error, not Python's already-printed progress prefix.
Concurrent multi-process read/modify/write is not serialized, matching the
source's lack of a transaction. No credential migration is performed.

## Validation and remaining scope

`tests/source_contract.py` loads the exact Git-pinned Python config handlers
with explicit isolated file/UI/cache boundaries. It verifies all five source
leaves against frozen expectations before native execution, without installed
Fabric imports or ambient config. `tests/fixtures/defaults.json` is the source
default file, not regenerated from Rust. Rust public consumers cover real
set/get persistence, listing/defaults/order, cache mutation/sentinel preservation,
named version, aliases/conversion, errors, migration, Unix permissions, and
shared writer failures.

```text
python crates/fabric-cli-local/tests/source_contract.py
cargo check --locked -p fabric-cli-local --all-targets
cargo clippy --locked -p fabric-cli-local --all-targets -- -D warnings
cargo test --locked -p fabric-cli-local
```

Run these through the coordinator's authorized bounded Linux validation lane.
Commands above are selectors, not a claim that validation has already passed.
Windows ACL enforcement and Windows/macOS newline/native path execution remain
platform debt; no three-OS or live proof is implied.
The remaining six assigned leaves are `cd`, `pwd`, `open`, `clear`, `desc`,
and `extension`. This checkpoint does not register them as parser-only stubs.
