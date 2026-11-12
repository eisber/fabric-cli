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

## Create policy contracts

The additive `create` module plans default-error, complete-content overwrite,
skip, and collection-scoped duplicate actions. A caller-owned schema validator
and exact target resolution feed one planner; file input does not select an
implicit update. Result guards retain native identity and operation evidence
without treating acceptance, partial results, or unknown outcomes as completion.

These are pure contracts, not Fabric endpoints or an executable command. Native
adapters must enforce the selected action and advertise its exact capabilities.
No HTTP executor, runtime, credential discovery, or SDK-to-CLI dependency is
introduced. See the [create v2 contract](../../../../authoring-docs/create-v2-contract.md) for
input, conflict, evidence, and remaining integration boundaries.

The `lakehouse_create` module adds a native metadata-only Lakehouse schema and
immutable POST request over the existing operation engine. It supports the
provider's name-conflict creation behavior and exact-existing skip, not complete
overwrite or repeated-name duplication. Raw input and native response evidence
remain separate from approval and receipt delivery. See the
[Lakehouse create boundary](../../../../authoring-docs/lakehouse-create-native.md)
for the bounded public-definition schema, service-owned SQL endpoint effect,
source revisions and executable-integration limits.

## MLModel native parameter and serving contracts

The additive `ml_models` module describes MLModel item and endpoint operations
over the existing caller-owned `ResourceOperations` boundary. The SDK facade
reexports the same module as `fabric_sdk::items::ml_models`; it does not add
another executor, credential provider, runtime, or polling engine.

Native scoring accepts the documented `inputs` array and optional
`formatType`/`orientation`, not stringified `name=value` arguments, notebook
parameters, or function-invocation wrappers. Predictions retain their native
JSON values. Input files/stdin, schema-directed table adaptation and
CSV/JSON/Parquet delivery belong to the shared hosts, not this SDK.

The module distinguishes documented user-only item APIs from endpoint APIs
supporting users, service principals and managed identities. Caller-supplied
identity metadata is a capability check, not token-claim verification.
Known-ID serving operations do not perform a user-only item lookup.

See [MLModel binding and output boundaries](../../../../authoring-docs/ml-model-native.md)
for native API provenance, acceptance/completion semantics, and integration
gates. These SDK primitives do not establish an executable v2 artifact facade
or a working shared tabular-export host.
