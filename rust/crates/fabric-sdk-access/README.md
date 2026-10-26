<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access SDK

`AccessClient::new(ApiClient)` and `AccessClient::list` implement resource-specific
ACL collections. `Resource` distinguishes workspace, administrator item access,
gateway, connection and Lakehouse data roles. The last route still uses Fabric
audience; it is not an ADLS request. List errors retain partial rows/progress.
Credentials, execution, bounds and cancellation are injected/reused.

This crate has no identity, CLI, deployment, HTTP backend, runtime or ambient
configuration dependency. Its successful actual consumer is covered through the
CLI resources integration tests with the existing shared executor. A caller
constructing a `Resource` must already have resolved and checked the source
resource support matrix. See [the source and route matrix](../../docs/access-compatibility.md).

`Resource::roles` and `mutation_request` cover only workspace/gateway/connection
ACL writes. `AccessClient::mutate_acl` executes a single typed `AclMutation`;
it does not retry writes or discover principal types. `PrincipalType::SOURCE_ORDER`
lets the command layer preserve source probing after explicit rejection.
`LabelItem`, `LabelMutation` and `mutate_label` provide administrator bulk
set/remove bodies. Actual 202 responses reconcile through the released shared
Python synchronous operation policy without replaying the write. Headerless
acceptance remains incomplete; completed 204 responses remain source-silent in
the CLI consumer rather than producing a fabricated success message.
