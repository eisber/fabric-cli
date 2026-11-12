<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Create v2 contract

This document records owner-approved creation policy behavior and the separate
CLI v2 implementation contract. Precise option grammar, exit and receipt-envelope
details, timing, and source revision pins are implementation-contract details,
not a claim of explicit owner approval for every broader user experience choice.
It is not a general command reference for the current executable. The default Rust
executable remains v1 `fab-rs`, where `create` is an alias for `mkdir`.

The separate public library contracts are `fabric-sdk-items::create` and
`fabric-cli-mutations::v2_create`. Source implementation is available for SDK
policy and CLI parsing; compilation and tests remain unverified.
The opt-in `v2-create-eventhouse` feature now supplies the bounded standalone
Eventhouse mount in `fab-cli-v2` with `headless`; see
[Eventhouse creation](eventhouse-create.md) and the [shared host](create-host.md).
This does not establish support for other types or all policies.

## Target grammar

The target grammar is design notation, not a runnable command example:

`fab-cli create <destination-or-parent-collection> [--input FILE|-] [--wait] [--force | --if-exists error|overwrite|skip|duplicate] [--format json] [--output/-o FILE|-] [--project/-q EXPR] [--profile NAME] [--timeout DURATION]`

In this notation, `--output/-o` means either `--output` or `-o`, and
`--project/-q` means either `--project` or `-q`. The `--if-exists` option takes
one of the four listed policy values. These options do not change the v1
`mkdir` contract.

| Argument or option | Target meaning |
| --- | --- |
| `<destination-or-parent-collection>` | Select the exact destination or an explicit parent collection, as supported by the resource schema. |
| `--input FILE|-` | Read the creation body from a file, or from standard input with `-`. Validate explicit input before choosing a no-write result. |
| `--wait` | Wait for operation completion within the total timeout budget. Acceptance alone is not completion. |
| `--force` | Request explicit `overwrite` policy. Conflict with every explicit `--if-exists` value, including `overwrite`. |
| `--if-exists error|overwrite|skip|duplicate` | Request one policy, subject to the resolved resource capability. |
| `--format json` | Select the JSON receipt format, not a resource content or media format. |
| `--output`, `-o` | Write the receipt to the selected file, or to standard output with `-`. |
| `--project`, `-q` | Project only receipt `data`, retaining metadata, resource IDs, effects, and operation handles. Do not change mutation input or destination selection. |
| `--profile NAME` | Select the named CLI profile; profile selection is not an SDK dependency. |
| `--timeout DURATION` | Set a positive integer duration with an `ms`, `s`, `m`, or `h` suffix, up to 24 hours. |

Without `--wait`, the default timeout is 30 seconds for submission. With
`--wait`, the default is five minutes total, including submission and waiting.
Zero, negative, fractional, unitless, malformed, and over-limit durations are
invalid; changing the unit does not permit exceeding the 24-hour limit.

### Destination and body

An exact destination identifies the intended resource. An explicit parent
collection identifies where to create a resource, with the new display name and
type selected by the resource schema and input body. Do not infer a `--name`
option or add path-selector syntax that is absent from the target grammar.

Resource paths retain type suffixes, such as
`WorkspaceName.Workspace/ItemName.Lakehouse`. This notation does not establish
which resource types or collections implement each policy. The resolved schema
and capability must support the requested destination form and operation.

Preserve raw input bytes before any declared format conversion. Do not silently
normalize line endings, change encoding, or reserialize structured content.
An explicitly advertised, deterministic format conversion can transform bytes
as defined by the resource contract.

Distinguish missing input, empty bytes, and invalid input. Missing input is
legal only where advertised, and empty bytes can be legal under an explicit
resource content contract. Reject missing or empty input when the target schema
does not permit it; never accept a regular file as empty definition parts without
reading its content.

## Existence policies

Policy support is explicit for each resource capability, not a property shared
by all Fabric APIs. Validate the requested policy before mutation, including
when the target is missing. Reject an unsupported policy rather than silently
creating, patching, deleting, or selecting a different policy.

