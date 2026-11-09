<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# KQLDatabase metadata and context handoff

The SDK's `fabric_sdk::kql_databases` module adds typed Fabric v1 metadata GET
and one-page LIST, plus a pure Kusto endpoint/database/context descriptor. It
does not implement another query, management-command, ingestion or CLI host.
Existing Kusto table APIs and the separately owned read-query binding remain
unchanged. No dependencies, feature defaults or authentication providers change.

## Metadata reads

`KqlDatabaseRequests::get` requires an exact `ResolvedTarget` with owner
`KQLDatabase`, item facet and metadata representation, no child, and real
workspace/item UUIDs. It uses `/v1/workspaces/{workspace}/kqlDatabases/{item}`.
`list_page` uses the matching collection with explicitly supplied recursion,
root folder and actual decoded continuation token. No name guessing or new
numeric default is installed.

Requests clone the original `RequestContext` and use the shared
`OperationSession` and caller-injected executor, cancellation and limits.
The selected Fabric host is retained with the Fabric audience; no alternate
credential or endpoint is selected. HTTP 200 with JSON metadata is a completed
read. An actually executed GET 202 remains a rejected shared operation error
with HTTP status, validated observed operation ID and raw response evidence,
not a completed metadata result or a polling request. A directly supplied
accepted outcome is likewise rejected by the decoder. Invalid acceptance IDs
and foreign operation links still fail the explicit
`FabricOperation::with_metadata_read_evidence` validation selected by this
metadata adapter. Only GET/ReturnAccepted can opt in. Default observer and
result GETs keep their original observed-HTTP-202 primary error classification
and do not interpret or follow acceptance headers. The supported
nonpolling read statuses and completion policy are unchanged.
Denied/absent/throttled/transport failures retain their shared
classification, never `exists=false` or empty metadata.

Native GET/LIST roots and provided non-null properties must be JSON objects;
positional arrays cannot deserialize into native records or an empty successful
page. Known fields and concrete owner identity are validated. Missing, null and future
database kinds remain distinct; unknown native JSON fields are retained in the
raw data projection. Observed endpoint strings are inert metadata until context
resolution. Non-JSON advertised response media is rejected. The shared executor
owns streaming limits and deadlines; the decoder does not promise lexical JSON
whitespace preservation.

LIST returns a complete *page*, not a complete inventory or stable snapshot.
Service order and duplicates remain intact. Active continuation URIs must match
the original exact Fabric host/collection and token; any repeated input token
or changed scope is rejected. Consumers use the existing generic pager with
their own finite invocation bounds, not an endpoint-local loop.

## Pure Kusto descriptor

`KqlDatabase::kusto_context` requires explicit `DatabaseSelector` and
`KustoResource` choices. `MetadataDisplayName` opts into the mapping used by the
official Fabric deployment example; `ConfirmedName` retains a caller-confirmed
native name. Neither chooses the item UUID on failure or authorizes an arbitrary
cross-target switch.

The metadata-supplied parent Eventhouse, query URI and database kind must be
usable. Query and ingestion origins are validated separately; ingestion is
never derived by prepending `ingest-` or used instead of the query endpoint.
Missing/null ingestion metadata is preserved. Public HTTPS root URIs under
`*.kusto.fabric.microsoft.com` or `*.kusto.windows.net` are admitted; unsafe raw
authority syntax, paths, query/fragment, credentials and role-swapped query/
ingestion hosts are rejected before they can become destinations.

The OAuth resource is an explicit choice: documented public Kusto resource
or the exact query origin. No audience is inferred from a Microsoft suffix and
no auth-metadata probe or resource fallback occurs. The original credential Arc,
account, tenant and Fabric metadata context are retained. The returned Kusto
scope is separate from that Fabric context; resolution never acquires a token
or asserts provider equivalence, effective permission, consent, read-only
execution support or production enablement. Query owners must independently
honor those guards before using this descriptor.

`ReadWrite` is a database kind, not caller permission. `Shortcut` denotes a
database follower, not a OneLake table shortcut or a writable source. Future
kinds remain readable metadata but cannot establish a supported native context.

## Discovery and remaining scope

`register(&mut Registry)` adds only metadata GET and one-page LIST and the
type-constrained `kql-database` alias. Generic/facade resolution selects the same
operation. Input/output schemas describe this finite subset, not a native
query/input schema or authorization check. No global command grammar or
default CLI behavior is changed.

Definitions, source parts, settings, table shortcuts, mutations, query/control/
ingestion, live service acceptance and automatic parent creation are excluded.
The host-neutral `KustoContext` is the tested handoff for the query owner, not
a competing engine-binding registration. Independent complete native/stable
gates remain required before review-ready.

## Sources and offline evidence

Base: `cc183dd43140350eb1f23af0636332150d1f6d50`, approved v2 layout checkpoint.
Selected preparation root: `207a140a-9f78-4f31-931d-1bdb9803b376` revision 5.

- [GET KQLDatabase](https://learn.microsoft.com/en-us/rest/api/fabric/kqldatabase/items/get-kql-database)
  and [LIST KQLDatabases](https://learn.microsoft.com/en-us/rest/api/fabric/kqldatabase/items/list-kql-databases):
  documented native fields, casing, response types and permissions; observed
  documentation commit `3c976b94870acae6618254d693ab33c3f7c46823`
  (rechecked November 8, 2026).
- [Deploy an Eventhouse with APIs](https://learn.microsoft.com/en-us/fabric/real-time-intelligence/eventhouse-deploy-with-fabric-api):
  native database name/query-origin resource example; observed documentation
  commit `5156dc524b5f03f820d4d6b55aa69caeded0cce5`.
- [Kusto HTTPS authentication](https://learn.microsoft.com/en-us/kusto/api/rest/authentication?view=microsoft-fabric):
  explicit public Kusto resource; observed documentation commit
  `85351b30116da41b75c5a4e8a5b7f5730d81ab08`.

`sdk/client/tests/kql_metadata.rs` exercises the public SDK boundary with
synthetic metadata and caller-injected shared execution. Coverage includes exact
routes/context/aliases, wrong-owner/media/identity rejection, unknown/null
fields, explicit resource/selector choices, unsafe endpoints, genuine paging,
unexpected acceptance through the real injected executor, positional-array
rejection, denied/throttled errors and cancellation before token/send.
Shared-operation regressions retain GET 202 evidence without polling or relaxing
origin, identifier, response-bound or status guards. These offline fixtures do
not contact Fabric, Kusto or an identity provider.
