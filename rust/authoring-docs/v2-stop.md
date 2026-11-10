<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Stop mirrored database replication

This page records the approved bounded contract for stopping replication on one
MirroredDatabase item through public Rust library adapters. Adapter source is
unmounted and unvalidated; this document does not establish implementation
acceptance. **Opt-in executable integration is
coordinator-pending and unavailable.** No executable feature name or enablement
command is advertised.

The approved library surfaces are `fabric_sdk_items::stop` for orchestration and
`fabric_cli_mutations::v2_stop` for argument validation and JSON delivery.
Libraries use the caller's existing executor, endpoint, credentials, async
runtime, and single cancellation/deadline budget. They do not create an
authentication flow, runtime, polling engine, or credential fallback.

The central operation descriptor, `PreparedOperation`, `ApprovedOperation`, and
executable service-host wiring remain coordinator-owned and are not implemented
by this slice. No shared registry or Model Context Protocol (MCP) execution
coverage is established. The bounded library contract is not a substitute for
those integration and validation gates.

Python `fab`, default executable behavior, and Rust v1 stop semantics remain
unchanged. This contract does not silently replace an existing root command.
See [resource mutation compatibility](mutation-compatibility.md) for the
separate v1 behavior.

## Address one mirroring target

The following executable-independent argument shape is for reference only.
**It remains unmounted and unavailable as an executable command.** The binary
name is intentionally omitted:

```text
stop /<workspace-guid>.Workspace/<item-guid>.MirroredDatabase/runtime/mirroring --yes --format json
```

Replace both identifier labels with authoritative, hyphenated, 36-character
hexadecimal globally unique identifiers (GUIDs). The workspace and item segments
require the exact `.Workspace` and `.MirroredDatabase` suffixes, followed by
`/runtime/mirroring`. Preserve identifier case spelling; the adapter does not
resolve display names or infer a target from the current directory.

Only one absolute ID-based target is admitted. Whole-item aliases, relative
paths, arbitrary URLs, wildcards, target lists, traversal, and `docs` or `src`
facets are rejected before executor or credential work. SQL database, capacity,
session, query, and run targets are outside this slice; no target falls back to
another service or operation.

## Record explicit mutation intent

`--yes` records explicit intent to request a stop on the validated target.
The fixed SDK input `StopOptions { approved: true }` represents that intent
only, not provider authorization. The Boolean remains an input to the bounded
SDK orchestrator, not a substitute for host approval.

The actual host must enforce its full effect, authentication, and authorization
policy through `ApprovalPolicy` before granting `approved: true`. `--yes` never
bypasses that policy, state validation, or service permissions. Libraries never
prompt, and there is no universal force policy.

Exact `Stopped` and `Stopping` prechecks need no mutation intent flag because
they submit no stop request. Exact `Running` requires explicit intent and host
policy approval before a mutation. A read still requires service authorization
even when no write follows.

JSON is the only supported output format, with `--format json` as the explicit
selection. Receipts for successful observations, no-ops, and in-progress outcomes
use stdout; errors use stderr and a nonzero exit status. This initial adapter
has no `--wait` or `--timeout` flags and does not install a separate timer.

## Check state before submitting

The adapter first makes one bodyless status request:

```http
POST /v1/workspaces/{workspaceId}/mirroredDatabases/{mirroredDatabaseId}/getMirroringStatus
```

The precheck requires a completed HTTP 200 response with a valid exact state
and no reported status error. State matching is case-sensitive, not substring
matching. An accepted or malformed status response does not authorize a write,
and the adapter does not follow an unexpected operation monitor.

| Precheck state | Outcome | Stop writes |
| --- | --- | --- |
| `Stopped` | Verified no-op; stopped is confirmed | None |
| `Stopping` | Already in progress; stopped is not confirmed | None |
| `Running` with explicit intent and host policy approval | Submit one stop request | At most one |
| `Running` without explicit intent | Approval required | None |
| `Initializing`, `Initialized`, `Starting`, or `Paused` | Precondition failure | None |
| Unknown, missing, null, non-string, or errored status | Invalid response or status error | None |

`Paused` is not an alias for `Stopped`. An already-in-progress result does not
create a new operation identity. Precheck failures never trigger a stop request.

## Separate request and state evidence

After an exact `Running` precheck, explicit intent, and host policy approval,
the adapter submits at most one bodyless stop request:

```http
POST /v1/workspaces/{workspaceId}/mirroredDatabases/{mirroredDatabaseId}/stopMirroring
```

The official stop API documents an empty HTTP 200 success response. That
response proves request completion, not that replication has reached `Stopped`.
The bounded adapter preserves acceptance separately if the shared executor
returns an accepted outcome; this does not advertise an asynchronous service
contract beyond the official API.

| Submission evidence | Observation policy | Meaning |
| --- | --- | --- |
| Accepted | No monitor following or polling | Accepted is not stopped |
| Completed HTTP 200 | One status readback at most | Request completed; state still needs evidence |
| Exact `Stopped` readback | No further requests | Stopped is confirmed |
| Another valid readback state | No further requests | Observed state only; stopped is not confirmed |
| Failed or invalid readback | Return an error with the completed mutation receipt | State remains uncertain |

