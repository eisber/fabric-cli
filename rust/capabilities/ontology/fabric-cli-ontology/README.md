<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Ontology capability adapter

This CLI-layer package implements the independently testable local portion of
the selected seven-alias Ontology facade: create, delete, get, link, list, move
and update, plus actual public metadata Create/Patch/Delete, SDK Get,
representation replacement and typed generation-2 native entity execution
through caller-injected hosts. It does **not** install a global registry, path
resolver, production host, SDK engine or MCP server, or implement the complete
Ontology feature.

`ontology_command` composes `operation_command` for each selected common verb.
`parse_common_args` and `parse_facade_args` use that same schema and produce
identical local arguments/errors. No second operation identity or specialist
alias is introduced. Parsed paths remain opaque until the accepted shared
resolver supplies canonical IDs. Local input/output are separate typed options;
parsing never opens a file, obtains credentials or dispatches remotely.

`bindings` and `select_binding` describe source-backed **proposals**, not enabled
operations. They distinguish public metadata, exact multipart definitions,
public minimized/expanded models, internal stored/expanded TMSL++ models and
native entity schema. Unknown type/generation and unsupported combinations fail
explicitly. A metadata discovery read can obtain generation before structured
representation dispatch. Generation must not be inferred from an item suffix.

Metadata patch, definition replacement and public/internal model authoring are
different body contracts. Link and Move require explicit input and are admitted
only for the native entity bindings below; they do not enable other facets.
Parser success only establishes valid local arguments, not complete
media/body/representation validation or permission to execute.

`validate_arguments` checks a resolved representation's input requirement,
metadata-patch/source-replace distinction, unsupported client conditions,
hard-only child deletion intent, unsupported recursion and actual LRO observation
applicability. Its success is local compatibility only; it does not read or
approve a body, reserve output, prove source permissions or enable dispatch.

`decode_json_input` applies an explicit raw-byte bound before decoding, rejects
non-UTF-8 input, unsupported media, duplicate keys at every depth, trailing
documents and non-object roots. It distinguishes missing, empty, malformed and
oversized input without echoing source text. Decoded values preserve null,
omission and numeric types; this decoder intentionally does not claim that
unknown/read-only fields have passed the producer's representation schema.

Only JSON receipt format is admitted in this local slice. No raw bytes,
recursive get, directory extraction, implicit OWL conversion, docs/context
authoring, item-collection list expansion, model-backed execution, import,
export, serve, wait, plan or apply alias is installed. `--wait` is an option,
not a root command; a host must still reject it for synchronous-only bindings.
Unsupported revision conditions must also fail before remote work.

Numeric defaults in the captured common pages remain draft choices. The parser
therefore retains omitted budgets as `None`; it validates explicit positive
byte/record bounds and duration syntax/24-hour ceiling without choosing a
hidden host timeout. The coordinator must supply accepted finite policies
before execution.

## Public SDK execution

`bind_public_operations` transactionally binds the caller's existing common
descriptors and schemas to these accepted selectors. It neither registers
duplicate descriptors nor assigns separate Ontology MCP tool IDs.

| Verb | Facet / representation | Existing operation ID |
| --- | --- | --- |
| Get | metadata / json | item.metadata.get |
| Get | definition / native | item.definition.get |
| Update, replace | definition / native | item.definition.replace |
| Get | model / minimized or expanded | ontology.model.get |
| Update, replace | model / minimized | ontology.model.replace |

The neutral registry/approval/receipt types are imported through the released
SDK-layer `fabric_sdk_ontology::v2` bridge, not a new CLI-to-core dependency.
The producer checkpoint is `907ccfad98e8df8400d36c37fe5222e5d12d69c1`; the
original source baseline is `eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`.
SDK credentials, endpoint binding, HTTP execution, bounded operation observation
and caller runtime are reused. There is no separate auth/runtime/poller.

`execute` accepts the same parsed arguments for generic and facade invocation.
The injected host supplies exact resolved IDs and canonical selectors, bounded
local input, finite deadline/cancellation policy, atomic no-clobber output
reservation, and approval bound to the immutable request/body/output/effects.
Optional local-write effects must have been declared by the existing descriptor.
Required effects cannot be omitted. Replacement records existing-only and
unguarded concurrency; unsupported revision conditions never disappear.

Only after these phases does the adapter call the released metadata,
definition/model Get, or definition/model replacement SDK API. Exact native
envelopes are retained; expanded models are not accepted as authoring inputs.
Definition replacement does not implicitly update metadata. Input JSON and
serialized result data have separate explicit bounds; oversized source is an
error, never clipped content. The native SDK validates its own request/response
contracts and retains service authority over individual model object semantics.
The host must verify feature gates, scoped authorization, complete input schema
and any required concurrency/approval restrictions before approving.

Successful/accepted JSON receipts go only to the reserved stdout/file writer;
failed/unknown results go to stderr with non-success `command_succeeded()`.
Original `PublicError`, native handles, uncertainty and engine warnings remain
available in `ExecutionReport`. `AdapterError::Output` retains that original
report even when the writer fails after partial local output; the SDK operation
is never replayed to repair a receipt.

