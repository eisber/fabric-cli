<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job commands

`fabric-cli-jobs` provides parsing and orchestration for `job start`, `run`,
`run-status`, `run-list`, `run-cancel`, `run-sch`, `run-update` and `run-rm`.
It composes the jobs SDK, shared operation handling and CLI output without
creating a separate executable or runtime.

## Additive v2 Run surface

`v2_run::command()` exposes the same `run PATH` leaf at the root and under
`artifact notebook`, `artifact user-data-function`, and `artifact lakehouse`.
It does not replace the v1 grammar or mount a production host.
`v2_run::register` uses the shared operation registry: root/facade discovery
returns the same binding identity and the configured adapter's input schema.
Bindings without an executable adapter are explicitly `Unsupported`; the
merged Notebook request, UDF JSON and Livy statement validators alone do not
enable execution.

The outer host resolves one exact target, authorizes its native effects and
selects an approved `RunAdapter`, a bounded `JsonInput` reader and the
`RunOutput` sink corresponding to `--output`. Call `prepare` before credentials
or remote execution, then `dispatch` with the original injected `JobClient`,
clock and interruption future. The prepared value retains that exact adapter
and output sink. Remote name resolution must consume the outer host's original
budget; this entry point accepts already resolved targets and does not discover
names or acquire credentials itself.

The initial representation is JSON, with a newline-terminated protected
execution receipt. Defaults are 30 seconds for Notebook/Livy submission,
5 minutes with `--wait`, and 2 minutes for a synchronous UDF with either
spelling. Explicit positive integer durations use `ms`, `s`, `m`, or `h`, up
to 24 hours, and replace the default. The shared `RunSession` owns the total
budget, actual handle/effect evidence, observation and output commit.
`RunDispatchError` distinguishes startup, retained phase failures and observed
unsuccessful workloads; callers must preserve that evidence and report nonzero
failure rather than retry execution.

Timeout and interruption never send remote cancellation or delete sessions.
`invocationId` is UDF correlation, not an asynchronous handle. `--check-exit`
is unavailable until a separate Beta verdict protocol is approved; saved-cell
and one-shot execution are rejected without fallback. No arbitrary URL,
standalone `invoke` or `wait`, auth discovery, new runtime or second poller is
introduced. Production UDF endpoint/schema/audience and Livy LH-12 bindings
remain separate prerequisites. No live workspace execution is claimed.

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
Conversion from `JobError` uses the same mapping as the command host, including
nested page-fetch failures. Exported reads retain native authorization codes,
exit status, cancellation, and original operation evidence.

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
