<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Pull-based native Fabric HTTP streaming

This adapter targets the SDK runtime contracts at
`8fcd0859eacc7a4fea4a29485bd0f46ee6476115`. Public composition is
`fabric_sdk_http::native_stream::NativeFabricStreamExecutor::new(context, http)`,
returning `Result<Self, NativeError>`. It implements `NativeStreamExecutor::open`
and returns a real `NativeStreamResponse` with an owned `NativeStreamBody`.
It does not collect the response or return a metadata-only execution receipt.

## Existing stack, single dispatch

The adapter owns the explicit caller `RequestContext` and an existing
`HttpExecutor`. Construction validates exactly equal Fabric endpoint bindings
and the Fabric audience without I/O. Both native adapters share request
preflight: the exact selected HTTPS origin, `/v1` prefix, safe headers, 4 MiB
request body limit and finite iterative JSON depth/node validation before
serialized allocation. Stream limits and all request validation precede
credential access. Arbitrary native, custom and non-Fabric MWC/workload origins
remain unsupported; no bootstrap or new authorization decision is introduced.

Core `RequestContext::execute` borrows the caller's `TokenCredential` and requests
only the existing Fabric scope, with no credential discovery or implementation.
The existing HTTP token guard and authenticated request builder are reused.
The injected `HttpClient::execute_request` is called once, including for GET,
inline POST, 429, 503 and redirects. It must already disable backend retries and
redirect following, and release local work on future/body drop.

The source API was inspected in the cached Azure Core / TypeSpec 1.1.0 sources:
`AsyncRawResponse::deconstruct()` returns an `AsyncResponseBody` implementing
`Stream<Item = Result<Bytes>>`. Only that real stream is pulled. The pinned
repository's buffered reader is named `read_with_retries`, not `read_bounded`;
its deconstruction/stream API was inspected rather than inventing a body API.
There is no new HTTP client, TLS backend, runtime, retry or authentication engine.

## Absolute time and dispatch evidence

One supplied `NativeDeadline` covers acquisition, header wait, body consumption
and caller pauses. `ExecutionTimer::sleep` races each pending operation using
the original remaining budget, never a fresh timeout. The same watchdog checks
before and after each future poll, rejecting late synchronous returns. Dispatch
also checks the absolute deadline after credential acquisition and request
construction, before sending. `ExecutionOptions` is not used to invent a second
stream budget; it retains its existing buffered-operation semantics.

The body retains a clone of the opening deadline. A shorter deadline supplied to
`read` can shorten observation; a fresh longer one cannot extend it. While the
caller is paused there is no background task or read-ahead. Elapsed pause time
is charged on the next pull. Expired resources are released on that pull or on
body drop; the adapter cannot run disposal in an unpolled, caller-owned future.

Credential failure is sanitized `InvalidResponse`, invalid token syntax is
`InvalidInput`, and acquisition timeout before dispatch is `TimedOut`, including
POST. Once dispatch is attempted, lost transport, response metadata, streaming
limits, timeout or aborted reads are `OutcomeUnknown` for non-GET/HEAD methods.
Read-only errors remain `InvalidResponse`, `Limit`, `TimedOut` or `Cancelled`.
This conservative method classification also covers a POST declared as a read.
When a valid non-2xx HTTP status has already been observed, subsequent failure
retains `Http(status)` rather than replacing that known status with uncertainty.
An observed HTTP failure is not a guarantee of mutation rollback; the caller
still owns any service-specific reconciliation needed for that status.
Well-formed responses preserve the actual status, optional Content-Type and
header values. 202 remains acceptance, not service completion.

## Pull bounds and disposal

Opening never polls the transport body. Each `read(max_bytes, deadline)` polls
at most one new transport fragment, or drains the already held remainder
without another transport poll. Exposed nonempty vectors are at most
`min(max_bytes, max_fragment_bytes)`. A zero read size is invalid and terminal.
Empty transport fragments are errors, never fabricated HTTP EOF.

Each whole fresh fragment is checked against remaining `max_body_bytes` before
any exposed data is copied. A fragment that would exceed the total is rejected
in full; a short prefix is not exposed as apparent partial success. An accepted
large fragment is split with zero-copy `Bytes` remainder slices. At most the
current transport fragment/remainder is retained, bounded by the validated
remaining total budget, not the whole response. The transport owns allocation
of its incoming fragment/backing buffer; this adapter cannot retrospectively
bound a caller client that allocates or prefetches unbounded data. Total bytes
refer to the body bytes delivered by that client, including native protocol
framing, not HTTP transfer framing or a compressed wire Content-Length.

