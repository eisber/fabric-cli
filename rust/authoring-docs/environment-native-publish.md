<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Environment native publishing

`fabric_sdk::environment_publish` implements the owner-selected native
Environment slice on the v2 base `cc183dd43140350eb1f23af0636332150d1f6d50`.
It does not replace v1 deployment. CLI/SDK/MCP
hosts can register the one operation and use the same prepared request, immediate
approval and protected receipt. Global CLI grammar and host credential composition
are shared with the existing explicit native head. `prepare` takes the actual SDK client and returns an opaque
`PreparedPublish`, retaining its immutable request context. Host policy can inspect
`operation()` and `context()` before approving. `publish` consumes the resulting
`ApprovedPublish` and rejects a different endpoint, account, tenant, audience or
credential provider before acquiring a token or dispatching HTTP. Cloned matching
contexts remain valid. Neutral registry intents must pass
`PreparedPublish::from_operation` validation before context-bound approval; an
unbound `ApprovedOperation` cannot authorize publication. Context provenance is
not serialized into receipts, and no token claims are inferred from metadata.

## Existing-staging executable

The `headless` build of `fab-cli-v2` mounts the native adapter:

```text
fab-cli-v2 publish /<workspace>.Workspace/<existingEnvironment>.Environment [--wait]
```

The selected publish root is `2b2dc086-3d84-4961-8269-3710a7d94441`
revision 10; its Environment child is `042396ff-29f8-46da-b890-6703f99ee512`
revision 4 (`environment-p0nfXJcd7c`). The public target is the existing
Environment item, not an item `/staging` path. Staging is the native effect
selected internally, as confirmed by the specification owner.
Only the selected `--wait` option is admitted. No `--input` is advertised for
already-saved staging, and no new publish payload, approval flag or plan/apply
protocol is introduced. Python `fab`, v1 `fab-rs` and other publish targets are
unchanged.

Names use the existing bounded exact typed-target resolver. ID-qualified typed
paths avoid enumeration; both forms verify the existing native Environment's
workspace, item ID and type before approval. Incomplete/ambiguous discovery,
denial or an identity mismatch prevents submission. Observation retains these
IDs and the originally returned version rather than resolving names again.

Identity and transport use the existing `FAB_RUST_AUTH_MODE` headless selection
and optional validated `FAB_RUST_FABRIC_HOST`. No login, identity fallback,
static-token shortcut or alternate HTTP runtime is installed. Approval requires
an interactive terminal and an explicit entered yes; EOF and empty input do
not approve. The prompt identifies the resolved target, Fabric endpoint,
account, tenant, audience, publication effect and unguarded concurrency. The
opaque SDK grant binds the same immutable client context. Approval does not
grant write permission or establish delegated scopes/tenant policy: the native
write request must independently be authorized by Fabric.

One existing API-host budget (`v2_policy::Limits::default().timeout`, currently
240 seconds) covers identity setup, discovery, verification, approval,
submission and observation. It is not a new service timeout or publish grammar
default. Both shared LRO and HTTP delay limits admit the documented 120-second
Retry-After; no service delay is shortened to fit the budget. Expiry stops local
observation with incomplete evidence, not remote cancellation.

With `--wait`, generic 202 reconciliation uses the SDK's shared LRO machinery,
without a result GET. Native 200 Running/Waiting/Cancelling with an actual
targetVersion uses `observe_version` through the shared observation driver,
honoring Retry-After (or its existing one-second read-observer cadence). Missing
versions, conflicting component states and denied/version-swapped observations
cannot establish completion. No-wait native running state remains Partial,
not an invented generic operation handle.

JSON receipts retain the original protected SDK receipt under
`meta.submission`; native version observation is additional host evidence, never
a rewrite of that original submission. Accepted/succeeded return exit 0;
failed/partial/unknown and delivery errors return nonzero. Receipt write/flush
failure preserves the known outcome and handle on stderr where possible, and
never repeats the POST.

### Transport dependency and remaining acceptance

PR 47 owns the HTTP 411 transport repair for empty-payload requests. This change
does not edit or duplicate that repair. The SDK request payload remains absent
(zero bytes), never `{}`. Offline injected HTTP checks do not prove an actual
service accepts the pre-47 transport framing. Live acceptance depends on the
reviewed transport fix and separate authorization; no live authentication,
publication, resource mutation or compute is part of this implementation.

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
operation handle. The adapter first dispatches with ReturnAccepted, validates
the exact initial 202 (including its required empty body), and retains that
response separately before shared reconciliation. Malformed/nonempty acceptance
is Unknown for both observation modes and is never followed by polling.
Waiting observes completion without a result GET for this
result-less action. A failed polling GET cannot establish failure of the original
publication. Timeout/cancellation/response loss retains uncertainty and any known
handle; no mutation replay, cancellation or rollback follows.

Native documented rejection statuses (400/401/403/404/409/429) and an observed
terminal LRO failure are classified as Failed. Unsupported statuses, redirects,
partial HTTP responses, server errors and failed post-acceptance reads are Unknown
with incomplete coverage and possible-mutation evidence, not certified failure.

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

The inspected PublishDetails and ComponentPublishInfo response tables do not mark
component fields as required. Overall native `Success` remains authoritative when
component information is absent or empty; supplied contradictory, malformed or
unknown component states still prevent complete success. This preserves the
documented optional response contract rather than inventing a mandatory field.

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

The SDK remains host-neutral. The explicit native command adds binary grammar
and injected-host wire/approval/receipt tests; it is not a production permission
or capability claim. Current complete native three-platform/stable gates and
authorized live acceptance remain required before review-ready.
DataAgent/Airflow Preview and Report public publishing are not registered here.
