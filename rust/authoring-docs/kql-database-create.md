<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit-parent ReadWrite KQLDatabase create

The optional `fabric-cli/v2-kql-database-create` feature supplies the typed
`KqlReadWriteCreate` binding. With `headless`, it mounts the existing selected
create grammar in `fab-cli-v2`; default features, v1 `fab-rs` and its
`create`/`mkdir` alias remain unchanged.

```text
fab-cli-v2 create /11111111-1111-1111-1111-111111111111.Workspace/Database_1.KQLDatabase --input create.json
```

```json
{
  "displayName": "Database_1",
  "creationPayload": {
    "databaseType": "ReadWrite",
    "parentEventhouseItemId": "22222222-2222-2222-2222-222222222222"
  }
}
```

Both GUIDs identify resources the caller already selected. The Eventhouse is
never inferred, looked up or created. A syntactically valid parent GUID does
not prove existence, membership in this workspace, permission or capacity
readiness. Fabric must authorize the actual request. No metadata is treated as
parent write permission.

## Contract and source

The authoritative [Create KQL Database API][api] specifies
`POST /v1/workspaces/{workspaceId}/kqlDatabases`, the `ReadWrite` discriminator,
the parent Eventhouse UUID, HTTP 201/202, and the alternative `Shortcut` payload.
It requires workspace Contributor permission, supported Fabric capacity, and
delegated `KQLDatabase.ReadWrite.All` or `Item.ReadWrite.All`; supported identities
include users, service principals and managed identities. This feature does not
provision any of those prerequisites.

Source is pinned to integration base
`ced983722015e440e989f07d1bf866821d27fe90`:
`ItemCollection::KqlDatabase` selects `kqlDatabases`;
`clients/cli/commands/mutations/src/create.rs` constructs the ReadWrite
`creationPayload` and suppresses implicit Eventhouse creation for an explicit
parent. Its v1 fallback otherwise creates an Eventhouse. This v2 binding does
not call that orchestration at all. The existing consumer test at
`commands/mutations/tests/consumer.rs` covers the old fallback, not this new
explicit-parent contract.

The shared host is consumed as logical diffs, in this order, without the
Notebook leaf or whole producer ancestry:
`e84426a5145ebbf12ce4599e9ae574e8c8a7b72b`,
`ce89c0726e188c131506d12bc7342692640b9425`,
`225903a35809f70b047f4e1ffc3af403cf295b57`,
`99dc9638cd43ae532b799027abeb8fabdbbf4b38`,
`9812bec9053d2c541172223c446703109c3acbe1`.
The shared approval/submission-evidence correction is consumed next as the
logical diff of `0454fb43f08c05f9f066ed5bcb686a8c426a0de1`, not its producer
ancestry. Its Notebook-specific selected-contract documentation context is
absent from this base and is not imported; the code, tests and shared-host
documentation are adopted unchanged.
The next shared logical diff is
`caf2f7f2789438cf3abe67242a13aeb048b1ce56`, enforcing workspace-root identity
and correcting the strict lint failure without suppression. Its shared code and
host documentation are exact; absent Notebook-specific contract prose is again
not imported.
The [selected create contract](create-v2-contract.md) is unchanged; its old
unmounted-implementation inventory is historical, not a feature-support claim.

## Strict bounded input

`--input FILE|-` is required. The shared host reads at most 4 MiB of unique-key
UTF-8 JSON and preserves the bytes sent to Fabric. Input must contain an exact
`displayName` matching the literal path name and a `creationPayload` containing
exactly `databaseType: "ReadWrite"` and a nonzero GUID
`parentEventhouseItemId`. This initial native subset accepts ASCII letters,
digits, underscores, periods and hyphens in names, without renaming or case
normalization.

Optional API fields are retained verbatim: `description` (string, at most
256 characters), `folderId` (documented null/workspace root only), and
`sensitivityLabelSettings` (`labelId` GUID, optional
`sensitivityLabelApplyStrategy` equal to `ApplyOrFail` or `Ignore`).
Unknown fields and undocumented nulls reject locally. The typed path supplies
the owner; the native request does not accept a generic `type` field.
This initial path addresses the workspace root, not a folder. A non-null
`folderId`, even a valid GUID, rejects before provider access rather than
creating somewhere different from the selected destination.

Missing/malformed parent, Shortcut/cluster/source fields, definitions, queries,
ingestion, connections and provisioning requests reject before provider access.
A valid but foreign or unauthorized parent GUID is a service decision, not a
local-validation promise. No Kusto endpoint is contacted.

## Policies, approval and operation evidence

The existing parser, SDK policy planner, `PreparedCreateCommand`,
`v2_create_main`, native identity session, HTTP/LRO engine and receipt sink are
reused. The binding enables only `error` and exact-existing `skip`.
Unsupported overwrite, force and duplicate reject before file/stdin access.
Even skip requires strict valid input. Complete bounded workspace-item
discovery checks name collision, ambiguity and owner before planning. It does
not discover a parent or establish an atomic check-and-create guarantee.
A raced POST conflict is an error, never a successful skip.
Discovery uses the generic Core `items?recursive=false` route with complete
bounded pagination; the typed POST has no discovery query. Nested rows cannot
satisfy a root skip, and same-name resources of other types are not a second
KQL match. Missing versus explicit null root folder observations remain
distinguishable in protected receipt metadata. A completed result with a
nested or malformed folder is an unknown nonzero outcome, not root success.

Immediate terminal approval shows the exact target, full body, selected
account/tenant, endpoint, policy, output and effects. Piped stderr is not an
approval channel; refusal accesses no mutation endpoint. Force cannot bypass
approval. There is no new authentication, parser, transport, LRO or policy
engine, and no implicit identity/resource fallback.
The approval borrows the exact already-prepared operation, including its
method, bound URL and outgoing bytes; it does not reconstruct the request
from a summary. This binding declares no additional child-resource creation.
`anticipatedEffects` remains a disclosure, not evidence that an effect occurred
or a permission/cost-free claim.

Only one native KQLDatabase POST may be submitted. HTTP 202 is acceptance, not
completion, and an operation ID is never a database ID. `--wait` uses the
original whole-command deadline and the existing poll/result machinery.
Available status, operation handle, accepted body, resource identity and
uncertainty survive polling, cancellation, timeout, projection and output
failure. Projection touches only receipt `data`. The no-clobber sink never
replaces an existing output. Cancellation and local output errors do not imply
remote rollback or authorize replay.
Protected `meta.submission.httpStatus`, `resource` and `operationHandle`
retain the original submission separately from `meta.observation.httpStatus`
and the latest response. A failed poll's HTTP 200 must not replace the original
202 acceptance or turn it into completion. Malformed complete acceptance or
final bodies retain explicitly labeled base64 evidence, not fabricated JSON.

## Acceptance evidence

`tests/kql_database_create.rs` exercises the real parser, shared host, SDK and
scripted HTTP, with only injected synthetic credentials. The separate binary
target tests the mounted head's help, local rejections, blocked stdin, original
deadline and approval boundary. Its closed-reader pipe is established before
spawn on every platform. Both targets deny unnameable test items.

The impact graph includes default and headless KQL profiles for Windows,
Linux and macOS. Source/tests and profile registration are not execution proof.
Current full platform CI, strict all-target checks/Clippy and exact-head
independent review remain release gates. No live Fabric, Entra, Eventhouse,
capacity, query, ingestion or provisioning acceptance is claimed.

[api]: https://learn.microsoft.com/en-us/rest/api/fabric/kqldatabase/items/create-kql-database
