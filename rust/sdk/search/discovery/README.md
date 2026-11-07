<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Neutral operation discovery

`fabric-sdk-discovery` is an SDK-layer, pure local contract catalogue for the
separately opt-in v2 discovery preview. It neither replaces the Python CLI nor
changes the default `fab-rs` grammar. It has only workspace `serde`, `serde_json`
and `thiserror` dependencies. There is no executor, CLI, identity, network,
runtime, file access, environment lookup, schema download or plugin loader.

One immutable typed registration drives the machine descriptor, CLI command/help
metadata, SDK input validator and MCP-compatible schema projection. Input
constraints are not separately hand-written for each head. The private finite
AST represents object fields, requiredness, nullability, string maxima, UUIDs,
enums, default annotations and mutually exclusive field groups. Adding a new
constraint requires updating the AST projections and their shared tests; arbitrary
JSON schemas or opaque JSON parameter signatures are not accepted as registrations.

## Initial denominator

| Target | Operation | Canonical contract ID | Supported input |
| --- | --- | --- | --- |
| Workspace | create | `fabric.core.workspace.create.v1` | Required object: `displayName` string, at most 256 Unicode scalar values; optional `description`, at most 4000; optional `capacityId` and `domainId` UUID strings |
| Workspace | update | `fabric.core.workspace.update-metadata.v1` | Required object with optional `displayName` and `description`, using the same string constraints; native metadata PATCH only |
| Workspace | delete | `fabric.core.workspace.delete.v1` | Absent body only |
| Notebook | run | `fabric.core.notebook.run-default.v1` | Absent body only; default `RunNotebook` execution without `executionData` or parameters |

These are **contract identities**, not job instance IDs or long-running operation
IDs. Contract envelope version and input schema version are `1.0`; Fabric Core API
version is independently `v1`. JSON Schema uses Draft 2020-12.

This is a supported **local structural subset**, not a full service validator.
Unknown fields and wrong/null types fail locally. Requiredness does not invent a
minimum string length: empty strings are structurally accepted where the source
only establishes a maximum, without promising service acceptance. Optional
metadata fields remain absent when omitted; `null` is not converted into absence
or an empty string. An empty update object is structurally accepted without
claiming a useful or service-accepted change. UUID validation checks exact
8-4-4-4-12 hexadecimal shape, not boolean parsing, version policy, existence or
authorization. Defaults are annotations and are never applied or inferred.

Workspace update exposes native PATCH semantics, **not an implementation of any
selected v2 update mode**. There is no generic `mode`, replacement, revision,
Spark-settings or capacity/domain-reassignment payload. Delete describes the
documented effect on the workspace **and its contained items**, but supplies no
recursive/permanent option, restoration contract or recoverability guarantee.
Notebook bodies, including `{}` and `null`, return `UnsupportedCapability`
because an exact parameter signature is unestablished. No Notebook create,
metadata update or delete registration is inferred from generic item APIs.

## Public interface

All interfaces are available directly from `fabric_sdk_discovery`:

```rust
pub enum Operation { Create, Update, Run, Delete }
// Operation::ALL: [Operation; 4] in Create, Update, Run, Delete order.
// Serialize => lowercase operation spelling.
// Operation::as_str(self) -> &'static str
// FromStr<Err = DiscoveryError> accepts only exact lowercase spellings.

pub enum TargetKind { Workspace, Notebook }
// TargetKind::ALL: [TargetKind; 2] in Workspace, Notebook order.
// Serialize/as_str => exact service type spelling.

pub struct TargetSelector { /* private, validated lexical state */ }
// TargetSelector::parse(&str) -> Result<Self, DiscoveryError>
// path(&self) -> &str
// kind(&self) -> TargetKind

pub fn discover(
    operation: Operation,
    target: &TargetSelector,
) -> Result<OperationContract, DiscoveryError>;

pub fn discover_for_facade(
    operation: Operation,
    target: &TargetSelector,
    kind: TargetKind,
) -> Result<OperationContract, DiscoveryError>;

pub fn registrations() -> &'static [OperationRegistration];
```

