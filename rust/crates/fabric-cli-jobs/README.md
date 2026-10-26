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

`JobHost::execute` consumes the parsed `--skill` as an explicit command-local
`JobRequestHeaders` value. It borrows scoped client/reconciler clones without
changing the existing host struct fields or the caller's client. All eight
leaves use it, including follow-up polling, pagination, timeout cancellation
and generic-operation/result requests. An explicitly empty `--skill` emits no
header. Concurrent or subsequent commands do not share mutable header state.
The header-aware HTTP path calls the existing checked `execute_bound` with the
fixed `PublicOrigin::Fabric`; it does not expand allowed origins or discover an
audience from a URL. Default headerless calls retain `execute_fabric`.

Hosts configuring `operations` with `--skill` use
`SharedJobOperations::from_job_executor`. A custom executor/reconciler that
cannot carry the selected header fails before any mutation rather than being
silently substituted. SDK header syntax/bounds are explicit safety validation,
not byte-for-byte Python Requests exception-message compatibility.

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

## Shared JSON/text consumer

`JobHost::run` is an additive complete-writer adapter over the existing handler.
The caller supplies `JobRenderOptions`: resolved configuration format, the
shared `TextOptions`, a source-equivalent character-width provider and explicit
`Presentation`. A command's `--output_format` takes precedence. It creates no
main/parser, terminal probe, logger, Unicode database or global state.
The older `execute`, `dispatch_json`, `run_json` APIs and host literals remain
available. Writers for the new adapter must implement `Write + Send`; synchronous
UI callbacks borrow them through a command-local mutex, never across an await.

Immediate grey/progress/warning/log-warning events and raw job-status tables
use the shared renderer even in JSON mode. Results preserve table/header and
key-value intent; deferred status hints are written only after the result.
Confirmation delegates to the injected `JobUi`, but its `emit` is not invoked
again, preventing a host UI from duplicating output. Declined removal and
Cancelled/Deduped/unknown terminal source branches produce no invented success
envelope.

`JobOutput::render` and `JobEvent::render` expose the same pure composition to
other hosts. Text formatting failures retain the shared typed error and any
partial source writes; `run` writes that partial output once before returning
the failure. Writer errors stop local work without a second fallback write,
success hint, remote rollback or request replay.

`JobCommandError::Service` now retains ordinary job HTTP evidence beside its
once-normalized diagnostic, enabling text-only Fabric request-ID decoration
without changing JSON output. Generic-operation JSON projection remains the
borrowed shared formatter. Generic-operation **text** still requires an
additive shared boundary accepting an already-normalized error: constructing
another `CommandError` would incorrectly default a period-only empty message.
Until that exact seam is released, `JobRenderError::OperationTextProjectionRequired`
returns the original operation error rather than a fabricated text diagnostic.
This explicitly unclosed branch is not claimed as full presentation parity.
