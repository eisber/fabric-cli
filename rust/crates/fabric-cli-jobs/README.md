<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric job command composition

This CLI-layer library owns all eight Python `job` leaves. It is not another
binary or parser owner. Compose `job_subcommand()` into the existing front
door, pass the `job` group matches to `command_from_matches`, and execute the
result with `JobHost`.

The host supplies the real `JobClient`, immutable context/catalog snapshot,
explicit home, JSON file reader, UI/confirmation boundary, configuration and
timestamp. `HttpJobExecutor` is only a thin adapter over the existing
`HttpExecutor::execute_fabric` and `wait_for`; it installs no HTTP, retry,
paging, credential or runtime implementation.

`JobHost::execute` returns `JobOutput` with the existing `OutputInput`,
header/key-value presentation flags and deferred events. Text front doors
use their shared renderer, then emit `after_output` events in order.
`run_json` uses the existing JSON envelope and caller-owned writers, emitting
deferred status hints only after the result is written. Writer failures
propagate. `dispatch_json` materializes the JSON plan only; it does not emit
deferred hints. Both JSON helpers reject explicit text selection before
service I/O; they do not silently substitute JSON.

`JobUi` receives source grey/warning/progress/log/table events and supplies
asynchronous confirmation. The library never opens a terminal, prompts,
discovers credentials, starts a runtime, or reads global configuration.
`JsonInput` lets a host provide file contents through its existing input
policy; `NoFiles` explicitly rejects file requests. Neither silently falls
back to ambient filesystem access.

`HttpJobExecutor` also implements the shared `OperationExecutor` trait. The
same caller-owned executor can therefore compose `SharedJobOperations` without
a new transport or polling loop. Cancellation is passed through SDK operations,
including the generic-operation bridge, rather than an outer race discarding
the operation session's reconciliation evidence. `JobHost::execute` returns `JobCommandError`: ordinary command errors retain the
existing `CommandError`, while the operation variant retains the original
`OperationError`. Its borrowed `envelope` method and the JSON convenience runners
reuse `fabric_cli_mutations::operation_error_output` directly. They do not create
a second `CommandError` from already-normalized output, which would replace a
source period-only message with an incorrect default after it became empty.
The same-layer formatter dependency is CLI-only; the SDK remains independent of
CLI implementations.

The registration preserves `path`, `params`, `config` and `input` token
cardinality, required IDs, signed timeout, positive polling interval, schedule
choices, last repeated option, and source option abbreviation. Root batch/REPL,
global `-help` normalization, configuration defaults and actual `fab-rs`
dispatch remain with the single front-door owner.

Run the offline example from `rust/`:

```text
cargo run --offline --locked -p fabric-cli-jobs --example offline_job
```

It uses synthetic credentials and HTTP only. It never contacts a service.
See the [compatibility matrix](../../docs/job-compatibility.md).
