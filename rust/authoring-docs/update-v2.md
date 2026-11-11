<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Typed item metadata Update

This additive v2 slice patches existing Notebook and Lakehouse metadata. It does
not change Python `fab`, Rust `fab-rs`, legacy `set`, or their frozen expectations.
The root host and `artifact notebook` / `artifact lakehouse` facades normalize to
the same SDK `item.metadata.patch` operation and protected schemaVersion `0.1`
receipt. Registry enumeration does not grant provider permission.

```text
fab-cli-v2 update /<workspace-guid>.Workspace/<item-guid>.Notebook --input metadata.json --mode patch --plan
fab-cli-v2 artifact lakehouse update /workspaces/<workspace-guid>/lakehouses/<item-guid> --input - --mode patch --plan
fab-cli-v2 artifact lakehouse update /<workspace-guid>.Workspace/<item-guid>.Lakehouse --input metadata.json --mode patch
```

The standalone experimental `fab-cli-v2` head supports local plans and help.
It also executes existing Lakehouse metadata PATCH after explicit, default-deny
terminal approval of the immutable target, body, options, identity context,
endpoint and request limits. Root `update` uses this host only for Lakehouse.
Notebook execution still returns `ApprovalHostRequired`; its host is a separate
slice. There is no `--force`, automatic approval policy or sign-in. Embedders use
`fabric_cli::update::PreparedUpdateCommand` with explicit caller approval,
request context, shared executor and whole-command interruption. The SDK can
also be used directly through `fabric_sdk::items::update`. Neither library creates
an HTTP client, credential provider, timer or runtime.

## Input and capability boundaries

Both `--input FILE|-` and `--mode patch` are required. JSON permits only optional
string `displayName` and `description`, with at least one supplied field.
Omission preserves that field; null is rejected, not converted to omission or
assumed to clear. Empty description is passed explicitly as a string. Description
has the documented 256-character bound; display names must be nonempty and
control-free, and the service enforces the item's naming rules. Duplicate and
unknown fields are rejected, including read-only IDs/properties and definitions.
The finite local input/output cap is 4 MiB, reusing the shared response bound.

Targets are explicit typed UUID metadata paths. No names, globs, bulk, implicit
context, untyped ID inference or child/source/settings paths are admitted in this
first slice. Additional types require their own verified endpoint/field binding.
SQLDatabase rename support is not inferred from a returned displayName.

`replace`, supplied `--if-match` (including empty), and `--wait` are rejected.
The metadata APIs document synchronous 200 responses and do not advertise an
If-Match guarantee or observer. No local hash, logical ID, metadata pre-read or
read/merge/write is presented as optimistic locking. Approval and plans retain
`ExistingOnly` intent and `UnguardedConcurrency` explicitly. Existing-only is not
an atomic client-side existence check; the native item PATCH enforces the
addressed ID, and NotFound never creates it.

Only JSON output is accepted. `--output/-o FILE|-` is exact and no-clobber; stdout
is the default. The host preflights output before approval or remote mutation,
then uses the existing exclusive-create file adapter at delivery. A racing file
is never overwritten. `--project/-q` affects data only, never receipt metadata,
identities, conditions or handles. The default total timeout is 30 seconds;
positive integer ms/s/m/h durations up to 24 hours are accepted. Embedded callers
must enforce this whole-command budget through interruption and delivery checks.
The standalone local plan does not install a cancellable stdin worker.

Native Lakehouse execution reads bounded input on a blocking worker, preflights
output, captures the explicit headless identity and Fabric endpoint, and renders
the context-bound request on stderr before approval. Confirmation reads the
attached controlling terminal (`CONIN$` or `/dev/tty`), never JSON stdin. An empty
answer declines; absent terminal, EOF and prompt errors send no PATCH. Set
`FAB_RUST_AUTH_MODE` using the existing headless host configuration; approval
does not grant provider permission or request an interactive login.
On Windows, select `--output FILE` or redirect stdout: duplicated raw Windows
console output cannot preserve Unicode in this bounded host.

One original deadline covers input, approval, submission and delivery.
Cancellation stops local waiting, not remote work. Blocking OS input/output may
outlive its abandoned worker; runtime shutdown is bounded and late output may
still finish. Approval and diagnostic output use owned OS handles and ASCII JSON
escapes (including surrogate pairs), preserving Unicode values without a global
stream lock. At expiry, runtime cleanup and best-effort diagnostic delivery each
have at most 100 ms of grace; a stalled diagnostic cannot prevent process exit.
A delivery timeout/cancellation retains the protected receipt and
never rolls back or replays. Only confirmed success exits zero; accepted,
partial, failed and unknown receipts remain intact and exit nonzero. This
stateless executable holds no cross-command metadata cache; no requested rename
is installed speculatively. `displayName` is the native Lakehouse metadata
rename contract, not a move, OneLake directory rename, or Tables/Files mutation.

