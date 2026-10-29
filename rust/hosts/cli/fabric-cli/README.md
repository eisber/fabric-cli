<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI application and command host

`fabric-cli` provides the `fab-rs` executable, root argument parser, reusable
command sessions and native host composition. Command behavior lives in focused
libraries; this crate connects them to explicit runtime capabilities.

## Run the application

From `rust/`:

```text
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-cli -- --version
cargo run --locked -p fabric-cli --features native-read -- deploy --help
```

Help, version and local command setup do not authenticate. Default and
broker-only builds do not install a remote execution host.

| Feature | Host capability |
| --- | --- |
| Default | Arguments, local commands and unconfigured-host diagnostics |
| `windows-broker` | Explicit experimental Windows login, status and logout |
| `native-read` | Windows service command host, including reads, jobs, transfers and deployment |

See [native host configuration](../../../docs/native-read-host.md) and
[Windows authentication](../../../README.md#windows-authentication) for selectors,
platform limits and live-operation requirements.

## Embed command execution

`parser::parse_args` accepts argv with the executable name.
`ReadHost::dispatch` returns a render plan; `ReadHost::run` writes it to supplied
streams. Inject the SDK client, executors, immutable current/home context,
metadata, configuration, timestamp and query provider.

`with_default_projection` selects the built-in `PythonQuery` provider, backed by
the JMESPath library. `NoQuery` explicitly disables query and path discovery.
Empty LS queries use default columns; GET with no expression lists property
paths, while `"."` retains the metadata value.

`ReadSession` composes asynchronous discovery and command execution.
`FolderReadHost` adds folder-aware listing without changing the base host's
public inputs. `JobSession`, `TableSession` and `FamilySession` compose their
focused handlers over the same execution and context policies.

Libraries do not discover credentials, create a runtime, install transport,
read global state or terminate the process. Cancellation is a supplied future;
partial pages never become successful complete results.

## Source command batches

Repeat `-c` or `--command` to execute ordered command strings through the same
per-command parser and dispatcher:

```text
fab-rs -c "config set show_hidden true" -c "config get show_hidden"
fab-rs --output_format json -c version -c version
```

This is the bounded translation of `main.py` and `core/fab_parser_setup.py` at
Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, not a shell-language
implementation. Preserve these source oddities:

- Split on Python Unicode whitespace, including U+001C through U+001F. Quotes
  remain in tokens; semicolons and pipes are ordinary data, not separators.
- Skip empty entries. After a returned nonempty entry, write the 30-hyphen
  separator to stderr unless that entry has the last **raw list index**.
- Continue after handled command errors; return the last executed command's exit
  status. Stop on parser misuse, explicit help exits, uncaught failures or
  cancellation; do not print the final summary after stopping.
- After more than one executed entry, print two stdout newlines and a success
  summary using the **raw list length**, even when earlier commands failed.
  The JSON summary's `command` is the complete original string list.
- The outer output override applies only to the summary. Each inner command
  parses its own override or reads configuration at its execution point. The
  summary reads the configuration after all commands return.
- A normal subcommand following root batch options replaces the command list,
  matching Python's shared destination. Root version does not replace a batch;
  root help still exits before dispatch. Nested `-c` is rejected.

The existing sanitized invalid-argument envelope remains an intentional
compatibility exception. Unknown batch command keys stop with exit 1 and a
sanitized `UnexpectedError`, never the source's echoed dictionary key. Debug
formatting of batch invocations also omits command strings.

`parser::batch::run` takes a `BatchInvocation` and caller-owned `BatchHost`.
`execute` receives split arguments and returns `CommandOutcome::Returned(u8)` or
`Stop(u8)`. `separator` and `summary` are ordered output callbacks; any callback
error stops without replay. `parse_command` reuses the existing parser and
distinguishes explicit help exits from returning bare-group help. No library
runtime, process exit, prompt, environment access or mutable global is added.

The binary retains one lazy local `NavigationState` throughout a batch; settings
use the existing file store. Remote context-changing `cd` in native batches is
explicitly unsupported until the native dispatcher exposes a shared context
hook. Batch authentication is explicitly unsupported because the existing auth
dispatcher erases cancellation into the same exit status as handled failure.
Separate explicit authentication remains unchanged. Default local execution has
no installed Ctrl+C handler; real OS-signal parity remains an integration gate,
not evidence supplied by synthetic cancellation tests. These limits prevent a
claim of complete source mode parity.

New source-derived evidence is in `tests/batch_source.json`, captured by
`tests/batch_reference.py` from immutable Git objects with synthetic handlers and
state. The existing references and fixtures are unchanged. Recheck it without
rewriting expected outputs:

```text
python hosts/cli/fabric-cli/tests/batch_reference.py --check hosts/cli/fabric-cli/tests/batch_source.json
cargo test --offline --locked -p fabric-cli --test batch_commands --test batch_parser --test argv_contract --test local_mounted
```

The same executable selectors and an all-target check are also required with
`--features native-read`. Source-only captures do not establish Rust compilation,
actual native cancellation, remote context reuse or platform parity.

## Interactive-shell decision boundary

The pinned `core/fab_interactive.py` requires a styled, escaped current-context
prompt, blinking cursor, in-memory editing/history with history search, and
POSIX `shlex.split` input. It contains exact raw quit/help/version aliases,
`fab` prefix guidance, per-command `SystemExit` containment and Ctrl+C
hint-and-continue behavior. EOF is caught by the inner generic exception
handler before the outer goodbye branch; padded aliases and malformed quotes
also have source-specific behavior that must not be silently improved.

No completer is supplied to `PromptSession`: command/path completion is not an
implemented REPL requirement to invent. `argcomplete` belongs to command-line
completion, and config completers are separate parser callbacks. The source
depends on `prompt_toolkit>=3.0.41` and `argcomplete>=3.6.2`, without an upper
version pin. Rust currently has no selected terminal editor dependency.

| Option | Decision and impact |
| --- | --- |
| Keep the current no-argument contract until backend review | Recommended. No invented REPL, dependency, lock change or platform support claim. Batch delivery remains independent. |
| Stdlib line-oriented shell | Can implement input and dispatch but cannot provide source-faithful portable editing/history-search/cursor/signal behavior. Requires explicit approval as a reduced contract, not REPL parity. |
| Reviewed terminal editor/backend | Can target the complete source contract, but requires dependency/security/license review, Windows/macOS/Linux terminal and redirected-input tests, and an explicit cancellation/runtime design. No backend is selected here. |

A complete source-faithful minimal REPL is therefore not feasible with only
stdlib and existing APIs. Any future backend-neutral shell contract should own
one mutable command session, inject input/prompt/history/output/cancellation,
and keep source `shlex` rules distinct from batch whitespace rules. Parent review
must establish this scope before central host redesign.

## Deployment and transfers

`DeployHost` loads configuration, templates and parameters before preparing an
owned repository snapshot. Its generic workflow handles selected item types,
scoped parameter resolution, publication, bulk operations and orphan removal.
Results retain nonfatal warnings and partial or uncertain service effects.

`FamilySession::dispatch_with_deploy_inputs` accepts explicit parameter
environment values. Default `dispatch` supplies an empty map. The native binary
alone captures process values, validates Unicode and keeps diagnostics redacted.
Those values are not credential inputs.

`TokioTransferFiles` and `TokioDeployFiles` run bounded local file phases on the
caller's blocking executor. Deferred path selection preserves classification
order and limits local authority to explicit arguments or trusted local
configuration documents.

## Output and execution boundaries

Use returned plans and original evidence until output delivery completes.
Writer errors propagate without replay. HTTP acceptance, local cancellation and
completed remote operations remain distinct outcomes.

The native binary owns one asynchronous runtime, cancellation handler and
command-selected watchdog. Explicitly unbounded job waits retain per-request
limits and Ctrl+C handling without an arbitrary whole-command cutoff.

Windows credentials require explicit account, tenant and client selection.
Other platforms can use injected library credentials; native credential
execution there fails explicitly. Resource/access skill headers, interactive
shell mode and several resource-specific capabilities remain unavailable.
