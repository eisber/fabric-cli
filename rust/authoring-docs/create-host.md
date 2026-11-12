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
- `anticipated_effects(plan)` discloses additional provider-native child creation
  or metered consumption using bounded `AnticipatedCreateEffect` descriptors.
  Their resource type, conditional description and contract pin come from the
  typed owner, not the host. Unknown/unrepresentable required effects must reject.
  The default empty list is only for bindings with no additional effects.

`command_for_binding(binding)` renders the existing parser grammar with the
selected owner, its exact typed path and the host-admitted policy subset.
Shared native help never names another provider or advertises replacement and
duplicate merely because a binding declares those capabilities.

Before opening a selected input file or consuming stdin, the host calls the
SDK's pure `validate_policy_support` admission. Unsupported policies reject
without input I/O. Admitted policies still read and validate all explicit input
before discovery or a possible unchanged skip result.

The binding must validate its schema, owner, body/name identity, and supported
native fields. It must independently bind original explicit bytes to the
planner's validated input, without changing multipart payloads or metadata.
Advertising a guarantee is an obligation, not evidence that the provider
supports it. An implementation must not infer race-free creation from a missing
preflight match or implement replacement by delete/recreate.
The host currently admits only native create and exact skip. It intersects
provider capabilities with that implemented subset before invoking SDK policy
validation, so even a future replacement-capable binding cannot dispatch an
unsupported replacement or duplicate through this host.

`PreparedCreateCommand::preflight(command, binding, cwd, stdin, owned_elsewhere)`
checks the selected type before reading input, arbitrates stdin, reads at most
4 MiB plus one overflow-detection byte, validates strict UTF-8 JSON with unique
keys, and preflights the existing exact no-clobber sink. Missing input differs
from empty bytes, JSON null, and an empty object. Files must be regular,
non-symlink entries; the host must own the parent namespace against ancestor
replacement. No input conversion is performed. Profiles are currently rejected,
not silently mapped to ambient identity.
Offline admission calls the SDK's `validate_create_request` without inventing
existence. Skip-only input remains valid until real discovery selects an existing
skip or rejects missing-target creation.

The first bounded selector is
`/<workspaceGUID>.Workspace/<literal-display-name>.<Type>`. The workspace must
be an explicit existing parent ID. The leaf is a literal creation name, including
when it looks like a UUID. Name-based workspace resolution, explicit collections,
folders and artifact facade spellings are not installed by this helper.
Bindings must not invent options or repurpose retrieval facet syntax.
Discovery uses the generic Core List Items route with `recursive=false` and no
`rootFolderId`, not a guessed parameter on a typed provider endpoint. The current
[Core List Items contract](https://learn.microsoft.com/en-us/rest/api/fabric/core/items/list-items)
(revision `edc068545c34ad50e4d543d40139cd360a3a2f7d`) defines this as direct
workspace items and shows root items with no `folderId` field. Within this
explicit root scope, omitted or null folder identity is root; a non-null valid
folder GUID is nested and cannot be selected as the root target. Unknown or
malformed folder identity rejects. A root candidate plus a same-name nested
candidate can select only the root; nested-only matches reject before approval
or mutation rather than becoming an unchanged result or an atomic-absence claim.
The existing paging/request limits and native create-only conflict guarantee
still apply.

`execute(binding, client, items, execution, approval, interrupted)` consumes the
prepared command. It uses the client's same immutable Fabric context for complete
item discovery, final SDK planning, approval, submission and polling. A partial,
ambiguous, wrong-kind or wrong-scope listing never authorizes creation.
Cardinality is checked within the selected owner type: an exact Notebook plus a
same-name different-kind item does not make the typed Notebook ambiguous.
Two matching Notebooks remain ambiguous; wrong-kind-only, unknown/malformed
matching identities and foreign workspace evidence still reject.
Display names must match ordinally before an existing leaf can be skipped;
case-insensitive collision detection does not prove that exact selection.
GUID scope comparisons are case-insensitive without changing names or raw input.
Completed create responses are checked again for root scope; a nested or
malformed returned folder identity cannot become confirmed completion.
Available folder fields remain in protected `meta.folderId` and
`meta.submission.folderId` with explicit field-observed flags.
`CreateApproval::authorize` receives a borrowed `CreateApprovalRequest` with the
exact plan, original input, context, remote-write effect and output/wait/timeout
intent. Its `operation()` and `url()` borrow the already-built method, bound
route, exact outgoing bytes and completion intent. Omitted input therefore
shows its actual generated body, not a prose placeholder. The same owned
operation is consumed once after approval; it is never rebuilt or changed.
`anticipated_effects()` carries at most 16 descriptors and 64 KiB of serialized
disclosure. They describe possible provider-owned effects, not child IDs or
observed outcomes, and authorize no extra HTTP call. Skip has no prepared
operation, no remote-write declaration and no anticipated creation effects.
`approval_question()` escapes terminal controls in exact body/context/effect
disclosure, including original line endings and JSON escape spelling.
Refusal dispatches nothing. There is no persisted approval, default
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
The host retains initial submission status and a bounded complete POST 202 body
independently of later polling. `meta.submission.httpStatus` is not overwritten
by the latest exchange's `meta.observation.httpStatus` or terminal operation
status. Known acceptance resource fields survive failed/cancelled polling and
malformed or mismatched final results in `meta.submission.resource`, outside
data projection. Those fields are accepted evidence, not completed resources;
the failed/unknown outcome remains nonzero and never becomes success.
An unparsed acceptance body is preserved as explicitly labeled base64 evidence.
An unexpected success-class status on an operation result remains an invalid,
unknown outcome, not a provider-confirmed rejection. The original shared-engine
typed HTTP error/status is retained independently of the receipt classification.

`CreateDelivery::report()` exposes unprojected evidence before delivery.
`deliver(stdout)` projects only `data`, retaining resource identity, policy,
action, effects, and operation handles in `meta`. Failure retains the original
`CreateReport` inside `CreateCommandError::Delivery`. Known resource identity
and operation handles remain distinct even on unverified results. Receipt
delivery does not establish remote completion.
Provider-declared `meta.anticipatedEffects` and the intended `meta.remoteWrite`
are separate from `mutationMayHaveOccurred` and observed resource evidence.

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

The provisional integration smoke's ordinary `headless` build does not enable
`v2-create` or `runtime-v2`. The Report66 conflict candidate additionally runs
bounded offline Windows tests for the shared LRO and create policy, Report SDK,
and the combined `headless,v2-create,runtime-v2` CLI. Combined root help must
disclose both Report creation and Livy listing; dispatch retains both opt-ins.
That candidate-only check is not full Report qualification or a replacement for
the native platform matrix.
