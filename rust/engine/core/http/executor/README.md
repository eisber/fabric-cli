<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Neutral HTTP execution

`fabric-http` executes bounded HTTP exchanges over a caller-owned Azure HTTP
client and asynchronous timer. Its request, response, execution and progress
contracts live in `fabric-core::http`. It has no production dependency on the
Fabric SDK facade, capability clients, credential providers, CLI or runtime.

The existing SDK logical layer is retained; physical placement under `core/`
does not relax the workspace layer policy. Architecture tests additionally
reject direct, optional, dev and transitive service dependencies from this engine
and `fabric-http-transport`.

The buffered policy is unchanged: one finite deadline, bounded decoded bodies,
GET-only status retries and no automatic mutation replay. Progress reports
headers, bounded partial prefixes, completion and body omission without
manufacturing a complete response after failure. The caller still owns
credential selection, cancellation, operation reconciliation and rendering.

Use `HttpExecution::execute_http(HttpRequest, progress)` for an injectable
service-neutral seam, or the existing explicit bound-origin methods. Tokens
remain opaque; the caller must acquire the selected origin's audience.
`execute_prepared` is the lower-level seam for adapters with their own fixed
endpoint policy: it does not validate an origin or acquire credentials.

Service implementations and compatible `fabric-sdk-http` imports now live in
[`engine/sdk/http`](../../../sdk/http/README.md). Their wrapper delegates to this
single engine; it does not install a second retry, deadline or streaming policy.
