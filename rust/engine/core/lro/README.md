<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal operation reconciliation

`fabric-sdk-operations::complete` accepts the **actual** initial response. It
has no mutation request to replay. All status/result requests are bodyless GETs
without caller headers; authentication is supplied by the injected executor.

The fixed policy is 600 monotonic seconds, starting at reconciliation, with
1-second initial delay, doubling pending-response backoff, no attempt limit,
and no separate interval cap. Valid `Retry-After` seconds or HTTP dates are
not-before constraints. Invalid values invoke the caller's safe diagnostic
hook and fall back to backoff. A successful Fabric status's `Retry-After` also
delays its result GET (invalid terminal hints fall back to one second). A hint
at or beyond the remaining deadline times out without sending that GET.
Sleep, credentials, HTTP retries, and final
result retrieval share the deadline and cancellation token. Local cancellation
does not cancel backend work.

Fabric accepts validated operation Location or ID-only handles. Conflicts fail
closed; job-instance Locations remain raw. Successful status without an advertised
result, and the documented `OperationHasNoResult` result error, return the actual
terminal status response. ARM prioritizes Azure-AsyncOperation over Location;
on completion it returns the initial body/headers with status normalized to 200,
matching the retained Python shape. Other audiences are raw.

Status and result-error JSON use the shared bounded `fabric-core-json` decoder.
Reserved serde marker names remain ordinary object keys, including in unrelated
metadata; returned terminal responses retain their original bytes and headers.

Known operation recovery errors contain a safe operation ID/status URL (query
omitted) and uncertainty, never arbitrary backend diagnostics. Redirects and
cross-origin status/result URLs are rejected. Raw initial HTTP errors remain
response data. Initial requests, including GET, execute once; only status/result
GETs may retry under the remaining operation budget.

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

`tests/synthetic.rs` and `tests/result_delay.rs` exercise offline protocol and timing cases using scripted
responses and a fake clock. They are requirement tests, not claims that Python
raw API previously performed automatic polling. These tests need integration
execution under the pinned toolchain before correctness is established.
