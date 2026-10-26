<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded native job reads

Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Released Rust prerequisite: `50eb337e696d5de8d49c65039ca9eff4ca3e0a60`,
adopted by ordinary merge after preserving the original `9c396ce` draft.
This adds only `job run-list` and `job run-status` to `fab-rs`. It does not
register job submission, cancellation, execution or schedule mutations.
These are partial entry points, not complete Python commands or live proof.

## Selection and composition

Default and broker-only builds recognize these leaves but return
`NotConfigured`, without discovering credentials. The existing `native-read`
feature selects the same Windows host used by LS/dir/GET. It requires the same
explicit `FAB_RUST_TENANT`, `FAB_RUST_ACCOUNT` and `FAB_RUST_CLIENT_ID` selectors,
optional `FAB_RUST_CONTEXT`/`FAB_RUST_HOME`, silent Fabric-only credentials,
fixed HTTPS origin, shared HTTP executor, CLI-owned runtime, Ctrl+C and
240-second overall deadline. Non-Windows execution is explicitly unsupported.
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

The parser reuses only the two released family registrations and its typed
conversion. It preserves source token paths, required status `--id`, repeated
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