`OperationContract` is `Clone + Debug + Serialize`, with these methods:

- `canonical_id() -> &'static str`
- `operation() -> Operation`
- `target() -> &TargetSelector`
- `registration() -> &'static OperationRegistration`
- `input_schema() -> serde_json::Value`
- `validate_input(Option<&serde_json::Value>) -> Result<(), DiscoveryError>`
- `mcp_tool() -> serde_json::Value`
- `help() -> String`
- `descriptor() -> serde_json::Value` (also the exact `Serialize` representation)

`OperationRegistration` exposes `operation()`, `target_kind()`, `canonical_id()`,
`summary()`, `input_schema()`, `help()` and `cli_descriptor()`. CLI hosts iterate
`registrations()` in deterministic registration order and group by `operation()`
to generate common commands, then use `target_kind()` for artifact narrowing.
`cli_descriptor()` includes `command`, `targetKind`, `operationId`, `about`,
`help`, `discoveryOnly` and the generated `input` descriptor. No frontend should
maintain another operation/type/schema catalogue. A facade first rejects an exact
kind mismatch, then delegates to `discover`; its descriptor is identical to the
common operation's descriptor.

Heads that need only verb/facade names can iterate `Operation::ALL` and
`TargetKind::ALL` and use `as_str()`, without importing CLI builders or copying
vocabularies. These arrays do not advertise every operation/kind combination;
exact support remains defined solely by the registration selected by `discover`.
The separate opt-in `fab-cli-discovery` binary can use these APIs without changing
v1 `fab-rs` parsing or composing its mutation builders.

`DiscoveryError` provides `InvalidOperation`, `InvalidTarget { reason }`,
`UnsupportedCapability { reason }`, and `InvalidInput { field, reason }`.
`DiscoveryError::code(&self) -> &'static str` returns stable snake-case machine
codes. Error reasons and field labels
are static: supplied paths, unknown field names and payload values are not echoed.
`CONTRACT_VERSION`, `INPUT_SCHEMA_DIALECT` and `MAX_TARGET_BYTES` are public.

### Selector and availability boundaries

`TargetSelector::parse` accepts `/New.Workspace` and
`/Sales.Workspace/Analysis.Notebook` without requiring an existing final item.
Names may include Unicode, spaces and unambiguous dots. Suffixes are exact and
case-sensitive. URLs, controls, wildcards, escaping, empty components, traversal,
ambiguous embedded type suffixes and malformed hierarchy are rejected.
`MAX_TARGET_BYTES = 4096` is a local UTF-8 lexical safety bound, **not** a
service resource-name limit.

Unknown kinds, `/src/definition`, `/functions/name` and all other facets return
explicit capability gaps; they cannot borrow a parent resource's schema.
Discovered targets always say `selection: lexical_only`,
`resolution: unverified` and `authorization: not_evaluated`. Create's intent is a
`prospective_destination`; other operations carry `existing_target_unverified`.
Every descriptor says `v2MutationExecution: unconfigured`. Registration does not
establish a resolved identity, backend binding, permission, configured host or
service availability. Calling `discover` never invokes `validate_input` or
requires an execution body.

The descriptor additionally records backend/API/native method, media/input
presence, native format/version (unestablished values are null), effects,
prerequisites, discovery versus execution result/error boundaries, coverage,
native semantic limitations, unsupported capabilities, example and source URLs.
This contract is neither backend data schema, readable schema docs, nor a
native authoring definition.

### SDK and MCP body presence

`input_schema()` describes a **present** body. Required bodies use an object
schema; no-body operations use the standard JSON Schema boolean `false` (there
is no valid JSON value for a body). Absence cannot be represented by JSON null.
The descriptor's `input.required`/`input.presence` separately records presence.
The SDK uses `None` for absence and `Some(&value)` for an exact, unnormalized body.

`mcp_tool()` is a **projection only**, not a running MCP server, an installed
tool, or permission to execute a mutation. Its `inputSchema` uses a strict object
envelope:

```text
{}                         => SDK None
{"input": <JSON value>}     => SDK Some(<JSON value>)
```

