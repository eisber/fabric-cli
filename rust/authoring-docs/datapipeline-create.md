<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native v2 DataPipeline create

The opt-in `v2-create` + `headless` build of `fab-cli-v2` mounts one native
metadata-only create binding:

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.DataPipeline
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.DataPipeline --input metadata.json --wait
```

This creates an empty item, not a configured runnable pipeline. This slice does
not submit a pipeline job, publish/update a definition or deploy content. It does
not claim that the service performs no other provisioning effects. Python `fab`,
default Rust features and the v1 `fab-rs create`/`mkdir` aliases are unchanged.

## Input and provider contract

The authoritative [Create Data Pipeline API](https://learn.microsoft.com/rest/api/fabric/datapipeline/items/create-data-pipeline)
was reconciled on November 12, 2026 (MicrosoftDocs source
`edc068545c34ad50e4d543d40139cd360a3a2f7d`). It documents
`POST /v1/workspaces/{workspaceId}/dataPipelines`, required `displayName`, optional
`description` (at most 256 characters), metadata-only creation, 201 and 202/LRO
responses, and `ItemDisplayNameAlreadyInUse`. Contributor workspace permission,
an appropriate delegated scope and supported Fabric capacity are service
requirements; offline fixtures do not establish live eligibility.

Omitted `--input` builds `{"displayName":"<literal-name>"}`. Explicit input must
be a strict UTF-8 JSON object containing that exact `displayName` and optionally
a string `description`. Empty bytes, null and `{}` are not explicit native create
requests. Description length counts Unicode characters, not UTF-8 bytes.
`type` is not a request field in the official typed endpoint; it is selected by
the case-sensitive `.DataPipeline` suffix, not accepted as an extra JSON field.
Every other field is rejected, including `definition`, `run`, job parameters,
`creationPayload`, folder and sensitivity-label settings. The last two are
official optional fields but intentionally outside this workspace-root slice.
No extra required definition fields or placeholder activities are invented.

`--input -` owns stdin; file input must be a regular, non-symlink file.
Omitted/file input never implicitly consumes stdin. Input is bounded at 4 MiB,
duplicate keys (including escaped equivalents) are rejected, and explicit bytes
are bound to schema validation and sent without reserialization.
Only `/<workspaceGUID>.Workspace/<literal-name>.DataPipeline` is accepted;
there is no workspace-name discovery, folder target or parent-collection create.
A GUID-looking leaf remains a literal display name.

## Policies, approval and outcomes

Only `error` (default) and exact-existing `skip` are supported. Explicit input
must validate even for skip. Complete bounded item discovery checks collisions;
it is not a race-free reservation. The provider's documented
`ItemDisplayNameAlreadyInUse` rejection backs the create capability; a concurrent
collision is returned without overwrite or retry. `--force` means unsupported
overwrite, not approval. Overwrite/duplicate reject before input I/O, credentials
or effects; no delete/recreate fallback exists.

The binding uses the existing SDK `ItemCollection::DataPipeline`,
`ResourceOperation` and `FabricOperation`, shared create planner/host and native
`NativeSession`, `ExecutionTrace`, approval, deadline and no-clobber sink. It does
not dispatch the v1 mutation handler or add a parser, authentication chain,
transport, pagination, polling or policy engine.
The host requires immediate terminal approval bound to the exact target, body,
selected account/tenant, endpoint, policy, output and effect. Nonterminal
execution refuses before credential/provider access. There is no unattended
approval bypass or static-token test mode in the production binary.

A 202 receipt is acceptance, not completion. An operation ID is never a resource
ID. `--wait` follows the existing bounded LRO/Retry-After handling; failed polling,
local cancellation/deadline and output/projection failure preserve available
identity, operation and uncertainty evidence without replay or remote rollback
claims. Projection changes only `data`, never protected metadata.

## Source and validation scope

The slice starts at `ced983722015e440e989f07d1bf866821d27fe90`; Python mapping
evidence is pinned at `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`
(`fab_types.format_mapping`, v1 common metadata body and typed SDK mutation
consumer). The v2 typed request deliberately omits v1's generic `type` and
`folderId: null` fields in favor of the official endpoint contract.
Only these ordinary shared-host diffs were adopted, in order, not producer
ancestry or a Notebook leaf:

```text
e84426a5145ebbf12ce4599e9ae574e8c8a7b72b
ce89c0726e188c131506d12bc7342692640b9425
225903a35809f70b047f4e1ffc3af403cf295b57
99dc9638cd43ae532b799027abeb8fabdbbf4b38
9812bec9053d2c541172223c446703109c3acbe1
```

`tests/datapipeline_create.rs` covers the actual parser -> shared host -> SDK ->
scripted `HttpExecutor` boundary, not a replacement backend.
`tests/datapipeline_create_process.rs` covers actual binary admission, deadlines,
no-clobber output, closed-reader pipes and preserved v1 aliases. These processes
do not acquire credentials or call Fabric; native mutation lifecycle is exercised
by the injected HTTP consumer. Both targets deny unnameable tests.
The impact catalog selects the binding and both explicit feature profiles on
Windows, Linux and macOS. Source/test inventory is not evidence that these tests,
three-platform CI, live Entra/Fabric creation or complete pipeline parity passed.
