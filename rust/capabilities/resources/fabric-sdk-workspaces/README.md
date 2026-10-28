<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace SDK

`WorkspaceClient` provides workspace listing and ordered metadata lookup.
Inject a credential, explicit account/tenant selection, `WorkspaceExecutor`,
limits and cancellation. Construction performs no I/O.

`new` uses public Fabric. `with_context(RequestContext)` retains the supplied
endpoint and original credential for listing, primary lookup, optional
enrichments and active continuations.

## Listing

Rows preserve service order, duplicates and unknown workspace types.
Page, item and response limits are explicit. Unsafe continuations, malformed
responses and later-page failures retain partial progress rather than returning
complete-looking results.

## Metadata

`get_workspace_metadata` reads the primary workspace and enriches it with
managed private endpoints, Spark settings and role assignments.
`WorkspaceMetadataResult` keeps the ordered object and typed optional
omissions. Credential, execution, cancellation and limit failures abort.

`enrich_workspace_metadata` accepts validated, completed primary metadata to
avoid repeating the initial GET. It does not interpret acceptance or an operation
handle as completed metadata.

## Composition

The SDK facade reexports the same wire/error types and delegates using the same
credential, selection and executor. The focused crate does not depend on the
facade, identity implementation, deployment or CLI.

Executors own transport, bounded streaming, retries and deadlines. No runtime,
global logger or ambient credential discovery is installed.
