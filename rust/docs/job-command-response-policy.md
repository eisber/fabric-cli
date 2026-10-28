<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Source job command response policy

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
all eight `commands/jobs` leaves, `fab_jobs_parser.py`,
`fab_cmd_job_utils.py`, `fab_api_jobs.py`, the HTTP helper and the actual
hierarchy support configuration. The unchanged 155/396 source contracts remain
authoritative. Their hierarchy test measures 44 pinned item types: precisely
Notebook, SparkJobDefinition, DataPipeline, Lakehouse and GraphModel support
jobs. The other 39 types intentionally retain `UnsupportedCommand` on all eight
leaves. No additional job type or endpoint is invented.

## Command consumers versus typed SDK consumers

`JobClient::response_for_command` is an explicit opt-in route-limited raw response
boundary. Existing `status`, schedule, submission and list SDK contracts remain
strict. The CLI alone selects the source command policy:

| Consumer | Source response policy |
| --- | --- |
| `start`, `run` | Only an actual 202 yields an accepted instance. Other source success statuses emit no result. Synchronous generic operations use the existing reconciler before this decision. |
| `run-cancel` | Only an actual 202 yields the asynchronous message or starts job-status waiting. A reconciled 200 does neither. |
| `run-status` | HTTP 200 displays any supported JSON value; no invented `status` or schedule `id` requirement. Other source success statuses emit no result. |
| `run-list` | False `value` shapes yield the source empty message. Arrays use first-row columns; an empty first object projects every later value to `{}`. Missing values and source-invalid rows fail, not fabricated empty success. |
| `run-sch`, `run-update` | Interpret JSON only for source 201/200 respectively. An ID can be any supported JSON value, formatted with the existing Python string renderer. Update still reads before patching and retains non-boolean enabled values. |
| `run-rm` | Source HTTP 200 reports removal without interpreting its body. Other success statuses emit no result; 202 never proves removal. |
| Status polling | Present non-string status takes the source unknown-status/table branch. Missing status fails. Non-200 success responses continue polling without an invented status or extra sleep. |

All pages still traverse the single shared token pager. Byte, page and row bounds,
continuation origin/route validation, credential scopes, skill propagation,
one-attempt mutations, read retry policy and incomplete-list errors are
unchanged. A failed/noncompleted later page retains committed partial progress
and cannot become complete success. The source's unsafe missing/cross-route run
Location behavior remains an explicit safety difference.

The CLI validates consumed JSON before SDK decoding. Valid signed/unsigned
64-bit integers, including `u64::MAX`, remain exact. Oversized Python integers,
NaN/Infinity and lone surrogates remain
explicitly outside the approved JSON/native numeric domain; there is no f64
fallback for integer parameters and no silent non-finite-to-null conversion.
Signed timeouts outside i64/native duration representability remain rejected.
Source conversion error wording is not claimed byte-identical.

Feasible lexical compatibility is implemented separately from value bounds:
parameter integers/floats, signed timeouts, positive custom polling intervals,
Retry-After integers, cron intervals and schedule digit-shape validation accept
the actual oracle's 68 Unicode decimal blocks. Integer underscores and ASCII
signs follow source grammar rather than JSON-number grammar. Finite `u64::MAX`
and `i64::MIN` spellings never pass through f64. Cron still forbids signs and
underscores; daily time prefixes and impossible timestamp shapes retain the
source's permissive validation. Newer Unicode database additions remain a
version-drift gate, not certified support.

## Native host seams reserved to integration

The lane does not edit `native_read.rs`. The integration owner must mount:

1. `settings::command_watchdog(job.as_ref(), settings.jobs)` instead of
   `command_deadline` at the native host's command-budget selection. The result
   is `Option<Duration>`: check `Instant::checked_add` only for `Some`; race
   `Some` against `tokio::time::sleep` and `None` against `pending::<()>`.
   Keep the existing Ctrl+C and bounded runtime shutdown branches.
2. An actual terminal confirmation in `NativeJobUi::confirm`. The jobs lane
   offers `fabric_cli::jobs::terminal::confirm_terminal().await`; integration
   may instead reuse its already-provided canonical CLI console confirmation.
   Never mount an unconditional yes or `NotSupported` placeholder.

No timeout and `run-cancel --wait` have no arbitrary whole-command deadline.
Negative runtime reaches the source negative-sleep error rather than an implicit
240-second watchdog. Explicit finite runtime preserves the existing source
elapsed/poll-overshoot/optional-cancellation budget. Every other command retains
its existing 240-second or separately bounded policy. Individual HTTP exchanges
and generic operations remain bounded; this does not launch background jobs or
permit mutation retries.

