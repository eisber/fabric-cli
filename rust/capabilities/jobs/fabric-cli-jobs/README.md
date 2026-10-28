<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job commands

`fabric-cli-jobs` provides parsing and orchestration for `job start`, `run`,
`run-status`, `run-list`, `run-cancel`, `run-sch`, `run-update` and `run-rm`.
It composes the jobs SDK, shared operation handling and CLI output without
creating a separate executable or runtime.

## Compose the host

Mount `job_subcommand()` in the root parser and pass the group matches to
`command_from_matches`. Construct `JobHost` with a `JobClient`, immutable
metadata/context, explicit home, JSON input reader, UI, configuration and
timestamp.

`HttpJobExecutor` delegates to the existing shared HTTP executor and asynchronous
timer. Generic operation reconciliation uses `SharedJobOperations`; no additional
transport, retry engine or polling loop is installed.

`JobUi` receives warnings, progress, logs and table events and supplies
asynchronous confirmation. `JsonInput` supplies selected file contents; `NoFiles`
rejects file input explicitly. Libraries never prompt, open a terminal, discover
credentials or read global configuration.

## Results and output

`JobHost::execute` returns `JobOutput`, including presentation flags and deferred
events. Render output before emitting `after_output` events in order.
`run_json` writes the JSON envelope and then deferred hints. `dispatch_json`
returns a plan without emitting those hints. Both reject explicit text selection
rather than silently changing the requested format.

`JobCommandError` retains command errors or the original `OperationError`,
including accepted operation IDs and uncertain effects. Writer failures
propagate and never replay a mutation.

## Input and execution policy

Inline JSON preserves integer precision within the supported i64/u64 domain.
Out-of-range integers, nonfinite values and malformed input fail before service
work. Injected file readers must apply equivalent validation.

The parser preserves variadic token cardinality, required IDs, signed timeouts,
positive polling intervals, schedule choices and repeated-option precedence.
The host controls configuration defaults and the outer cancellation boundary.

`--skill` is command-local and propagates through all eight leaves, pages,
status polls, timeout cancellation and operation/result requests. An empty value
emits no header. Adapters unable to carry a requested header fail before mutation.
Header state is never shared across commands.

Run the synthetic offline consumer from `rust/`:

```text
cargo run --offline --locked -p fabric-cli-jobs --example offline_job
```
