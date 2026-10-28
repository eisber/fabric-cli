<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Definition transfer commands

`fabric-cli-transfer` composes `export`, `bulk-export` and `import` over
`fabric_deploy::definitions`. It registers deployment arguments for the central
CLI, which executes them through its separate `DeployHost`.

## Host contract

Mount `transfer_subcommands()` and convert matches with `command_from_matches`.
The central lexical adapter handles the `-tenv` spelling without installing a
second parser.

Supply a caller-bound SDK client, operation executor, metadata, file policy, UI,
limits and explicit execution mode. `TransferMetadata::resolve_target` and
`complete_snapshot` separate target resolution from collection discovery.
Snapshots must be complete and preserve resource support and traversal order.

`TransferUi` supplies confirmation, selection, warnings and progress. Libraries
never prompt or discover credentials. The caller owns runtime and stream delivery.

## Operations

| Command | Behavior |
| --- | --- |
| `export` | Single-item or selected/all workspace definitions, local or Lakehouse output, per-type formats and ordered writes |
| `bulk-export` | Recursive workspace/folder snapshots, local output preconditions, All/Selective requests and response-index summaries |
| `import` | Raw local definitions, create/update selection, overwrite confirmation, Environment readiness/publication and actual returned IDs |

Remote import and nonlocal bulk-export destinations are rejected. Report
definitions with non-null `byPath` references are invalid import payloads.
Interactive workspace export selection requires an injected UI; `--all` avoids
that selection step.

## File capabilities

### Asynchronous local filesystem boundary (TD03)

The host awaits directory resolution, import preparation, export preflight,
directory creation and ordered writes. Synchronous injected adapters remain
supported; OS-backed hosts must override asynchronous methods to offload work.

The central CLI's `TokioTransferFiles` serializes work on the existing blocking
executor. Deferred constructors bind exact selected import/export arguments
without filesystem access until local classification succeeds. Import and export
aliases remain independent. Existing traversal, link/reparse, device-name,
collision, containment and byte limits apply.

An in-memory export plan is not written progress. Preparation errors report no
file writes; actual write errors retain completed and possibly incomplete paths.
OS calls can continue after their futures are dropped.

## Results and delivery

`TransferResult` retains exported/skipped IDs, completed paths, declines and
deferred import cache effects. Apply cache effects only after successful result
delivery. Initial decline and overwrite decline have distinct effect semantics.

`TransferError` keeps original operation IDs, uncertain mutations and prior
file progress. `render_result`, `render_error` and `write_result` use shared
output policies and propagate delivery errors. Never replay a mutation or a
partial write to repair output.

Strict decoding and portable path safety remain explicit constraints. Accepted
HTTP responses do not prove completed publication or upload.
