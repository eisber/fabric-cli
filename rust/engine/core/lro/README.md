<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal operation reconciliation

`fabric-sdk-operations::complete` accepts the **actual** initial response. It
has no mutation request to replay. All status/result requests are bodyless GETs
without caller headers; authentication is supplied by the injected executor.

Fabric operation/job fallback delays are 2, 7, then 20 seconds repeatedly. Azure/Storage default
to each response's `Retry-After`, or 10 seconds when absent/invalid.
There is no attempt limit. Valid `Retry-After` seconds or HTTP dates are
not-before constraints. Invalid values invoke the caller's safe diagnostic
hook and fall back to the client schedule. A successful Fabric status's `Retry-After`
also delays its result GET (invalid terminal hints fall back to two seconds).
A hint at or beyond a configured remaining deadline times out without that GET.
Sleep, credentials, HTTP retries, and final
result retrieval share the deadline and cancellation token. Local cancellation
does not cancel backend work. `complete` has no overall deadline by default;
`complete_with_config` accepts validated optional `LroConfig` durations:
`poll_interval` replaces the client schedule but cannot shorten a server hint,
and `timeout` sets a monotonic total reconciliation budget. Individual status/result
GETs, including credentials and retries, are bounded to 240 seconds or the
remaining total budget, whichever is smaller. No overall deadline means
indefinite cancellable waiting, not unbounded HTTP requests.

Fabric accepts validated operation Location or ID-only handles. Conflicts fail
closed. Successful status without an advertised
result, and the documented `OperationHasNoResult` result error, return the actual
terminal status response. Azure/Storage 201/202 prioritizes Azure-AsyncOperation over Location;
on completion it returns the initial body/headers with status normalized to 200,
matching the retained Python shape. As in Python, an advertised header containing
the Azure endpoint contributes its suffix to the fixed Azure polling origin;
origin-relative headers also work. Storage uses the identical status-JSON and
Location HTTP-status protocols, with its own fixed OneLake origin and Storage
token scope. Its advertised headers use the same fixed-endpoint suffix resolution
as Azure, never switching to ARM or an advertised host. Responses without a
recognized handle and Power BI responses are raw.

Failed status or result responses expose a bounded service error identifier when
available, without forwarding service error messages. Codes must be nonempty,
at most 128 bytes, and contain only ASCII letters, digits, `.`, `_`, or `-`;
invalid codes are omitted with a diagnostic. When no code is supplied, the
failure remains `Service`.

Fabric job-instance Locations use a separate JSON status protocol. Only exact
`/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobInstanceId}`
routes are recognized, after the same URL safety checks. Job polling uses the
same 2-, 7-, then repeated 20-second schedule as Fabric operations, never earlier
than a valid `Retry-After` (invalid hints emit a safe diagnostic).
It shares the optional overall budget and safe GET
executor. `NotStarted`/`InProgress` remain pending; `Completed`/`Deduped` return
the actual terminal job response. `Failed`/`Cancelled`, unexpected HTTP statuses,
and malformed/unknown JSON statuses fail with query-redacted job-URL recovery.
No job result URL is fetched, no mutation is replayed, and local timeout or
cancellation never cancels the remote job. Unsupported job routes remain raw.

`complete_observed` accepts a caller-owned `PollObserver` in addition to timing
configuration. Its `accepted` callback receives the validated operation kind
and actual initial HTTP status once before the first wait. By default it forwards
to the existing default no-op `started` callback, preserving existing observers.
This acknowledgement is not an observed backend execution state.
The default no-op `waiting` callback exposes the selected status-check delay,
including server not-before constraints, before sleeping. Time spent in that
callback counts toward the delay rather than adding an extra wait.
Raw responses and invalid handles
do not notify; cancellation before reconciliation suppresses the callback.
Its default no-op `polling` callback runs after each required delay, before
credentials or HTTP for that logical status GET. It does not fire separately
for retries or result GETs and never changes the polling schedule.
Each recognized successful status GET supplies a `PollProgress`
containing only a canonical status and protocol label, including failed/cancelled
terminal statuses before their service error. Initial responses, result GETs,
failed HTTP attempts and unknown/malformed statuses do not generate observations.
All callbacks share cancellation and the overall deadline; callback errors retain
known recovery and stop local polling. No console I/O or runtime discovery occurs
in the engine. Existing `complete`/`complete_with_config` callers remain unobserved.
The CLI owns its independent one-second terminal refresh, five-second
append-only waiting heartbeat, and terminal coloring;
neither timer nor presentation policy is part of the engine.

Status and result-error JSON use the shared bounded `fabric-core-json` decoder.
Reserved serde marker names remain ordinary object keys, including in unrelated
metadata; returned terminal responses retain their original bytes and headers.

Known operation recovery errors contain a safe operation ID/status URL (query
omitted) and uncertainty, never arbitrary backend diagnostics. Fabric trusts
service-advertised polling/result hostnames without a hostname allowlist, matching
Python's shared LRO client. HTTPS/TLS, URL-component and polling-route checks
remain; raw requests and other audiences retain fixed-origin validation.
Redirects are rejected. Raw initial HTTP errors remain
response data. Initial requests, including GET, execute once; only status/result
GETs may retry under the remaining operation budget.

Unsafe operation URL errors identify only the rejecting validation stage:
length, control/space characters, unsafe path, or selected HTTPS origin/normalized
components. They never include the rejected URL or response headers. The last
category covers origin, credentials, fragments, normalized length and path checks;
it does not identify which component failed. These diagnostics do not change URL
acceptance or establish the cause of an earlier unclassified rejection.

The engine accepts the Azure SDK's
`Arc<dyn azure_core::credentials::TokenCredential>` directly; it does not define
another credential/token trait. HTTP, monotonic/wall-clock, sleep, and cancellation
contracts live in `fabric-core`. `fabric-sdk-http::HttpClient` composes these and
bounds SDK acquisition externally, dropping the future on deadline/cancellation.
SDK failures become sanitized `Unauthorized` errors; deadline/cancellation remain
distinct. `fabric-sdk-transport::NativeHttpClient` supplies pooled Hyper/Rustls
HTTPS with OS certificate verification, no automatic redirects or replay, and
16 MiB request / 32 MiB wire and decoded response bounds.

`fabric-core::operation_handle` shares handle validation between transport and
reconciliation. The native transport publishes a validated accepted response head
to the executor-owned `ResponseHeadCapture` **before** body reads/decompression.
Stream/decode failures, stalled bodies, cancellation, and outer executor timeout
retain only safe, query-redacted recovery evidence. Unsafe/conflicting handles
fail explicitly during reconciliation; raw/async execution can still inspect
the initial response without following those handles. Body failures retain safe
handle-validation diagnostics without hiding timeout/cancellation categories.
No error triggers mutation replay. Injected streaming transports
must override `HttpTransport::send_with_observer` to publish their heads early;
the default adapter supports already-materialized responses only.

Provenance: donor architecture and JSON numeric-formatting logic were inspected at
`f8cb97e92eab669785954335e0e7276d30a3377e`. The implementation is narrowed to the
approved public-protocol slice. No service facades, paging, resource packages,
ambient identity, or private reference source are included.

`tests/synthetic.rs`, `tests/result_delay.rs`, and `tests/progress.rs` exercise offline protocol, observation and timing cases using scripted
responses and a fake clock. They are requirement tests, not claims that Python
raw API previously performed automatic polling. These tests need integration
execution under the pinned toolchain before correctness is established.
