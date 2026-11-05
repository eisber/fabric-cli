<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Ontology workload SDK

This deterministic SDK targets the **private Digital Operations V4 workload**,
not the public Fabric item/model API. Source contract:
`workload-digitaloperations` commit
`a5513205ddd862d450f14a6fdd14e78b40576719`, especially
`Controllers/V4/Ontology`, `API/Gen4/Specs/scripts.tsp`, and the V4 RDF controller,
constants and import/export managers. Source existence is not rollout,
authorization, flight or deployment evidence.

## Shared execution contract

`workload::{WorkloadIdentity, WorkloadRoute, WorkloadMethod, WorkloadRequest,
WorkloadResponse, WorkloadError, WorkloadErrorKind, DispatchState,
WorkloadFuture, WorkloadExecutor}` is the one reusable execution boundary.
Other private-workload consumers (including DataAgent) reuse it rather than
define a second transport. Identity is a logical identifier, never a URL.
Routes are relative, built from individually encoded segments and query values.
Requests expose method (including PUT), content type, exact bytes, an optional
native If-Match token, and streaming response bound. `with_if_match` preserves
the caller's exact token and rejects controls, blank values, non-ASCII and
tokens over 8 KiB. It is not a fabricated Ontology precondition: Ontology native
endpoints do not expose a documented client If-Match contract. No arbitrary
Authorization, host or routing headers are accepted.
The executor is injected by the caller and performs **exactly one dispatch**.
It must enforce the response bound before buffering, reject ambiguous duplicate
headers, and return original statuses, headers and body bytes, including 202
and failures. Errors distinguish not-sent, accepted, completed and unknown
dispatch. No error implies rollback. Do not automatically replay mutations.
`execute_once` is the shared boundary's postcondition check; it rejects duplicate
headers, invalid statuses and oversized decoded bodies. Executors must still
enforce bounds while streaming. An executor violating the bound yields an
explicit Limit with only a bounded body prefix retained. Debug/Display redact
request/response body, routes, preconditions and executor diagnostics.

No HTTP implementation, token exchange, credential discovery, sign-in, fixed
host, redirect following, poller, runtime, query, embedded model or agent is
provided. The caller owns authenticated private routing, explicit authorization,
cancellation/deadline policy and reconciliation. Dropping a future cannot prove
remote cancellation or absence of effects.

## Authoring surface

`OntologyTarget` requires explicit item type `Ontology`, feature level **2**,
workspace/item UUIDs, caller-provided logical workload identity and resolved TMSL
applicability. No operation discovers rollout or authorization. The executor
must bind that identity to the caller-approved capacity, tenant, audience and
owner identity; this package never converts a public Fabric endpoint override
into a private workload binding.

`OntologyClient` exposes typed create/get/patch/delete/page/list over
`entityTypes`, `entityRelationships`, `dataSources`, `expressions`, `namespaces`,
TOM `relationships`, and `rules`. REST uses lineage tags (not necessarily UUIDs)
except TOM relationships and reusable properties, which use names.
`MemberId<T>::lineage(LineageTag)` and `MemberId<T>::named(ObjectName)` are
disjoint typed constructors. Reserved namespace tag `default` remains valid.

**Metrics and reusable properties are advertised as direct-endpoint read-only.**
They do not implement `MutableResource`, so generic direct writes do not
compile. The separate source-defined script language supports metric and
reusable-property kinds; reusable-property scripts and reads require their
additional feature gate. A table `measure` is not a root `metric`.
The capability ledger is `capabilities::{RESOURCES, GAPS}`.

Write DTOs deny unknown/projection fields rather than silently dropping caller
content. Canonical field/enum/keyword casing is required by this SDK even where
the backend accepts additional case-insensitive spellings. `MergePatch<T>`
validates closed field/type schemas recursively, preserving omission, explicit
null and whole-array replacement. Replacement elements are full create
projections. Nested column/property/measure lineage tags are preserved.
Immutable inheritance fields cannot be patched; inheritance authoring (including
null/empty overrides) is gated independently. Backend topology, source connector
security, source permissions, validation and concurrency remain authoritative;
the SDK does not imitate the server's model engine.

`NativeResource<T>` contains typed known fields and the exact original object.
Unknown nested fields, server projections, identity, nulls and original bytes
remain in `raw`/`Outcome::response`; these read values are not unchecked write
DTOs. The internal definition read is a TMSL++ envelope, not the public
`{name, description?, model}` projection. Expansion is explicit and fail-closed.
No whole-definition JSON write escape hatch is supplied.

