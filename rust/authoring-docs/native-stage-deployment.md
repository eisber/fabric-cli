<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native pipeline-stage deployment

The additive `fabric-deploy::stage` API and `fabric-cli::stage_deploy` consumer
implement a bounded native pipeline-stage operation. This is not the legacy
`fab-rs deploy --config` repository publication/prune workflow, a custom deployment
language, or a general source-to-workspace deployment API. V1 behavior and its
independent oracles are unchanged.

## Supported first slice

The SDK accepts exact pipeline and source/target stage UUIDs. `StageReader`
acquires metadata from the native stage-read API, checks returned stage IDs and
requires existing assigned workspace IDs. The resulting private stage values
retain the exact credential provider, account/tenant selection and endpoint.
Preparation rejects substituted pipeline/stage/context identities, the same
workspace and nonconsecutive or backward stages. Metadata preflight is not a
service authorization or concurrency lock.

The native JSON request requires `sourceStageId` and `targetStageId`:

```json
{
  "sourceStageId": "22222222-2222-2222-2222-222222222222",
  "targetStageId": "33333333-3333-3333-3333-333333333333",
  "items": [
    {
      "sourceItemId": "66666666-6666-6666-6666-666666666666",
      "itemType": "Notebook"
    }
  ],
  "note": "Deploy selected source-stage content"
}
```

`SelectionIntent::Selected` requires 1-300 unique source item IDs and exact
Notebook, Report or SemanticModel item types in this initial adapter.
`SelectionIntent::AllSupported` requires `items` to be absent, not null or an
empty array. It explicitly selects the service's all-supported-source-items
scope. The 300-item service limit still applies to that scope; the client does
not partition it into more deployments. Notes are limited to 1024 characters.
Input bytes have an explicit caller ceiling. Unknown and duplicate input fields,
including escaped duplicate spellings and duplicates inside items, fail before
credential selection in the native executable.

No options, Beta plans, workspace creation/capacity selection, backward/skipped
stages, prune/delete/rollback, implicit dependency expansion or client parameter
transforms are supported. Unsupported input is rejected before mutation.
No `items: []`/null meaning is inferred from undocumented native behavior.

## Native API, identity and effects

The adapter uses Core v1:

| Purpose | Method and native path | Behavior |
|---|---|---|
| Exact stage preflight | GET `/v1/deploymentPipelines/{pipelineId}/stages/{stageId}` | Native stage UUID, order and assigned workspace, not a name search |
| Deploy | POST `/v1/deploymentPipelines/{pipelineId}/deploy` | Immediate native result or 202 acceptance |
| Optional observation | GET `/v1/operations/{operationId}` | Existing shared LRO state policy, not a new poller |

The SDK uses the caller-injected Fabric request context/audience and existing
`OperationExecutor`. The outer host selects and verifies the credential
provider; libraries do not prompt, acquire ambient credentials, retarget
accounts or install a runtime/transport. Pipeline admin and appropriate
source/target workspace contributor permissions are required by the service.
The deploy delegated scope is `Pipeline.Deploy`; stage reads use
`Pipeline.Read.All` or `Pipeline.ReadWrite.All`. Service principal/managed
identity deployment is supported only if **all involved items** support those
identities. Reading stage metadata does not prove deployment is authorized.
Additional item, capacity, enhanced-metadata and tenant constraints apply.

Fabric owns lineage-based ordering, item pairing and folder propagation.
Matching names do not establish pairing: deployment can create a duplicate
unpaired item instead of overwriting it. Required dependencies must already
exist in the target or be selected for deployment; the adapter never silently
expands selection. Native folder propagation can affect unselected target item
placement, so approval must consider more than the selected item definitions.

The host must approve the exact immutable native payload, explicit all-items
scope when selected, creates/overwrites/folder propagation and unguarded
concurrency. `PreparedDeployment::authorize` has no default "yes" policy and
returns a nonserializable, one-consumption `ApprovedDeployment`. Approval does
not grant service permissions or promise success.

## Outcomes and budgets

