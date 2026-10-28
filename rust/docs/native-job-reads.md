<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded native job reads

Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Released Rust prerequisite: `50eb337e696d5de8d49c65039ca9eff4ca3e0a60`,
adopted by ordinary merge after preserving the original `9c396ce` draft.
The first slice added `job run-list` and `job run-status` to `fab-rs`.
The next additive compositions register the remaining six source job leaves as
described below. All eight registrations come from the existing jobs family.
These are partial entry points, not complete Python commands or live proof.

## Additive submission and synchronous execution

The second microbatch mounts the released family's `start` and `run`
registrations and typed conversion once. `Invocation::JobRun` preserves
`-P/--params`, `-C/--config`, `-i/--input`, signed `--timeout`, positive
`--polling_interval`, output selection and hidden skill selection. Unsupported
options still fail during parsing; no placeholder handlers are registered.
The existing read DTO and `JobReadSession` struct-literal API remain available.

`JobSession` borrows the same `ReadSession`/command-local `Snapshot` and
constructs the existing `JobClient`, `HttpJobExecutor` and
`SharedJobOperations`. It injects the caller's `JsonInput`, `JobUi` and
`JobConfig` into `JobHost`; it implements no submission, poll, timeout,
reconciliation, retry or credential algorithm. The native host retains its
single current-thread runtime, Ctrl+C and overall deadline, with silent explicit
Windows credentials only. Linux/macOS credentials are still unsupported.
Default/broker-only builds remain `NotConfigured` without credential discovery.

