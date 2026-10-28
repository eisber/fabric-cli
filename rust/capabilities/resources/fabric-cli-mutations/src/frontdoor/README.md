<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource command adapter

Mount `resource_subcommands()` in the root Clap command. Convert matches with
`command_from_matches`, then execute the typed `ResourceCommand` through
`ResourceHost`. The adapter does not install a runtime, credential provider or
second parser.

## Capabilities

| Commands | Handler responsibility |
| --- | --- |
| `exists` | Resolved resource ID checks without a redundant request |
| `mkdir` (`create`) | Workspace capacity selection, item and folder creation |
| `rm` (`del`) | Confirmation, hard-delete policy, accepted outcomes and cache effects |
| `set` | Workspace, item and folder metadata updates |
| `ln` (`mklink`) | Lakehouse shortcut configuration and conflict policy |
| `start`, `stop` | MirroredDatabase state validation and lifecycle requests |
| `assign`, `unassign` | Capacity and workspace assignment |
| `cp` (`copy`), `mv` (`move`) | Storage transfer composition through `ResourceTransferHost` |

Supply `ResourceContext` over the existing command-local metadata snapshot,
`MutationHost` with the same `ResourceOperations` session, UI, enriched metadata
and `ResourceConfig`. `exists`, `mkdir` and `ln` allow unresolved creation
targets; removal and updates require existing resources.

`ResourceTransferHost` uses `OneLakeWriteHost` and optional bounded
`ResourceFiles`. It retains `OneLakeWriteResult` and `OneLakeWriteError`
separately from Fabric mutation outcomes. A host without transfer capabilities
rejects copy/move explicitly.

## Results

`MutationResult` retains presentation intent, cache effects, declined prompts,
accepted/completed outcomes and dependency evidence. The central host applies
effects and context changes, selects output format, and writes shared JSON or
text output. Output failure must not discard earlier creation evidence or
replay a request.

## Limits

Resource skill headers are not wired and requested values fail explicitly.
Virtual-resource mutations, recursive storage traversal, local destinations,
capacity ARM lifecycle and domain assignment require their own capabilities.
Do not substitute successful no-ops for unsupported branches.

Two-path transfers assign the final positional token to the destination.
Workspace capacity is configured with `mkdir -P capacityName=...`; `--type`
belongs to shortcut creation. Parsing performs no service work.
