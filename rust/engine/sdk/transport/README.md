<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# SDK transport compatibility

`fabric-sdk-transport` retains existing imports, `hyper-rustls` forwarding,
`TokioTimer`, the SDK consumer example and the optional `notebook-websocket`
adapter. It creates no runtime or credentials.

Generic HTTP/TLS, proxy/trust settings, decompression and its wire-level tests
live in `fabric-http-transport` under `engine/core/http/transport`. That package
has no production, dev or optional dependency on the facade or capability clients.
Its timer implements the same neutral `ExecutionTimer` contract. The local
compatibility timer also implements the Notebook deadline adapter when enabled.
