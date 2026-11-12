<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native HTTP/TLS transport and WebSocket data paths

`fabric-sdk-transport` supplies `TokioTimer` and an optional Azure-compatible
`NativeHttpClient`. It is not a service client, identity provider, retry engine,
pager or operation poller.

There are no default features. Enable `hyper-rustls` to select the native HTTP
backend. The SDK facade and focused service crates do not select it implicitly.

The independent `notebook-websocket` feature provides a bounded RFC 6455 JSON
`NotebookChannel` over an already-upgraded caller-owned Tokio stream. It selects
only protocol framing, not HTTP connect/upgrade, TLS, credentials or runtime
discovery. Its owner-bound managed-driver composition and limits are described
in [Notebook WebSocket data path](../../../../authoring-docs/notebook-websocket-native.md).
The caller still supplies the authenticated upgrade and actually ready session.

## Configure a client

`NativeHttpClient::new(TransportOptions)` validates explicit options and
configures trust without service I/O. Clones share a connection pool.

| Option | Requirement |
| --- | --- |
| Connection, read and write timeouts | Nonzero, at most 24 hours |
| Pool idle timeout | Nonzero, at most 24 hours |
| Maximum idle connections per host | 1-1024 |
| Trust | Platform verification with optional explicit additional roots |
| Proxy | Explicit `None` or `StaticHttp` policy |

Certificates are validated DER with bounded allocation. Construction never
falls back to insecure certificate or hostname validation. No ambient credentials,
global runtime or logger are initialized.

Poll requests and timers on a caller-owned Tokio runtime with I/O and time
enabled. Timer cancellation does not undo a server-side mutation.

## Dedicated managed-identity HTTP

`NativeHttpClient::for_managed_identity(options, exact_endpoint)` is a separate,
explicit capability. It accepts one literal loopback/link-local HTTP endpoint
without user information, query or fragment and rejects proxy configuration.
Only GET requests to that exact origin, port and path are allowed; all other
destinations, including HTTPS service URLs, fail before network I/O. Credential
adapters must additionally validate the provider's query parameters and rotating
App Service header. No DNS broadening or redirect following is introduced.

The CLI uses this client only for the selected Azure VM/App Service credential.
Its service executor uses a separate ordinary client. `NativeHttpClient::new`
remains HTTPS-only and still configures the connector to enforce HTTPS.

## Transport guarantees

- Redirects are returned as responses, never followed.
- Transport-level request retries are disabled. The shared HTTP executor owns
  any GET retry policy.
- HTTPS uses HTTP/1.1 and TLS 1.2/1.3. HTTP/2, HTTP/3, TLS early data and client
  certificates are not enabled.
- Empty byte-body POSTs explicitly send `Content-Length: 0` when the caller
  supplies neither Content-Length nor Transfer-Encoding. Zero body bytes remain
  distinct from an explicit JSON `{}`; read methods, streamed bodies and
  caller-supplied framing are not changed by this policy.
- Socket timeouts are separate from the shared executor's overall deadline.
- Responses preserve actual status and initial headers. Cookies are not managed.
- Pull-driven gzip and zlib-wrapped deflate decoding retains backpressure and
  bounded chunks. Unsupported encodings, malformed bodies and trailers fail
  explicitly.

Platform certificate stores are used for trust. Additional roots do not conceal
store-loading failures. Proxy selection is explicit rather than discovered from
process environment.

Errors are sanitized and exclude URLs, queries, tokens, bodies, proxy credentials
and provider error chains. Finite request/response allocation remains enforced
by the composed shared executor.

Typed upstream I/O failures remain `Io`; upstream data-conversion/other failures
retain their non-I/O classification. Decoder-origin gzip/zlib corruption and
clean compressed truncation are `DataConversion`, not transport-recovery
evidence. Hyper parse errors are distinct from typed socket failures and
premature message EOF. Unknown, TLS and unproven user failures are not
automatically labelled I/O.

Existing Livy routes and existing Arrow Accept media additionally reject
duplicate singleton format/handle metadata before Azure `Headers` combines it.
Generic response conversion and legitimate repeated non-singleton headers are
unchanged. This is not a guarantee against raw duplicates erased by an earlier
parser or a custom HTTP client. See
[native streaming provenance and metadata limits](../../../../authoring-docs/native-stream-http.md).

## Diagnostics and HTTP 411

The Rust `fab-rs` executable currently provides sanitized command error receipts,
not a configured HTTP logger or tracing subscriber. `get --verbose` controls
resource output, not transport diagnostics. The Python CLI's `debug_enabled`
logging is separate; setting it or `RUST_LOG` does not enable native Rust request
capture.

For HTTP 411 (Length Required), capture the actual native wire framing rather
than only the SDK request object. The offline TLS regression uses synthetic
credentials and a loopback peer that rejects requests without Content-Length:

```text
cargo test --offline --locked -p fabric-sdk-transport --features hyper-rustls --lib framing_tests -- --nocapture
```

Its JSON diagnostic allowlists the method, logical route, body byte count,
Content-Length, Transfer-Encoding, HTTP status and request count. It excludes
URLs, target identifiers, authorization, cookies, request data and response
bodies. This is test-only evidence, not live service acceptance or a production
logging facility. The length-rejection case also checks that a POST is not
replayed.

For a future operational logging surface, use structured `tracing` events and
async spans, with subscriber initialization and a bounded, opt-in file sink owned
by the executable. Libraries must not initialize a global logger. Allowlist
method, framing, byte counts, status, elapsed time and sanitized failure classes;
do not dump HTTP requests or provider errors. A separate log sink preserves
stdout results and the single JSON error envelope on stderr. Correlation IDs
need explicit bounded validation before inclusion. Logging setup, redaction,
file-write failures and retention require their own contracts and coverage.

## Offline composition

The `sdk_consumer` example uses synthetic credentials and cancels before network
access:

```text
cargo run --offline --locked -p fabric-sdk-transport --features hyper-rustls --example sdk_consumer
```
