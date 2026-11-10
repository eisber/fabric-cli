<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# MirroredDatabase lifecycle START

The experimental `fab-cli-v2` resource head supports one explicit START binding:
mirroring for an existing MirroredDatabase. This is separate from the existing
Python CLI, `fab-rs`, and job submission. Other lifecycle backends are not
implemented by this binding.

```text
fab-cli-v2 start /<workspace-id>.Workspace/<item-id>.MirroredDatabase/runtime/mirroring --approve --timeout 30
fab-cli-v2 start --schema
fab-cli-v2 start --help
```

Both IDs must be UUIDs. The exact runtime facet is required; item-root aliases,
names, relative paths, multiple targets, patterns and workload/session targets
are rejected before identity selection. `--schema` and `--help` are local and do
not initialize credentials, transport or a runtime. Schema discovery does not
establish resource existence or permission.

## Approval, budget and execution

`--approve` authorizes only this invocation's declared mirroring effects. It is
not an override for state checks, service permissions or tenant policy. Missing
approval fails before identity selection. The SDK and embedded CLI adapters
require an immediate caller-supplied policy for the prepared target and effects;
approval is bound to the original request context.

`--timeout` is a required positive integer in seconds, at most 86400. There is no
guessed lifecycle timeout default. The native head applies one total local
deadline across setup, status preflight and START, using its existing explicit
identity, HTTP and runtime composition. An embedded caller supplies its own
deadline/cancellation future and request limits. No `--wait`, `--force`, input,
output-file or generic retry option is advertised in this milestone.

Remote execution requires the existing explicit `FAB_RUST_AUTH_MODE` selection.
No implicit credential chain, interactive sign-in, permission change, consent
fallback or alternate service route is installed. No authentication or live
service operation is needed for the offline acceptance tests.

## Native contract and state

The binding uses the Fabric audience and two bodyless native actions:

1. `POST /v1/workspaces/{workspaceId}/mirroredDatabases/{itemId}/getMirroringStatus`
2. `POST /v1/workspaces/{workspaceId}/mirroredDatabases/{itemId}/startMirroring`

Only a completed HTTP 200 status response with a valid `Stopped` or `Initialized`
state permits START. Failed, malformed or incomplete preflight results block the
mutation. `Paused`, initializing/transition states, `Running` and unknown states
are not silently treated as stopped. Preflight is an observation, not an atomic
compare-and-start guarantee: a concurrent START, STOP or deletion can still cause
the service to reject the action.

A documented HTTP 200 START response establishes that the **action completed**.
It does not prove that the database is now `Running`, that replication has caught
up, or that downstream workloads succeeded. There is no synthetic workload run
or cancellation handle. This binding does not poll undocumented acceptance or
perform a second START to observe the first.

The caller needs read and write permissions on the mirrored database. Delegated
scopes are `MirroredDatabase.ReadWrite.All` or `Item.ReadWrite.All`. The documented
endpoint supports users, service principals and managed identities; scope
selection does not itself establish resource access.

## Receipts and failure handling

CLI, SDK and other caller-supplied heads share the same operation registration,
prepared intent and common execution receipt. Outcome, resolved target,
preconditions and effects evidence remain outside result data. Ordinary command
receipts are newline-terminated JSON; successful action receipts use stdout and
failed or unknown outcomes use stderr with a nonzero exit.

Cancellation before START prevents mutation. Interruption, response loss or a
local deadline during a dispatched START can leave its remote effect unknown.
Retain the receipt and native evidence. Output failure also retains the prior
receipt as diagnostic evidence; it never replays the operation.

No error or timeout implicitly submits a workload, issues STOP, cancels remote
work, resumes another resource, rolls back changes or retries an uncertain
mutation. Throttling and service rejection remain explicit failures rather than
permission to retry indefinitely. No universal restart/resume/cancel equivalence
is implied.

## Offline validation scope

The lifecycle acceptance targets are `fabric-sdk/mirroring_start`,
`fabric-cli/start_command`, and, with `fabric-cli/headless`,
`fabric-cli/v2_start_binary`. They use synthetic credentials, scripted HTTP and
local subprocess preflight, not a tenant or running mirrored database. The
existing impact graph selects these targets and their applicable host profile.
Existing v1 state/prompt behavior and the independent Python oracle are unchanged.
Offline results do not establish native tenant, identity-policy or live-service
acceptance on any platform.

## Sources

- [Fabric Start Mirroring API](https://learn.microsoft.com/en-us/rest/api/fabric/mirroreddatabase/mirroring/start-mirroring)
- [Fabric Get Mirroring Status API](https://learn.microsoft.com/en-us/rest/api/fabric/mirroreddatabase/mirroring/get-mirroring-status)
- [Pinned Python START state policy](https://github.com/microsoft/fabric-cli/blob/8ad4c703bdd6738189f59318a04cab297b6c4381/src/fabric_cli/commands/fs/start/fab_fs_start_item.py)

The first two API documents identify source revision
`b4d803a1e7f495553fcb7ef509391016e676d91d`. The selected grouped START proposal is
document `2456711f-3607-42e5-b4d6-1ed7186b965a`, revision 11. Strict v2 preflight,
an explicit ID-only milestone, invocation approval and explicit local deadlines
are intentional boundaries, not claims of byte-for-byte v1 CLI parity.
