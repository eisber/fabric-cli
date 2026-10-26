<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Job command compatibility

## Source and ownership

Python authority: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
This slice reads every function in `commands/jobs`, `client/fab_api_jobs.py`,
`utils/fab_cmd_job_utils.py`, the polling helper, parser registration and the
[full argument inventory](../tests/compatibility/commands/inventory.json).
The independent reference executes unchanged source functions after comparing
their ASTs with the exact Git source pin. It does not import product
initialization, discover credentials, or call a service.

`fabric-sdk-jobs` owns service behavior. `fabric-cli-jobs` owns the public typed
handlers, registration and thin shared-executor adapter. The existing
`fabric-cli` owner integrates the single front door, context loading,
configuration, input files, UI and shared text rendering. This slice does not
modify that owner's parser/main, another lane's mutation/definition/table/access
implementation, or protected existing fixtures.

The adopted Rust prerequisite is `cc908ce08617f92601aa2dd68622797ccbcde9e7`;
the coordinated shared HTTP prerequisite is
`68e15d38bcc3df751b893414265876304a16a2f8`.
Existing divergent upstream `8ad4c703bdd6738189f59318a04cab297b6c4381`
ancestry is preserved, not used as the job oracle. New upstream item types are
a separate drift backlog, not additions to the pinned job support matrix.

## Command and option matrix

All rows are public library handlers, not claims of an integrated production
binary, live authentication, or native CI on all platforms.

| Leaf | Source behavior translated | Independent and native evidence |
| --- | --- | --- |
| `job start` | Async submission, configuration/parameters/input, source job type, 202 handle and ID output | Execution/wire/CLI fixtures; real shared executor; no definition LRO |
| `job run` | Initial sleep, custom interval, response Retry-After, signed timeout, every terminal status, cancel-on-timeout configuration | Wait/CLI fixtures, failed/unknown raw tables, keep-running versus cancel requests |
| `job run-list` | Instances versus schedules, empty messages, page order and first-row-column projection | Source API/handler fixtures; real shared `TokenPager` and later-page failure/partial/limit cases |
| `job run-cancel` | Required ID, one cancellation request, async message without ID data, optional actual status wait | Wire/CLI fixtures, service-confirmed cancellation, no 202-as-completed |
| `job run-status` | Required ID, instance versus schedule routes, full ordered response and headers presentation | Wire/CLI fixtures, null/future fields and service errors |
| `job run-sch` | Cron/daily/weekly fields, UTC, default disabled, full input envelope and conflict mapping | Schedule/wire/CLI fixtures, required-field and invalid-day/time errors |
| `job run-update` | Existing schedule GET before PATCH, enable/disable-only updates, preserve existing enabled state, full input | Schedule/wire/CLI fixtures, conflicting switch validation before mutation |
| `job run-rm` | Required ID, warning and confirmation, force bypass, one DELETE and source HTTP 200 completion | Wire/CLI fixtures, declined/confirmed/forced paths |

All leaves use the existing resolver for absolute, relative, quoted/list-token,
workspace and folder context. The source-supported resources are
`SparkJobDefinition` (`sparkjob`), `Notebook` (`RunNotebook`), `DataPipeline`
(`Pipeline`), `Lakehouse` (`TableMaintenance`) and `GraphModel` (`refreshGraph`).
Other item types fail with `UnsupportedCommand`; virtual/OneLake/local
branches retain the existing resolver's explicit unsupported behavior.

`-P/--params`, `-C/--config` and `-i/--input` retain `nargs="+"`.
Input cannot combine with parameters or Notebook config. Notebook parameters
keep `{type,value}` wrappers; pipeline values do not. Pipeline array/object
labels do not enforce the resulting JSON type. Duplicate parameter names
replace the value without changing its first position. Bool conversion is
exactly `value.lower() == "true"`, not strict boolean validation.
Notebook config is rejected for other job types. File inputs use the injected
host reader; no SDK/CLI library discovers ambient files.

`--timeout` accepts signed integers, including zero and negative values.
`--polling_interval` must be positive and strictly below a supplied timeout.
`--schedule`, `--wait/-w`, `--force/-f`, `--enable` and the source's inverted
`--disable` storage are explicitly represented. Parser cardinality/required
flags are checked against the unchanged full inventory.
No job query option exists in the source inventory; this slice adds none.
The hidden `--skill` value is retained in the typed command but rejected before
I/O until coordinated host request-header plumbing exists; it is never silently
accepted and discarded.

## Preserved oddities, not redesigns

- A 202 run response yields a handle, not completion. A 202 cancellation response
  is also merely accepted, even though Python's async message says "cancelled".
- Polling sleeps before the first status GET. The initial wait is capped at the
  timeout, but later sleeps may overshoot it. Request time is accounted in
  whole seconds. This is source elapsed accounting, not a new hard end-to-end
  wall-clock deadline.
