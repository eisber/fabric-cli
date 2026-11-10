<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace core commands

The compatibility executable is **`fab-rs`**, not a grouped workspace-only CLI.
Its existing command parser, context resolution, SDK clients, shared HTTP
executor and mutation policies implement the following workspace operations.
The examples use a workspace display name, including the `.Workspace` suffix.
They do not authorize any live operation.

| Operation | Actual compatibility spelling | Effect |
|---|---|---|
| Discover workspaces | `fab-rs ls /` | Reads accessible workspace metadata |
| Discover items | `fab-rs ls /Demo.Workspace` | Reads that workspace's item collection |
| Get metadata | `fab-rs get /Demo.Workspace -q .` | Reads primary metadata and existing optional enrichments |
| Check existence | `fab-rs exists /Demo.Workspace` | Resolves the workspace; returns false for an absent name |
| Create | `fab-rs mkdir /Unique.Workspace -P capacityName=none` | Creates an explicitly unassigned workspace |
| Update description | `fab-rs set /Unique.Workspace -q description -i "new description" -f` | Patches the selected metadata property |
| Rename | `fab-rs set /Unique.Workspace -q displayName -i Renamed -f` | Patches the display name |
| Delete | `fab-rs rm /Unique.Workspace -f` | Deletes the workspace itself, after item discovery |

`dir`, `create` and `del` are aliases of `ls`, `mkdir` and `rm`, not additional
business operations. `--output_format json` selects compatibility JSON output.
`capacityName=none` explicitly omits capacity assignment; it is not permission
to provision paid capacity. Without that parameter, existing default-capacity
resolution applies. `-f` bypasses the relevant confirmation, not authorization,
resource discovery or service errors. Non-forced workspace removal uses the
existing item-selection policy and must not be confused with forced workspace
deletion.

Default builds recognize these commands but do not provide remote identity or
transport. Actual native execution needs the existing `native-read` or
`headless` host and a separately configured supported identity. An Azure CLI
test-setup identity is not automatically a Rust native binding. Help and
offline injected HTTP do not prove live service permission. See
[native host configuration](native-read-host.md).

The existing native read host accepts a per-process `FAB_API_ENDPOINT_FABRIC`
hostname, such as `msitapi.fabric.microsoft.com`. The SDK/session composition
preserves this binding for workspace discovery, initial mutations, operation
polling and results, with the Fabric audience. Foreign result links are rejected,
not followed or retried on the public service. This does not select credentials
or grant access to that deployment.

The compatibility native front door permits the existing `exists`, `mkdir`,
`set` and `rm` resource variants, including their aliases, on explicitly selected
nonpublic endpoints. Their existing workspace/item/folder resolution and service
policies still apply; the gate does not infer a target from a suffix or grant
provider permission. Link, copy, move, start, stop, capacity assignment and ACL
families do not gain nonpublic endpoint support. Parsed root-verb gate tests and
the injected session tests below are not live CLI identity or CRUD proof.
Tenant-wide removal remains unsupported on nonpublic endpoints after typed
context resolution, before collection discovery or any mutation.

The separate, opt-in **`fab-cli-workspace`** binary provides bounded v2 reads,
including actual mounted `artifact workspace get` and `artifact workspace list`
facades. It does **not** provide grouped workspace create/update/delete.
See [workspace v2 reads](workspace-v2-reads.md) for its explicit UUID addressing,
required budgets, partial-result behavior and feature selection. Workspace
reset/restore are not advertised. Existing capacity assign/unassign commands
have their own placement/permission contract and are not part of this core
CRUD claim.

`fabric-cli` integration target `workspace_core` exercises actual root parsing,
read/family dispatch and shared HTTP under `native-read` or `headless`, with only credential,
HTTP, clock and confirmation capabilities injected. It covers discovery,
metadata, existence, create aliases without capacity assignment, duplicate and
malformed creation responses, single-property updates, removal aliases and
provider denial. Both public and explicitly selected MSIT endpoint bindings
retain the Fabric audience. No test discovers ambient identity or sends live
requests. Existing source fixtures and independent Python oracle are unchanged.

A completed workspace-create response with an empty resource ID is rejected as
`InvalidResponse`, matching the existing missing/non-string ID safety check.
The error retains the observed HTTP status and possible mutation effect; callers
must reconcile instead of resubmitting. This narrowly scoped malformed-response
safety difference is not a Python-parity claim. HTTP 202 without completion
evidence remains accepted, without an invented resource ID, created output or
cache entry.