Only an actual exact `Stopped` observation confirms stopped, including the
initial no-op case. A completed request followed by `Stopping` or `Running`
does not confirm stopped. Retain an operation ID only when the service supplies
one; do not infer an ID from the target or create one for an existing stop.

The SDK contract separates `outcome`, `observed_state`, and `request_outcome`
from `mutation_may_have_occurred` and `stopped_confirmed()`. A JSON receipt must
preserve the same distinction between request evidence and confirmed state.
The initial limit is one precheck, at most one stop submission, and at most one
readback, all within the caller's single budget.

## Retain uncertainty after failures

Service failures, response loss, local interruption, deadline expiry, and output
failure do not prove that a dispatched mutation had no effect. Errors preserve
the target and stage-appropriate mutation uncertainty. A readback failure after
a completed stop also retains the completed mutation receipt; it is not
reclassified as a failure before submission.

There is no automatic replay or second stop write. Local cancellation does not
cancel the remote stop, and failure does not start replication, delete an item,
cancel a scheduler run, or restore earlier state. Do not retry an uncertain
mutation solely because output delivery or readback failed.

SDK callers retain typed underlying error evidence rather than arbitrary
service text being presented as success. CLI diagnostics contain sanitized
classifications instead of copying provider messages or object keys. Output
failure remains an error even when the remote request completed, and it does
not erase the completed receipt.

## Understand persistent effects

The adapter requests the provider's stop-mirroring operation; it does not delete
the item or shut down the source database. Data retention depends on the provider
contract, not on request acceptance or completion. Stopping does not guarantee
that pending work drains. It does not undo earlier effects or provide rollback.

For Azure SQL Database-backed mirrors, existing replicated tables remain in
OneLake when replication stops. **Stopping and starting these mirrors fully
reinitializes replication and retrieves all source data again.** The
[Azure SQL Database mirroring FAQ][azure-sql-faq] documents these provider-specific
retention and full-resnapshot behaviors; do not generalize them to every mirror
provider.

Stopping is not a general cost-off switch. Query compute, retained storage
beyond applicable allowances, and source or network charges can remain relevant.
Review the provider-specific [Azure SQL Database mirroring FAQ][azure-sql-faq]
for persistence, restart, and billing details.

## Permissions and supported identities

The [Stop mirroring API][stop-api] requires read and write permissions on the
mirrored database. Its delegated scopes are `MirroredDatabase.ReadWrite.All` or
`Item.ReadWrite.All`; scopes do not replace resource permissions. The
[Get mirroring status API][status-api] documents its own read permission and
scope requirements for observation.

The official APIs support users, service principals, and managed identities.
This service support does not establish that an executable host or identity
provider is configured, or prove access in a particular tenant. The library
uses only the caller's injected identity and selected endpoint.

The public-cloud token audience is `https://api.fabric.microsoft.com`, with
`https://api.fabric.microsoft.com/.default` as the token request scope.
Changing a target path does not select another audience or grant permission.
See [endpoint and credential context](endpoint-context.md) for the existing
caller-owned selection boundary; this adapter adds no sign-in instructions or
credential settings.

## Planned offline validation

**The following targeted tests are planned and unexecuted for this documentation
task.** Package names match the repository manifests; the test targets belong
to the separately authored SDK and CLI slices. Run them from `rust/` only after
the source targets are present and validation is authorized:

```bash
cargo test --locked --offline -p fabric-sdk-items --test stop
cargo test --locked --offline -p fabric-cli-mutations --test v2_stop
```

The planned tests use injected offline executors and writers, not live Fabric
operations. They need to cover strict target validation, exact state checks,
explicit-intent gating, request order and counts, acceptance, one readback, retained
uncertainty, completed receipts after readback failure, and output failures
without replay. These commands do not establish executable composition,
authentication, tenant permissions, or implementation acceptance.

No build, test, or live validation was run for this document. Executable
registration, opt-in host integration, help, and combined validation remain
coordinator work. No working executable invocation or feature name is inferred
from the library contract.

## Official references

- [Stop mirroring][stop-api] documents the stop request, permissions, scopes,
  supported identities, and HTTP 200 response.
- [Get mirroring status][status-api] documents the POST status action, state
  values, authorization, and response.
- [Azure SQL Database mirroring FAQ][azure-sql-faq] explains provider-specific
  data persistence, full reinitialization, and billing boundaries.

[stop-api]: https://learn.microsoft.com/en-us/rest/api/fabric/mirroreddatabase/mirroring/stop-mirroring
[status-api]: https://learn.microsoft.com/en-us/rest/api/fabric/mirroreddatabase/mirroring/get-mirroring-status
[azure-sql-faq]: https://learn.microsoft.com/en-us/fabric/mirroring/azure-sql-database-mirroring-faq
