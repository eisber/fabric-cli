<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Report native reads

The additive `fabric_sdk::reports` module binds typed Report metadata and
native definition parts to the existing item/representation clients. The
experimental `fab-cli-v2 get` backend selects this binding for an explicitly
typed `.Report` owner. Untyped item paths retain generic behavior; the existing
Python CLI and `fab-rs` compatibility handler are unchanged.

```text
fab-cli-v2 get /<workspace-id>.Workspace/<report-id>.Report
fab-cli-v2 get /<workspace-id>.Workspace/<report-id>.Report/src/definition
fab-cli-v2 get /<workspace-id>.Workspace/<report-id>.Report/src/definition/formats/PBIR
fab-cli-v2 get /<workspace-id>.Workspace/<report-id>.Report/src/definition/parts/StaticResources/logo.jpg --format raw
```

Names use the existing bounded resolver; IDs are canonical UUIDs. Native format
is selected by the definition path, not `--format`, which remains presentation.
Only explicit `PBIR` and `PBIR-Legacy` native selectors are advertised. An
unsupported selector fails without source or credential dispatch.

## Native boundaries

| Read | Public Fabric v1 request | Boundary |
| --- | --- | --- |
| Metadata | `GET /workspaces/{workspace}/reports/{report}` | Report read rights; one response, no definition or model query |
| Definition | `POST /workspaces/{workspace}/reports/{report}/getDefinition` | Report read **and write** rights; encrypted-label usage restrictions; label excluded |
| Accepted definition | Existing `/operations/{id}` and advertised `/result` | Shared observer only when `--wait`; no replay or new poller |

The caller injects credentials, endpoint, transport, cancellation and total
deadline. `ReportMetadata` checks owner type and actual item/workspace identity;
it does not establish model access or freshness. An absent `workspaceId` remains
absent rather than a fabricated source field. Denial and malformed/mismatched
metadata/source fail explicitly; metadata is not a definition fallback.

The raw multipart envelope preserves unknown fields, part order, exact paths,
payload type and binary resources. Selected InlineBase64 parts decode exactly;
no UTF-8 coercion, schema migration, output cleanup or filesystem extraction runs.
Provider-advertised `mediaType` is retained; suffixes do not invent MIME types.
Duplicate exact paths, malformed encodings/binding JSON, mixed legacy/enhanced
layouts and conflicting declared/requested formats fail.

Required report JSON (legacy `report.json` or enhanced
`definition/report.json`) and every enhanced `page.json` must decode as
syntactically valid JSON objects. Invalid syntax, null, arrays/scalars or
non-UTF-8 required content sets `invalidReportStructure` partial evidence while
preserving the original envelope/bytes. It cannot enable complete or raw
delivery. Unrelated resources remain opaque, even with a `.json` suffix;
this check is not a comprehensive JSON-schema validator.

Definition JSON carries `meta.reportSource` outside projected data. It records
declared format versus observed layout, `definition.pbir` version/schema,
enhanced `definition/version.json` version, native paths/media, model reference
and incomplete reasons. Known structural versions are definition.pbir `1.0`
(legacy) and `4.0`, and enhanced definition `2.0.0`. Future versions/encodings
and missing required report structure remain **partial**, with original data
retained. Future definition versions keep their unknown model-reference shape
as opaque data rather than coercing it into a legacy binding. This is structural
coverage, not validation of every report JSON schema or evidence of a complete
service-state backup.

Partial native source can produce only the existing unprojected JSON receipt
on stdout, never a complete file or raw part delivery. Accepted source also
cannot produce a source file. Complete delivery reuses the existing byte budget,
no-clobber output and local-write failure evidence. Service ceilings do not
override the caller's smaller invocation/transport limits.

## Related-model prerequisite

