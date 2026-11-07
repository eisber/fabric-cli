<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access SDK

`AccessClient` provides resource-specific ACL collections and supported ACL and
label mutations over an injected `ApiClient`.

`Resource` distinguishes workspace, administrator item access, gateway,
connection and Lakehouse data roles. Data-role requests use the Fabric audience,
not the storage audience. Callers resolve resource identity and support policy
before constructing the request.

## Operations

`AccessClient::list` retains ordered rows and partial progress on error.
`Resource::roles`, `mutation_request` and `mutate_acl` support workspace, gateway
and connection ACL writes. `PrincipalType::SOURCE_ORDER` exposes the explicit
principal probing order for command consumers.

`LabelItem`, `LabelMutation` and `mutate_label` provide administrator bulk label
set/remove requests. Received acceptance is reconciled through the shared
operation session without resubmitting the mutation. A headerless acceptance
remains incomplete; HTTP 204 does not invent a response body.

Credentials, execution limits and cancellation are injected. The crate provides
no identity implementation, HTTP backend, runtime, CLI or ambient configuration.
