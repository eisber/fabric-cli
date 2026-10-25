<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit native HTTP/TLS transport

`fabric-sdk-transport` is an optional SDK-layer implementation of
`azure_core` 1.1.0's `HttpClient`, plus `TokioTimer` for the existing
`fabric-sdk-http::ExecutionTimer`. It is not an identity backend, CLI host,
REST client, retry engine, pager, or operation poller.

The crate has **no default features**. Without features it supplies only the
timer. Select `features = ["hyper-rustls"]` to enable `NativeHttpClient`.
Neither the SDK facade nor the focused service SDKs depend on this crate.
Adding the workspace member does not select HTTP/TLS for a direct SDK consumer.
The public [standalone consumer](examples/sdk_consumer.rs) shows composition;
it uses an expired synthetic credential and cancels before any network request.
It is not a production credential factory.

## Construction and runtime

`NativeHttpClient::new(TransportOptions)` returns `Result<_, TransportError>`.
There is no default configuration or opaque externally supplied client whose
redirect, retry, proxy, or trust settings cannot be checked. Construction
validates settings and configures trust, but does not discover credentials,
perform service I/O, create a runtime, or initialize a logger or global crypto
provider. `Clone` shares one pool.

The caller explicitly supplies:

| Setting | Contract |
| --- | --- |
| `connect_timeout` | Nonzero TCP/DNS/CONNECT/TLS establishment bound |
| `read_timeout` | Nonzero timeout for a pending socket read |
| `write_timeout` | Nonzero timeout for a pending socket write |
| `pool_idle_timeout` | Nonzero idle connection retention bound |
| `max_idle_per_host` | 1 through 1024 idle connections per origin |
| `trust` | Explicit `TrustPolicy::Platform`, optionally with additional roots |
| `proxy` | Explicit `ProxyPolicy::None` or `StaticHttp` |

All durations must be at most 24 hours; at most 128 additional roots are accepted.
`RootCertificate::from_der` validates DER up to 64 KiB per certificate.
Construction errors are typed and sanitized; there is no panic-on-build,
fallback root bundle, invalid-certificate mode, or insecure-hostname mode.

Poll requests and timer futures on the caller's Tokio runtime with I/O and time
enabled. `TokioTimer` uses `tokio::time::sleep`; dropping its future cancels the
wait. It does not create blocking threads or use a blocking sleep. The HTTP
library uses the caller runtime for connection work; the platform DNS resolver
uses Tokio's established resolver support. Cancelling local work cannot undo a
server-side mutation.

## Enforced transport settings

- Hyper's pooled client explicitly sets `retry_canceled_requests(false)`.
  There is no additional status, connection, protocol-NACK, or mutation retry.
  Only the existing `HttpExecutor` owns GET retries and Notebook polling waits.
- Hyper returns redirects as responses; there is no redirect-following layer.
  Bearer headers are never forwarded to a redirect target.
- HTTPS uses HTTP/1.1 with a 32 KiB Hyper read/header buffer and an explicit
  per-client Rustls `ring` crypto provider. TLS 1.2 and 1.3 are enabled; TLS early
  data, client certificates, HTTP/2 and HTTP/3 are not enabled.
- `hyper-timeout::TimeoutConnector` supplies the socket/connection settings.
  Its read timeout starts on a pending read and resets on successful I/O;
  successful writes restart an active read wait. These are socket settings,
  not another overall request deadline. `HttpExecutor` retains its existing
  240-second default overall deadline and operation-specific decoded byte cap.
- Request method, URL/query, headers and body are passed through. Only
  `Accept-Encoding: gzip, deflate` is added when the caller did not supply it.
  The executor remains the source of Authorization and Content-Type.
- Actual status and initial response headers, including Content-Encoding,
  Content-Length, Location, Retry-After and empty 202 responses, are retained.
  Azure normalizes header names and has one string per name: repeated field
  values are joined with `, ` in wire order, and HTTP Latin-1 octets are mapped
  without loss, matching Requests' header view. Cookies are not managed.
- Response bodies are pull-driven, not eagerly collected. The established
  `async-compression` gzip and RFC zlib-wrapped deflate decoders produce chunks
  of at most 8 KiB. Decompression does not strip or rewrite response headers.
  Backpressure and dropped readers release the source; the existing executor
  stops at its decoded limit, including a 4 MiB compressed-bomb boundary.
  No unbounded decoded allocation occurs inside the transport.

Malformed compressed bodies, unsupported encodings/encoding chains and response
trailers fail explicitly with sanitized Azure errors. Trailers cannot be added
to Azure's already-returned initial header map without buffering the body.
Brotli, Zstd and legacy raw/unwrapped deflate are not implemented or advertised.
HEAD, 204, 304 and known-empty bodies do not try to decode a nonexistent entity.
All request/stream errors discard underlying provider errors and their source
chains: Display/Debug do not contain URLs, query strings, tokens, proxy
credentials, bodies, or TLS/provider diagnostic text.

## Trust and proxy scope

On Windows and macOS, `rustls-platform-verifier` performs native certificate
validation with explicitly supplied additional roots. On Linux,
`rustls-native-certs` loads the platform roots and Rustls' established
`WebPkiServerVerifier` validates certificates. Linux construction fails on an
empty store, loader errors or invalid roots; additional roots never conceal
platform-store unavailability. Selecting `TrustPolicy::Platform` explicitly
selects the native loader's Linux `SSL_CERT_FILE`/`SSL_CERT_DIR` behavior.
Windows and Apple use their native policy mechanisms instead. No trust store
or environment variable is modified, and there is no process-global provider
installation.

