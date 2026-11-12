<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native v2 blank SemanticModel creation

The opt-in `v2-create-semantic-model` feature selects one blank SemanticModel
binding in the `fab-cli-v2` head (`headless,v2-create-semantic-model`). The shared
`v2-create` feature alone installs no SemanticModel leaf. Default builds, Python, the v1
`create`/`mkdir` alias, semantic-model reads and BPA are unchanged.

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.SemanticModel
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.SemanticModel --input metadata.json --wait
```

Omitted input selects the existing pinned blank template. Explicit input is
strict unique-key JSON containing the exact outer `displayName` and optionally
a string `description` of at most 256 characters:

```json
{"displayName":"Example","description":"Blank template"}
```

An empty object, null, mismatched name, unknown field or arbitrary `definition`,
`creationPayload`, source, connection, folder or model input is rejected, including
under `--if-exists skip`. This is not a definition-free create API, import,
DirectLake configuration, arbitrary TMDL editing, model authoring, connection
binding, refresh, run, TOM or .NET integration. No related operation is authorized.

## Pinned content and native contract

The source baseline is `ced983722015e440e989f07d1bf866821d27fe90`.
`commands/mutations/src/create.rs` already calls `templates::semantic_model`;
the embedded template itself is pinned to Python `cf67fd52`. The narrow public
`BlankSemanticModelCreate` adapter in that same CLI crate reuses that builder.
No engine-to-CLI dependency, template framework or template change is introduced.

The native body has an outer display name, optional description, and a
`definition.parts` array with exactly these five `InlineBase64` parts:

| Path | Content |
| --- | --- |
| `.platform` | Existing platform metadata |
| `definition.pbism` | Existing format version and settings |
| `definition/database.tmdl` | Existing database definition |
| `definition/model.tmdl` | Existing model definition |
| `definition/tables/Table.tmdl` | Existing sample table definition |

Part bytes and order are unchanged. In particular, `.platform` retains its
`Blank` display name and zero logical ID; the requested literal name, even one
shaped like a GUID, belongs only to the outer `displayName`. "Blank" denotes
this existing sample-table template, not a model with no tables.

The [official Create Semantic Model API][create] documents
`POST /v1/workspaces/{workspaceId}/semanticModels`, HTTP 201 and 202, operation
headers and the `ItemDisplayNameAlreadyInUse` error. The [definition contract][definition]
documents mutually exclusive TMDL/TMSL, the `definition/` TMDL parts,
`definition.pbism`, and `InlineBase64` encoding. The current service documentation
also describes other creation modes; this adapter deliberately does not expose
them. Documentation/schema evidence is not live tenant or provisioning evidence.

## Policy, execution and evidence

The released [shared create host](create-host.md) supplies the sole SDK planner,
bounded input, complete discovery, explicit immutable identity/endpoint,
immediate approval, one POST, native operation polling and no-clobber receipt
delivery. Unsupported overwrite, duplicate and force policies reject before input
I/O or credentials. Existing exact correct-kind leaves may be skipped unchanged;
case collisions, ambiguity and incomplete discovery fail. Preflight is not an
atomic reservation. A raced provider rejection, including HTTP 409, is a failure,
never a skip, overwrite or permission to replay.

The shared root-folder repair `caf2f7f2` uses only the generic Core `/items`
discovery route with `recursive=false`; the typed SemanticModel POST has no
discovery query parameters. Paging retains that root scope. Omitted or null
`folderId` is root; nested-only matches and malformed folder identities reject.
When both root and nested same-name rows exist, only the root can be selected.
Completed create results must also identify a root item. Returned folder fields
and whether they were present remain protected in `meta` and `meta.submission`,
even when data is projected or a later poll fails.

`--wait` uses the existing native `WaitForResult` policy and `Retry-After`.
HTTP 202 is accepted, not completed. Resource and operation IDs stay distinct.
Known acceptance evidence survives poll, body, cancellation and output errors.
Projection applies only to `data`; reconciliation metadata remains unprojected.
Local cancellation and failed output are not remote rollback.
An unexpected success-class operation-result status is `InvalidResponse` with
an unknown, nonzero outcome, not proof of provider rejection or completion.
The shared correction `0ec38482` retains the original typed HTTP cause and
status separately from receipt classification and initial acceptance evidence.
Genuine provider errors remain failures; neither case permits replay.
The shared strict-response correction `44576b07` uses the existing bounded
unique-key decoder for nonempty create results. Duplicate keys at the root or
inside nested objects, including escaped spellings of the same key, produce an
unknown, nonzero result with exact raw-response evidence. A previously observed
valid acceptance identity remains protected; duplicate final keys cannot replace
it or establish completion.

The shared native terminal runner presents the exact template-expanded body,
selected action, identity, audience, endpoint and effects for immediate approval.
The original owner's shared helper repair `0454fb43` provides a borrowed view
of the same prepared operation consumed once by execution. Approval does not
reconstruct the body from metadata. Terminal controls are escaped only for
display; transmitted bytes are unchanged. Blank creation declares no additional
provider-owned child creation or metered operation beyond the one native request.
Initial submission status, resource and operation handle remain protected in
`meta.submission`, separately from later observations and data projection.

Offline tests cover parser-to-host-to-SDK HTTP routing and pinned bytes, strict
input, policy and discovery refusals, approval denial, native 201/202/LRO,
partial responses, cancellation, receipt delivery and executable pipe admission.
Passing offline evidence does not establish live Entra/Fabric access, licensing,
capacity, model refresh or successful provisioning. Required native CI and
exact-head independent review remain delivery gates.

[create]: https://learn.microsoft.com/en-us/rest/api/fabric/semanticmodel/items/create-semantic-model
[definition]: https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/semantic-model-definition
