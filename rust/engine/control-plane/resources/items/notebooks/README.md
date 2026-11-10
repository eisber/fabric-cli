<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Notebook service client

`fabric-sdk-notebooks` owns Notebook definition models, service requests and
bounded discovery/create/update sessions.

## Ownership

Notebook service behavior lives here, not in the convenience SDK facade.
Repository preparation, parameter transforms and publication workflows belong to
[`fabric-deploy`](../../../deployment/README.md). HTTP execution and long-running
operation reconciliation reuse caller-injected shared policies.

## Entry points

`NotebookSession::new` takes a caller-owned `RequestContext`, `NotebookExecutor`,
limits and cancellation future. Construction performs no I/O. `models.rs` owns
definition, request/response, executor and error contracts; `client.rs` owns
service/session implementation.

Existing consumers can keep using `fabric_sdk::notebooks` and
`Client::notebook_session`; they forward to these exact types. Direct consumers
do not depend on the SDK facade or an identity provider.

## Execution contract

Discovery must be complete before deciding to create an absent item. Requests,
response bytes, discovery results and polling delays are bounded. POSTs are not
replayed after lost responses; acceptance, cancellation and completed effects
remain distinct. The package creates no runtime, transport or credential source.