Actual EOF drops the transport immediately and is idempotent. Errors discard
the stream/remainder and become terminal. Dropping the body releases both.
Dropping a **polled pending read** also closes it terminally through a drop
guard; no later pull resumes that cancelled local operation. Dropping an
**unpolled read future** has not started an operation and leaves its owning body
untouched. Dropping `open` releases pending credential or header work. None of
these paths sends remote cancellation or proves mutation rollback.

## Metadata limits and explicit interface gap

Both native adapters now share the same metadata parser. It checks at most
64 fields and 16 KiB of names plus values before copying, validates token-shaped
header names and rejects control characters and all observable case-insensitive
duplicates. Public debug/error formats redact URLs, bodies, credential selectors,
tokens, media and header values.

**Unimplemented below this interface:** Azure `Headers` is a case-insensitive
HashMap; insertion normalizes names and replaces duplicate fields before the
executor receives them. A custom client can therefore erase duplicate/case
evidence. The existing native transport also combines repeated wire values.
This adapter cannot certify rejection of raw duplicate headers already erased
or combined upstream, and does not guess by rejecting every comma-containing
header. Raw duplicate rejection requires a separately reviewed transport/raw
metadata contract; no such SDK, transport or manifest change is made here.
The SDK agent registry exposes sibling task IDs but no coordinator/host
recipient, so this gap is retained in this immutable handoff rather than sent
through an unrelated identity or independent Tachikoma messaging.

## Authored offline evidence, not executed tests

`tests/native_stream.rs` uses instrumented Azure HTTP/body, credential and timer
fakes around the production adapter. It covers no-collect opening, fragment
splitting, exact and excessive total bytes, pauses/no read-ahead, request/limit
preflight, endpoint/audience mismatches, exact scopes/wire/media, single POST
dispatch and uncertainty, auth/header/body stalls and late returns, decreasing
shared budgets, response metadata limits, EOF/error/drop disposal and redaction.
A source-level parser test covers duplicate/case variants that Azure's public
header map cannot represent. Test scopes deny `unnameable_test_items`.

No build, Cargo check, Clippy, test execution, live backend or authentication
was run: the owner explicitly prohibited builds for this lane. Formatting,
source/API inspection, static scope checks and Git whitespace checks do not
establish compilation or a passing runtime suite. The coordinator must run the
focused `native_stream` integration test, the `native_stream::tests` unit tests,
the existing `native_runtime` regression tests and an all-target check before
claiming integrated acceptance.

## Proposed transport evidence ABI

An additive `NativeError::Transport` can be projected at exactly two adapter
boundaries: `HttpClient::execute_request` errors and errors yielded by the actual
body stream. Only distinguishable I/O evidence should receive that projection
for GET/HEAD. Credential acquisition remains a separate pre-dispatch path;
metadata validation, empty chunks, limits, deadlines, status failures and
protocol/format errors must retain their existing classifications. Mutations
still report `OutcomeUnknown` where no outcome is provable. The proposed variant
is evidence for the owner-bound operation, not retry/replay authority, and must
not broaden retry of `InvalidResponse`.

The ABI alone is insufficient to certify that distinction for the current native
backend. `fabric-sdk-transport/src/decode.rs` converts every upstream stream
error into a generic `io::Error`, then maps every `ReaderStream` failure to
Azure `ErrorKind::Io`. This conflates genuine I/O loss with malformed gzip/zlib
and upstream `DataConversion` errors, including unrepresentable HTTP trailers,
even on the identity encoding path. `native.rs` also flattens Hyper request and
response errors without retaining their typed causes. The concrete adapter
cannot recover information already erased there, and must not infer it from
diagnostic text, a fabricated `Http(0)` or failure timing.

The native transport owner must preserve real I/O versus protocol/decoding error
classification before an adapter mapping of `ErrorKind::Io` can establish that
distinction for that backend. No transport source, SDK ABI, exhaustive host
mapping, manifest or retry policy is changed in this preparatory handoff.
Dedicated fake-client regressions exercise direct `DataConversion` header/body
failures for GET/HEAD and POST, protecting non-retryable protocol failures.
Projection to the new public variant remains pending its exact committed ABI;
the currently committed SDK contract still has no `Transport` variant.
