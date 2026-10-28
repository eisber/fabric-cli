<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource mutation commands

`fabric-cli-mutations` composes workspace, item, folder and storage mutations
with explicit metadata, confirmation, execution and output capabilities.
The [resource adapter](src/frontdoor/README.md) provides root command
registrations and typed command conversion.

## Host composition

`MutationHost` consumes `ResourceOperations`, `MutationUi` and
`MutationMetadata`. Creation, updates, removal and existence checks return
`OutputInput` and ordered `CacheEffect` values. The caller applies cache effects
and refreshes affected contexts.

`FabricMutationBackend` connects handlers to the SDK's `OperationSession` and
shared HTTP executor. Generic mutations and deployment share the same operation
reconciliation policy. Credentials, prompts, configuration, clocks and streams
are always caller-owned.

Production handlers call `MutationUi::confirm_result` and
`RemovalUi::select_result`. Their defaults wrap the legacy infallible methods,
so existing adapters keep their behavior. Terminal hosts override the fallible
methods to propagate EOF, cancellation, input and writer failures as
`MutationError`; those failures are not ordinary declines or empty selections.
Force bypasses confirmation only where the source handler already permits it,
without bypassing earlier validation or reads. Storage and removal keep their
typed error and partial-result wrappers.

## Mutation behavior

- Creation validates resource support, parameters and duplicates before dispatch.
  Workspace capacity selection distinguishes omission from JSON null.
- Item creation includes the resolved folder ID. A headerless HTTP 202 remains
  accepted without inventing a created resource or completed state.
- Updates patch the selected top-level property. Workspace Spark settings use
  their dedicated endpoint rather than a wrapped workspace patch.
- Existence checks use the resolved context ID and preserve lookup failures.
- Removal distinguishes decline, acceptance, completion and hard-delete policy.
  An accepted delete is not proof that the resource is gone.

Operation failures retain IDs and mutation uncertainty. Later failures keep
earlier outputs and cache effects; neither cancellation nor output failure
causes automatic replay or rollback.

## Selection and templates

`RemovalHost` consumes a complete `RemovalCatalog`, explicit sorting and
asynchronous selection. Non-forced workspace removal deletes selected supported
items, not the workspace itself. Tenant selection remains explicit even when
force is supplied.

`create_with_template_enumeration` accepts a complete ordered template tree.
See [creation templates](templates/README.md) for path validation, dependent
creation and ordering rules.

## Storage and definitions

Storage results retain their own create/append/flush evidence through
`ResourceTransferHost`. Definition transfer and metadata mutation are distinct
operations; callers must supply the matching capabilities instead of routing
every resource through a generic fallback.

The shared error projection `operation_error_output` preserves normalized
service classifications and reconciliation evidence. Rendering and writer
failures remain explicit.

## Support boundaries

Command support depends on the resolved resource and supplied adapter. Hidden
administrative mutations, recursive storage operations, ARM lifecycle and domain
assignment are not implied by registering a verb. Resource skill headers
currently fail explicitly when requested.
