<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared opt-in create host

`fabric_cli::create_command` is enabled only by `v2-create`. It supplies bounded
JSON input, immutable preparation and immediate approval, complete bounded item
discovery, operation execution, and no-clobber receipt delivery. It is not a
generic artifact registration, provider guarantee, or new policy engine.
The existing `fabric-cli-mutations::v2_create` parser and
`fabric-sdk-items::create` planner remain the grammar and policy authorities.
Default builds and the v1 `create`/`mkdir` alias are unchanged.

## Typed binding interface

A native leaf implements `CreateBinding: CreateInputValidator`:

- `item_type()` selects exactly one case-sensitive Fabric owner type.
- `capabilities()` supplies one schema and proven provider guarantees.
- `operation(plan, raw, wait)` builds that leaf's typed native operation.

The binding must validate its schema, owner, body/name identity, and supported
native fields. It must independently bind original explicit bytes to the
planner's validated input, without changing multipart payloads or metadata.
Advertising a guarantee is an obligation, not evidence that the provider
supports it. An implementation must not infer race-free creation from a missing
preflight match or implement replacement by delete/recreate.

`PreparedCreateCommand::preflight(command, binding, cwd, stdin, owned_elsewhere)`
checks the selected type before reading input, arbitrates stdin, reads at most
4 MiB plus one overflow-detection byte, validates strict UTF-8 JSON with unique
keys, and preflights the existing exact no-clobber sink. Missing input differs
from empty bytes, JSON null, and an empty object. Files must be regular,
non-symlink entries; the host must own the parent namespace against ancestor
replacement. No input conversion is performed. Profiles are currently rejected,
not silently mapped to ambient identity.

The first bounded selector is
`/<workspaceGUID>.Workspace/<literal-display-name>.<Type>`. The workspace must
be an explicit existing parent ID. The leaf is a literal creation name, including
when it looks like a UUID. Name-based workspace resolution, explicit collections,
folders and artifact facade spellings are not installed by this helper.
Bindings must not invent options or repurpose retrieval facet syntax.

`execute(binding, client, items, execution, approval, interrupted)` consumes the
prepared command. It uses the client's same immutable Fabric context for complete
item discovery, final SDK planning, approval, submission and polling. A partial,
ambiguous, wrong-kind or wrong-scope listing never authorizes creation.
Display names must match ordinally before an existing leaf can be skipped;
case-insensitive collision detection does not prove that exact selection.
GUID scope comparisons are case-insensitive without changing names or raw input.
`CreateApproval::authorize` receives a borrowed `CreateApprovalRequest` with the
exact plan, original input, context, remote-write effect and output/wait/timeout
intent. Refusal dispatches nothing. There is no persisted approval, default
approval implementation, force bypass or second submission attempt.

The host supplies an interruption future covering the original total deadline,
including local preflight and approval; it must not start a fresh operation
budget after those phases. The existing `OperationSession` supplies bounded
201/202 execution and LRO handling. Acceptance is not completion. Cancellation,
lost responses, malformed results, failed polling, and local delivery failures
never trigger mutation replay or rollback. The caller must enforce the deadline
before synchronous delivery; an OS file/stream write already in progress is not
an atomic cancellable transaction.

`CreateExecution` bundles the operation executor with a command-local header
evidence accessor. The native runner attaches the existing `ExecutionTrace` only
to mutation/poll execution, not discovery. A valid 202 operation UUID already
received in headers survives a later body failure, response limit or cancellation.
This additional evidence never upgrades an unknown/failed report to accepted or
completed, and it never invents a resource ID.

`CreateDelivery::report()` exposes unprojected evidence before delivery.
`deliver(stdout)` projects only `data`, retaining resource identity, policy,
action, effects, and operation handles in `meta`. Failure retains the original
`CreateReport` inside `CreateCommandError::Delivery`. Known resource identity
and operation handles remain distinct even on unverified results. Receipt
delivery does not establish remote completion.

## Custody and validation

Notebook owns this minimal shared seam in the current batch. Lakehouse and Report
owners consume the exact released helper commit and supply only their own typed
bindings, validators, capability evidence, dispatch selection and tests. They
must not duplicate policy, HTTP, identity, approval or output engines.
No sibling provider is enabled by this helper alone.

`tests/create_host.rs` exercises the helper with a synthetic typed binding,
independently of any native Notebook leaf. Native leaves additionally need real
parser-to-host-to-SDK-to-scripted-HTTP tests and executable boundary tests.
The impact graph includes an explicit `v2-create` profile so the feature-gated
helper tests cannot silently count as an empty default target.