## Approval, effects and outcomes

The trusted caller policy receives the immutable exact target, typed body,
options, context and shared request limits. Approval must borrow that request;
another request's approval is refused. A bound invocation consumes its only
submission attempt before awaiting execution, including dropped execution futures.
This prevents library replay, not deliberate independent invocations by a host.
Provider write permission and host approval remain independent.

Execution reuses `ItemMutations::update_with_completion` and
`OperationSession::with_context`: one type-specific PATCH, no pre-read, create,
definition update, polling, retry after uncertainty, or rollback.
Only a 200 response confirming the same workspace/item/type and requested values
establishes success. Unconfirmed fields are partial; invalid identity/response
and lost responses are unknown. Unexpected 202 retains an available real
operation handle as acceptance; without a handle it is unknown, not completed.
This defensive handling does not advertise an LRO for the metadata APIs.
412 is PreconditionFailed with no fetch-and-overwrite. 403 is denial, not absence.
Established input, authentication, authorization, missing-target, precondition,
and throttle rejections (400/401/403/404/412/429 without an accepted handle) remain
failed receipts. After dispatch, 500/502/503/504 and other unadvertised statuses
are unknown when the shared engine retains possible-mutation evidence. A status
and an error body do not prove the metadata write was rejected before effects.
The receipt retains the actual HTTP status and uncertainty without inventing a
handle, replaying, or claiming the resource stayed unchanged. Safe receipt codes
are independent of raw provider error codes; typed causes retain the originals.

Host-owned caches should invalidate affected metadata and path aliases after a
dispatched mutation, especially accepted/partial/unknown outcomes. Do not apply
the requested new name as a confirmed service value or claim cross-process cache
invalidation. The library applies no speculative cache effects. A local writer
failure after execution retains the original receipt and handle evidence;
delivery failure does not undo remote success.
Implicit `Debug` on Update reports, receipt-build errors, deliveries and host
errors emits only safe classifications and evidence summaries, not returned
metadata, raw provider diagnostics or native writer messages. Unconfirmed foreign
metadata and provider errors remain available through explicit typed inspection,
including after a writer failure. JSON receipts continue to withhold unconfirmed
foreign data and raw diagnostics; no shared global error type is changed.

Plans use `meta.executionMode=plan`; declared effects are prospective, not applied.
No credentials, remote request or approval is needed for local plan delivery.
The shared executed receipt is used unchanged. Generated docs, cells and narrow
semantic edits cannot fall back to full-definition replacement.

The additive `PreparedUpdateCommand::with_context` / `execute_bound` seam lets
native hosts perform asynchronous approval of that exact bound request.
The existing `execute` API delegates through the same seam. The Lakehouse policy
consumes one affirmative decision and refuses changed body, target, type,
options, limits or context.

## Source and validation

Selected grouped specification: update document
`0370dbc8-a3f3-4272-b912-13111abfe926`, revision 10, and metadata page
`41acb49e-2cc0-43d7-a20f-90cfb22907c7`, revision 6. Python source mapping pin
`8ad4c703bdd6738189f59318a04cab297b6c4381` is derivation only; existing Rust
parity expectations retain `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

Native contracts:
[Update Notebook](https://learn.microsoft.com/rest/api/fabric/notebook/items/update-notebook),
[Update Lakehouse](https://learn.microsoft.com/rest/api/fabric/lakehouse/items/update-lakehouse),
and [Update Item](https://learn.microsoft.com/rest/api/fabric/core/items/update-item).
Caller permissions are item read/write and the corresponding type-specific or
Item.ReadWrite.All scope, with the Fabric audience. This is not live acceptance,
tenant permission proof, atomicity, or preservation beyond writable metadata.

Offline contracts live in `engine/control-plane/resources/items/tests/update.rs`,
`clients/cli/commands/mutations/tests/v2_update.rs`, and
`clients/cli/app/tests/update_v2.rs`. They assert exact path/body/count,
strict omission/null/duplicate fields, denied/NotFound/412/unknown/partial,
approval mismatch, pre-dispatch cancellation, no replay, local no-clobber races,
data-only projection and retained evidence after writer failures.
Cycle-one counterexamples cover ambiguous server responses and marker-based
SDK/host JSON and `Debug` redaction while verifying exact retained raw causes and
one PATCH. The incorrect new-v2 test grouping 500 with established rejections was
corrected; legacy tests and frozen expectations are unchanged.
Current full native three-platform/stable and applicable optional-feature gates
remain required before review-ready; a focused draft is not gate acceptance.
The Lakehouse host's offline tests are in
`clients/cli/app/src/v2_update_lakehouse_host_tests.rs`; executable entry,
strict-input and stalled-stdin deadline checks extend
`clients/cli/app/tests/update_v2.rs`. They use synthetic executors only, without
live reads, authentication or mutation.