The native `datasetReference` is preserved as byPath or byConnection data.
Binding generation comes from the declared `definitionProperties` schema,
not the `definition.pbir` version: version `4.0` can use either schema.
Schema `1.0.0` requires all six legacy connection fields; five allow string/null,
and `pbiServiceModelId` allows integer/null. In particular, a null
`connectionString` is valid legacy source. Schema `2.0.0` retains the strict
nonempty connection-string check. Unrecognized schema generations retain
`unknownBindingSchema` partial evidence and the untouched inert reference,
rather than interpreting it using either known generation. Schema-less source
retains the existing strict binding check; no schema is fetched during reads.
`relatedModelAuthorized` is always false: this read never resolves or authorizes
that model. Model navigation requires explicit canonical model coordinates
(including its real workspace) and separate authorization by a model provider.
A report's workspace is not inferred as the model's workspace. No model creation,
refresh, query, served-document read or permission fallback occurs.

PBIR-Legacy `report.json` is **not PBIX**; `definition.pbir` alone is not a
complete report. Native definitions do not include model rows, labels, ACLs,
credentials or complete runtime/service state. Served metadata/SOFe documents
are neither this writable representation nor source-completeness evidence.
RDL is owned by `PaginatedReport`, not this Report binding. PBIX/RDL downloads,
rendered exports, rebind, authoring, clone, publish and public sharing remain
outside the slice.

## Registration and validation

`reports::register` adds metadata/native read entries and a type-constrained
`report` alias to a caller-owned neutral registry. Entries disclose definition
read/write and encrypted-label prerequisites and declare only `RemoteRead`.
Registration is not authorization or installation of a CLI/MCP host; the get
head uses the same SDK binding without a second runtime.

Offline `fabric-sdk --test reports` and `fabric-cli --test get_backend`
contracts cover exact routes/owner checks, parts/media, unknown versions,
malformed and denied source, partial receipts, projection-preserved evidence,
binary bytes, predispatch rejection and accepted-source observation. Existing
generic representation/get tests remain independent. No live API/credential
acceptance or three-platform completion is claimed by these tests.

## Definition-create schema adapter

`reports::create::ReportCreateValidator` supplies offline Report-specific input
validation to the existing SDK create planner. It does not mount a command,
authorize a write or add an executor. Its `submit` method delegates an actual
create plan to the existing `ItemMutations` request path, retaining the caller's
completion policy and shared operation failure evidence without replay.
The opt-in `v2-create` module `fabric_cli::v2_create_report` implements the released
shared host's `CreateBinding`. It verifies original strict JSON bytes against the
planner's immutable input, then builds only the native Report POST. Whitespace,
multipart payloads and native extension fields are sent unchanged. The shared
host owns input, approval, discovery, LRO observation and receipt delivery; this
binding adds none of those engines.
Immediate approval borrows the same prepared method, bound URL, completion
policy and original body bytes that the shared operation engine consumes once.
The terminal question escapes controls and preserves the exact wire-body
spelling instead of presenting a reserialized substitute. Report definition
creation does not advertise an additional model/child creation or compute
effect; its anticipated-effects list is empty, not proof about observed service
state or related-model permissions. An unchanged skip has no prepared request.