| Policy | Exact existing target | Missing target |
| --- | --- | --- |
| `error` | Reject the collision without mutation. | Create when the capability supports the request. |
| `overwrite` | Replace all advertised create-managed content while retaining resource identity and unrelated settings. | Create when the capability supports the request. |
| `skip` | Return an unchanged result without mutation or a content-equality claim. | Create when the capability supports the request. |
| `duplicate` | Create a separate resource through a supported explicit parent collection. | Create a separate resource through a supported explicit parent collection. |

### Overwrite and force

`--force` is an alias for explicit overwrite, not a confirmation-only switch.
Overwrite replaces the complete content advertised by the resource's create
contract; it is not a partial property update. The capability must identify
that replacement scope and support replacing it while retaining identity.

Overwrite preserves unrelated permissions and settings outside that advertised
scope. It must not delete and recreate the resource, substitute a new identity,
or claim full replacement after updating only selected properties. Reject the
request before mutation when the backend cannot satisfy this contract.

### Skip

For an exact existing target, skip reports that the operation leaves the
resource unchanged. It does not assert that the existing content equals the
requested content. For a missing target, skip follows the supported creation
path rather than returning an unchanged result.

Explicit input must pass schema and body validation before skip can return
unchanged. An existing resource does not hide malformed input, an invalid
requested type, or an unsupported policy. Skip does not authorize an update or
an implicit comparison operation.

### Duplicate

Duplicate requires an explicit parent collection whose capability permits
repeated display names. It always requests a new identity, including when no
resource has the requested display name. The schema and body select the new
resource's name and type.

Duplicate does not resolve an exact destination to an existing identity. It
does not automatically rename a resource, generate a unique display name, or
bypass a unique-key constraint. Reject a destination or capability that cannot
support repeated display names rather than changing the requested name.

## Validation and mutation boundaries

Validate argument conflicts, explicit input, destination shape, schema, and
requested policy support before any mutation. In particular, supplying
`--force` with any explicit `--if-exists` value is invalid. A missing target
does not make an unsupported requested policy acceptable.

Creation must not implicitly publish, run, or refresh a resource. Overwrite must
not fall back to deletion followed by creation. A capability failure is a
rejection, not permission to change semantics to obtain a successful response.

### Library and host responsibilities

The SDK create contract owns typed, reusable policy behavior without depending
on CLI crates. Callers supply credentials, transport, cancellation, and timing
capabilities. CLI argument parsing, profiles, files, streams, and receipt
presentation remain CLI or host responsibilities.

SDK schema evidence records caller validation against the resolved resource
contract. It does not establish a general-purpose JSON Schema validation
capability or automatic validation of every JSON body. Valid JSON syntax alone
is not evidence that a body satisfies the resource contract.

The CLI library contract composes those inputs without establishing an
executable mount. A root parser registration, service-host adapter, and
resource-specific backend evidence are separate integration requirements.
Passing a library test cannot establish that the current binary supports the
target command or that a Fabric API supports every policy.

## Outcomes and operation safety

Keep requested policy, selected action, completion evidence, and effects
distinct in the outcome. An overwrite request against a missing target selects
creation, while skip against an exact existing target selects unchanged.
Neither case changes the policy that the caller requested.

Apply `--project` only to receipt `data`. Metadata, resource IDs, effects, and
operation handles remain outside projection and must remain in the receipt,
including when projected data is empty or null. Projection must not discard
evidence needed to identify the resource or reconcile an accepted operation.

- Preserve the requested policy independently of the selected action.
- Distinguish acceptance from confirmed completion.
- Distinguish a service operation ID from a resource ID.
- Preserve known resource identity without inventing one from an operation ID.
- Record observed effects independently of completion or receipt delivery.
- Preserve partial results and uncertainty when a later step fails.

An accepted operation ID is not a created-resource ID or completion evidence.
Partial, unknown, cancelled, and timed-out results must retain available
operation and effect evidence. Cancellation or timeout stops local work; it does
not prove that remote work stopped.

Do not automatically replay a mutation or claim rollback after cancellation,
timeout, an unknown response, or a later failure. Failure to project or write a
receipt does not undo remote effects. `--wait`, `--format`, and `--output` must
not convert acceptance or successful output delivery into completion evidence.

