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
