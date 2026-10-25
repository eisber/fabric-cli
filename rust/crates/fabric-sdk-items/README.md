<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Handwritten item LIST and GET SDK

`fabric-sdk-items` implements generic workspace item listing and single-item
lookup, plus additive source-routed metadata GET. It is a real SDK-layer crate with no facade, identity implementation,
deployment, CLI, Python runtime or HTTP/TLS backend dependency. Callers inject
`Arc<dyn azure_core::credentials::TokenCredential>`, explicit
`CredentialSelection`, an `ItemExecutor` and cancellation. Construction performs
no I/O. The caller must bind its credential to the selected account and tenant.

Use `ItemClient::list_items` and `ItemClient::get_item` directly, or the additive
`fabric_sdk::Client::{list_items,get_item}` facade methods. For ordered metadata,
use `ItemClient::get_item_metadata` for the unchanged generic route, or
`get_item_metadata_from_collection` with finite `ItemCollection` for specialized
metadata. Both are also available through the facade. The facade forwards
the same credential and selection. Existing workspace imports and Notebook
executor signatures are unchanged. `fabric-sdk-http::HttpExecutor` implements
`ItemExecutor` in a separate module through its **private shared `execute`**
engine, preserving standard headers, streaming limits, deadlines and GET
retries. No copied HTTP policy, transport selection or mutation replay is added.

## Python source and consumers

Reference: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; the source in this checkout is unchanged.
SHA-256 is computed over LF-normalized complete source files:

| Source under `src/fabric_cli` | SHA-256 |
| --- | --- |
| `client/fab_api_workspace.py` | `c2512eec29b12b60a29e013ff0a296833f86e59904c76418ef406e903eb49ab8` |
| `client/fab_api_item.py` | `d0f257eb327f759bbdc468a8f9be1af4cce0116e7f4ee6f8e2536c10ca36d740` |
| `client/fab_api_client.py` | `d7d978f315921ec4559a2fcefc992435152758b74470d1d969006def088e8e0d` |

The [independent oracle](../../tests/compatibility/items/README.md) pins all seven
executed files and six inspected consumers. It runs actual source AST definitions
behind fake transport/auth/context and a no-I/O guard, not a second Python
implementation of the algorithm.

| Source boundary | Implemented Rust contract |
| --- | --- |
| `fab_api_workspace.ls_workspace_items(args, workspace_id)` | GET `/v1/workspaces/{workspace_id}/items` |
| `fab_api_item.get_item(args)` | GET `/v1/workspaces/{workspace_id}/items/{item_id}` |
| `fab_item_util.get_item_with_definition(..., obtain_definition=False)` | `ItemCollection` selects the exact `format_mapping` segment; ordered metadata through the same GET policies |
| `fab_api_client.do_request` | Fabric default scope, JSON Content-Type, caller query dictionary semantics and status-specific error-code precedence |
| `_handle_successful_response` | Opaque token traversal; header presence overrides body, including empty header suppression; URI alone does not start paging |
| `ApiResponse.append_text` | SDK rows retain page order/duplicates; actual `pages_received` replaces no raw Python envelope metadata |

`fab_mem_store._get_workspace_items_from_api` consumes LIST and separately
constructs CLI items, resolves folders and suppresses unsupported CLI types.
`fab_item_util.get_item_with_definition` and filesystem GET consumers can select
specialized collections, definitions or metadata aggregation. Only the finite
metadata route selection is added; definitions and augmentation are **not**
silently moved into the SDK. Existing generic and Notebook APIs are unchanged.

`ItemCollection::from_item_type` maps the 44 pinned types case-insensitively
to their exact source segments, including `GraphQuerySets`, `graphqlapis` and
`variablelibraries`. Unknown types return `None`, never a generic route.
The enum cannot contain caller-supplied URLs, collection paths or origins.
The helper supports routes that the separate Python filesystem GET policy
rejects: the CLI continues rejecting those 13 types before credentials/HTTP.
See [specialized GET compatibility](../../docs/specialized-get-compatibility.md).

## Data, query and error contracts

`Item` requires string `id` and `displayName`, not a UUID or known type.
`additional_fields` preserves all other JSON, including absent versus null
`type`, future strings, non-string types, description, folder and nested fields.
`item_type()` is only a convenience string accessor. Rows are not sorted,
deduplicated, case-folded, Unicode-normalized, or locally type-filtered.

`Item` retains its public fields, struct-literal construction and
`BTreeMap<String, Value>` extras. Its serialization still emits `id`,
`displayName`, then sorted extra keys. Existing `get_item` and `list_items`
behavior is unchanged.

The additive `ItemMetadata` instead retains the complete insertion-ordered
parsed HTTP object, including known-field positions. It validates the same
required string fields, offers `id()`, `display_name()`, `item_type()`,
`as_value()` and consuming `into_value()`, and serializes without moving fields.
Missing/null/non-string `type`, unknown fields, nested ordering, integer versus
float values and Unicode are retained. Its `Debug` redacts all metadata.
Both GET representations share the same private GET decoder and existing read
machinery; there is no second credential/request/retry path.

