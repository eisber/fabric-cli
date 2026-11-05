<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# DataAgent ontology management SDK

This SDK owns the **DataAgent partner workflow**, not standalone Ontology
authoring or runtime answering. It plans typed requests and consumes injected
executors. No transport, identity discovery, token exchange, fixed workload host,
retry, poller, runtime, CLI authentication fallback, or implicit publish is
installed. Private execution requires a caller-configured MWC executor; source
and offline checks do not establish live availability.

## API surfaces

| Surface | Implemented operations |
| --- | --- |
| Public, published only | List attachments; read ontology elements with optional returned `rootId` |
| Private MWC, explicit staging/published | List/read attachments; ontology elements; selected `ontologydatasources` list, elements/schema; authored few-shot list/member reads |
| Private MWC, staging only | Attach/remove ontology; native entity selection; generated `contextmarkdown`; authored source instructions GET/PATCH/PUT/DELETE; few-shot create/patch/delete; unavailable-source custom-setup cleanup |
| Private MWC, explicit whole-agent operation | `POST staging/publish` |

`PublicClient` uses the existing `OperationSession`/`OperationExecutor` and an
explicit immutable `RequestContext` with caller-injected Azure credentials.
It exposes no public staging/member/schema/guidance/publish API and never falls
back to MWC. `PrivateClient` uses the released
`fabric_sdk_ontology_workload::workload::WorkloadExecutor`; the caller owns its
logical-workload routing, authentication, deadlines and streaming bounds. Caller
partition metadata does not verify the executor's actual token claims.
The pinned released request/condition/one-dispatch contract is
`863a28f7b39305c3bf6a264b561cf32d029bd65f` (on top of
`72132c7809024e7df77b199c77491fbb807f4ac4`), not unfinished peer source.
Private dispatch reuses its `execute_once` postconditions and native If-Match
setter, including duplicate-header/status and decoded-response bounds.

Private methods return a typed `Operation<T>` with inspectable exact
`WorkloadRequest` and declared `Effects`. Execute it once with
`PrivateClient::execute`. Public reads use a different `PublicRead<T>` type and
executor, so private route applicability cannot be inferred from a public URL.
All JSON authoring uses native `application/json`, including PATCH; this source
does not declare JSON Merge Patch media semantics.

## Identity, concurrency and authority

The DataAgent workspace/item, referenced ontology workspace/item, configuration
stage, caller partition and returned virtual source ID remain distinct.
Attachment add is idempotent per **DataAgent + ontology item ID**, not a
display-name key or a global ontology cache. Existing attachment identity is
returned and validated, not manufactured from the input. Re-adding is not a
request to replace its name, source workspace, selection, instructions or examples.

`SourceScope` contains its agent, ontology and stage. Its `source_id` is the
returned virtual source key; it must not be reconstructed from a physical
container ID. In particular, an Eventhouse binding can use the child KQL
database identity while retaining binding type `Eventhouse`. Identical physical
IDs under different ontology/agent/stage scopes do not share authored setup.
Returned unavailable-source tombstones and PendingConversion metadata are
preserved, not treated as an empty successful list.

Returned ETags become `Validator`s scoped to caller and exact resource. Optional
conditions stay optional and are sent unchanged. Stale HTTP 412 and native 409
conflicts remain distinct failures with original provider data; there is no
unconditional retry, GET/RMW emulation or condition weakening. An add accepts
the native collection condition or existing attachment condition. Context,
schema and source-list validators cannot be reused as authored-source validators.
The source publish endpoint does **not** consume If-Match, so publish has no
misleading condition argument.

Selection uses the native `elementIds` + `isSelected` body, not the public
direct-datasource `elementIdentifiers` shape. Omitted IDs explicitly mean all
entities; an empty supplied list is invalid. Returned IDs are opaque and encoded
once, not decoded/rebuilt from display names. Grouping and physical schema nodes
are navigation, not selectable ontology entities; native rejections are retained.
Per-entity NotFound/Failed statuses are not upgraded to successful selection.

Authored instructions/few shots remain source authority. PATCH omits unchanged
fields; PUT explicitly clears omitted instruction/description fields. Source
PATCH ignores null rather than interpreting it as JSON Merge Patch deletion.
Deleting instructions delegates to the source operation that preserves
description/intact examples. Blank few-shot questions are valid source drafts:
their validation state is preserved and is not a certification for runtime use.
Lengths use source .NET UTF-16 counts, not UTF-8 bytes/scalar counts.

`ContextMarkdown` is a generated selected-entity, per-user-cache served
projection. It is not stored authored guidance, an answer, a write DTO or a
retrieval fallback. Documentation navigation, authored source guidance,
configuration-download documents, and runtime Responses are separate.
No `/responses`, ontology query, LLM invocation or example execution is used to
retrieve missing content.