## Scripts, bindings and effects

`ScriptRequest` supports exactly one `create`, `createOrReplace`, `alter`,
`delete`, or flat `sequence` of 1..=1000 leaves. Typed builders brand the body,
parent and reference together; external JSON goes through the same closed
schema checks. Create uses a parent (empty/omitted for roots); replace/alter/delete
address the object itself by **name**, never REST lineage tag.
All 16 source kind shapes are covered, including table columns/partitions/measures,
entity properties/additional tables, and rule references. A `dataSource` script
kind does not exist: the native kind is `table`. `referencedRelationship` has no
alter projection.

The entire sequence is one POST to `definition/scripts`, never client-side
RMW or a loop of independently committed leaves. Strict-create collisions,
missing alter/delete targets, final validation and conflicts are returned as
provider errors, without replay or automatic upsert. `deferValidation=true`
relaxes only the source-selected intermediate metric rules; other rules still
run and final state must validate before the server commits. These are pinned
backend guarantees, not a local simulation or a live-service validation claim.
Result indices/counts, verbs and closed targets are checked. The source mapper
echoes the **original** target for alter/replace even on rename. Table-measure
renames explicitly report that authored metric metadata may be removed; the
SDK never invents a metric-binding update.

Entity backing data sources support bounded list/get and referenced-expression
reads by table name, with optional explicit binding-resolution cache refresh.
Unbind-one and clear-all reset affected references but leave ontology tables,
physical data and ACLs present. Definition DELETE is an idempotent collection
**reset**, retaining the skeleton and item identity. The actual controller
returns **204**, despite the broader TypeSpec description mentioning a returned
document. Item deletion is separately owned by the Fabric item SDK.

## Paging and RDF

Paged reads require explicit page/item/aggregate-byte budgets. Tokens are opaque,
bounded and encoded back onto the same immutable route. Continuation URLs are
never followed. Missing `value`, malformed or repeated tokens, tokenless
continuation URLs, errors, acceptance and limits are explicit partial outcomes
with prior decoded items and observed responses retained. A valid `value: []`
with no continuation is complete, not an error. Rule entity filters are repeated
exact names with OR semantics, not CSV.

Server RDF export supports **Turtle** and **RDF/XML** only. `.owl` means RDF/XML,
not arbitrary OWL syntax. JSON-LD is rejected despite its backend enum/MIME entry.
Binary output is bounded by the caller and the source **10 MiB** cap. MIME type
must match the requested format. Content-Disposition is untrusted data; no
filename or file is created automatically. RDF is explicitly a lossy schema
projection, not native backup: the byte response supplies no exhaustive
per-construct loss diagnostics, and inheritance/reusable-property/auto-projection
have independent server mapping gates.

**Import is disabled:** the pinned manager checks empty target, then calls
whole-definition createOrReplace separately. No exposed atomic create-only
precondition or verified race/no-clobber/effect guard exists. A 30 MiB source
import ceiling and ZIP parser limits do not fix that concurrency gap.
`rdf_import_capability` returns the concrete gap before executor invocation.
Native TMDL returns 501; query, agents, public model/ALM and item deletion are
explicitly outside this package.

## Offline validation contract

`tests/contracts.rs` drives generated real requests and typed/binary responses
through a recording executor, including every direct collection, script kind,
feature/identity/schema rejection, partial paging, no replay, reset/unbind
distinctions, request/response boundaries, unsupported RDF formats, source loss
warnings, and unsafe-import rejection. It denies unnameable test items.
Tests validate deterministic SDK behavior, not deployed backend transactions,
caller permissions, private routing/authentication, or live source-model access.
Actual check evidence must name the exact committed revision; formatting alone
is not compilation or a successful contract-test claim.

## Integration ownership

The package uses only existing centralized `serde`, `serde_json` and test
`tokio` dependencies. Workspace membership discovers `capabilities/*/*`.
Coordinator-owned root manifest delta: add
`"capabilities/ontology/fabric-sdk-ontology-workload"` to `default-members` and
`fabric-sdk-ontology-workload = { path = "capabilities/ontology/fabric-sdk-ontology-workload" }`
to `[workspace.dependencies]` when composing consumers. Refresh `Cargo.lock`
under the coordinator's shared build slot. No root manifest/lock edits belong
to this lane.
