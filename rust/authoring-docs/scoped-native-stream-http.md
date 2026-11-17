<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit-scope native HTTP streaming

`fabric_sdk_http::native_stream::{NativeScopeStreamExecutor, NativeStreamPreflight}`
provides an additive composition for a caller-selected HTTPS endpoint and
literal audience scope. The same names are available in
`fabric_sdk_http::scoped_native_stream`. The existing
`native_stream::NativeFabricStreamExecutor` keeps its Fabric-only constructor,
endpoint policy and behavior.

## Injection and service policy

`NativeScopeStreamExecutor::new` takes, in order, a `NativeEndpoint`, a literal
audience `String`, an `Arc<dyn TokenCredential>`, an `Arc<dyn HttpClient>`,
an `Arc<dyn ExecutionTimer>` and an `Arc<dyn NativeStreamPreflight>`.
All are retained immutably. Construction performs no I/O. There is no ambient
credential discovery, identity fallback, interactive consent, mutable auth
policy, token-injection API, CLI process, logger or runtime installation.

The endpoint must be HTTPS, including for injected offline clients, and cannot
end in repeated path separators. Requests
must retain its exact origin, including its selected port, and remain within
its base path at a segment boundary. The audience is supplied independently,
never inferred from the host. A scope is at most 4,096 bytes and must be a
literal, canonical HTTPS URL with a non-root path, no credentials, query,
fragment, escapes, whitespace or control characters. It is passed verbatim as
the only scope in one `get_token` call, with no request options. Syntax
validation does not verify a token's audience, tenant, account, claims or
permissions; caller credential selection and service authorization remain
separate responsibilities.

The shared native request preparation enforces safe headers, 16 KiB URL and
header budgets, at most 64 prepared request headers (including Accept and
Content-Type, before the shared builder adds Authorization), a 4 MiB request body budget,
and bounded JSON depth and node counts. Explicit body bytes are not parsed or
reserialized. JSON bodies use the existing serializer and allocation guards.
Authorization, cookies, host, framing and hop-by-hop overrides are rejected.

`NativeStreamPreflight::validate(&NativeRequest, &NativeDeadline)` receives only
immutable references. It runs after generic request validation and before
credentials, then runs again after token acquisition and validation, immediately
before the final remaining-budget check and HTTP send. Its error is returned
without dispatch or an uncertainty claim. It cannot replace the request,
scope, token or body. If acquisition expires the absolute deadline, sending is
rejected even when the credential returns synchronously in a late poll.

The caller's service preflight owns route, method, database, correlation,
read-only options, permissions and explicit server-timeout policy. For example,
a query adapter must reject a server timeout that no longer fits the original
remaining budget after a slow credential acquisition. The generic adapter
does not parse query languages, guess public-cloud policy, clamp timeouts or
offer a boolean policy bypass. This capability does not enable any query or
AI Hub command by itself.

## Shared execution and disposal

Both streaming adapters use one request builder, dispatch helper, metadata
parser, deadline guard, bounded response body and uncertainty classifier.
Each open invokes `HttpClient::execute_request` once. Redirects, 429 and 503
are returned as actual responses, never followed or replayed by this adapter.
The injected HTTP client must already disable backend redirects and retries;
the trait does not expose a way to inspect or override that configuration.
The client also owns incoming fragment allocation, decompression and any
prefetching, which cannot be retrospectively bounded here.

One supplied absolute `NativeDeadline` covers acquisition, headers, body reads
and caller pauses. `ExecutionTimer` guards pending operations. A later body
read can shorten but cannot extend the opening deadline. No background task
reads ahead or disposes an unpolled body during caller pauses.

The existing `NativeStreamLimits`, `NativeStreamResponse` and
`NativeStreamBody` contracts are unchanged. Whole incoming fragments count
toward the body budget before copying; returned fragments are nonempty and
bounded by the caller's read size and fragment limit. Empty incoming fragments
are terminal errors, not false EOF. Only actual stream EOF returns `None`.
Errors, EOF and drop release local body resources. Cancelling a polled pending
read closes that body terminally. None of these actions requests remote
cancellation or proves rollback.

Pre-dispatch errors remain their actual validation, credential or timeout
errors. Post-dispatch GET/HEAD errors retain transport, protocol, limit,
deadline and cancellation evidence. For other methods, including a POST
declared as a read, lost observation remains `OutcomeUnknown` unless a known
non-2xx status supplies `Http(status)`. Successful HTTP status is not proof of
workload completion or permission suitability.

The existing [streaming evidence and raw-header visibility limits](native-stream-http.md)
also apply. Azure's header map can erase raw duplicates before this seam;
observable duplicates are rejected, but full raw-wire duplicate rejection is
not certified for arbitrary injected clients.

## Offline evidence and limits

`fabric-sdk-http/tests/scoped_native_stream.rs` injects real implementations of
the public Azure credential and HTTP traits and the existing asynchronous
timer. Its synthetic fixtures cover literal scope acquisition, exact
origin/method/headers/body, immutable policy snapshots at both validation
points, pre-credential rejection, post-token timeout/policy rejection,
single-dispatch statuses and errors, pending/late operations, bounded pulls,
EOF, disposal, cancellation and uncertainty. Existing Fabric stream and
buffered native runtime tests remain separate regression obligations.

The package is currently at `engine/core/http/executor`. Physical relocation
to `engine/sdk/http` is a separate compatibility refactor; this adapter does not
adopt an unqualified extraction or introduce neutral runtime aliases of its own.
The existing package identity and compatibility imports remain stable.

These are authored fixtures, not a live-service claim. Compilation, test
execution, optional-feature consumer checks and publication require their
separate resource admission. No real tenant data, networking, credentials,
consent or live queries are used. Service-specific permission and live
authorization acceptance remain unverified.
