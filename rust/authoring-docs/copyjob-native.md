<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native CopyJob read bindings

The additive `fabric_sdk::copyjobs` module implements native CopyJob metadata,
one workspace metadata page, and exact public definition retrieval. It reuses
the existing item reader, shared operation executor, credentials, byte bounds,
time/cancellation and observer. It installs no runtime, HTTP engine or poller.
V1 commands, generic item definitions and source oracles remain unchanged.

The separate opt-in [metadata-only CopyJob create binding](copyjob-create.md)
adds one specialized CREATE route. It does not configure or run a copy job and
does not expand these read descriptors or their definition schema.

## Scope and native routes

All routes use the caller-selected Fabric endpoint, public-cloud audience
`https://api.fabric.microsoft.com`, and API prefix `/v1`. No source/destination,
OneLake, SQL or ARM credential is selected by these operations.

| Binding | Method and route | Result and permission |
| --- | --- | --- |
| Metadata | GET `/workspaces/{workspace}/copyJobs/{item}` | Completed CopyJob metadata; item read permission and CopyJob/Item Read or ReadWrite delegated scope |
| Workspace metadata page | GET `/workspaces/{workspace}/copyJobs?recursive=...` | One native `value` array with inert continuations; workspace Viewer role and Workspace.Read.All or Workspace.ReadWrite.All |
| Native definition | POST `/workspaces/{workspace}/copyJobs/{item}/getDefinition` | Exact multipart definition or real 202 operation handle; item read **and write** rights, encrypted-label usage restrictions |

The type-specific definition page does not state delegated scopes. The generic
Get Item Definition page documents Item.ReadWrite.All or type-specific
ReadWrite.All; this is a documentation distinction, not a read-only grant.
All three CopyJob APIs document user, service-principal and managed-identity
support. Tenant settings, dependent connections, host policy and actual
permissions remain service-authoritative and have not been live-validated.

`CopyJobTarget` requires concrete workspace/item UUIDs before credentials or
execution. Metadata checks exact CopyJob type and matching UUIDs, preserving
case-insensitive UUID equality without rewriting returned strings. Missing,
null, malformed and wrong-owner identities fail. Unknown metadata fields and
parsed ordering remain intact; no definition read augments metadata.

List options explicitly select native recursion and root folder; no new numeric
or recursion defaults are added. Continuation tokens are encoded once. The
single-page binding never follows a URI or claims exhaustive enumeration.
Common list/paging consumers must enforce aggregate limits and validate every
follow-up origin/path. Native empty and singleton arrays are not unwrapped.
Acceptance and malformed metadata cannot become an empty completed page.

## Source representation and generic consumers

The existing opt-in `SdkGetBackend` recognizes typed CopyJob metadata and
`src/definition` paths. The root grammar is unchanged:

```text
get /<workspace>.Workspace/<item>.CopyJob
get /<workspace>.Workspace/<item>.CopyJob/src/definition
get /<workspace>.Workspace/<item>.CopyJob/src/definition/parts/copyjob-content.json --format raw
```

Names use existing bounded workspace/item resolution; UUID-qualified paths do
not enumerate. Typed paths verify metadata through the native CopyJob route
before source retrieval. Untyped `/workspaces/{id}/items/{id}` paths retain
their existing generic semantics and do not infer a CopyJob owner. Projection,
file no-clobber, deadlines and delivery remain in the shared get owner.
CopyJob runtime, docs and physical-file facets are rejected before discovery
in this slice; no OneLake or job fallback is implied.

Definition retrieval submits exactly one empty-body POST. The optional native
format token is validated but no undocumented format name is invented.
Completed definitions require one exact `copyjob-content.json` InlineBase64
part decoding to object-shaped JSON with properties/jobMode and an activities
array of objects. This is bounded structural validation, not a complete
connector/authoring schema or an execution-readiness verdict. Future fields and
job modes, part order and optional `.platform` remain unmodified. Known content
is never reconstructed from display metadata or a filename alias.

202 retains the actual shared operation ID without polling in the SDK leaf.
The common get observer can observe that original handle and applies the same
CopyJob validation to its completed result. Denial, malformed results and
response loss preserve typed failures; no metadata fallback, second POST,
timeout cancellation, rollback or synthetic success is used.
Sensitivity labels are not part of the definition. Checkpoints, credentials,
run history and source/destination data are not promised as portable source.

`copyjobs::register` adds three read-only descriptors and exact bindings to the
existing neutral v2 registry. Root and `copy-job` facade resolution return the
same descriptor; schemas, effects and native prerequisites are available to
CLI/SDK/MCP consumers. This does not install another parser or executable MCP
tool. Workspace item listing is not the selected CopyJob facade's run-list
capability. Execute, reset, lifecycle mutations, copy/move, entity discovery,
source editing and schedule management remain unregistered.

## Evidence and offline coverage

Official contracts:

- [Get Copy Job](https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/get-copy-job)
  and [List Copy Jobs](https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/list-copy-jobs),
  documentation revision `5df184c567fafea7284357607ebac79d6301a698`.
- [Get Copy Job Definition](https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/get-copy-job-definition),
  revision `3450951c8dc66a9ffd10b423b8aff5948da0006c`.
- [CopyJob definition](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/copyjob-definition),
  revision `73228d354864abcc87aa126c8c787fcb9e4204a1`.

Offline tests in `engine/sdk/tests/copyjobs.rs` assert exact routes/bodies,
UUID/type validation, field shape, metadata/source separation, optional parts,
no guessed formats, malformed/denied/lost responses, accepted handles, byte
bounds, cancellation, one-page counts/continuations and neutral projections.
`clients/cli/app/tests/get_backend.rs` covers typed root projection, native
source routing, wrong owners, denied source, unsupported facets and original
operation observation with completed-result validation. No live tenant or
credential tests are authorized or claimed.
