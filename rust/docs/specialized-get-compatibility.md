<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Source-routed metadata GET

Source: `microsoft/fabric-cli` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Implementation base: published `eisber/fabric-cli` checkpoint
`d9df14a6acf7fe3d80adf3bb74ee10ad0a7ac92f`.
This bounded slice translates the metadata helper, **not**
`fab_fs_get_item.exec` in full.

| Source boundary | Native contract |
| --- | --- |
| `fab_types.format_mapping` | Finite SDK `ItemCollection`, all 44 explicit segments, no guessed plurals |
| `fab_api_item.get_item(item_uri=True)` | `get_item_metadata_from_collection`, fixed Fabric origin and validated opaque workspace/item IDs |
| `fab_item_util.get_item_with_definition(obtain_definition=False)` | Exact collection route and complete validated ordered parsed JSON |
| `command_support.yaml`, FS_GET | Existing 13 command rejections remain before credential/HTTP; 31 item types reach metadata GET |
| Read host / query / output | Actual SDK facade to `ReadHost` to `PythonQuery` to existing envelope/streams/exits |

The additive focused and facade method returns existing `ItemMetadata`.
`Item`, its fields and struct literals, `get_item`, `get_item_metadata` and
`list_items` are unchanged. The new method shares `get_one<T>` and `read`;
query replacement/encoding, scopes, cancellation priority, credential and
executor drop, errors, deadlines, retries, continuation rejection and body
bounds are not reimplemented. No dependencies, features, identity backend,
ambient state or production host are introduced.

## Routing and rejection are different contracts

Source collection spelling is preserved even when it looks inconsistent:
`GraphQuerySet` uses `GraphQuerySets`, `GraphQLApi` uses `graphqlapis`,
`UserDataFunction` uses `userdatafunctions`, and `VariableLibrary` uses
`variablelibraries`. The enum exposes no arbitrary URL/path constructor.
`from_item_type` returns `None` for unknown types, including a collection
segment supplied instead of a type. The CLI does not silently fall back.

All 44 mapped types have metadata-helper routes, but Dashboard,
PaginatedReport, SQLEndpoint, MirroredWarehouse, Datamart, OrgApp, MetricSet,
ApacheAirflowJob, Exploration, RetailDataManager, Healthcaredatasolution,
SustainabilityDataSolution and AISkill remain filesystem GET rejections.
The SDK does not acquire CLI types or reproduce CLI command restrictions.
Future types remain explicit failures at context/collection validation.

## Independent comparisons

`commands/cases.json`, `native-boundaries.json`, all old item fixtures and source
digests remain byte-for-byte unchanged. `--request-mode source-routed` requires
actual native requests equal `python_requests`, as well as exact stdout/stderr
bytes, exits, scopes and correlation. There is no automatic URI waiver.
The default `historical-generic` request mode still asserts the old generic
routes; historical typed-order and no-projection modes also remain explicit.
Those modes reject the current source-routed probe rather than changing
expectations to fit it.

All **25** protected actual command outcomes are executed: **22 fully exact,
three unchanged native safety differences**, zero URI-only, metadata-order or
projection failures. The four additional raw-body samples are source-exact
including requests. These preserve parsed key order, values and types, not
whitespace, escape spellings or original numeric lexemes.

`specialized-routes.json` is a separate hand-authored table, checked against
the complete pinned `format_mapping` and FS_GET rejection inventory.
`specialized_reference.py` executes **176 actual Python helper observations**
(44 collections at 200/401/403/404) and compares **124 actual native command
outcomes** for the 31 command-supported types. The remaining 13 helper routes
are not mislabeled CLI parity: native public command tests separately require
the original rejection envelope with no token or request.
Unknown source type failures, route/query injection rejection, every protocol
field, all existing historical modes and unchanged safety exceptions are
guarded separately.

Public SDK tests compile the same consumer through focused and facade imports,
checking every route, generic API compatibility, exact Python query encoding,
and invalid IDs before credentials. Shared HTTP consumers exercise actual
specialized URLs/scopes, retry, status precedence, stream cap-plus-one and
in-flight cancellation/body disposal. Existing Notebook, folder, workspace,
capacity and deployment APIs are not rewritten.

## Explicit remaining boundaries

Definitions, connections, schedules, Environment and mirrored-database
augmentation, export, full GET orchestration and arbitrary extension routes
are unported here. The binary still returns `NotConfigured` for valid reads;
no live service/identity/TLS selection is made.

The protected query denominator remains 108 exact / 118, with ten explicit
unsupported cases; this route change does not relabel those boundaries.
Local Windows targeted checks are not parent integration, full-workspace or
native Linux/macOS CI evidence. Execution receipts and exact commands belong
in the local task handoff; the integration owner owns publication and review.
