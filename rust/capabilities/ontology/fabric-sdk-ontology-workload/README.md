<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Ontology workload SDK

This deterministic SDK targets the **private Digital Operations V4 workload**,
not the public Fabric item/model API. Source contract:
`workload-digitaloperations` commit
`a5513205ddd862d450f14a6fdd14e78b40576719`, especially
`Controllers/V4/Ontology`, `API/Gen4/Specs/scripts.tsp`, and the V4 RDF controller,
constants and import/export managers. Source existence is not rollout,
authorization, flight or deployment evidence.

## Shared execution contract

`workload::{WorkloadIdentity, WorkloadRoute, WorkloadMethod, WorkloadRequest,
WorkloadResponse, WorkloadError, WorkloadErrorKind, DispatchState,
WorkloadFuture, WorkloadExecutor}` is the one reusable execution boundary.
Other private-workload consumers (including DataAgent) reuse it rather than
define a second transport. Identity is a logical identifier, never a URL.
Routes are relative, built from individually encoded segments and query values.
Requests expose method, content type, exact bytes and streaming response bound.
The executor is injected by the caller and performs **exactly one dispatch**.
It must enforce the response bound before buffering, reject ambiguous duplicate
headers, and return original statuses, headers and body bytes, including 202
and failures. Errors distinguish not-sent, accepted, completed and unknown
dispatch. No error implies rollback. Do not automatically replay mutations.

No HTTP implementation, token exchange, credential discovery, sign-in, fixed
host, redirect following, poller, runtime, query, embedded model or agent is
provided. The caller owns authenticated private routing, explicit authorization,
cancellation/deadline policy and reconciliation. Dropping a future cannot prove
remote cancellation or absence of effects.

## Integration ownership

The package uses only existing centralized `serde`, `serde_json` and test
`tokio` dependencies. Workspace membership discovers `capabilities/*/*`.
Coordinator-owned root manifest delta: add
`"capabilities/ontology/fabric-sdk-ontology-workload"` to `default-members` and
`fabric-sdk-ontology-workload = { path = "capabilities/ontology/fabric-sdk-ontology-workload" }`
to `[workspace.dependencies]` when composing consumers. Refresh `Cargo.lock`
under the coordinator's shared build slot. No root manifest/lock edits belong
to this lane.
