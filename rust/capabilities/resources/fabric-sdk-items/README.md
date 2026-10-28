<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Item reads and metadata

`ItemClient` provides workspace item listing, generic item lookup and
collection-specific metadata reads. The facade forwards the same operations
and credential selection.

| API | Route or representation |
| --- | --- |
| `list_items` | Workspace `/items` collection |
| `get_item` | Generic `/items/{id}` lookup returning `Item` |
| `get_item_metadata` | Generic lookup retaining ordered metadata |
| `get_item_metadata_from_collection` | Validated `ItemCollection` route |

## Context and execution

Inject a credential, explicit selection, `ItemExecutor` and cancellation.
Construction performs no I/O. `new` keeps public Fabric defaults;
`with_context(RequestContext)` binds initial and continuation requests to the
selected endpoint and Fabric audience.

`fabric-sdk-http::HttpExecutor` implements the executor through its shared
streaming, deadline and GET retry policy. The client does not install a
transport, runtime or credential discovery mechanism.

## Data contracts

`Item` requires string `id` and `displayName`; identifiers remain opaque.
Additional fields preserve missing versus null values, unknown types, nested
data and Unicode. Listing does not sort, deduplicate or filter rows locally.

`ItemMetadata` retains the complete insertion-ordered response object. It
provides accessors, `as_value` and `into_value` without moving known fields
during serialization. Debug output redacts metadata.

`ItemCollection::from_item_type` maps known item types to finite collection
segments. Unknown types return `None`; arbitrary URLs and route strings cannot
be smuggled into the enum. Command-level support policy remains separate from
the SDK route map.

## Paging and errors

Limits bound requests, rows, pages and bytes. Continuations validate origin and
route and detect cycles. Later-page failures retain partial progress and never
return a complete-looking list.

Malformed input, credential failure, service errors, unsafe links, cancellation
and execution limits remain distinct. Definition retrieval and enriched command
metadata composition are separate operations.