The `input` property is required exactly when the body is required.
`inputSchema.properties.input` equals `input_schema()` byte-for-structure;
other envelope properties are rejected. Thus a delete/default-run projection
accepts `{}` as **no body**, while `{"input": {}}` and `{"input": null}` are
rejected. For create/update, `{"input": {...}}` carries the body constraints
unchanged. `_meta.fabricDiscovery` records the fixed unresolved target,
canonical ID, presence mapping and unconfigured execution. It does not claim
that these described mutations are read-only.

## Source evidence

Descriptors embed source URLs, full revisions and their limited evidentiary scope:

- Workspace create/update/delete Learn references:
  documentation commit `9feaea0cb4ae5c7942ca78399640061545c3df95`.
  The delete reference documents deleting contained items and HTTP 200 without
  establishing recovery guarantees.
- Job Scheduler run-on-demand Learn reference:
  `b4d803a1e7f495553fcb7ef509391016e676d91d`. It documents a no-body request,
  job-type-specific `executionData` and limited support for per-run parameters.
- Notebook public API documentation:
  `932298cc5ea37344bf2d10b403ac7a56ac073b8a`; execution support is not an exact
  per-notebook parameter signature.
- Existing `WorkspaceMutations` routes and `JobType::Notebook`/`submit(None)`:
  Rust baseline `5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b`.
  Their use of `serde_json::Value` is explicitly **not** schema proof.

Links are evidence only and are never fetched during discovery, validation or
tests. Native definitions/media, dynamic function signatures and complete
service policy semantics remain explicit gaps, not arbitrary JSON passthrough.

## Files and validation handoff

Owned files are `Cargo.toml`, `README.md`, `src/lib.rs`, `src/target.rs`,
`src/constraints.rs`, `src/registry.rs` and `tests/contracts.rs`.
Workspace dependency/default-member/lock/facade/CLI wiring belongs to the
coordinator, not this crate assignment.

**No Cargo build, test, lint, formatting or architecture command was run in the
SC-1 implementation session, as assigned.** Source was inspected only. Actual
all-target compilation, lint, formatting, downstream and architecture validation
must be performed by the coordinator in the serialized validation slot.
The library test target and integration test crate deny `unnameable_test_items`.
The following 16 tests are implemented but **unrun**:

Unit test in `src/registry.rs`:

- `fixture_preserves_required_default_enum_nullable_and_exclusions_across_heads`

Public-boundary tests in `tests/contracts.rs`:

- `operation_spelling_and_serialization_are_exact_and_stable`
- `prospective_and_existing_selectors_are_lexical_and_preserve_names`
- `malformed_paths_are_rejected_without_echoing_supplied_values`
- `unknown_kinds_facets_and_functions_never_use_parent_contracts`
- `exact_operation_kind_matrix_has_only_the_documented_denominator`
- `catalogue_is_unique_deterministic_and_has_versioned_source_evidence`
- `create_schema_and_validation_preserve_absent_null_and_empty`
- `string_maxima_count_unicode_scalars_not_utf8_bytes`
- `uuid_constraints_validate_structure_not_boolean_pseudo_parsing`
- `all_registered_fields_reject_wrong_types_and_unknown_fields`
- `update_is_optional_metadata_patch_not_generic_v2_replacement`
- `deletion_accepts_absence_only_and_does_not_claim_recoverability`
- `notebook_run_is_no_body_only_and_parameters_are_explicit_capability_gaps`
- `facade_discovery_is_the_identical_contract_after_exact_kind_narrowing`
- `cli_sdk_mcp_and_serialization_project_one_registration_without_side_effects`

The synthetic unit registration exercises enums, defaults, requiredness,
nullability and exclusions without claiming those fields are real Fabric
parameters. Public tests use only the exported SDK API, compare all projections,
and verify lexical/discovery state remains unchanged. This crate has no I/O
interfaces or execution host to initialize; end-to-end CLI no-file/stdin/auth/
HTTP assertions and feature-disabled/default-v1 regression tests belong to the
separate host integration assignment.