With `headless,v2-create`, the experimental binary mounts only this typed create
binding:

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.Report --input FILE|-
```

The name is literal even if UUID-shaped and selects only a workspace-root Report.
Discovery uses the generic `/items?recursive=false` route and preserves that
filter across bounded paging. A valid nested match is excluded from root
cardinality; nested-only or malformed matching folder evidence rejects before
approval. The CLI binding rejects a non-null input `folderId` instead of silently
creating somewhere other than the selected root. Omission and explicit null are
preserved unchanged. Completed responses must also establish a root folder,
while available folder evidence survives failures outside projected data.

Native definition JSON is required; `--format json` selects the receipt, never
PBIX or a definition conversion.
Profiles, collections, name-based workspace discovery and artifact facades
are not mounted. Immediate terminal approval is mandatory; nonterminal
execution fails before identity or provider access. This opt-in mount changes
neither default builds nor v1's `create`/`mkdir` alias.

The SDK `report_create` and CLI `v2_create_report` test sources include
parser-to-shared-host-to-SDK scripted HTTP and actual binary preflight/stream
cases. Source availability is not execution evidence; no native qualification
or live operation is claimed here. The impact graph includes the Report tests
and the helper's opt-in profiles. Full native CI remains required.

The public Create Report API documents `ItemDisplayNameAlreadyInUse`, not an
upsert or repeated-name create contract. The Report capability permits
default-error creation and an unchanged exact-existing skip result; the native
POST, not a race-free preflight claim, enforces creation conflicts. Overwrite,
force and duplicate fail as unsupported, including for a missing target. A skip
must still validate explicit input. No full replacement, delete/recreate or
automatic renaming is advertised.

The SDK adapter selects only an exact named Report in an explicit workspace.
Its native request body requires a matching `displayName` and a `definition`;
optional `description` is limited to 256 characters, and `folderId` is a UUID
or the API's explicit null workspace-folder selection. This reusable native
schema does not widen the CLI's root-only selector described above. Unknown
outer fields, label-setting requests and the separate `creationPayload` mode
are not advertised. They fail rather than being silently removed.

Definition validation reuses the native Report codec above. PBIR and
PBIR-Legacy layouts remain mutually exclusive. Missing required structure,
unknown versions or binding schemas, unsupported encodings and malformed
required JSON cannot become create input. Recognized native structure is not
full validation of every report JSON schema and does not establish model
permissions. Part paths, order, binary payloads, native extension fields and
the original model reference are preserved without cleanup or resolution.
The validator returns only create-input evidence, never replacement evidence.
It performs no model reads, authorization, creation, rebind or cross-workspace
lookup. The service remains responsible for its actual permissions and
semantic validation.

Receipts keep initial submission status, operation handle and any supplied
Report identity in `meta.submission`, separately from the latest HTTP
observation. Failed/cancelled polling and malformed final JSON retain this
protected submission evidence even when `data` is projected away. Unparsed
acceptance/final response bytes are explicitly labeled base64 evidence, not
repaired content or a completed Report. Typed exact-name cardinality ignores
same-name items of other known types; multiple Reports, wrong-kind-only matches,
malformed matching identities and wrong workspace evidence still reject.
`folderIdObserved` distinguishes an omitted source field from an explicit null
root-folder observation; neither invents a child folder identity.
An operation-result response with an unexpected success-class status (including
201, 202 or 204 where the result contract requires 200) remains nonzero and
`unknown` with an `InvalidResponse` receipt. The original shared-engine typed
HTTP error and status remain available; the receipt does not turn them into a
definitive provider rejection or claim the initial Report submission was undone.
Actual provider HTTP failures and failed operation states retain their separate
failure classification. None of these cases replays the Report POST.
All nonempty create responses use the existing bounded strict JSON decoder.
Duplicate root, nested or escape-equivalent keys in an immediate 201,
non-waited 202 or final 200 response fail with an unknown, nonzero outcome,
not last-value-wins metadata. Original response bytes remain labeled base64,
and any earlier accepted Report identity and operation handle remain protected
outside projected data. This does not change the global JSON parser or the
shared SDK operation engine.

## API references

- [Create Report](https://learn.microsoft.com/en-us/rest/api/fabric/report/items/create-report):
  native `POST /workspaces/{workspace}/reports`, definition request shape,
  contributor role, read/write scopes and 201 versus 202 responses.
- [Get Report](https://learn.microsoft.com/en-us/rest/api/fabric/report/items/get-report):
  metadata only and read scopes/rights.
- [Get Report Definition](https://learn.microsoft.com/en-us/rest/api/fabric/report/items/get-report-definition):
  POST, read/write permissions, labels and LRO.
- [Report definition](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/report-definition):
  mutually exclusive PBIR/PBIR-Legacy, required native parts and model reference.
- [Project report folder](https://learn.microsoft.com/en-us/power-bi/developer/projects/projects-report):
  format/version/schema, byPath versus byConnection and native structure.
- [Definition properties schema 1.0.0](https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/1.0.0/schema.json)
  and [schema 2.0.0](https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json):
  distinct legacy nullable field types and modern connection representation.
- [Long-running operations](https://learn.microsoft.com/en-us/rest/api/fabric/articles/long-running-operation):
  status/result observation and acceptance headers.
