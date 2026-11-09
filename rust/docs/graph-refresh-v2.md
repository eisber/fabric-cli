<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native graph refresh preview

This opt-in slice supports **only** an ID-qualified `GraphModel/runtime/graph`
target. It does not replace the v1 job API or imply refresh support for semantic
models, catalogs, SQL endpoints, tables, or other artifact types.

Compile the existing explicit v2 execution head with `headless,v2-refresh`.
Default builds and `fab-rs` keep their existing grammar and behavior.

```text
fab-cli-v2 refresh --schema
fab-cli-v2 refresh /<workspace-id>.Workspace/<graph-id>.GraphModel/runtime/graph \
  --preview --approve --timeout 600
fab-cli-v2 refresh /<workspace-id>.Workspace/<graph-id>.GraphModel/runtime/graph \
  --preview --approve --timeout 600 --wait
```

The sample timeout is an explicit caller choice, not a default. `--timeout`
requires positive integer seconds, at most 24 hours. `--preview` opts in to the
Preview capability; `--approve` approves this invocation's exact graph target
and metered execution effects. Neither bypasses provider authorization.
Schema and help are local and require no identity or network.

## Native request and receipt

The [official GraphModel background job API][refresh] specifies Fabric v1:

```http
POST /v1/workspaces/{workspaceId}/graphModels/{graphModelId}/jobs/refreshGraph/instances
```

No body or query parameter is sent. The selected caller credential requests the
Fabric audience; the API requires `Item.Execute.All` and supports user, service
principal, and managed identities. No new consent or identity fallback occurs.
The graph item and required native loading infrastructure must already exist.
This slice does not initialize it in the portal or create another item.

- HTTP 200 establishes synchronous endpoint completion without inventing a job
  ID, graph query result, or query-readiness guarantee.
- HTTP 202 is **accepted**, not refreshed. The response must contain a validated
  selected-host core item job-instance `Location` and positive integer
  `Retry-After`. The instance GUID is retained independently of request or
  generic operation IDs.
- Exactly one POST is dispatched. Throttling, transport loss, invalid acceptance,
  local interruption, and output failure never authorize automatic replay.
- Errors retain bounded response evidence, the target, and whether dispatch
  began. Oversized bodies are explicitly omitted, not success-shaped defaults.

`fabric_sdk_jobs::graph_refresh` implements the native request over the existing
job client's credential, endpoint, HTTP, headers, and response bounds.
`fabric_sdk_graph` supplies the graph-only descriptor, shared effect/approval
contract, scoped acceptance, and optional observation.

The focused `sdk/graph` package consumes `fabric-sdk` observation and
`fabric-sdk-jobs` submission without adding a reverse facade-to-jobs dependency.
It is an SDK-layer consumer, not a CLI bridge or a new HTTP/runtime foundation.

## Observation and local interruption

`--wait` consumes the existing `fabric_sdk::observe` job observer after the
submission's first `Retry-After` delay. There is no second poller, timer runtime,
metadata readiness check, generic LRO reconciliation, or result/query fallback.
The existing request/byte/delay bounds remain in force.

Only the exact accepted job is read through the core job status route. A
`Completed` job establishes successful work. `Failed`, `Cancelled`, and `Deduped`
are non-successes; deduplication does not prove this request refreshed the graph.
Unknown or partial/future status values remain explicitly incomplete with the
last native state retained by the SDK. HTTP success alone is not work success.

One caller deadline/cancellation signal spans credential acquisition, submission,
first delay, and observation. The executable uses the existing `NativeDeadline`
and its explicit native identity/runtime/HTTP host. A timeout shorter than the
first delay stops local observation without polling early. The accepted handle
and dispatch evidence remain available on interruption.

Timeout and Ctrl-C never send remote cancellation or claim rollback. No remote
cancel, generic operation result, graph query, CDF, SQL, notebook, or model
fallback is available through this command.

## Output and supported boundaries

Output is JSON only. The receipt keeps the operation, exact target, native job
handle when established, original submission status, observed state, execution
effects, and mutation uncertainty. SDK callers retain native response/status
details; CLI diagnostics use safe classifications rather than raw provider text.
An accepted response is a successful submission exit, not a completion claim.
Failed/incomplete observation is a nonzero exit with retained acceptance.
Output errors do not resubmit work.

The native executable writes received successful, failed, and interrupted
outcomes through that same root receipt contract. A nonzero exit does not wrap
the receipt under a generic `evidence` field. Genuine pre-dispatch argument,
approval, or host-configuration failures still use the generic error envelope.
If receipt writing or flushing fails, the best-effort root diagnostic preserves
the native handle, mutation flags, and original outcome status and error under
the `OutputFailure` cause. It never dispatches another refresh.

The facade's registry is one operation, not a duplicated root/tool handler.
Future hosts must apply the same target constraint and injected approval policy.
It does not advertise a universal artifact refresh or an implemented MCP host.

Offline tests cover exact requests, body absence, selected audience/endpoint,
200/202 distinctions, initial and subsequent delays, unsuccessful/unknown states,
wrong-target links, malformed headers, response limits, credential failure,
response loss, dispatch/cancellation races, approval, and once-only output.
No live refresh or platform/authentication proof is implied by these tests.

Native-entry tests also exercise the scripted POST-to-observer-to-executable
delivery path for failure, timeout/Ctrl-C after acceptance, synchronous completion,
asynchronous acceptance, observed completion, writer/flush failure, and
pre-dispatch error exits. These tests use no production identity or service.

## Source provenance

This slice starts from the authorized v2 checkpoint
`cc183dd43140350eb1f23af0636332150d1f6d50`. The official refresh documentation
reports source revision `b4d803a1e7f495553fcb7ef509391016e676d91d`.
It intentionally uses the native `refreshGraph` path, not the pinned Fabio
`RefreshGraph` query spelling or metadata polling. Existing Python/Rust v1
job wire behavior and independent compatibility oracles are unchanged.

[refresh]: https://learn.microsoft.com/en-us/rest/api/fabric/graphmodel/background-jobs/run-on-demand-refresh-graph