## Effects, completion and bounds

GET does not promise an inert service operation. The inspected controllers use
management-interaction migration, reconciliation and per-user cache paths.
`Effects` declares these effects before dispatch. `responseSource=any` preserves
cache-first/serve-stale behavior; explicit `live` requests fresh discovery.
Search/recursive reads, whose source ignores that refresh override, are not
silently substituted by this single-level API.

Attaching initializes selected live entities. Removing the attachment
recursively removes its DataAgent-owned authored source subtree, not the Ontology
item or the legacy ontology-as-datasource folder. Unavailable-source cleanup
uses the native live-availability check; still-available/no-setup cases remain
409. Source-side few-shot validation/embedding maintenance can occur during
authoring. Explicit publish covers the **entire DataAgent staging tree**, can
refresh embeddings/templates, and is never chained to an edit.

`Outcome::Accepted` retains the exact 202 headers/body without claiming completion
or following Location. Caller-owned reconciliation is required. Execution
failures retain the shared executor's dispatch certainty. Cancellation before
dispatch is NotSent; cancellation after dispatch starts is conservatively Unknown,
not rollback or permission to replay. Malformed mutation success data retains
the response and unknown resulting identity/effects rather than empty success.
Display/Debug of structured errors do not implicitly print service bodies.

Private element traversal reuses `fabric-sdk-paging::TokenPager`, not an
endpoint-owned pager. It enforces page/row/decoded-byte bounds, repeated cursors,
same origin/exact route, and unchanged optional page ETags. The caller supplies
an approved continuation origin for URI **validation only**; it never becomes a
transport destination. Traversal failures carry committed earlier rows/pages.
The injected executor must enforce the response bound while streaming; a
violating response is rejected and bounded retained evidence is marked truncated.

List/member methods return source page envelopes with explicit continuation
metadata. `collect_elements` alone claims complete traversal. The inspected
attachment/source/few-shot lists currently return terminal pages; several expose
no effective cursor parameter. Public ontology elements support only `rootId`.
This SDK does not invent a cursor argument for those APIs or silently claim
all-page coverage from a single page.

## Evidence and integration

Implementation baseline: `eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`.
Backend reference: existing read-only ML checkout at HEAD/ref
`e628064e31e64f390a1c277c0c5fa038ede1666d`. Local source cleanliness was not
certified; this is source-shape evidence, not byte-certified committed service
content, deployment proof, or a live test.

Relevant source paths beneath `Components/DataAgent/`:

- `Microsoft.Fabric.Workload.DataAgent.Service/Controllers/DataAgentV3Management{PublicAPI,Mwc}Controller.cs`
- `Microsoft.Fabric.Workload.DataAgent.Contracts/{ConfigurationV3,OntologiesV3,DataSources}/`
- `Microsoft.Fabric.Workload.DataAgent.Configuration/DataAgentV3ConfigurationStore.{Ontologies,OntologyElements,OntologyDataSources,FewShots}.cs`
- `Microsoft.Fabric.Workload.DataAgent.Configuration/DataAgentStoragePathSegmentValidator.cs`
- `Microsoft.Fabric.Workload.DataAgent.Service/Ontology/OntologyDataSourceReadService.cs`
- `Microsoft.Fabric.Workload.DataAgent.Core/AiSkillFewShotUtils.cs`

The earlier testing skill's statement that there are no public ontology routes
is stale relative to current published list/elements controller declarations.
Source declarations do not establish caller authorization, feature flights,
front-door rollout or private executor availability. No skill recipes, live API,
auth flow, model, query or deployment was executed.

Tests contain synthetic inline fixtures authored from those DTO/route contracts;
no private service payloads, tenant IDs, credentials or copied notebook data.
They cover exact method/path/media/body/identity, explicit stage/caller scope,
source isolation, preserved authoring authority, malformed responses, HTTP
failures, acceptance/uncertainty/cancellation and bounded paging.

Only package source/tests/README are lane-owned. The integration coordinator
must supply the package manifest, centralized dependencies, default member and
lock entry. Required package settings: workspace version/edition/rust-version/
license/publish/lints; `package.metadata.fabric.layer = "sdk"`.
Dependencies: `azure_core`, `fabric-core`, `fabric-sdk-workspaces`,
`fabric-sdk-operations`, `fabric-sdk-paging`, `fabric-sdk-ontology-workload`,
`serde`, `serde_json`, `thiserror`. Dev dependencies: `async-trait`, `tokio`.
No facade, CLI, core, host, shared manifest, lock or documentation-navigation
file is changed in this lane.

Build/test execution requires the coordinator's explicit serial slot. Actual
revision-bound check results and unexecuted platform/live gaps are returned in
the handoff; the existence of these fixtures is not a passed-check claim.
