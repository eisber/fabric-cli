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

In a `native-read` build, `fab-rs auth login` uses the saved selection or opens
WAM's work-account picker. Successful login saves only the account, tenant and
client ID; `fab-rs ls /` then reuses that selection with silent WAM credentials
and native HTTPS transport. Explicit argument/environment selectors take
precedence. Tokens stay in WAM. See the [selection and storage
contract](../../../docs/native-read-host.md#selection-and-execution).

Authentication supports `--output_format text` and `--output_format json`.
An explicit format overrides the saved `output_format`; without a saved value,
auth defaults to text. JSON retains its existing auth result/error shape for
scripts. Both formats keep success on stdout and failures on stderr, without
printing access tokens or changing timeout/cancellation behavior.

```powershell
fab-rs auth status --output_format text
fab-rs auth status --output_format json
```

Listing queries operate on an array of children. Use
`fab-rs ls 'Demo.Workspace' --long -q '[].id'` for the item IDs, or
`fab-rs get 'Demo.Workspace' -q id` for the workspace's own ID. A bare `id` applied
to a listing array returns the source-compatible scalar string `None`; it does
not select each item's ID. Short workspace-item listings expose names only;
`--long` makes their IDs available to the query.

Local commands and remote families share the same default user configuration
directory: `%LOCALAPPDATA%\Microsoft\FabricCLI` on Windows, the application
support directory on macOS, or the XDG configuration directory on Linux.
`FAB_RUST_CONFIG_ROOT` remains an explicit override, not a required setup step.
Help and version do not create configuration; settings initialize when needed.

For an all-feature development executable and its production updater helper:

```text
cargo build --offline --locked -p fabric-cli --all-features --bin fab-rs
cargo build --offline --locked -p fabric-cli-update --features bounded-helper --bin fab-rs-update-helper
```

This includes Windows broker, native transport, headless and Windows
certificate-store credentials, REPL and self-update code. It does not configure
release verification keys, establish installer custody or make an unsigned
development build eligible for updates. The helper is a separate executable;
test-fixture helper binaries are not part of this development pair.

Mounted command renderers use the embedded source-compatible Unicode width
provider for tables, including catalog search truncation, job status tables,
local configuration and transfer output. Renderer-generated ellipses and
non-ASCII service values use the same complete scalar coverage; ASCII-only
measurement is not a production default. JSON-only failure diagnostics remain
independent of width lookup. The immutable Unicode policy is documented by the
[output component](../fabric-cli-output/README.md#unicode-text).

| Feature | Host capability |
| --- | --- |
| Default | Arguments, local commands and unconfigured-host diagnostics |
| `windows-broker` | Explicit experimental Windows login, status and logout |
| `native-read` | Windows service command host, including reads, jobs, transfers and deployment |
| `headless` | Cross-platform service host with explicitly selected unattended credentials |
| `repl` | Optional Reedline shell for terminal-only, no-argument invocation |
| `self-update` | Optional signed-release checking and standalone update host |

See [native host configuration](../../../docs/native-read-host.md) and
[Windows authentication](../../../README.md#windows-authentication) for selectors,
platform limits and live-operation requirements.

## Headless service commands

Build with `--features headless` and explicitly set `FAB_RUST_AUTH_MODE`.
Supported modes are `client-secret`, `client-assertion`, `workload-identity`,
`managed-identity-vm` and `managed-identity-app-service`. Select a public-cloud
tenant and the documented provider inputs; the presence of a secret never
selects a credential. Service commands acquire tokens lazily, without interactive
login. Service-principal identity labels are client IDs, not user UPNs or
independently verified claims.

See the exact [environment bindings and host contracts](../../../docs/native-read-host.md#headless-credentials).
Credentials are not command arguments or saved settings. The
[identity API](../../../infrastructure/identity/fabric-identity/README.md)
documents renewal, redaction and unsupported providers. Offline tests do not
establish live platform support, service authorization or GA readiness.

## Optional release updates

The `self-update` feature adds outer-only update commands:

```text
fab-rs update --check
fab-rs update --apply
```

Release builds pin the `microsoft/fabric-cli` publisher and an independently
approved verification public key. Discovery uses the `rust-vSEMVER` namespace;
other product releases in the repository are not update candidates. Missing
publisher trust fails before networking. No GitHub credential, Fabric credential,
downloaded verification key or alternate repository is used.

Automatic checking or installation is opt-in through `FAB_RUST_UPDATE_POLICY`
(`off`, `check` or `install`) and an explicit absolute
`FAB_RUST_UPDATE_STATE_ROOT`. Checks are rate-limited across processes. Help,
version, authentication, batch, redirected and CI execution do not start an
automatic check. Explicit updates cannot run inside a batch or interactive shell.

Installation requires standalone build and installer receipts. Unknown and
package-manager-owned installations are not overwritten. Automatic installation
stays within the current major version; major upgrades require explicit opt-in.
After a successful automatic replacement, the process exits with status **75**
and states that the original command did not run and must be rerun. Pre-commit
update failures warn without suppressing the requested command; uncertain native
replacement stops for recovery. Commands are never replayed automatically.

See the [update component contract](../../update/fabric-cli-update/README.md)
for signed metadata, exact target/feature matching, bounded downloads, Windows
working-directory/helper restrictions, retained recovery files and acceptance
limits. Update-manifest signatures do not establish native package signing,
notarization, helper cleanup or post-install health. Production trust setup and
native update/recovery acceptance remain release gates.

## Optional interactive shell

Build with `--features repl` to start the interactive shell when no arguments are
supplied. Combine it with `native-read` to use the configured remote host:

```text
cargo run --locked -p fabric-cli --features repl
cargo run --locked -p fabric-cli --features native-read,repl
```

The terminal editor and shell loop live in the separate
[`fabric-cli-repl`](../../repl/fabric-cli-repl/README.md) package. They are not SDK
dependencies and are excluded from the application's default dependency graph.
Removing the optional feature and private entry module does not require changing
shared command, batch or SDK libraries.

The shell delegates commands to the existing parser and dispatcher, retains
navigation in memory and does not restore or write command-line context files.
History is bounded and memory-only. `help`, `version`, `quit` and `exit` are
interactive aliases. Input is tokenized with quoting and escapes, not evaluated
by an operating-system shell. No completer or persistent history is configured.
Both stdin and stdout must be terminals; redirected input fails before editor,
configuration or credential setup. Use command arguments or `-c` for automation.
Help and version arguments never enter the editor.
Unclosed quotes and trailing escapes end the session after a diagnostic with a
normal session exit status. Resource-limit and terminal-editor failures instead
return a nonzero status; output failures propagate.

Real-terminal editing, key bindings, signal handling and terminal-state
restoration require native Windows, macOS and Linux acceptance. Fake-input tests
and dependency-graph checks do not establish that platform support.

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

## Command batches

Repeat `-c` or `--command` to execute ordered command strings through the same
per-command parser and dispatcher:

```text
fab-rs -c "config set show_hidden true" -c "config get show_hidden"
fab-rs --output_format json -c version -c version
```

Batch input is bounded command tokenization, not a shell-language interpreter.
The execution contract is:

- Split on Unicode whitespace, including U+001C through U+001F. Quotes
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
- A normal subcommand following root batch options replaces the command list.
  Root version does not replace a batch;
  root help still exits before dispatch. Nested `-c` is rejected.

Invalid arguments use the sanitized error envelope. Unknown batch commands stop
with exit 1 and a sanitized `UnexpectedError`. Diagnostics and debug formatting
of batch invocations omit supplied command strings.

`parser::batch::run` takes a `BatchInvocation` and caller-owned `BatchHost`.
`execute` receives split arguments and returns `CommandOutcome::Returned(u8)` or
`Stop(u8)`. `separator` and `summary` are ordered output callbacks; any callback
error stops without replay. `parse_command` reuses the existing parser and
distinguishes explicit help exits from returning bare-group help. No library
runtime, process exit, prompt, environment access or mutable global is added.

The binary captures the initial context selection once at batch entry and borrows
one caller-owned `NavigationState` across native family commands. Remote `cd`,
relative reads/jobs and `pwd` use the updated in-memory path even when context
persistence is disabled. Settings still use the existing file store; enabling
persistence or changing settings later does not reload the initial context.
Context persistence remains opt-in: the batch host does not force context-file
writes or modify environment variables. A completed context change is not rolled
back by a subsequent output failure.

Authentication uses the same explicit broker selection and command engine as a
single invocation. Its typed outcome distinguishes cancellation from a handled
provider failure even though both retain exit 1. Cancellation, timeout and
signal-handler failure stop before the next command and suppress the summary.
No new credential backend, acquisition rule or token input is introduced.

Default local execution has no installed Ctrl+C handler. The existing Tokio
signal support is feature-gated with authentication; a portable default/local
batch signal host still needs a reviewed dependency/runtime contract. Synthetic
cancellation tests do not establish OS-signal handling. Portable signal
acceptance remains a release gate, independent of the shared native context hook.

Offline contract fixtures and their independent checks are maintained under
`tests/`. Recheck them without rewriting expected outputs:

```text
python hosts/cli/fabric-cli/tests/batch_reference.py --check hosts/cli/fabric-cli/tests/batch_source.json
cargo test --offline --locked -p fabric-cli --test batch_commands --test batch_parser --test argv_contract --test local_mounted
```

The same executable selectors and an all-target check are also required with
`--features native-read`. Contract captures do not establish actual native
cancellation, remote context reuse or platform acceptance.

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

## Native Dataflow query execution

The additive v2 `execute` root binds an explicit
`/<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/<query-name>` target
with required `--wait`. `--format json|csv|parquet` uses the shared native Arrow
tabular result component; Parquet requires a new file destination. Optional
`--input FILE|-` supplies strict JSON for `customMashupDocument` and
`arrowVersion` (1 or 2), not SQL text.

The host requires target/input-bound approval before credentials, performs
no-clobber output preflight, and preserves real operation evidence on local
cancellation, timeout or output failure without replay. Noninteractive native
processes without an approval capability fail closed; embedders inject
`execute::host::ExecutionApproval`. The default binary returns `NotConfigured`.
See the [complete execution contract](../../../docs/dataflow-execution.md) for
finite limits, exact format references, permission/gateway prerequisites,
unimplemented providers and the detailed refresh-log API gap. Existing v1
JMESPath metadata projection is unchanged. Excel output is deferred.

## Output and execution boundaries

Use returned plans and original evidence until output delivery completes.
Writer errors propagate without replay. HTTP acceptance, local cancellation and
completed remote operations remain distinct outcomes.

The native binary owns one asynchronous runtime, cancellation handler and
command-selected watchdog. Explicitly unbounded job waits retain per-request
limits and Ctrl+C handling without an arbitrary whole-command cutoff.

Windows credentials require explicit account, tenant and client selection.
Other platforms can use injected library credentials; native credential
execution there fails explicitly. Resource/access skill headers and several
resource-specific capabilities remain unavailable.