- Timeout with keep-running enabled emits a warning and no success envelope.
  Default timeout handling submits cancellation and returns its async ID output;
  it does not claim service-confirmed cancellation.
- `Cancelled` and `Deduped` log warnings and produce no success envelope.
  `Failed` prints a raw table then raises `JobFailed`; an unknown status prints
  a table and returns without failure. Those raw tables occur even in JSON mode.
- List rows are projected onto keys of the first row. Fields occurring only in
  later rows are omitted, but status GET retains all parsed fields and order.
- Normal success envelopes name `job`, while synchronous completion uses the
  full `job run`/`job run-cancel` path copied by the polling helper. Decorated
  command failures also use the full leaf path. This difference is preserved,
  not normalized into a new output convention.
- Schedule time validation is a prefix regular-expression shape check, not
  calendar/clock validation. `99:99suffix` and impossible timestamp components
  remain accepted. Days are case-sensitive and comma whitespace is not trimmed.
- `--enable` alone during creation reaches Python's `json.loads(None)` error;
  it is not silently turned into a fabricated default schedule.
- Inline single quotes are replaced with double quotes, including apostrophes
  inside strings, matching the source rather than implementing a new JSON syntax.
- Synchronous cancellation can select the HTTP helper's generic Fabric-operation
  path when Location is not a job-instance URL and operation headers exist.
  A reconciled HTTP 200 causes the source job handler to emit no result and do
  no job-status polling. `cancel_for_wait` preserves this through the injected
  `JobOperationReconciler`; it never substitutes Notebook publication polling.

## Explicit boundaries and remaining gates

The shared executor retains Fabric HTTPS origin binding, response-byte bounds,
read-only retry policy, mutation single-attempt semantics and per-exchange
deadlines. Its `wait_for` has a maximum delay and a per-wait deadline; neither is
an overall job deadline. A service retry/poll delay beyond the caller's configured
shared bound fails explicitly. A zero delay needs no timer allocation.

Identifiers and accepted Location values cannot change origin, route, resource,
query or fragment. Missing/cross-resource job locations fail rather than
constructing Python's empty ID. Malformed typed responses and unexpected
success statuses fail rather than silently producing no result.
These are explicit bounded SDK safety differences, not unqualified Python parity.

JSON integer parameters now preserve native signed and unsigned 64-bit values; non-finite float
parameters are explicitly unsupported. Python's arbitrary-size integers,
non-finite JSON and Unicode decimal-number syntax are not certified by this
slice. Rust parser/conversion diagnostics can differ from Python exception
details; translated command codes and the source's semantic errors are retained.
The JSON convenience runner uses a full leaf command for handler errors;
Python main omits that field for uncaught non-`FabricCLIError` conversion
exceptions. Exact main-level unexpected-exception normalization remains a
front-door compatibility gap, not a claim of byte-for-byte error parity.

Text terminal width/color/layout, root/group custom help, `-help`, batch/REPL,
global option/configuration propagation and a production identity/context host
require the front-door integration. `execute` supplies the actual output data,
source flags and immediate/deferred UI events for that integration; JSON output
uses the existing shared envelope and writers. Windows local offline evidence
does not establish Linux/macOS native runner success.
`SharedJobOperations` now supplies the actual generic-operation adapter over
the released `OperationSession::reconcile_accepted(PythonSynchronous)`. It
never issues the initial mutation, and `JobError::Operation` retains the
original typed/raw failure, operation ID, cancellation and uncertainty.
`HttpJobExecutor` can supply both operation interfaces over the existing HTTP
engine. If a host omits a required reconciler, the command still returns
`NotSupported`, not a successful-looking cancellation/completion.

The resumed increment preserves the original 155 expectations and adds
396 unchanged-Python branch checks, including source hierarchy support for
all 44 pinned item types and eight leaves, additional payload/schedule/polling
cases, source full-name progress events and non-boolean schedule enabled-value
retention. Public native tests use real job/CLI/shared HTTP consumers; seven
concrete bridge tests cover successful result retrieval, raw terminal errors,
HTTP errors, bounds, transport loss, cancellation and no mutation replay.
This is evidence for those cases, not closure of all shared integration gates.

Operation-to-resource error-format conversion, hidden skill-header plumbing,
wide/non-finite JSON representation and shared text/main-level exception
projection still require coordinated shared interfaces. The SDK operation
failure is retained losslessly; the legacy CLI error convenience projection
does not yet promise the shared resource formatter's detailed operation output.

Online validation is unexecuted. Account approval alone does not authorize
job start/cancel/deletion. Exact tenant, client registration, disposable workspace,
permitted resource IDs/types, operation bounds and cleanup scope are still
required. No token/cache extraction, ambient credential fallback, live job call,
push, pull request or release is performed by this slice.