Terminal confirmation is default-no, flushes before reading, reports typed
writer/read errors before DELETE, and runs its blocking read off the async
runtime. Cancellation can leave a local terminal read finishing later; it cannot
continue a dropped mutation handler. Force bypasses all prompting.

## Offline evidence

`jobs/command_source.py` captured 210 additive observations from unchanged source
before the native expected-output test was added. `command_source.json` records
the pin, inputs, streams, requests, bodies, scopes, effects and confirmation
counts across all eight leaves and five resources. Its default run compares a
fresh actual-source capture against that immutable file. The four historical
read-gap fixture inputs are unchanged; their runner now requires exact equality
with their existing source output instead of requiring a Rust failure.

`job_command_parity` drives the actual root parser, session and shared
`HttpExecutor` against the source capture, plus all-leaf authorization,
cancellation, missing-field and unusual status cases. `command_policy` separately
protects strict SDK readers and partial-list behavior. `job_watchdog` and
`job_terminal` exercise the two native seams without real time or a terminal.
`numeric_source.py` adds eleven actual-source finite numeric observations and
the complete 68-block Unicode 15.1 decimal inventory. `job_numeric_parity`
exercises those root argv/session/HTTP observations; SDK numeric tests compare
every Unicode scalar with the captured inventory, while explicit over-bound and
non-finite regressions retain the approved rejection policy.
Existing selected settings, skill, generic-operation, execution, mutation and
read tests remain required and are not newly claimed missing work.

Local evidence is source-only and rustfmt/diff inspection until integration
grants a named serialized Cargo slot or returns actual CI diagnostics. No Cargo
check/test/build success, native mount, service execution or live workload
acceptance is inferred from metadata or formatting.

The local source runs passed the preserved 155/396 suites, 58 skill-header and
25 operation-output contracts, 68 read observations, 38 execution and 64 mutation
observations, and the 210/10/11 additive captures. These counts overlap in scope
and are not a command-completion or coverage sum.

Minimal serialized Cargo commands for integration, from `rust/` (not run by
this lane without a named slot):

```text
cargo test --locked -p fabric-sdk-jobs -p fabric-cli-jobs
cargo test --locked -p fabric-cli --test job_command_parity --test job_numeric_parity --test job_watchdog --test job_terminal
```

The existing read, execution, mutation, selected-settings, shared-operation and
skill consumers, strict Clippy, and native-read feature compilation must also
remain green in root CI. The lane will repair actual returned diagnostics;
source capture, metadata and formatting do not replace that author-fix loop.

The endpoint follow-up adds `JobClient::with_context(context, executor, limits)`
and `context()`, retaining the original public-default constructor. Typed and
raw routes, token-pager bases and bound run handles use the selected endpoint.
Authentication uses the existing core request primitive with fixed Fabric scope,
not another credential path. `RunHandle::from_bound_command_response` is the
explicit raw-response counterpart; its old constructor remains public Fabric.
`SharedJobOperations::with_context(context, executor, limits)` and
`from_job_executor_with_context(context, executor, limits)` now carry that
context through the existing `OperationSession::with_context`. Original bridge
constructors retain their public defaults. Header-scoped clones retain the same
credential Arc and binding. `JobSession` derives both clients directly from
`reads.sdk.context()`, so metadata, submission, cancellation, schedules, pages,
status polls and generic-operation/result requests share one authority.

`HttpJobExecutor` accepts the already-configured shared `HttpExecutor`; its
`endpoint()` accessor exposes that binding. Fabric remains the explicit service
identity, not an inferred token scope. A non-Fabric executor binding fails even
on the header-aware path instead of using the shared public-origin fallback.
No HTTP retry/timeout, auth provider, transport feature or dependency changes.

`endpoint_reference.py` passed 540 unchanged-source job API/HTTP endpoint, scope
and skill observations across public/daily/MSIT, all five source resources and
missing/empty/selected skill values. The native `job_endpoint_context` consumer
adds all eight leaves (including actual pages/status waits), generic operation
poll/result chains for every synchronous leaf, and foreign-host rejection.
Focused SDK tests preserve constructor/credential identity, strict audience and
service validation before credentials, and cross-endpoint handle rejection.
These new Rust consumers remain CI-gated until the root executes them.

For the endpoint increment the smallest additional root command is:

```text
cargo test --locked -p fabric-cli --test job_endpoint_context
```
