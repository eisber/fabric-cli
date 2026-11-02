<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog and raw API command consumers

Source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. These are real library
consumers, not executable/parser registration or successful live commands.
No Python/az/azd production bridge, ambient identity or runtime is created.

| Surface | Implemented behavior / evidence |
| --- | --- |
| `find` CLI | Catalog POST, source page size 1000, all-page collection, type equality/negation and canonicalization, short/long projection, description-column presence, hierarchy/flat workspace fields, query filtering/scalar-empty behavior, source progress/output |
| `find` interactive | Page size 30, output before injected continuation prompt, cumulative displayed count, empty-page automatic advancement, EOF/decline and finite cycle/page/row bounds |
| Hidden SDK reads | Connections, gateways, domains, connection types, Spark pools, managed private endpoints, per-item external shares, domain workspace membership and workspace identity metadata |
| `api` | Five methods, explicit origin/audience, JSON/local-file/multipart input, source parameter/header parsing, URI-query precedence, status/headers/text output, whitespace-empty body, truthy query replacement and falsey fallback |
| Execution | Real SDK -> transport-neutral API -> existing shared executor -> injected Azure HTTP client; no duplicate retry/TLS/LRO engine |

`REGISTRATIONS`, `FindOptions`, `find`, `FindOutput`, `find_interactive`,
`ApiOptions`, `ApiInput`, `read_api_input`, `build_api_request` and `api` form the
typed front-door integration surface. The separate CLI owner retains argument
cardinality/aliases, path/context, text formatting, REPL/batch and binary writes.
`desc`/support matrices remain CLI-local, not catalog SDK policy.

## Source quirks preserved

Search continuation drops the original filter/search fields. Queries operate on
display dictionaries, not raw catalog entries. A scalar search projection emits
no items. A description on any pre-query row adds the description column to every
row. Negative Dashboard filtering is allowed although positive filtering rejects
that type. Repeated parameter keys preserve source dictionary insertion order.

Raw API uses `raw_response=True`: a 403 response is status/text **data** with the
source success envelope, not a claim the operation succeeded. Raw requests do not
implicitly paginate or poll 202 responses. Empty query arrays/objects retain the
original raw payload. Power BI deliberately uses the pinned **Fabric scope**,
with the fixed `https://api.powerbi.com/v1.0/myorg` prefix.

## Explicit safety/integration boundaries

Only fixed public Fabric, OneLake DFS, ARM and Power BI origins are representable.
Sovereign/private/custom hosts, URL credentials/fragments, route traversal,
non-HTTPS/non-443 targets and auth/host/proxy/hop-header overrides are rejected.
Request URL/body/header bytes and header counts are bounded before credentials.
This intentionally restricts Python's custom host/environment/header behavior.
Missing API input files fail rather than accidentally sending a bodyless write.
Multipart filenames/boundaries are validated against header injection; the host
supplies the boundary explicitly. Dynamic Python/OS user-agent construction is
not fabricated by the SDK and remains host-owned.

The exact released `e850b32` `execute_bound` adapter now executes all four fixed
origin/audience pairs and validated custom headers using the existing engine.
All origin/method combinations and multipart framing reach the injected Azure
HTTP client in actual consumer tests; scope mismatches and credential-header
overrides fail before token acquisition. Cookie and Set-Cookie are also forbidden.
This is offline wire evidence, not live account/service acceptance.

`HiddenResource`/`CatalogClient::get_hidden` provide source singleton routes.
`get_metadata` reads primary Domain/Connection/Gateway metadata; only Domain adds
the complete `domainWorkspaces` collection, in source order. Optional service and
shape errors preserve source field omission and remain in `HiddenMetadata`
diagnostics. Cancellation, identity, transport, unsafe continuation and finite
limits abort explicitly, following existing workspace metadata safety.
`supported_connection_types(Some(gateway_id), ...)` retains the source gatewayId
query on every page. No alternate gateway or hidden collection route is guessed.

CLI `find_hidden_name` and `resolve_hidden_name` preserve exact case-sensitive
first full-suffix name matching after trimming boundary slashes; the latter uses
the existing complete catalog route, not a shadow request/cache. `domain_name_by_id`
returns the first display name or source empty string for missing ID/match.
`read_hidden_metadata` returns internal ordered metadata plus optional GET
query/default path-discovery output; verbose=false suppresses only output.
No new context model, parser, global state or managed mutation writer is added.

`fabric-core-json` from exact released `e218c98` is the single shared request and
merged ACL serializer. Actual request bytes, not only parsed bodies, are checked
against Python ASCII escaping, spacing, insertion order and numeric fixtures.

Source-invalid JSON/schema, unsafe continuation, finite bounds and cancellation
remain typed failures, never successful empty defaults. Catalog errors preserve
source raw error-code/message handling separately from normal Fabric HTTP errors.
Live validation is unexecuted: exact tenant/client/resource allowlists and
disposable mutation/cleanup scope have not been provided. Native full/CI
integration remains the coordinator's gate.
