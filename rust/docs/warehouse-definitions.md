<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Warehouse definition retrieval

The SDK retrieves one Warehouse's complete native definition. This is separate
from the five [Warehouse management reads](warehouse-management-reads.md):
`getDefinition` uses **POST**, requires read **and write** permission, and can
return an accepted long-running operation instead of completed data. It does not
deploy, build, extract, validate SQL semantics or connect to a database.

## SQL project and DACPAC

Warehouse defaults to `sqlproj`; this is intentionally different from the
SQLDatabase codec's `dacpac` default.

| Native format | What the response represents | What it does not establish |
|---|---|---|
| `sqlproj` | One `.sqlproj` project plus ordered SQL object files, optional `.sharedqueries/*.sql`, and optional root `.platform` | A built project, valid SQL/model, or an executable/deployable package |
| `dacpac` | One opaque binary `.dacpac` database model and optional root `.platform` | Table rows, a BACPAC/data backup, complete settings/ACLs/labels, or round-trip equivalence |

Building a SQL project into a DACPAC is a separate tool-driven operation. Shared
queries are editor assets excluded from the built SQL model. The SDK preserves
those files but never runs .NET, MSBuild, SqlPackage, SQL, external references or
project imports. `.platform` is opaque native content, not a copied sensitivity
label or verified metadata schema.

The conceptual article calls the package `sqldw.dacpac`; the endpoint example
uses `definition.dacpac`. Neither is a mandatory filename. Safe received names,
nesting, part order, exact canonical Base64 and binary bytes are retained without
renaming, newline/BOM conversion, sorting or text decoding.

## Public SDK surface

The existing `fabric-sdk-sql-database` package exports:

```rust,ignore
use fabric_sdk_sql_database::warehouse::{
    WarehouseDefinitionFormat, WarehouseRequests, WarehouseTarget,
};
use fabric_sdk_sql_definition::DefinitionLimits;

// target has strict native workspace/Warehouse GUIDs and exact "Warehouse" type.
// All six limits are explicit caller-selected nonzero inclusive bounds.
let request = WarehouseRequests::get_definition(
    &target,
    Some(WarehouseDefinitionFormat::Dacpac),
    limits,
)?;
```

`None` omits `format` and resolves the effective format to `Sqlproj`.
`Some(Sqlproj)` or `Some(Dacpac)` sends exactly `?format=sqlproj` or
`?format=dacpac`. This is a native representation selector, **not** an output
serializer. The request has no body and uses `CompletionPolicy::ReturnAccepted`.
It does not change `WarehouseRead::ALL` or any CLI grammar.

The existing facade exposes:

```rust,ignore
fabric_sdk::warehouse::read_definition(
    &client,          // immutable explicit identity/endpoint RequestContext
    &executor,        // caller's existing OperationExecutor
    &request,         // WarehouseDefinitionRequest
    cancelled,        // caller's cancellation/deadline future
).await
```

The function dispatches one POST through `OperationSession`. It installs no
transport, authentication provider, runtime, pager, retry or observation engine.
The same scope, credential and endpoint are used; no identity fallback occurs.
The endpoint supports users, service principals and managed identities. The
request's identity support and delegated scopes describe provider documentation,
not locally verified claims or permission grants.

`WarehouseDefinitionRead::Completed(Owned<WarehouseDefinition>)` contains the
selected request owner and admitted definition. The API does not echo native
owner IDs; `Owned` does not fabricate a service identity verification.
`WarehouseTarget` rejects SQLDatabase, WarehouseSnapshot, Lakehouse, SQLEndpoint
and other owner types before request construction.

`WarehouseDefinitionRead::Accepted` retains the selected owner, effective format
and actual optional operation ID. It contains no completed definition.
`accepted_handle()` returns the existing shared `ReadHandle::Operation` only
when a genuine validated ID was received. Headerless acceptance stays incomplete
without a usable handle; the SDK never fabricates one or resubmits the POST.

Use the **existing shared observer** with the same `Client` and handle for
readiness and an explicitly requested result. Its selected-origin/exact-result
advertisement checks and request/byte/delay/cancellation bounds remain unchanged.
When waiting is false, running work reports `ResultNotReady`; HTTP delivery alone
does not establish completed work. Pass the successful authoritative result bytes
to `request.decode_completed_result(&result.bytes)`. That method does no I/O and
cannot itself prove readiness, endpoint/identity provenance or handle matching.
Do not pass unrelated result bytes, status monitors or metadata as source.

