<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Environment native publishing

`fabric_sdk::environment_publish` implements the owner-selected native
Environment slice on the v2 base `cc183dd43140350eb1f23af0636332150d1f6d50`.
It does not replace v1 deployment or enable a new executable command. CLI/SDK/MCP
hosts can register the one operation and use the same prepared request, immediate
approval and protected receipt. Global CLI grammar and host credential composition
are not changed.

## Native contract

The release method is
`POST /v1/workspaces/{workspaceId}/environments/{environmentId}/staging/publish?beta=false`
on the caller's validated Fabric endpoint. The request body is absent, not `{}`.
Only already-saved staging state is published. Saving an authoring representation,
definition replacement, library upload, pipeline deployment and Report
publish-to-web are different operations, never implicit prerequisites.

Official sources:

- [Publish Environment](https://learn.microsoft.com/en-us/rest/api/fabric/environment/items/publish-environment),
  documentation source `1f81e1a95f5ed29a327baa3b83de48f8f26e7aff`.
- [Get Environment](https://learn.microsoft.com/en-us/rest/api/fabric/environment/items/get-environment),
  documentation source `5df184c567fafea7284357607ebac79d6301a698`.
- [Fabric LRO](https://learn.microsoft.com/en-us/rest/api/fabric/articles/long-running-operation)
  and [identity support](https://learn.microsoft.com/en-us/rest/api/fabric/articles/identity-support).

Write permission and delegated `Item.ReadWrite.All` or
`Environment.ReadWrite.All` apply to publishing. Users, service principals and
managed identities are documented, subject to tenant policy and native
authorization. A host approval policy must verify prerequisites before dispatch;
registration, metadata and fixtures do not establish those permissions.
Credentials are injected for the Fabric audience; no login or fallback occurs.

Preparation explicitly declares `ExistingOnly` and `UnguardedConcurrency`.
The API has no caller-supplied revision fence. Another ongoing publish may be
rejected by the provider; this is not permission to replay or invent a locking
token. No stale version is silently adopted.

## Acceptance and completion

Hosts explicitly choose `ReturnAccepted` or `WaitForOperation`; there is no new
default wait or numeric bound. Caller `OperationLimits` and the executor's
deadline/delay limits remain authoritative. The official 202 example requests
120 seconds: supply compatible limits or retain a classified incomplete result,
never shorten the service delay.

For 202, the shared operation engine retains and validates the genuine generic
operation handle. Waiting observes completion without a result GET for this
result-less action. A failed polling GET cannot establish failure of the original
publication. Timeout/cancellation/response loss retains uncertainty and any known
handle; no mutation replay, cancellation or rollback follows.

For 200, `publishDetails.state` is authoritative, not the HTTP status:

| Native response | Receipt |
|---|---|
| Success, with supplied components agreeing | Succeeded |
| Waiting / Running / Cancelling | Partial, with explicit incomplete publication |
| Failed / Cancelled | Failed, not rollback |
| Unknown/malformed state or inconsistent response | Unknown or Partial, never complete |

The common Accepted receipt requires a genuine operation handle. Native 200
does not advertise one; its `targetVersion` is **not** a Fabric operation ID.
Therefore active native publication uses Partial with retained native state.
Version and state also remain in unprojected receipt source evidence, while
the execution result retains the exact response and original operation error.
Absent versus null native fields remain distinguishable in raw details.

`observe_version` makes exactly one GET of the selected Environment and verifies
its owner IDs, type and exact original targetVersion. Hosts schedule further
reads using their existing timing policy; this adapter installs no poller/runtime.
A concurrent new version, denied read or unknown state is an explicit error
retaining the observed response. No latest-version fallback proves completion.

## Scope and validation

Offline tests cover exact route/body/count, approval refusal and substitution,
states/components/version races, 202 handles and LRO failure, response loss,
denied polling, finite limits and no replay/cancel. They use injected credentials
and execution; no live Fabric publishing is performed.

This is a host-neutral SDK slice, not complete command acceptance or a production
permission/capability claim. Executable composition, owner-approved grammar and
current native three-platform/stable gates remain required before review-ready.
DataAgent/Airflow Preview and Report public publishing are not registered here.
