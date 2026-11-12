<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native v2 CopyJob creation

The opt-in `v2-copyjob-create` feature mounts only metadata-only CopyJob creation
in `fab-cli-v2`, over the shared `v2-create` host. Build the executable with
`--features headless,v2-copyjob-create`. Default/v1 `fab-rs` behavior is unchanged.
This creates an empty item, **not a configured runnable copy job**.

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.CopyJob
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.CopyJob --input metadata.json --wait
```

The workspace is an explicit existing UUID. The name is literal, even if it
looks like a UUID. Workspace names, folders, collections, profiles, artifact
facades and source-copy execution are unsupported. Creation requires immediate
terminal approval bound to the exact target, body, identity, endpoint and effects.
Redirected/nonterminal execution refuses before identity or service access.
The host uses the explicit existing native identity selection; it does not sign
in interactively or fall back to another account, credential or audience.

## Input and provider contract

The [official Create Copy Job contract](https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/create-copy-job)
at documentation revision `edc068545c34ad50e4d543d40139cd360a3a2f7d`
requires `displayName` and makes `definition` optional. Its metadata-only example
uses `displayName` and `description`, with HTTP 201 or 202 and Fabric LRO headers.
The caller needs workspace Contributor access, a supported Fabric capacity,
and delegated `CopyJob.ReadWrite.All` or `Item.ReadWrite.All` where applicable.
User, service-principal and managed identities are documented; actual tenant
policy and permissions remain service-authoritative and are not live-validated.

This leaf deliberately supports a narrower schema than the full provider API:

```json
{"displayName":"Exact","description":"Optional metadata only"}
```

Omitted input produces only `{"displayName":"<literal-name>"}`. Explicit input
must be one UTF-8 object with unique decoded keys, an exact matching string
`displayName`, and optional string `description` of at most 256 characters.
Empty input, null, arrays, duplicate keys (including escaped duplicates),
unknown fields and mismatched names fail before discovery or approval.
Explicit bytes are bounded to 4 MiB and sent unchanged. The SDK independently
binds those bytes to the immutable validated plan before constructing a request.
The advertised schema is `fabric.copyjob.metadata-create/v1`.

The local literal-name subset is nonblank, trimmed, at most 256 characters,
without controls, path separators, percent escaping, query/fragment delimiters
or dot traversal. Names are never normalized. These local admission limits do
not claim a complete provider naming-rules implementation; further provider
validation failures remain failures.

No `type`, `folderId`, sensitivity-label/default-identity settings, definition,
parts, source/destination, connection, job mode or schedule fields are accepted.
No connector provisioning, job submission, definition import, refresh, run,
schedule, source-copy or discovery operation is inferred from creating metadata.

## Policy, execution and evidence

The existing bounded host lists generic workspace item metadata with
`recursive=false` to establish an exact workspace-root target, retaining that
selection across complete bounded paging. Case differences, wrong types/scopes,
incomplete discovery and root ambiguities fail instead of choosing the first
item. Nested same-name items do not add root cardinality, but nested-only matches
and malformed folder identities reject before approval or mutation. Completed
201/final-200 responses must also identify a root item; actual folder evidence
remains protected as `folderId` and `folderIdObserved`, including in submission.
The SDK planner owns policy; explicit input is validated even for existing skip.
`error` rejects an existing item. `skip` reports an exact existing item unchanged,
without asserting content equality; a missing target proceeds to create.
Overwrite, `--force` and duplicate are rejected by the existing SDK admission
before input I/O, not translated into upsert or delete/recreate.

The only mutation is one POST to `/v1/workspaces/{workspaceId}/copyJobs`.
The documented `ItemDisplayNameAlreadyInUse` provider error backs collision
rejection; a preflight miss is not an atomic namespace reservation.
A racing collision, including HTTP 409, remains a service failure, never an
unchanged skip result or permission to replay. No separate HTTP or poll engine
is introduced. `--wait` reuses shared LRO reconciliation under the original
deadline; otherwise HTTP 202 remains accepted, not completed.

Approval borrows the exact prepared method, endpoint-bound URL and outgoing
body, including the generated displayName-only body when input is omitted.
The same operation is then consumed once. This metadata-only binding declares
no additional child-resource or job-execution effects; the primary item write
remains explicit. Protected `meta.anticipatedEffects` describes anticipated
effects only, never observed outcomes or fabricated child identifiers.

Receipts retain policy, selected action, resource ID, operation ID and observed
effects separately. Body/poll/output failures and cancellation retain known
acceptance handles and resource fields without claiming remote cancellation,
rollback or completion. Projection changes only `data`. Receipt files use the
shared no-clobber sink. A failed local write does not replay the POST.
`meta.submission.httpStatus`, `meta.submission.resource` and
`meta.submission.operationHandle` preserve the initial submission independently
of a later poll/result status. In particular, malformed or incomplete final
results cannot erase a known accepted resource from that protected metadata.
Malformed no-wait acceptance retains its bounded raw bytes as labeled base64,
not a completed resource or an invented ID.
As documented by the shared host, a synchronous OS write already in progress is
not an atomic cancellable transaction.

## Source mapping and validation boundary

The approved source baseline is `ced983722015e440e989f07d1bf866821d27fe90`.
Its `v2_main.rs` had no CREATE mount, and `create-v2-contract.md` recorded the
missing executable host. Existing `ItemMutations::create_with_completion` and
the CopyJob collection establish the native route. The historical Python
`test_mkdir_item_success[CopyJob].yaml` records a generic metadata POST/201 with
`displayName`, `type: CopyJob` and `folderId: null`, without a definition.
That recording is historical behavior, not proof of the current full schema or
collision guarantees. This typed leaf uses the official specialized endpoint
and does not send the generic-only `type` field or redundant null folder.

Only the authorized shared-host logical diffs are consumed:
`e84426a`, `ce89c07`, `225903a`, `99dc963`, `9812bec`, and the separately
released `0454fb4` prepared-approval/submission-evidence and `caf2f7f`
workspace-root/strict-lint updates.
Notebook leaf implementation and producer ancestry are not adopted.

`engine/sdk/tests/copyjob_create.rs` covers schema, unsupported policies and
immutable raw-byte binding. `clients/cli/app/tests/copyjob_create.rs` exercises
the actual parser, shared host, SDK and scripted production HTTP, including
201/202, collisions, exact skip, approval denial, malformed results, LRO
failures, received-header preservation, byte bounds, cancellation, projection
and no-clobber/output failures. Prepared-body approval and protected initial
submission evidence are asserted on these real paths. A root-folder regression
also rejects treating an existing folder child as an exact workspace-root skip.
Tests cover root-plus-nested cardinality, malformed folder values, complete and
failed paging, completed wrong-folder identity and protected acceptance folder
evidence. The released root helper still requires actual native execution.
Its headless profile exercises the real binary,
blocked stdin deadline and portable closed-reader stdout/stderr pipes.
The impact graph includes isolated and headless feature profiles on all three
native OSes. Configured tests are not evidence of execution: current local,
native CI and independent exact-head review results remain separate handoff gates.
No live Fabric, Entra, credential, data, connector or compute operation is used.