An HTTP rejection of the initial submission is a failed request. An HTTP error
observing an already accepted operation is instead an unknown final outcome,
retaining the original operation handle and observed HTTP status. Only a
provider-reported terminal failure establishes that accepted work failed.

`bind_metadata_operations` and `execute_metadata` reuse the existing
`item.metadata.create`, `item.metadata.patch` and `item.delete` descriptors.
Create uses the real workspace collection, never a fabricated item ID, and
accepts only display name and optional description. Patch preserves omitted
fields; generation, native definitions, null values and unsupported revision
conditions are rejected before approval or credentials. Delete uses the generic
item SDK route; permanent deletion requires explicit intent and never means a
definition reset. The caller must enforce destination-name binding.

Metadata mutations share the immutable approval/output preparation and
`CapabilityReport` receipt pipeline. Waiting is explicit. Provider-confirmed
terminal cancellation is failed, while interruption of local observation after
acceptance remains unknown, retaining possible-mutation evidence and the
original operation handle. Neither condition permits automatic replay.

Projection, other schema-resource families, internal models, OWL conversion, docs/context and model-backed
execution remain explicit execution gaps, not fallback SDK calls. The local
catalog is broader source evidence, not an enabled-operation list.

## Native entity execution

`bind_native_operations` binds existing descriptors transactionally. It does not
register a second alias, framework or MCP tool. Native schema selectors retain
the exact owning workspace/item and generation 2; members require a child whose
kind is `entity.lineage`. Names and generation-1 numeric identities are not
coerced into lineage selectors.

| Verb | Facet / representation | Existing operation ID | Input |
| --- | --- | --- | --- |
| Get | schema.entity / native | ontology.entity.get | None |
| List | schema.entities / native | ontology.entity.list | Opaque optional continuation |
| Create | schema.entities / native | ontology.entity.create | Complete closed native EntityType DTO |
| Update, patch | schema.entity / native | ontology.entity.patch | Typed JSON Merge Patch |
| Delete | schema.entity / native | ontology.entity.delete | Explicit permanent intent |
| Link | schema.binding / native | ontology.entity.bind | backingTable and optional additionalBackingTables only |
| Move | schema.entity / native | ontology.entity.rename | name only, preserving the addressed lineage identity |

`execute_native` accepts an injected `NativeHost`, extending the existing
approval/input/output/deadline host with an explicit `NativeContext`. Context
contains the existing workload executor, verified feature gates and a
`NativeBinding` recording account, tenant, native audience and capacity/routing
binding. No credential lookup, token exchange, public-to-private fallback,
private URL construction or alternate runtime is installed. Approval sees the
exact context, body, native target, input/output paths and finite bounds.

Strict SDK DTO, feature, route, body and identity preflight runs without
dispatch. Create calls native strict collection creation, never upsert. Patch
preserves absent/null/array semantics; Link and Move restrict its allowed fields
without client read/modify/write. The host must authorize source dependencies,
reference effects and any deletion cascade within exact scope. Unsupported
If-Match, wait, recursion, generation, facet, media or fields fail before native
dispatch. Ordinary V4 writes declare RemoteWrite (and RemoteDelete for delete),
not graph publication inferred from a generation-1 recipe. Native discovery
retains its metered/read effects.

List reads one bounded native page. Remaining continuation or a locally cut
page produces Partial coverage and non-success command status; a cut service
page has no fabricated resume token. Oversized data is retained as unpresented
evidence rather than emitted as clipped source. Private 202 responses without a
supported scoped observation handle produce Unknown and retain the original
response; a private Location is not promoted into a public Fabric operation ID.
Local cancellation distinguishes not-sent from possibly dispatched work, never
cancels remote work or permits replay. Typed native causes and post-execution
output failures use the same CapabilityReport pipeline as metadata mutations.
Possible-mutation evidence is independent of outcome certainty: completed native
write responses that fail local processing still retain possible effects.
Uninterpretable completed responses are Unknown; provider HTTP rejection may be
Failed without falsely claiming that no mutation could have occurred.

## Source and spec evidence

Selected grouped Ontology root: `ab21dcdb-bf6f-453b-a0c3-760b41267d02` revision 18.
The seven children delegate to their common operation contracts. The active
grouped path contract leaves concrete escaping/namespace decisions open.

Backend source pin: `a5513205ddd862d450f14a6fdd14e78b40576719`.
PublicOntologyModelController distinguishes public forms and a 16 MiB write
cap. OntologyDefinitionController separates internal stored/expanded reads and
returns 501 for internal TMDL rendering. OntologyEntityTypesController provides
native V4 lineage-tag CRUDL and JSON Merge Patch. IOntologyScriptManager declares
transactional strict scripting; it is not permission for generic createOrReplace
under create. Source presence does not establish rollout or caller permission.

Tests cover alias/schema equivalence, exact vocabulary, typed arguments,
numeric/JSON boundaries, representation separation and explicit unsupported
states. Injected execution tests exercise actual SDK wire requests, credentials,
generic/facade identity, receipt streams, predispatch approval/collision failures
and post-acceptance output failure without replay. They do not prove a production
path resolver, filesystem reservation host, tenant permission or live rollout.
Rust checks require an explicitly allocated shared-host validation slot.
Workspace membership is discovered by the existing glob;
default-members, centralized internal dependencies and lockfile changes remain
coordinator-owned.
