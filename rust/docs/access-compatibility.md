<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access and resource-command library contract

Python authority: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
This is additive library composition, not binary registration or live parity.
The separately owned front door supplies explicitly resolved targets and
configuration; no runtime, identity backend or second parser is created.

| Leaf/resource | Route / implemented consumer |
| --- | --- |
| ACL LS/GET workspace | `GET /v1/workspaces/{id}/roleAssignments`, `value` |
| ACL LS/GET item | `GET /v1/admin/workspaces/{ws}/items/{id}/users`, `accessDetails`; admin warning |
| ACL LS/GET gateway | `GET /v1/gateways/{id}/roleAssignments`, `value` |
| ACL LS/GET connection | `GET /v1/connections/{id}/roleAssignments`, `value` |
| ACL LS/GET Lakehouse OneLake | `GET /v1/workspaces/{ws}/items/{id}/dataAccessRoles`, `value`; security warning |
| Label list-local | Explicit local path/buffer -> `labels` -> existing JSON output; unset-config warning/help |
| ACL SET workspace/gateway/connection | GET collection, source text-substring identity check, PATCH supplied identity or POST explicit principal kinds in source order |
| ACL RM workspace/gateway/connection | GET collection, exact UPN/app ID/display name/object ID match, DELETE matched assignment ID |
| Label SET/RM supported items | Fabric admin `items/bulkSetLabels` / `items/bulkRemoveLabels`, source bulk body and Standard assignment |

`acl get` is a collection query, **not** an identity-specific endpoint.
Its missing query inventories JSON paths; `.` returns raw rows. GET suppresses
falsey projections. LS sorts transformed rows by role/permission and applies
resource-specific short/long columns; a query sees full transformed dictionaries.
Item permissions are lexicographically sorted lists, not joined strings.
OneLake long columns intentionally omit nonexistent `id`/`name`.

All service requests inject Azure `TokenCredential`, explicit account/tenant and
Fabric selection. Lakehouse data roles are Fabric requests, not storage requests.
`fabric-sdk-api` composes `HttpExecutor::execute_fabric`: existing origin checks,
GET-only retries, deadlines and streaming limits stay unchanged. The new shared
collection policy is reused by the access/catalog families. Cancellation, bounds,
malformed rows, cycles and changed continuation origins fail explicitly with
partial progress; mutations/202 are never silently completed.

`fabric-cli-resources::REGISTRATIONS`, `AclRead`, `read_acls`,
`CommandValue::write_json`, `write_error`, `list_local_labels` and
`labels_from_bytes` are the typed integration surface. JSON writes use caller
time/streams and existing output/query crates. Warnings are returned for the
host logger; diagnostics go to stderr. No global config or ambient account is read.

The caller must enforce Python's support matrix before constructing a resource:
ACL reads exclude SemanticModel/Environment/Dashboard; only Lakehouse supports
OneLake ACLs. Mutation support is narrower than read support. Hidden-resource
resolution is not provided by the current context crate. `acl_resource` performs the existing resolved workspace/item exclusions before
service I/O; hidden and OneLake targets remain explicit host inputs.
`read_named_acls` preserves the source OneLake `BadRequest` to
`UniversalSecurityDisabled` message/guidance using required display metadata.
Warnings are retained even when SDK/query/projection fails. The source handles
that OneLake failure inside the handler; `CommandError::handled` and `write_error`
now preserve its zero exit with a Failure JSON envelope. The binary front door
must consume this result rather than replace it with generic failure exit 1.

`AclExportSink`/`export_acls` provide the real SDK/query-to-storage integration
boundary for GET `-o`, including falsey projections that stdout would suppress.
No default successful no-op is installed. The host/transfer owner still supplies
the actual local/OneLake writer, timestamp/naming and cleanup; these tests verify
source export intents, not filesystem/OneLake write parity. Family text output
now uses the released `fabric-cli-output::text` implementation through
`render_family_output`/`render_family_error`. `FamilySurface` selects ACL table
headers, ordinary GET, local labels, mutations, FIND and raw API routing;
`FamilyPresentation` supplies caller-owned format/width/Unicode/presentation.
No second table/JSON/error formatter or terminal discovery is introduced.
The central host still must call this family API; it is not a competing parser.
The binary is unchanged. Live validation is unexecuted until exact
tenant/client/audience/resource and disposable cleanup scope are supplied.

ACL mutation roles are validated case-insensitively but the input case is sent.
Confirmation is injected and asynchronous, including overwrite confirmation.
Declining returns source-silent success without writes. Label names are exact,
case-sensitive local matches; both label commands require a configured path,
but removal does not open it. Warehouse/SQLEndpoint label commands are rejected.
Source oddities are preserved: ACL SET searches the serialized response (including
nonprincipal fields) and PATCHes the supplied identity, whereas RM resolves the
assignment ID. These are not silently redesigned.

One safety difference is deliberate and explicit: source SET broadly catches
every failed principal attempt. The port advances only after a fully received
HTTP 400 rejection. Authorization, transport, cancellation, timeout, 5xx and
accepted/incomplete failures stop immediately; an uncertain write is not replayed.
202 responses now use the released shared Python synchronous operation policy:
poll when actual operation headers exist, fetch advertised results, retain the
source body for no-result completion, and preserve failure/cancellation evidence.
Headerless 202 remains explicitly incomplete rather than successful. Completed
204 update/remove/label responses preserve source silence (no fabricated success
message); non-successful principal-add attempts are not replayed. Diagnostics and
warnings preceding failure remain with the error. No live writes were performed.

The additive edge oracle preserves duplicate first-match removal, source text
substring false positives, empty-role validation and first local-label match.
Label SET deliberately differs from list-local for a missing `labels` key:
source SET treats it as an empty list and reports label-ID not found, whereas
list-local reports invalid entries. Malformed principal rows remain recorded
Python `KeyError`/`AttributeError` observations; Rust reports typed
`InvalidEntriesFormat` without writing or claiming identity-not-found.
Request bodies and merged serialized ACL text use the released shared
`fabric-core-json` Python-default encoder, not compact serde bytes. Exact Unicode,
spaces, numeric and escaped-substring cases are independent source expectations.
The formerly nested non-GUID operation safety test is now a registered test.
The source accepts non-GUID operation IDs; the shared operation policy rejects
them with typed uncertainty before polling. That exception remains separate from
valid-GUID source completion fixtures.

Local label definition input is bounded to 4 MiB, including the caller-buffer
path; excessive input is explicit failure, not truncation/default data.

See [independent access fixtures](../tests/compatibility/access/README.md).

The additive family output oracle executes original ACL/label/FIND/raw handlers
and the actual Python UI at a fixed timestamp and terminal width. Sixty-four
JSON/text cases preserve stream bytes and interleaving. Native consumers execute
the real SDK, existing HttpExecutor and shared renderer, not a replacement
service mock layer. Warnings remain logger observations. Source-silent empty ACL
lists and declined mutations remain silent; empty configured label output still
takes the source renderer's empty-data branch. The shared Unicode property
boundary remains explicit: even the ellipsis generated by truncation requires
the caller's correct width (Python East Asian Ambiguous is one), not an implicit
approximation or suppressed missing-width error.
