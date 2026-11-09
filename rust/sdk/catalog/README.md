<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog and metadata reads

`CatalogClient` consumes an injected `ApiClient` for catalog search and hidden
resource metadata. Execution, credentials, limits and cancellation remain with
the caller.

## Search

`SearchOptions` builds `POST /v1/catalog/search`. `search` uses the shared
collection pager; `search_page` returns one page for an interactive caller.
Continuation bodies contain only the continuation token and original page size.
Service error bodies retain their codes and messages.

### Additive v2 single-page requests

`CatalogRequest::initial(search, filter, page_size)` accepts search-only,
filter-only, or combined requests, but rejects blank supplied search and an
entirely unconstrained request. `page_size: Option<usize>` defaults to 50 and
must be 1..=1000. Immutable variant payloads expose read-only accessors.

`CatalogFilter::new(include_types, exclude_types, workspace_ids)` accepts owned
string vectors. Includes are OR-ed, excludes are AND-ed, and workspaces are
OR-ed; the three groups are combined with AND. There is no raw-filter input.
Up to 500 total include/exclude type values are accepted, each nonblank and at
most 50 Unicode scalar values, without quotes or control characters. Unknown
type names are sent unchanged rather than restricted to a local allowlist.
Up to 12 workspace IDs must use exact 8-4-4-4-12 hexadecimal GUID syntax.
An empty `CatalogFilter::default()` is valid with nonblank search.

`CatalogRequest::resume(token)` accepts a nonblank opaque token and preserves
its contents exactly. Its POST body contains **only** `continuationToken`;
search, filter and page size cannot be added to a resume request.
This intentionally differs from legacy `SearchOptions` continuation bodies.

`CatalogClient::search_v2_page(&request, byte_limit, cancelled)` uses the same
injected API client, bounded execution, cancellation and service-error decoder.
It returns one `SearchPage`, preserving raw rows, unknown type values, service
order and the next token, including on an empty page. It does not follow
continuation URLs, fetch additional pages, hydrate hits or compare scores.
Callers must enforce cross-page request/row/cycle bounds. Results cover only
the caller-authorized, indexed service scope; a terminal page or zero matches
does not establish tenant-wide completeness.

The public Catalog API is Preview. Its documented contract is pinned to
[REST documentation revision `5df184c567fafea7284357607ebac79d6301a698`](https://github.com/MicrosoftDocs/fabric-rest-apis-docs/blob/5df184c567fafea7284357607ebac79d6301a698/docs-ref-autogen/fabric/core/Catalog/Search.yml).
The adapter does not acquire consent, discover credentials, or extend resource
permissions. Existing `SearchOptions`, `search`, `search_page` and legacy
`find` behavior remain unchanged.

## Metadata

`HiddenCollection` covers connections, gateways, domains, connection types,
Spark pools, private endpoints, external shares and domain workspaces.
`HiddenResource` and `get_hidden` address singleton resources.

`get_metadata` enriches Domain metadata with workspace membership.
`workspace_identity` reads the optional identity from workspace metadata.
Optional omissions are represented by `MetadataOmission`; credential,
execution, cancellation and limit failures abort rather than returning
apparently complete metadata.

`supported_connection_types` accepts an optional gateway ID and uses the same
pager. `list_hidden_with_query` accepts ordered initial query pairs.
An optional `QueryDispatchTrace` observes actual dispatched requests without
automatically carrying query state between commands.

The crate installs no HTTP backend, CLI, runtime, global configuration or
identity implementation.

## Domain Contributor synchronization

The additive [`service`](src/service.rs) module binds one documented admin action
to caller-resolved Domain/tenant identity, immediate approval and shared native
execution. It performs no catalog discovery. See [the service contract](../../docs/service-domain-sync.md)
for grouped syntax, permissions, receipt semantics and explicit unsupported scope.