`ProxyPolicy::None` never reads proxy environment variables or OS proxy settings.
`StaticHttp(StaticHttpProxy::new(...))` selects one unauthenticated HTTP proxy
origin. HTTPS uses `hyper-util`'s established CONNECT tunnel, with its bounded
8 KiB/100-header proxy response parser; Authorization is sent only inside the
validated TLS connection. There is no handwritten HTTP/TLS or proxy protocol.

System/ambient proxy discovery, PAC/WPAD, SOCKS, HTTPS proxies, proxy credentials,
integrated authentication, custom bypass lists, client-certificate
authentication, enterprise revocation/offline policy certification, sovereign
clouds and private links are outside this slice. Credential-bearing proxy URLs,
non-HTTP schemes, paths, queries and fragments are rejected, not ignored.
Only Windows, macOS and Linux desktop targets are in scope. Native trust and
proxy fixtures do not establish general enterprise or live-service support.

## Source mapping and provenance

Python reference:
`src/fabric_cli/client/fab_api_client.py` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`_get_session` and `do_request`. The reusable pooled client and gzip/deflate
advertisement correspond to the reusable Requests session. Existing Rust
executor policies retain the previously approved overall-time/decoded-byte
bounds, no redirects and no mutation replay; they do not claim Python's
per-attempt timeout or mutation-429 retry behavior. Explicit caller runtime,
trust and proxy selection replace the source's ambient session configuration.
The [shared HTTP source matrix](../fabric-sdk-http/README.md) remains authoritative
for retries, deadlines and operation-specific exceptions.

The owner approved using established Hyper/Rustls instead of the provisionally
preferred Reqwest on October 25, 2026. Reqwest 0.13.5's
`src/async_impl/client.rs` sets its Tower retry policy but does not expose
Hyper's `retry_canceled_requests` setting. Hyper-util 0.1.21 defaults that setting
to true. `reqwest::retry::never()` therefore cannot prove that **all** implicit
pooled-request recovery is disabled. This adapter sets the underlying switch
directly; no unstarted-request replay exception was introduced.

All dependency versions/features are centralized in `rust/Cargo.toml`; the
registry sources and checksums in `Cargo.lock` are authoritative. No upstream
implementation was copied into this crate.

| Released dependency | Selected features | License |
| --- | --- | --- |
| hyper 1.12.0 | client, http1 | MIT |
| hyper-util 0.1.21 | client-legacy, client-proxy, http1, tokio | MIT |
| hyper-rustls 0.27.10 | http1, ring, tls12 | Apache-2.0 OR ISC OR MIT |
| hyper-timeout 0.5.2 | no defaults | MIT OR Apache-2.0 |
| rustls 0.23.46 | std, ring, tls12 | Apache-2.0 OR ISC OR MIT |
| rustls-platform-verifier 0.7.1 | no defaults | MIT OR Apache-2.0 |
| rustls-native-certs 0.8.5 | Linux-only, no defaults | Apache-2.0 OR ISC OR MIT |
| async-compression 0.4.51 | tokio, gzip, zlib | MIT OR Apache-2.0 |
| tokio-util 0.7.20 | io | MIT |
| http 1.5.0 | std | MIT OR Apache-2.0 |
| http-body-util 0.1.5 | no defaults | MIT |
| tower-service 0.3.3 | no defaults | MIT |

Tokio stays at the existing 1.54.1 workspace version, with explicit time,
network, I/O and synchronization features. Test-only rcgen 0.14.10,
tokio-rustls 0.26.6 and flate2 1.1.10 use MIT OR Apache-2.0 licenses.
Existing Azure and other dependency versions/default-transport features remain
unchanged. `ring` needs the platform C toolchain; this is not a claim that the
entire transitive dependency closure is unsafe-free or has no native build
prerequisites. The new crate itself inherits `unsafe_code = "forbid"`.

## Offline evidence

Synthetic loopback TLS tests run the **real SDK -> existing HttpExecutor ->
native transport** for workspace pagination and Notebook empty-202 ->
Running -> Succeeded -> result retrieval. A private `cfg(test)` TCP-destination
connector seam leaves the SDK URL, TLS SNI and HTTP Host fixed to
`api.fabric.microsoft.com`; there is no public arbitrary-origin SDK bypass.
The public static-proxy API separately routes real SDK execution through a
local CONNECT/TLS fixture, without that private seam.

Other contracts cover exact request bytes/query/headers, initial repeated and
Latin-1 response headers, redirects with zero second-host connections, direct
GET status/I/O without retries, executor-only GET retry counts, POST no replay,
exact/+1 decoded 4 MiB gzip bounds, pull-driven 8 KiB decoding, invalid
compression, infinite/stalled bodies, overall deadline/cancellation/drop,
finite socket waits, untrusted certificates and wrong hostnames, concurrent
requests/pool reuse, and construction without a runtime/global initialization.
Ambient-proxy isolation uses a bounded child test process with synthetic proxy
variables, never process-global environment mutation.

No test uses an account, real token, live Fabric/login endpoint, persisted
credential, external certificate service, or OS trust-store modification.
The initial local execution is Windows x86_64; Linux/macOS native execution and
the parent's integrated feature-graph/CI gates remain separate evidence.