### Error guidance

Keep errors actionable without suggesting unsupported behavior. Recommend
update only when the resolved resource supports the intended update, and
recommend `--force` only when complete identity-preserving overwrite is
supported. A duplicate-policy rejection must not suggest automatic renaming as
if it were the same operation.

Do not construct a suggested retry from a raw shell argument string. Use
structured option guidance or separately represented arguments, without
echoing secrets or treating untrusted values as shell syntax. Diagnostic
guidance must not cause automatic mutation replay.

## Implementation coverage and gaps

Source implementation is available, but compilation and tests remain
unverified. Source availability does not establish completed contract coverage.
The existing
[resource mutation compatibility record](mutation-compatibility.md) describes
bounded v1 behavior, not v2 create acceptance. The
[resource mutation library](../clients/cli/commands/mutations/README.md)
continues to document its existing handlers separately.

| Surface | Coverage boundary |
| --- | --- |
| Current executable | V1 `fab-rs`; `create` remains the `mkdir` alias, not this v2 contract. |
| SDK and pure policy | Source implementation available for `fabric-sdk-items::create`; compilation and tests unverified. |
| CLI library and parsing | Source implementation available for `fabric-cli-mutations::v2_create` and parsing; compilation and tests unverified. |
| Executable integration | Opt-in standalone Eventhouse metadata create; no default/v1 change or general all-type mount. |
| Resource backend | No backend proof establishes that a resource implements all requested policies. |
| Live and platform acceptance | No live service or platform acceptance evidence is supplied here; no performance claim is made. |

CLI planning delegates to the SDK planner as the single shared policy authority.
The observation envelope retains available operation handles, resource identity,
and effects through accepted, partial, unknown, failed, and rejected observations.
Later failures must not discard evidence already observed.

## Planned acceptance tests

The following cases are acceptance requirements, not tests run for this
documentation change. Use offline, synthetic inputs for policy and library
checks, then validate the separately integrated executable. Live acceptance
requires separately authorized resource scope and evidence.

### Parsing and validation

- Accept the target grammar, option aliases, file input, and standard input.
- Reject `--force` with each explicit `--if-exists` value.
- Reject unsupported policies before mutation for existing and missing targets.
- Reject malformed explicit input before an existing-target skip result.
- Preserve raw input bytes before declared format conversion, and verify any
  explicitly advertised deterministic conversion against its resource contract.
- Distinguish missing, empty, and invalid input; accept missing or empty input
  only where the target contract permits it.
- Read regular-file content rather than silently treating it as empty
  definition parts.
- Distinguish caller-provided schema validation evidence from JSON syntax
  validation or a general-purpose SDK schema capability.
- Reject unsupported destination forms without guessing a name flag or selector.
- Check positive integer timeout units, the 24-hour boundary, invalid values,
  and the distinct submission and total-wait defaults.
- Check that format, output, and projection affect the receipt, not resource media.

### Policy and identity

- Check `error`, `overwrite`, and `skip` with existing and missing targets.
- Verify that overwrite replaces every advertised content field while retaining
  identity, unrelated permissions, and unrelated settings.
- Reject incomplete overwrite support without falling back to delete and create.
- Verify that skip returns unchanged without claiming equality or writing.
- Verify that duplicate requires a supported explicit parent collection.
- Verify that duplicate creates a new identity with and without a display-name
  collision, without renaming or bypassing unique-key constraints.
- Verify that no policy implicitly publishes, runs, or refreshes a resource.

### Outcomes and integration

- Check that requested policy, selected action, completion, and effects remain
  separate for every policy branch.
- Preserve operation IDs separately from resource IDs after acceptance.
- Apply projection only to `data`, retaining metadata, resource IDs, effects,
  and operation handles even when the projected result is empty or null.
- Preserve partial and unknown evidence through cancellation, timeout,
  projection failure, and output failure without replay or rollback claims.
- Verify SDK independence from CLI parsing, profiles, and output types.
- Verify the actual root and service-host path only after it is mounted.
- Record resource-specific backend, live, and platform gaps separately from
  offline library results.
