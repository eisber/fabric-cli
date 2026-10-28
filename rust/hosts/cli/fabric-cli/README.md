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