Submission and observation share the existing operation engine and finite
submission request/response/poll-delay budget. The caller supplies cancellation
that includes its **original total deadline**; stopping that future is not a
remote cancellation request. The outer host owns bounded input acquisition and
stage preflight and debits those reads from its overall budget. It must not reset
the total deadline when passing pre-acquired stage metadata to the CLI head.

The observer retains the actual POST response before polling, including distinct
`x-ms-operation-id` (generic LRO) and `deployment-id` (deployment history). These
IDs are not interchangeable. Missing, malformed or duplicate acceptance-ID
headers fail with uncertainty retained, before any observation. Shared operation
link validation prevents cross-origin/route substitution.

The native host also uses the existing HTTP response-progress callback to retain
the actual POST status and independently validated UUID handles as soon as the
headers arrive. A body-stream failure, overflow, cancellation or deadline cannot
erase those handles. Header-only progress retains an unknown outcome and mutation
uncertainty, not a complete response body or terminal success; it never triggers
polling or another POST. The callback stores no body prefix or unrelated headers.

The receipt's HTTP status is the submission status, not a later 200 state-read
status. A 202 receipt is `accepted`, not completed. Explicit native `Succeeded`
is required for success, and every supplied execution-plan step must corroborate
it. Failed, pending, unknown or error-bearing steps cannot be reported as a
completed successful deployment. An absent optional execution plan does not
invent a step inventory. Native failure retains the execution plan when supplied;
it is `partial` only when a native step explicitly succeeded. Missing plan
evidence is not an assertion of zero retained effects. Unknown native status,
malformed response, permission denial, cancellation and transport/deadline
failure remain explicit errors with known handles and mutation uncertainty.

Native terminal success does not mean data refreshed, app content/settings
published, credentials/permissions copied or every downstream data plane ready.
No transaction, service atomicity, compensation, cancellation API or rollback
is invented. No failed request, lost response, timeout, cancellation or failed
local output authorizes a second POST.

Cancellation after an immediate response remains the primary failure, but the
receipt retains the bounded original POST response, valid deployment ID and
observed plan effects. Foreign or malformed response identity is not adopted.
The raw response is available only as unprojected evidence: default JSON and
Debug output do not include its private service data. Retaining a completed
response does not turn interrupted local execution into a successful result.

The CLI output interface preflights before approval/submission, then delivers
one receipt. Output failure retains the original accepted/completed/failed
result. Debug formatting omits input notes and raw service bodies. The default
receipt does not contain tokens, raw headers or deployment notes.

## CLI and production binding boundary

Build `fabric-cli` with the opt-in `v2-deploy` feature to enable an isolated
head, outside the v1 grammar:

```text
fab-rs --v2-deploy deploy /.deployment-pipelines/<PIPELINE_UUID>/stages/<TARGET_STAGE_UUID> --input native.json --max-input-bytes 4096 --selected-items
```

Use `--all-items` instead of `--selected-items` only when `items` is absent from
the native request. `--wait` requests shared native completion observation.
The addressed target UUID must match the native input's `targetStageId`.
The byte ceiling is explicit. Input is a bounded regular local JSON file, not
stdin or a repository configuration. No output-file or receipt-replay grammar is
introduced.

Help/version performs no credential, configuration or input-file access. Builds
with only `v2-deploy` retain `NotConfigured` for execution. Combine `v2-deploy`
with `native-read` (Windows broker) or `headless` (explicit unattended identity)
to mount the existing native Fabric identity/HTTP host on the same `fab-rs`
marker. Identity selection and endpoint configuration follow
[native host setup](native-read-host.md); there is no ambient account search,
interactive login fallback, Python bridge or hidden live-service fallback.

The host validates bounded local input before selecting credentials, reads the
two exact stages, verifies the real assigned workspaces and requests immediate
terminal approval. Approval displays the selected identity/provider, tenant,
Fabric audience and endpoint, pipeline, source/target workspaces and exact native
body including notes and selected-items/all-items scope. The existing ASCII JSON
encoder escapes terminal controls and Unicode direction controls without losing
the native values. Only an explicit entered yes
approves; empty answers decline and EOF, unavailable terminals or failed prompt
delivery fail closed. Structured stdin is never used for approval. There is no
`--force` or unattended auto-approval, even with a headless credential.

