<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Opt-in standalone Eventhouse creation

The `fabric-cli` feature `v2-create-eventhouse` composes the existing `v2-create`
host with one native Eventhouse binding. With `headless`, the actual
`fab-cli-v2` binary mounts:

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.Eventhouse [OPTIONS]
```

Default builds, `fab-rs`, and its v1 `create`/`mkdir` alias are unchanged.
This is not KQL database creation, a query, shortcut, run, start/stop, or an
artifact facade. It never invokes the v1 KQL dependency handler.

## Metadata and policies

Missing `--input` produces `{"displayName":"<literal-name>"}`. Explicit input
is bounded to 4 MiB, strict UTF-8 JSON with unique keys, and must be an object
with `displayName` exactly equal to the literal destination name. The only other
admitted field is the optional string `description`, at most 256 characters.
Empty input, null, empty objects, duplicate/escaped duplicate keys, mismatched
names, and all unknown fields fail locally, including for `--if-exists skip`.
The bounded preview admits ASCII letters/digits, underscores, periods and
hyphens in names; it does not claim Unicode name support.

The type is selected only by the `.Eventhouse` suffix. The REST request schema
does not advertise a `type` input field, so even `type: Eventhouse` is rejected
rather than removed or forwarded silently. Definitions, creation payloads,
minimum-consumption settings, folder selection, sensitivity labels,
dependencies, connections, and run/compute settings are outside this subset.
Their presence rejects before credentials, discovery or mutation.

Only provider-rejecting create (`error`) and exact existing unchanged `skip`
are enabled. Unsupported overwrite, `--force`, and duplicate fail before input
I/O. Explicit bodies still validate before skip. Complete bounded workspace item
discovery rejects case-only collisions, ambiguous names, wrong kinds and wrong
scope; it never consults ambient navigation or resolves a UUID-shaped leaf as an
existing item ID. Missing-target skip follows creation. Discovery is not an
atomic reservation: the POST can still report `ItemDisplayNameAlreadyInUse`.
There is no full replacement, rename, delete/recreate, or race-free guarantee.

## Provider contract and service effects

Authoritative sources, consulted November 12, 2026:

- [Create Eventhouse REST API](https://learn.microsoft.com/en-us/rest/api/fabric/eventhouse/items/create-eventhouse):
  `POST /v1/workspaces/{workspaceId}/eventhouses`; required `displayName`,
  optional `description`, supported Fabric capacity, contributor workspace role,
  delegated `Eventhouse.ReadWrite.All` or `Item.ReadWrite.All`, and user/service
  principal/managed identity support. The API reports 201 creation or 202
  provisioning with a distinct operation handle. Its native schema also
  advertises optional settings deliberately excluded above.
- [Create an eventhouse](https://learn.microsoft.com/en-us/fabric/real-time-intelligence/create-eventhouse):
  the documented product creation experience creates an Eventhouse **and a
  same-name default child KQL database**, sharing capacity/resources. This is
  service-side behavior, not proof of the exact effects of every REST response.
  Do not equate one client POST with no server-created child, or infer a child
  identity that the provider has not returned.

Immediate approval must disclose exact target, selected action/policy, outgoing
body, endpoint, account/tenant and anticipated service-side effects. No live
Fabric/Entra authentication, provisioning, compute or mutation is authorized by
these offline tests.

The client issues exactly one Eventhouse POST. It issues **zero** KQL database
POSTs, does not discover/provision children, and never starts compute or runs a
query. Waiting uses the existing bounded `OperationSession` with `WaitForResult`,
not a second operation engine. Receipt `data` retains actual returned properties
(including child IDs if supplied); protected metadata retains only observed
resource identity, operation handles, and mutation uncertainty. It does not claim
that a child exists or does not exist from an empty response.

Acceptance is not completion. An operation ID is not a resource ID. Lost,
malformed, incomplete, timed-out or cancelled results and projection/file/stream
failures retain available evidence without POST replay, remote cancellation or
rollback. The shared no-clobber sink protects existing files and output races.
See [shared create hosting](create-host.md) for the execution/delivery boundaries.

## Provenance and acceptance

Base: `ced983722015e440e989f07d1bf866821d27fe90`. Native source route:
`ItemMutations::create_with_completion` and `ItemCollection::Eventhouses`.
The existing consumer's Eventhouse assertion belongs to KQL dependency creation;
it is not evidence of this standalone mount. Only the logical generic helper
diffs `e84426a`, `ce89c07`, `225903a`, `99dc963`, and `9812bec` are consumed, in that
order; no Notebook leaf or whole peer PR is imported.

`tests/eventhouse_create.rs` exercises the parser, shared host, SDK planner,
injected credential and scripted HTTP executor. `tests/eventhouse_create_binary.rs`
exercises the actual opted-in executable's local help, policy/input/output
preflight, approval fence, deadlines and closed pipes. Both deny unnamed tests.
The impact graph registers feature-specific targets on Windows, Linux and macOS.
Source availability or a queued CI run is not completed native validation,
cross-platform acceptance, live service evidence or review readiness.
