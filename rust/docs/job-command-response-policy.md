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