The CLI supplies a reader for explicitly selected execution/configuration files.
It rejects malformed/unsupported JSON through the existing lossless output
parser. Native configuration currently uses the source default
`cancel_on_timeout=true` when no Rust configuration root is selected; an explicit
local-family store supplies `job_cancel_ontimeout` and default output through
the [native settings snapshot](native-family-host.md#native-read-and-job-settings).
No Python profile is discovered. Offline callers inject both cancellation
settings. Immediate grey/progress/warning events and failed/unknown raw status
tables use the shared non-TTY renderer. Deferred async status hints are written
after the result, preserving stream order. Writer errors remain failures.

Accepted submission is not completion. `start` emits the accepted ID;
`run` delegates real status polling, initial sleep, custom interval and timeout
policy to the released handler. Local cancellation drops work without inventing
rollback; a timeout cancellation message remains explicitly asynchronous.

`job_execution_probe` injects only synthetic credentials, HTTP, time, files and
UI into that production parser/session. The additive
`tests/compatibility/job_execution/compare.py` freezes 38 observations from
unchanged pinned Python parser/wrapper/payload/API/polling/UI functions before
launching Rust. It compares both output formats, all five source resources,
inline/file input, Notebook parameters/configuration, pipeline parameters and
both timeout policies: streams, exits, ordered methods/URLs/bodies, scopes,
file reads and waits. Existing read fixtures/oracles remain unchanged.
These are offline cases, not a live job submission grant or full parity claim.

The released family still needs a lossless raw-JSON input boundary for inline
payloads and structured pipeline parameters. Direct `serde_json` decoding can
round integers outside the supported JSON domain. This is a reported family
API gap, not closed by the native file reader or response guard; no full
input-domain compatibility is claimed by this candidate.

## Additive cancellation and schedule changes

The third microbatch mounts `job run-cancel`, `job run-sch`, `job run-update`
and `job run-rm`. The complete family registration is now mounted once;
`Invocation::JobMutation` converts its existing typed result without parsing
payloads, inventing schedule defaults or copying service logic.
It preserves required IDs, `--wait/-w`, `--force/-f`, inline/file input,
cron/daily/weekly fields, enable/disable flag occurrences, output and skill.

All four paths reuse the same `JobSession`, credential selection, metadata
snapshot, files/UI/configuration capabilities and `SharedJobOperations`.
Cancellation delegates one POST and optional actual job-status wait; accepted
generic operations use the existing shared operation/result retrieval, not a
second poller. Schedule updates retain the family's GET-before-PATCH and enabled
state rules. Removal never assumes consent: an injected UI can confirm, decline,
fail or be cancelled before DELETE. Native terminal prompting remains explicitly
unsupported; the native removal happy path requires the caller's `--force`.
Non-forced native removal fails `NotSupported` without DELETE rather than
blocking the current-thread runtime or silently approving the action.

The same `job_execution_probe` now accepts these typed invocations.
`tests/compatibility/job_mutations/compare.py` independently freezes 64 source
observations covering four leaves, five resource types, both output formats,
real status waits, daily/weekly schedules, full inline/file schedule envelopes,
enabled-state updates and both confirmation outcomes. Its request bodies,
methods, streams, exits, credential scopes, file reads, waits and confirmation
counts are compared with the production session. Focused native tests additionally
exercise shared generic-operation retrieval, invalid source arguments,
confirmation cancellation/failure, unauthorized reads and non-replayed mutations.
No live job, schedule, account or credential store is used.

This is a source candidate until the exact Linux compile/lint/tests and native
comparisons pass. Registration or passing Python-only fixtures do not advance
mounted-command coverage. The family raw-JSON input fix must be explicitly
released and validated in the final composition; this microbatch does not copy
an unpublished family patch.

## Selection and composition

Default and broker-only builds recognize these leaves but return
`NotConfigured`, without discovering credentials. The existing `native-read`
feature selects the same Windows host used by LS/dir/GET. It requires the same
explicit `FAB_RUST_TENANT`, `FAB_RUST_ACCOUNT` and `FAB_RUST_CLIENT_ID` selectors,
optional `FAB_RUST_CONTEXT`/`FAB_RUST_HOME`, silent Fabric-only credentials,
fixed HTTPS origin, shared HTTP executor, CLI-owned runtime, Ctrl+C and
the bounded command-kind deadline below. Non-Windows execution is explicitly unsupported.
There is no second host, runtime, retry loop, poller, ambient identity,
static-token input, sign-in prompt or persisted session.

`JobReadSession` borrows the existing `ReadSession` for asynchronous metadata
discovery and constructs the released `JobClient` with that SDK's exact
credential and selection. The same command-local `Snapshot` and pure resolver
load workspace/item/folder scopes before the existing `JobHost` executes.
The job adapter delegates HTTP and time to `HttpJobExecutor`; pagination stays
in the existing SDK `TokenPager`. The released `SharedJobOperations` is composed
over that same executor, credential and selection; read GET202 reconciliation
still requires a family API and is not invented here.
The old read session's public fields,
default strict SDK behavior and source-synchronous workspace GET opt-in remain
unchanged.
The only new internal dependency edges are `fabric-cli -> fabric-cli-jobs` and
`fabric-cli -> fabric-sdk-jobs`; registry packages and default native
identity/transport feature selection are unchanged.

### Command watchdog versus job timeout

Python has no implicit job timeout. Its explicit signed `--timeout` applies to
the family's elapsed wait, not credential/metadata setup or timeout cleanup.
The host must not drop that handler at 240 seconds when the caller selected a
longer finite wait: doing so skips `job_cancel_ontimeout` policy altogether.

The native watchdog remains 240 seconds for reads, asynchronous submission,
schedule mutations, negative run timeouts, runs without `--timeout`, and
`run-cancel --wait` (which has no source timeout option). It does not inject a
family timeout into the latter two unbounded Python flows. Extending those
flows needs a separate finite-bound policy decision; the current cap remains
an explicit preview limitation, not Python parity.

Only synchronous `job run --timeout N` with `N >= 0` receives a derived finite
wall-clock budget. Using existing executor and operation limits:

```text
setup allowance       = HTTP timeout                              (240 seconds)
job wait allowance    = 2 * N
final poll allowance  = HTTP timeout + maximum executor delay      (300 seconds)
cleanup, if enabled   = cancellation POST HTTP timeout
                      + operation maximum requests
                        * (HTTP timeout + operation maximum poll delay)
                      = 240 + 100 * (240 + 60) seconds
host deadline         = setup + job wait + final poll + optional cleanup
```

The second `N` accounts for source/family truncation of each status request's
elapsed time to integer seconds with positive polling intervals. The final poll
can overshoot the family timeout. Cleanup reserves the existing bounded
generic-operation reconciliation envelope, not an arbitrary new job default;
it can therefore be much larger than an ordinary successful cancellation.
When `job_cancel_ontimeout=false`, none of that cleanup allowance is added.
The family still makes the only cancellation-policy decision, never the watchdog.
No HTTP timeout, retry, polling or request-count limit is changed.

Checked arithmetic rejects unsupported duration ranges before runtime or
credential connection. Runtime deadline representability is also checked.
The watchdog remains finite even for pathological zero-delay service polling,
clock/accounting anomalies or stalled provider cleanup. It is not a guarantee
that every adversarial response can reach family cleanup. Ctrl+C retains its
immediate local-cancellation behavior: neither Ctrl+C nor watchdog expiry
requests remote cancellation or implies rollback. Accepted cancellation is
still asynchronous, not a claim that the job has stopped.
After retaining the command result, native runtime shutdown has a separate
one-second bounded wait. Unfinished blocking work may still complete afterward;
the interruption diagnostic states this, rather than silently claiming cleanup.

The read-only DTO reuses the released family typed conversion. It preserves
source token paths, required status `--id`, repeated
scalar flags, `--schedule`, long-option abbreviations and global
`--output_format json|text`. There is no source query/filter/sort option for
these leaves: `-q`/`--query` and mutation-only switches fail during parsing.
List order is service/page order, not an extra CLI sort.
The existing sanitized invalid-arguments diagnostic remains byte-compatible.

## Supported branches

| Branch | Behavior |
| --- | --- |
| Run list | Instance endpoint, bounded complete paging, first-row key projection in original order |
| Schedule list | Job-type schedule endpoint; the same paging/projection, distinct empty message |
| Run status | Required ID, full ordered object with string status; future status strings remain data |
| Schedule status | Required ID, full ordered schedule object with a string ID, without instance-status interpretation |
| Empty list | `No runs found` or `No schedules found`; not an empty metadata snapshot |
| Context | Absolute, relative, tokenized/quoted, current, explicit home and folder resolution |
| Failures | Existing full leaf error envelopes, JSON stdout, Unauthorized exit 4, no partial-page success |
| Presentation | Shared JSON encoder or explicit text tables with headers, no colors and ASCII terminal properties |

The five pinned source resources are Spark job definitions, notebooks, data
pipelines, lakehouses and graph models. Other resources fail explicitly.
No skill headers are sent by default. Explicit `--skill` uses the released
`JobRequestHeaders`/`JobClient::with_request_headers` support; empty values emit
no header. Discovery retains its original unscoped metadata requests, while
job pages retain the selected skill. The checked executor forwards header
capabilities, rather than silently discarding a selected value.
Present empty values remain distinct
from absent arguments. Null, empty, false and finite numeric fields are not
stringified or normalized to invented defaults.

## Explicit gaps

The released family APIs cannot yet express every source branch. These gaps
are reported to the integration owner for the existing jobs family owner:

- Non-200 success responses fail nonzero. Python's handlers emit no output
  for such responses, potentially after source HTTP operation reconciliation.
  No accepted response is treated as completed or replayed. No generic GET
  operation policy is added by this slice.
- The released status API requires a JSON object with a string `status`;
  the schedule API requires an object with a string ID. Python can print other JSON values,
  including missing/null status fields. Lists require an array of objects;
  Python has additional malformed/missing/value-shape branches.
- Python arbitrary-size integers, NaN/Infinity and lone surrogates require a
  lossless family/output value API. The host validates successful job JSON
  through the existing `parse_data` policy before SDK decoding, so unsupported
  data fails with an execution/error envelope rather than silently rounding an
  integer into a float. Direct/default SDK consumers are unchanged.
- Unicode-dependent text width/casing/repr, real terminal policy, persisted
  Python context/configuration, batch/REPL, custom source group help and
  cross-platform credentials remain unsupported. JSON Unicode is supported
  within the shared JSON-domain limits.

These restrictions are not waivers or proof of full command parity. Existing
independent source expectations are not changed to match native behavior.

## Offline evidence boundary

`job_read_session_probe` injects only synthetic credential, HTTP and time into
the same production parser and `JobReadSession`. It is a development executable,
not a test flag or alternate production command engine.
`tests/compatibility/job_reads/cases.json` is hand-authored from the pinned
source before native comparisons. Its runner executes the unchanged job API,
list/status handlers, list projection and real JSON/text UI source, then freezes
all observations before launching the native consumer. It compares exact
streams, exits, ordered requests and Fabric scopes for both routes/formats and
all five resources, plus present-empty and selected skill cases using the
released source header contracts. Four separately classified API-gap cases must fail
nonzero; they cannot count as exact matches.

Metadata discovery expectations are independently specified synthetic fixtures,
not a claim that this runner executes Python's entire context/cache engine.
Rust production-boundary tests additionally exercise paging/later-page errors,
context, cancellation, shared HTTP deadlines, absent headers, unrepresentable
values and writer failures. Actual `fab-rs` subprocess tests separately cover
registration, malformed arguments and missing/invalid selectors.
No service, valid account, broker cache, mutation or live job is exercised.

## Candidate validation status

This source candidate is **unvalidated** locally. Both immutable v3 receipts
(`gate-jobs-01.json`, `gate-jobs-02.json`) refused host quiescence before any
validation command started; neither is a test failure or a passing receipt.
The coordinator owns isolated hosted CI at the exact candidate SHA. Source
self-review and formatting do not establish compilation or any passing test
count. No registration/coverage metric is advanced by this candidate.
