<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Local commands and configuration

`fabric-cli-local` provides `cd`, `pwd`, `open`, `clear` (`cls`), `desc`,
`extension`, `version`, and the `config set`, `get`, `ls` (`dir`) and
`clear-cache` handlers. Parsing, persistence and platform effects are separate
capabilities.

## Host integration

Mount `root_commands()` and convert matches with `from_matches()`.
`LocalInvocation` contains the typed command and output selection.
Group-only `config` renders help without executing a leaf handler.

Create `ConfigStore::new(explicit_root)` and call `execute` with caller-owned
`LocalEffects`. The store uses `config.json` beneath that root; it does not
discover a home directory or credentials. Capacity changes consume an explicitly
provided metadata snapshot. Cache clearing must invalidate CLI metadata only,
not broker or token storage.

For navigation, compose `NavigationHost`, `NavigationState`, `HierarchyLookup`
and the same configuration store. Supply the actual working directory, explicit
home, session ID and current node. `restore` reads only the selected
`context-<session>.json`; it never scans or deletes other session files. Validate
identity before restoring remote context.

## Platform effects

`SystemBrowserLauncher` passes separate arguments to the platform browser
launcher; it does not build a shell command string. Invoke it only for an
explicit browser request. `AnsiTerminal<W>` writes terminal control sequences
to the supplied writer. Missing required effects fail explicitly.

Output uses the shared renderer and caller-supplied timestamp, width policy,
presentation and streams. `LocalError::command_error()` projects failures into
the command error pipeline. Libraries never print to global streams or exit.

## Configuration behavior

Settings preserve insertion order and unrelated keys. Writes use four-space,
ASCII-escaped JSON with no trailing newline. Unix directory and file modes are
`0700` and `0600`. Existing `fab_`-prefixed keys are migrated during initialization.

Keys are lowercased. Values are trimmed and stripped of surrounding quote
characters; enumerated values remain case sensitive. Empty or unset
`config get` values emit no output. Local label paths must exist. Capacity
selection matches display names case insensitively and stores both name and ID.

`version` always uses its text presentation. Interactive mode is not implemented;
requests to enter it fail rather than persisting an unusable mode.

Run the configuration example with a disposable directory:

```text
cargo run --locked -p fabric-cli-local --example config_consumer -- <temporary-directory>
```
