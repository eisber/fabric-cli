<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Nonrecursive Notebook deletion and operation metadata

The additive `delete` command is a v2 preview. It does not change `rm`, `del`,
their options, or existing Python/Rust compatibility contracts.

The initial admitted target is exactly:

```text
fab-rs delete /<workspace-guid>.Workspace/<notebook-guid>.Notebook --permanent
```

Both IDs use hexadecimal, hyphenated GUIDs. Names, ambient working context,
multiple targets, other item types, representations and recursive traversal are
not admitted. The native operation uses the type-specific Notebook endpoint;
there is no generic-item fallback.

## Permanent intent and approval

Without `--permanent`, this initial adapter fails before a mutation: it cannot
guarantee recoverable deletion across tenant settings. It never silently changes
that request into permanent deletion. This intentionally limits usefulness for
callers who can soft-delete but lack permission for permanent deletion.

`--permanent` requests `hardDelete=true`. Generic item documentation requires
Workspace Admin for hard deletion; the Notebook-specific reference states
notebook write permission. The service remains authoritative and permission
denials are preserved. No client-side role claim grants authority.

Permanent item deletion is irreversible at the item lifecycle level. It is not
a promise of immediate physical erasure of files or a retention-setting change.
Recovery and file retention are distinct service policies.

The native host additionally requires interactive approval of the exact target,
effects and selected context. Noninteractive native callers fail before deletion;
there is no `--force`, `--approved` or automatically approving agent path. SDK
hosts may inject their own explicitly authorized approval policy. Approval is
bound to one immutable assessment and cannot be reused for a different request.

## Capability limits and observation

`--recursive=true` and a supplied `--if-match` fail before mutation. The admitted
Notebook delete API does not advertise conditional revisions. An existence
lookup cannot manufacture optimistic concurrency, and no condition is ignored.

Only HTTP 200 is documented as terminal success for this endpoint. Defensive
handling of service acceptance preserves that distinction; it is not a claim
that Notebook deletion advertises asynchronous execution. `--wait` uses the
existing bounded operation engine only when service observation is available.
There is no invented cancellation or rollback handle.

The timeout is a positive integer with `ms`, `s`, `m` or `h`, at most 24 hours.
The default is 30 seconds, or five minutes with `--wait`; an explicit value
replaces that default. The native budget covers approval, credential setup,
submission and observation after local validation. Timeout and interruption
preserve known evidence and never repeat or undo the delete.

An interrupted native approval stops the enclosing batch or REPL before another
input read. Its blocking console reader may still be finishing after bounded
runtime shutdown; another editor must not compete with that reader for stdin.

## Typed metadata across layers

The shared core represents operation annotations as typed runtime data, not
string tags or compile-time reflection:

- The descriptor declares potential effects and capability limits.
- The immutable assessment binds exact target, requested mode and requirements.
- Execution evidence records actual completion, acceptance, denial or uncertainty.

The SDK carries this metadata through receipts and failures. The CLI consumes it
for approval and JSON output rather than guessing safety from a command name.
Metadata does not authenticate a caller, grant service permission, prove tenant
recovery settings or authorize a cascade.

## Output and host boundaries

The receipt uses a separate versioned JSON envelope. `--project` changes only
`data`; target, effect, approval and operation evidence remain in `meta`.
Unknown facts remain unknown. Accepted is not completed, and ambiguous transport
failure is not evidence that deletion failed harmlessly.

`DeleteCommandError::sdk` retains the original SDK failure in a box, keeping the
command error small without changing its typed source or reconciliation data.

`--output` reserves a new local file before mutation and never truncates an
existing destination. A failed operation may leave the reserved file empty;
failed output may leave it partial. Neither is claimed to be a completed receipt
file. Output failure retains available evidence on the error path, never by
replaying deletion. No arbitrary file cleanup is performed.

Default builds validate the command but have no production identity or transport.
The explicitly selected native host reuses existing credential, endpoint,
transport and runtime composition; it performs no credential fallback or login.
Tests use injected offline providers. Native authentication, live service
behavior and other platforms require their own acceptance evidence.

## API references

- [Delete Notebook](https://learn.microsoft.com/rest/api/fabric/notebook/items/delete-notebook)
- [Delete Item](https://learn.microsoft.com/rest/api/fabric/core/items/delete-item)
- [OneLake file retention](https://learn.microsoft.com/fabric/onelake/configure-soft-delete)
- [Item recovery](https://learn.microsoft.com/fabric/admin/item-recovery)
