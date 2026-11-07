<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# V2 operation registration contracts

This additive, pure core crate registers operation IDs, schemas, declared effects
and positively enumerated provider bindings. An artifact facade is a type
constraint over the same operation, not a duplicate implementation or MCP tool.
SDK, CLI and MCP consumers can inspect the same descriptors without starting a
runtime, finding credentials, executing a provider or depending on one another.

Targets are already resolved selections: exact owner type, registered facet,
native representation and observed generation. This crate does not select a
filesystem path grammar, resolve display names, construct URLs, authorize requests
or implement an execution head. Unknown generations do not select a known
generation's schema; generation-independent support must be declared explicitly.
Overlapping bindings for the same action are rejected instead of using first
match or fallback routing.

Existing items, workspace-owned item collections and tenant collections have distinct identity
kinds. An item-create target retains its actual parent workspace without
inventing an item UUID before creation. Scope is part of capability selection;
an item-only binding cannot silently authorize a collection or vice versa.
Tenant discovery retains an explicitly selected tenant UUID, never a synthetic
workspace. The common identity's workspace accessor returns `None` for a tenant
collection; item-specific adapters must require their actual workspace.

`resolve` is discovery, **not execution approval**. Its result retains conditional
prerequisites and explicit unsupported reasons. An execution adapter must check
the binding and caller/host policy before dispatch. Registered effects describe
the operation; they do not grant permission to perform it. Registration alone
does not establish native transport availability, deployment or command parity.

`PreparedOperation` binds the discovered capability to immutable resolved
identity, input and effects. The caller's mandatory `ApprovalPolicy` sees that
exact value, including conditional prerequisites. `ApprovedOperation` borrows
it for immediate use: it cannot be deserialized, persisted as a reusable grant,
or rebound to a different request. Typed operation adapters still own input
validation and native authorization. There is no default allow policy.

Descriptors distinguish required effects from explicitly selectable effects.
An output-file request can select declared `LocalWrite` before approval without
inventing a second operation ID; preparation cannot remove required effects or
add an undeclared remote effect. Output reservation and the exact local path
remain the host adapter's responsibility.

`ExecutionReceipt` retains target/effects/evidence outside projected result data
and distinguishes succeeded, accepted, partial, failed and unknown outcomes.
Acceptance requires a provider handle. Incomplete coverage, HTTP denial and
acceptance-only responses cannot become healthy completed output. Fabric
operations, conversations/turns and MCP task handles retain distinct namespaces.
No constructor creates a provider handle, cancellation, rollback or retry.

The existing v1 executable, path resolver, output and independent oracle are
unchanged. Invocation/receipt adapters and provider operation implementations
remain separately owned; this crate does not install an execution head.
