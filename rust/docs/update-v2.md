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
```

The standalone experimental `fab-cli-v2` head supports local plans and help.
It intentionally refuses mutation execution with `ApprovalHostRequired`; it does
not install an automatic approval policy or sign in. Embedders use
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

Host-owned caches should invalidate affected metadata and path aliases after a
dispatched mutation, especially accepted/partial/unknown outcomes. Do not apply
the requested new name as a confirmed service value or claim cross-process cache
invalidation. The library applies no speculative cache effects. A local writer
failure after execution retains the original receipt and handle evidence;
delivery failure does not undo remote success.

Plans use `meta.executionMode=plan`; declared effects are prospective, not applied.
No credentials, remote request or approval is needed for local plan delivery.
The shared executed receipt is used unchanged. Generated docs, cells and narrow
semantic edits cannot fall back to full-definition replacement.

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

Offline contracts live in `sdk/items/tests/update.rs`,
`frontends/cli/commands/resource-mutations/tests/v2_update.rs`, and
`frontends/cli/app/tests/update_v2.rs`. They assert exact path/body/count,
strict omission/null/duplicate fields, denied/NotFound/412/unknown/partial,
approval mismatch, pre-dispatch cancellation, no replay, local no-clobber races,
data-only projection and retained evidence after writer failures.
Current full native three-platform/stable and applicable optional-feature gates
remain required before review-ready; a focused draft is not gate acceptance.
