<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native HTTP and TLS transport

`fabric-sdk-transport` supplies `TokioTimer` and an optional Azure-compatible
`NativeHttpClient`. It is not a service client, identity provider, retry engine,
pager or operation poller.

There are no default features. Enable `hyper-rustls` to select the native HTTP
backend. The SDK facade and focused service crates do not select it implicitly.

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

## Transport guarantees

- Redirects are returned as responses, never followed.
- Transport-level request retries are disabled. The shared HTTP executor owns
  any GET retry policy.
- HTTPS uses HTTP/1.1 and TLS 1.2/1.3. HTTP/2, HTTP/3, TLS early data and client
  certificates are not enabled.
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

## Offline composition

The `sdk_consumer` example uses synthetic credentials and cancels before network
access:

```text
cargo run --offline --locked -p fabric-sdk-transport --features hyper-rustls --example sdk_consumer
```