This is parsed-JSON fidelity, not byte-for-byte HTTP-body preservation:
whitespace, escapes and numeric token spellings may differ when serialized.
For duplicate JSON keys, the existing Python `json.loads` and ordered
`serde_json::Value` parsers retain the last value at the first key position;
additive synthetic cases exercise those actual parser semantics, including
nested duplicates. No arbitrary duplicate-key or non-finite-number contract is
introduced. Empty string IDs/names in response data remain valid strings just
as for `Item`; request identifiers still use the existing route safety checks.

Options carry ordered string query pairs. Duplicate names replace values in
the original key position, matching a Python dictionary. A `type=Notebook`
query is forwarded, not enforced locally. Continuation replaces any initially
supplied token in place. Encoding matches Python's UTF-8 `urlencode`/`quote_plus`,
including space as `+`, literal `+` as `%2B`, `*` as `%2A` and unescaped `~`.
The WHATWG form encoder differs on the last two, so the small endpoint query
encoder is explicitly covered by a separate source-executed punctuation golden.

401/403 use the first truthy public/secondary error header, falling back to
`Unauthorized`/`Forbidden`. 404 is always `NotFound`. Other statuses use the
top-level string body `errorCode` or `UnknownError`. Valid `FabricServiceError`
data and original error headers are retained, but not interpolated into Display.
Provider errors are classified as `Credential`, without diagnostic text or
fallback. No error returns a successful empty result.

`Page<Item>` and `FabricServiceError` are the existing workspace SDK types,
not copies. Its actual pager is private and workspace-specific; reusing it
would require changing the separately owned workspace source/interface.
This crate therefore keeps one item-specific traversal, sharing its read,
query, cancellation and error helpers between LIST and GET. No speculative
common paging crate or core-layer redesign is introduced.

## Explicit safety and typed-response exceptions

These are deliberate Rust requirements, **not Python quirks or full parity**:

- Default nonzero bounds are 100 pages, 100,000 accumulated rows (duplicates
  count), and 4 MiB per response. Exactly the limit succeeds when terminal;
  another page/row/byte fails. A rejected page is not partially appended.
- LIST returns `ItemListError { kind, partial }` on failure, with committed full
  pages and the next unconsumed token when known. Partial data is never a complete
  success. The source instead recursively raises without partial results.
- Active supplied continuation URIs must use the public HTTPS Fabric origin,
  default port, no user information/fragment, and the exact encoded collection
  path. Wrong paths, dot-segment normalization and foreign origins fail; the URI
  is never followed. Terminal unused URI strings are ignored. Python ignores
  all continuation URIs. Two unchanged comparable goldens intentionally expect
  Rust `InvalidContinuation` instead of following their foreign-URI tokens.
- Repeated tokens, including an initial token, fail. Cancellation is checked
  before credentials, during credentials/execution and between pages; a fair
  yield between ready pages avoids starving the caller's runtime. Cancellation
  wins simultaneous readiness and drops in-flight local futures.
- Identifiers remain opaque strings, but empty/dot/dot-dot, control characters,
  separators and query/fragment injection are rejected before authentication.
  Other segment bytes are URL-encoded; literal `%2e%2e` is double-escaped as
  literal identifier data, not interpreted as traversal.
- Initial 200/201 success is supported. A followed 201 is an explicit incomplete
  `UnsupportedStatus(201)`: Python fetches then silently discards that page.
  202/LRO and 204 are unsupported completed typed responses.
- Invalid JSON, missing/null/non-array `value`, invalid required item fields
  and non-string continuation values fail. Python's API boundary can return
  these raw values and fail later in consumers. GET with truthy continuation
  fails instead of aggregating arbitrary list-valued object properties.
- SDK output is typed rows, not Python's raw envelope, stale `content`/headers,
  recursive metadata merge or observed `total_pages: 1` bug.

## Offline evidence and remaining gaps

The [item matrix](../../docs/item-compatibility.md) records the exact gates and
commands. Independent source fixtures include 30 core comparisons, 26 separately
labeled Python-only quirks, and one additive punctuation case. Rust checks
28 core cases exactly, two explicit origin exceptions, the punctuation case,
safe multi-page traversal, malformed pages, paths, service errors, cancellation,
limits at exact defaults and +1, and standalone/facade consumers. Shared-adapter
tests execute GET retries, headers, byte limits, deadlines and exhaustion through
the public facade; existing workspace/Notebook adapter tests remain intact.
Standalone and facade consumers also exercise ordered metadata and old `Item`
struct literals. The real CLI read host now consumes this boundary before
native query/path discovery. Its source-routed comparator asserts all 25 original
command outcomes as 22 exact and three unchanged safety differences, including
all six previously generic-versus-specialized URI differences. Historical
generic/typed/no-projection modes remain explicit and retain their old evidence.
An additive hand-authored route inventory checks all 44 actual source helper
routes and 31 CLI-supported routes at 200/401/403/404; direct/facade consumers
also retain generic API and struct-literal compatibility.
See the [command oracle](../../tests/compatibility/commands/README.md).

Not implemented: arbitrary `item_uri`/`ext_uri`, definitions, create/update/delete,
all item-specific service families, CLI projection/cache/folder resolution,
Python arbitrary GET pagination, private links/non-Fabric audiences, live schema
or credential claims. The [HTTP adapter limits](../fabric-sdk-http/README.md)
still apply, including caller-configured transport/decompression/trust,
unsupported HTTP-date Retry-After and no production HTTP/TLS factory.
No live authentication/service/proxy calls or release/publication are involved.