The existing native **240-second total command deadline** starts before input
and includes credential setup, both stage reads, approval, POST, optional
observation and receipt delivery. Each stage preflight is limited to one
operation request; both are debited from the existing 100-request operation
budget. Ctrl+C only stops local work. Actual submission responses, handles and
partial effects remain in the SDK result on interruption or delivery failure.
Success/acceptance receipts go to stdout; failures go to stderr. Delivery uses
owned stream handles under that same deadline, with at most one bounded stderr
fallback, never a global stream-lock retry. If both pipes cannot accept output,
the process exits nonzero rather than hanging or claiming receipt delivery.
Runtime shutdown has the existing one-second bounded cleanup grace; abandoned
blocking local reads/writes may still complete. None authorizes remote replay.

The current generic registry's target identity types do not represent native
pipeline stages. This lane does not fabricate workspace/item identities or
change shared core contracts to make discovery claim production support.
This explicit UUID-only marker does not register general pipeline discovery or
advertise pipeline-stage support through workspace/item discovery.

## Offline contracts and provenance

The immutable source base is
`cc183dd43140350eb1f23af0636332150d1f6d50` in `eisber/fabric-cli`.
Existing Rust configuration/workflow/definition APIs remain separate source
history, not the stage-deployment implementation or a service atomicity oracle.

Official references:

- [Deploy Stage Content](https://learn.microsoft.com/en-us/rest/api/fabric/core/deployment-pipelines/deploy-stage-content):
  required stages, 300-item limit, permissions, identity restrictions, native
  response headers and separate Beta `deploymentPlan` support.
- [Get Deployment Pipeline Stage](https://learn.microsoft.com/en-us/rest/api/fabric/core/deployment-pipelines/get-deployment-pipeline-stage):
  stage UUID/order/assigned workspace and read authorization.
- [Get Deployment Pipeline Operation](https://learn.microsoft.com/en-us/rest/api/fabric/core/deployment-pipelines/get-deployment-pipeline-operation):
  deployment-history ID, native operation statuses and execution-plan steps.
- [Long running operations](https://learn.microsoft.com/en-us/rest/api/fabric/articles/long-running-operation):
  accepted versus terminal state and operation/result links.
- [Deployment process](https://learn.microsoft.com/en-us/fabric/cicd/deployment-pipelines/understand-the-deployment-process):
  pairing, dependencies, folder propagation and properties not copied.

`engine/control-plane/deployment/tests/stage.rs` exercises exact payload/routes/request counts,
stage/context ownership, all-versus-selected intent, service limits, immediate
success/partial failure, distinct handles, shared observation, denied approval,
invalid responses, service permission failures and mutation uncertainty.
`clients/cli/app/tests/stage_deploy.rs` tests the actual parsed SDK consumer,
output preflight and post-dispatch delivery failure; `stage_deploy_entry.rs`
checks default/opt-in executable isolation.

Targeted offline commands:

```text
cargo test --offline --locked -p fabric-deploy --test stage
cargo test --offline --locked -p fabric-cli --features v2-deploy --test stage_deploy --test stage_deploy_entry
cargo test --offline --locked -p fabric-cli --features native-read,headless,v2-deploy --bin fab-rs native_read::stage_deploy -- --test-threads=1
cargo test --offline --locked -p fabric-cli --features native-read,headless,v2-deploy --test stage_deploy --test stage_deploy_entry -- --test-threads=1
cargo test --offline --locked -p fabric-cli --test stage_deploy_entry
```

The impact registry includes the SDK/CLI targets, isolated `v2-deploy` profile
and composed `v2-deploy-native` profile. The native host tests run real parser,
file validation, stage SDK and existing HTTP executor against scripted HTTP;
official credential selection is exercised only against synthetic responses.
Current complete native Windows/Linux/macOS, stable and independent-review gates
remain required before review-ready publication. This document is not a claim
that those checks or live tenant acceptance have passed.
