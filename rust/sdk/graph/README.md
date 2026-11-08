<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Graph refresh SDK

`fabric-sdk-graph` prepares and executes one explicitly approved
`GraphModel/runtime/graph` refresh. It consumes the native graph-specific
submission in `fabric-sdk-jobs` and the existing `fabric-sdk::observe` job reader.
It creates no HTTP stack, identity provider, runtime, retry engine or second
poller. The [preview contract](../../docs/graph-refresh-v2.md) documents routes,
approval/effects, native handles, local deadlines and unsupported fallbacks.

Inject the same credential/endpoint context into the SDK and job clients and use
the caller's existing operation executor. `prepare` validates the ID-only target;
the shared `ApprovalPolicy` authorizes that immutable operation. `execute`
requires one caller-owned cancellation/deadline future across submission and
optional observation. Accepted work remains in errors after interruption.

The package remains separate from the facade to preserve its acyclic dependency
graph, including legacy HTTP dev dependencies. Neither the v1 job wire contract
nor the shared run foundation is changed.
