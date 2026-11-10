<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared resource contracts

`fabric-sdk-resource-contracts` owns service-neutral Fabric resource-operation
descriptions, completion policies, results, errors, JSON pages and service-error
wire types.

## Ownership

Use these contracts from endpoint clients and operation executors. This package
does not execute requests, poll operations, acquire credentials or own workspace
behavior. Reconciliation belongs to `fabric-sdk-operations`; endpoint/credential
binding belongs to `fabric-core`.

## Entry points

The crate root exports `ResourceOperation`, `ResourceOperations`,
`ResourceMethod`, `CompletionPolicy`, `ResourceOutcome`, `ResourceError`,
`FabricServiceError` and `Page`.

The existing `fabric_sdk_workspaces::mutations` and workspace model paths reexport
these same definitions. They do not define a second set of request/error types.
