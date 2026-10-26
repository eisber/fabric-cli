<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog and hidden reads

`CatalogClient` accepts the transport-free `ApiClient`. `SearchOptions` builds
actual `POST /v1/catalog/search` requests. `search` reuses the bounded shared
collection pager; `search_page` gives a caller-owned interactive host one page.
Continuation bodies contain only `continuationToken` and the original `pageSize`,
not the initial search/filter. Catalog raw service errors retain the source body
code/message rather than substituting ordinary HTTP authorization semantics.

`HiddenCollection` provides connection/gateway/domain/connection-type, workspace
Spark-pool/private-endpoint, item external-share and domain-workspace reads.
Domains use `domains`; other collections use `value`. `workspace_identity`
extracts the optional identity from a real workspace GET, not a made-up identity
collection. These APIs do not mutate generic resources or own `desc`/support policy.

No HTTP, facade, CLI, identity, deployment or runtime dependency is introduced.
All execution/cancellation uses the shared injected API boundary. Tests exercise
the actual shared HTTP executor and independently captured pinned Python routes.
See [catalog and raw compatibility](../../docs/catalog-api-compatibility.md).

`HiddenResource` and `get_hidden` add actual singleton routes for Domain,
Connection, Gateway, Spark pool, private endpoint and external share.
`get_metadata` adds Domain's complete workspace membership in source order;
optional service/shape omissions remain typed `MetadataOmission` records instead
of fabricated empty fields. Identity/execution/cancellation/safety failures abort.
`supported_connection_types` accepts the source optional gatewayId query and
uses the same original-byte `TokenPager` for every page.

`with_query_dispatch_trace` opts the composed API client into a caller-owned
`QueryDispatchTrace`. `list_hidden_with_query` accepts explicit initial pairs
without changing `list_hidden` or mutating the supplied pairs. The trace reports
the last request actually handed to the executor, not `Collection::next_token`;
query lifetime/carry remains a command-specific compatibility caller decision.
See [the source lifetime and dispatch contract](../../docs/query-dispatch-compatibility.md).