## Immutable content and bounded evidence

`fabric-sdk-sql-definition::warehouse::parse_warehouse_definition_response`
provides the pure content boundary. `WarehouseDefinition` exposes only immutable
views:

- `format()` and `declared_format()` separate the effective format from omission.
- `parts()` preserves exact paths and decoded opaque bytes in native order.
- `media_types()` distinguishes missing, null and exact advertised strings.
  Empty and future MIME strings stay data; no MIME type is guessed.
- `response_bytes()` is the **complete original response**, including original
  Base64 strings, format omission, JSON bytes, media and unknown fields.
  `has_unknown_fields()` signals retained future envelope/definition/part fields,
  not validation of their semantics.

There is no unchecked constructor, mutable part view, `Deserialize`, lossy JSON
round-trip or filesystem writer. Duplicate structural keys are rejected before
JSON normalization. Unknown fields do not repair missing required content.
Explicit null/wrong-type format is invalid shape; unknown format is
`UnsupportedFormat`; a known conflicting format is `FormatMismatch`.
Metadata/error/monitor bodies are not successful definitions. Unknown payload
encodings and unrecognized part kinds fail explicitly rather than dropping parts.

The Warehouse profile reuses shared primary-part, Base64 and portable path
admission. It requires exactly one nonempty primary `.sqlproj` or `.dacpac`,
permits empty SQL/shared-query files, and rejects mixed formats. Absolute,
traversing, backslash, reserved-device/ADS/control and trailing-dot/space paths
fail. Exact duplicates, case aliases including parent-directory aliases, and
file/directory collisions fail without normalization or automatic renaming.
These checks do not authorize a local write or establish host filesystem safety.

Every `DefinitionLimits` field is mandatory: complete response wire bytes,
aggregate decoded bytes, per-part decoded bytes, part count, UTF-8 path bytes and
path depth. The response byte limit is passed to the shared executor and checked
again before parsing. The other limits apply before excess part allocation or
Base64 decoding. Native content is returned whole or rejected, never paged,
clipped, truncated or projected to fit a limit. No numeric CLI/product defaults
are chosen. Retaining both encoded response and decoded parts is a bounded-memory
tradeoff, not streaming or zero-copy.

## Permissions and failures

The caller needs read and write permission for the Warehouse with
`Warehouse.ReadWrite.All` **or** `Item.ReadWrite.All` delegated scope.
`Warehouse.Read.All`/`Item.Read.All` do not satisfy this definition API.
Encrypted sensitivity labels can require additional usage rights; the returned
definition excludes the sensitivity label. No label-removal action is performed.

`WarehouseDefinitionError::Operation` retains original shared failure evidence:
denial, not-supported, provider/credential failure, wrong audience/origin,
timeout, cancellation, request/byte bounds, actual native operation ID and
dispatch uncertainty. The shared engine conservatively records POST dispatch as
possibly having an effect; that is not a schema mutation or replay authority.
`UnexpectedStatus` and `Content` retain the actual bounded raw response and any
known native operation ID. Content errors carry the shared codec classification
and part ordinal where available. No complete source succeeds from partial data.
Implicit Debug/Display omit content, paths, media, provider text and handles.

There is no denial-to-metadata fallback, generic `/items` fallback, SQL endpoint
fallback, consent/login discovery, automatic POST retry or remote cancellation.
Local cancellation and timeouts do not prove rollback or remote completion.

## Sources and review scope

Implementation base: `99ce46c05f7601d3c1556b1e60e62716ae2dd35a`, containing the
merged Warehouse management-read slice. This is additive native SDK behavior,
not an unqualified Python/v1 parity claim.

- [Get Warehouse Definition](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/items/get-warehouse-definition),
  documentation commit `057e1331579cb90d4d31bf15fbd07b9cb5f22ce5`.
- [Warehouse definition](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/warehouse-definition),
  documentation commit `5fa1082b5c4192ae8792d14143ae6ca1aeb1dbac`.

Offline tests exercise the exact route/body/query/owner, 200/202 and shared
readiness/result flow, selected-host and foreign links, protected errors,
binary/media/future-field fidelity, shared multipart safety, all six limits,
cancellation, timeout and no replay/fallback. Existing SQLDatabase tests retain
their original strict profile and default. Native multi-platform integration,
combined consumer validation, independent semantic/architecture review and live
service/package round-trip proof remain distinct evidence gates. This slice
authorizes none of the live operations.
