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
When a valid non-2xx HTTP status has already been observed, subsequent transport
failure retains `Http(status)` rather than replacing that known status with uncertainty.
Metadata/protocol/input validation failures and local limits retain their actual
read error even for non-2xx responses. Mutation handling is unchanged: a known
non-2xx status retains `Http(status)`; otherwise lost observation remains unknown.
They must not become status-only errors that a consumer could wrongly retry
after rejected metadata erased a server-directed delay.
An observed HTTP failure is not a guarantee of mutation rollback; the caller
still owns any service-specific reconciliation needed for that status.
Well-formed responses preserve the actual status, optional Content-Type and
header values. 202 remains acceptance, not service completion.

The optional `livy-arrow` feature forwards the SDK's raw Arrow contracts and
enables the composed executor/driver regression in `tests/native_stream.rs`.
Its `native-http-arrow` impact profile runs that test on all native platforms.
This tests actual adapter-to-driver error propagation, not live Arrow support.

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

The native transport now rejects ambiguous singleton metadata **before** its
Azure conversion for existing `/livyApi/versions/` routes or the existing Arrow
media in Accept. The guarded names are Content-Type (the actual format/version
carrier), Content-Encoding, Content-Length, Retry-After, Location, ETag,
`statementId` (statement handle) and `id` (HC/REPL handle, not a statement ID).
Any two physical values, including identical or case-varied ones still visible
in Hyper's HeaderMap, fail with sanitized `DataConversion`. No guessed Livy
format headers, outbound flags or new wire fields are introduced.

The scope is intentional: global rejection would change generic behavior,
including legitimate repeated Content-Encoding metadata. Outside the existing
Livy/Arrow request selection, the raw-to-Azure conversion is unchanged.
Repeated non-singleton headers retain the established comma joining and Latin-1
conversion, including on Livy responses. A comma inside one field is not treated
as evidence that there were duplicates. This changes neither generic public
output APIs nor service/client configuration.

**Remaining raw-wire limitation:** Azure `Headers` is a case-insensitive HashMap;
custom clients can erase duplicates before the adapter. Hyper may normalize or
reject raw syntax before its HeaderMap reaches this seam. The guard cannot
recover information already erased by any upstream parser, does not cover every
possible singleton name, and deliberately does not change generic non-Livy
duplicate handling. Full raw duplicate rejection is not certified.

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

## Adopted transport evidence ABI and provenance

The ABI at `a360d5a35abad679745fdb6f22fceda83e5ae03f` was cherry-picked as
`00f396cacc550d3420feea78ca82f0c1360e5952`, preserving the original streaming
and preparatory commit ancestry. `NativeError::Transport` is now projected at
exactly two streaming boundaries: `HttpClient::execute_request` errors and
errors yielded by the actual body stream. Only Azure `ErrorKind::Io` receives
that projection for GET/HEAD. Credential acquisition is a separate pre-dispatch path;
metadata validation, empty chunks, limits, deadlines, status failures and
protocol/format errors retain their existing classifications. Mutations
still report `OutcomeUnknown` where no outcome is provable. The variant
is evidence for the owner-bound operation, not retry/replay authority, and must
not broaden retry of `InvalidResponse`.

The narrowly repaired native decoder carries an opaque, sanitized upstream kind
through StreamReader and gzip/zlib decoding. Actual upstream Io, DataConversion
and Other remain distinguishable; decoder-origin corruption and clean compressed
truncation are DataConversion, not recoverable Io. Original diagnostics, bodies
and tokens are not kept in the marker or returned error.

At the Hyper boundary, typed HTTP parse errors are DataConversion. Known socket
I/O, premature message EOF and timeouts provide Io evidence. TLS errors, user
errors without stronger upstream evidence, cancellation/closed channels without
actual I/O evidence and unrecognized failures remain Other. Error-source walking
is finite and never classifies from diagnostic text. There is no fabricated
`Http(0)`, new dependency or retry engine.

New decoder tests cover all three upstream kinds under identity/gzip/deflate,
corrupt/truncated compression and genuine mid-header I/O. Raw HeaderMap and
loopback native-client fixtures cover scoped duplicate rejection, legal repeated
fields, Hyper parse failure versus EOF, trailer/chunk protocol errors, actual
gzip corruption, real mid-body disconnects and mutation uncertainty. These
fixtures are authored, not executed: builds and test execution remain reserved
to the parent's serialized validator.

The pinned SDK's older `ArrowError::from(NativeError)` still has a wildcard to
InvalidInput; the parent-owned Arrow operation/resume composition must preserve
the new Transport evidence rather than lose it there. No SDK runtime or Arrow
module was manually edited after the exact ABI cherry-pick.
