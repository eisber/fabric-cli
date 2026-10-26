<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Local command family

This runtime-neutral CLI-layer crate translates local behavior from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, without a shipped Python bridge.
The crate implements **eleven library handlers**: `cd`, `pwd`, `open`, `clear`
(`cls`), `desc`, `extension`, named `version`, `config set`, `config get`,
`config ls` (`config dir`), and `config clear-cache`.
They are not counted as production executable commands until the central owner
mounts them and proves actual binary invocations.

## Host contract

Mount `root_commands()` into the existing front door. Pass the root matches to
`from_matches()` to obtain `Option<LocalInvocation>`; the invocation contains a
typed `LocalCommand`, optional `output_format`, and source `skill` argument.
Use `LocalCommand::command_path()` for the shared output command field.
Parsing performs no I/O. Group-only `config` selects `ConfigHelp`, which renders
source help successfully through the existing output library (not a twelfth leaf).

Create `ConfigStore::new(explicit_root)` and call
`execute(&invocation.command, &store, &mut effects)`. The store uses only
`explicit_root/config.json`. It does not discover HOME or credentials.
`LocalEffects::clear_cli_cache` must invalidate the host's seven CLI metadata
cache families, not broker/credential/token storage. `capacities()` supplies
already-fetched non-secret capacity metadata from the existing SDK; an async
host fetches that snapshot before executing a default-capacity change.
No new HTTP, auth, runtime, registry, plugin loader, or executable is added.

For navigation, wrap the platform/cache effects in `NavigationHost` with mutable
`NavigationState`, the existing `HierarchyLookup`, and the same store. State
construction takes the existing current node, explicit `HomeContext`, actual
local working directory, caller-selected numeric shell ID, and runtime-mode
flag. It never guesses a shell from process ancestry. Context lookup uses
`PathResolver`; capabilities use existing `CommandPolicy`, not forked algorithms.
The host must authenticate/validate its identity before restoring persisted
context; this crate does not discover identity or migrate credentials.
`NavigationState::restore` loads only `context-<session>.json`. A true return
means the central host should emit the source `Command context path` warning.
No stale-session file sweep, process probe or peer-session cleanup occurs.

`LocalEffects::browser_identity` supplies only a user-identity flag and explicit
non-secret `upn`/`tid` metadata. `open_browser` and `clear_terminal` are required
real effects for those commands; missing adapters fail `NotConfigured`.
`SystemBrowserLauncher` provides one explicit native path (`xdg-open`, macOS
`open`, Windows `rundll32.exe`) with separate argv entries, never a shell command
string. Constructing it performs no action. The caller must only invoke it for
an explicit user browser request. `AnsiTerminal<W>` writes clear/home/scrollback
control bytes to the injected writer. Tests capture both adapters offline.

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

`cd` changes actual injected context and, only in command-line mode with
`context_persistence_enabled=true`, writes source `{"path": "/..."}` bytes into
its session file. Write failures are explicit and preserve previous in-memory
state, unlike the source warning-and-continue path. Home and relative traversal
come from the existing resolver. `pwd` deliberately prints the source's
four-field pretty JSON to **stderr**, even for JSON output selection, rather
than inventing an envelope or silently returning an OS path.
`open` preserves workspace/item routes, experience selection, source unaugmented
URL output, and ordered browser `upn`/renamed `ctid` parameters. On identity or
launch failure it returns a structured error before success output; Python had
already printed the URL. No token acquisition or browser action occurs in tests.
Claim query values remain source-unescaped; they are not shell-evaluated.

`desc` reports the real source support matrix, not a blanket claim that all
listed commands are implemented by Rust. `src/support.json` is a development-time
extraction of the pinned YAML, enum spellings and browser URI mapping.
`tests/build_support_reference.py` is an explicit source-data extraction tool,
not shipped runtime machinery. `navigation_source_contract.py` verifies this
snapshot independently of native output.
Dot suffixes and actual resolved contexts are supported, including virtual
resources and folders. Unknown lookup errors remain structured failures rather
than Python's broad exception suppression.
The source help advertises `desc all`, but no such special expansion exists:
this preview preserves its warning/usage/exit-2 path. `extension` is exactly the
successful unsupported-feature warning, without a plugin framework.

## Validation and remaining scope

`tests/source_contract.py` loads the exact Git-pinned Python config handlers
with explicit isolated file/UI/cache boundaries. It verifies all five source
leaves against frozen expectations before native execution, without installed
Fabric imports or ambient config. `tests/fixtures/defaults.json` is the source
default file, not regenerated from Rust. Rust public consumers cover real
set/get persistence, listing/defaults/order, cache mutation/sentinel preservation,
named version, aliases/conversion, errors, migration, Unix permissions, and
shared writer failures.
`navigation_source_contract.py` executes the six additional Python handlers with
synthetic navigation state, isolated context persistence, fake identity metadata,
browser capture and terminal capture. Native consumers cover all six through
family registration/conversion and actual `NavigationHost` execution, plus
relative/home traversal, restore, output bytes/streams, platform-effect failures,
identity rejection and source support-model enumeration.

```text
python crates/fabric-cli-local/tests/source_contract.py
python crates/fabric-cli-local/tests/navigation_source_contract.py
cargo check --locked -p fabric-cli-local --all-targets
cargo clippy --locked -p fabric-cli-local --all-targets -- -D warnings
cargo test --locked -p fabric-cli-local
```

Run these through the coordinator's authorized bounded Linux validation lane.
Commands above are selectors, not a claim that validation has already passed.
Windows ACL enforcement, real Windows/macOS launcher behavior, newline/native
path execution, Windows `dir/del/move/copy/mklink` spellings inside source `desc`
reports, and non-ANSI Windows terminal clearing remain platform debt; no
three-OS or live proof is implied. REPL/batch lifecycle, ambient identity/home
discovery and shell-process discovery remain central-host gaps, not stubs or
claimed parity.
