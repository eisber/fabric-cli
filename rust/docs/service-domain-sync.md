<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Grouped Domain Contributor synchronization

This bounded v2 library binding implements:

```text
service domain sync PATH --role Contributor
```

The role is required explicitly; no numeric or role default is introduced.
`fabric_cli_resources::service::service_command()` mounts only this owned subtree
in a separately composed v2 host. Its isolated `command()` and `parse_args()` are
available for embedded/offline consumers. The existing v1 front door, executables,
identity providers and frozen fixtures are unchanged. A registered schema is not
a configured production resolver or permission grant.

## Native action and authorization

The SDK `fabric_sdk_catalog::service` binds operation
`fabric.service.domain.sync` to one native request:

```http
POST /v1/admin/domains/{domainId}/roleAssignments/syncToSubdomains

{"role":"Contributor"}
```

The [published API](https://learn.microsoft.com/en-us/rest/api/fabric/admin/domains/sync-role-assignments-to-subdomains)
requires a Fabric administrator. Delegated users require `Tenant.ReadWrite.All`.
The API lists user, service-principal and managed-identity support. Delegated
scopes do not apply to application-only access; applicable
[Fabric identity controls](https://learn.microsoft.com/en-us/rest/api/fabric/articles/identity-support)
and [admin API allowed-group settings](https://learn.microsoft.com/en-us/fabric/admin/service-admin-portal-admin-api-settings)
still apply. Neither `.default` nor a Domain/workspace admin role establishes the
required authority. This library never enables settings, obtains consent,
changes providers or tries a less/more privileged endpoint.

The schema's `DomainRole` enumeration includes Admin, but this action explicitly
rejects syncing admins (`SyncingAdminsToSubdomainsIsNotSupported`). Only
Contributor is advertised here. Sync changes permissions on subdomains; it is
not file sync, data sync, ACL principal grant/revoke, artifact execution or raw API.

Documentary source: MicrosoftDocs/fabric-rest-apis-docs
`b4d803a1e7f495553fcb7ef509391016e676d91d`,
`docs-ref-autogen/fabric/admin/Domains/Sync-Role-Assignments-To-Subdomains.yml`.
Fabio `557f098ec087c1626c1d892db09360083ff95105`,
`src/commands/admin/domains.rs::sync_domain_roles_to_subdomains`, supplies the
same route/body source intent. It is not proof of current tenant access.

## Shared interfaces and boundaries

`DomainResolver` supplies one exact canonical target, tenant and Domain UUID.
Wrong owners, facets, representations and malformed UUIDs fail locally.
No automatic tenant catalog listing, display-name fallback or Domain-to-item
coercion occurs. The existing neutral model lacks a service-resource identity:
its real `TenantCollection` identity retains the tenant, the canonical path
addresses the Domain, and immutable approved input retains `domainId`.
There is no synthetic workspace/item ID.

`register` adds one conditional binding atomically to the caller's registry.
The existing neutral registry's `Update` category describes the remote write
effect, not a new generic CLI update alias. `prepare` retains exact Domain/role,
`RemoteWrite`, admin/identity prerequisites, `ExistingOnly` and
`UnguardedConcurrency`. The API supplies no caller revision condition; the
precondition is explicit intent, not an invented ETag enforcement.

Immediate `ApprovalPolicy` must verify the prerequisites and exact effects.
The SDK validates approved input/binding and the request context's Fabric origin,
audience and matching tenant again before credential acquisition. Caller-selected
eligible hosts remain bound through the existing `RequestContext`; no audience
is inferred from the hostname. Resource permissions are ultimately enforced by
the service. No local boolean claims that those permissions were tested live.

`synchronize` reuses the existing `OperationSession`/`OperationExecutor`, credential
injection, response bounds and cancellation. It admits at most one native request.
The caller supplies its existing cancellation/deadline future on its runtime,
including an overall deadline around resolution/approval/execution. There is no
new timer, runtime, HTTP implementation, retry stack or remote cancel operation.
Executors must obey the shared no-mutation-replay contract.

## Results and failures

Only the documented `200` with an actually empty body completes this void action.
Nonempty bytes (including JSON null, objects, arrays, error or pending state)
are unsupported observations, not silently discarded success. No affected
subdomain/principal/assignment count is fabricated. The neutral receipt retains
the canonical target, declared effects, preconditions and actual status; coverage
of individual assignments remains Unknown. Neutral source evidence retains the
exact native Domain UUID and Contributor role outside the result projection.
Original bounded native response
bytes and shared errors remain beside the receipt for explicit inspection.

Undocumented `201`, `202`, `204`, `206`, redirects or other protocol statuses are
Unknown, never completed or an invented asynchronous job. No monitor is polled
or redirect followed. Native rejection statuses 400, 401, 403, 404, 409, 422,
429 and 500 are Failed, retaining
its exact status and structured raw error separately from sanitized output.
Denied is not absence. Throttling is not success; no POST retry is attempted.
Transport/deadline/cancellation/response-limit failure after dispatch is Unknown
with `mutation_may_have_occurred`, not rollback or permission to replay. Partial
response loss cannot establish partial assignment counts.

The CLI adapter sends success receipts to stdout and failures/uncertainty to
stderr. Non-completion is nonzero. Writer failure retains the executed report,
so presentation failure never replays an action. Raw provider messages are not
implicitly printed. Receipt/source-validation errors additionally retain the full
approved target/input/effects/preconditions, native Domain/role source facts and
original observations, even if the provider returned an invalid HTTP status.
Neither capacity lifecycle/activation, gateway lifecycle,
tenant policy/restore, root raw API nor MCP hosting is implemented by this slice.

## Offline evidence

`fabric-sdk-catalog/tests/service_sync.rs` covers exact payload/target/request
count, role/owner/UUID/tenant/audience/origin rejects, unchanged prerequisite
approval, documented empty completion, native errors, response loss and
pre/post-dispatch cancellation without polling/replay.
`fabric-cli-resources/tests/service_sync.rs` covers grouped syntax, invalid-role
rejection before resolution, resolution/approval denial, parser-to-native
composition, receipt streams/exits and writer-failure evidence.
An injected Azure HTTP client also exercises the actual shared HTTP executor for
public/daily context, exact request/JSON/content type, native errors and no
mutation retry on throttling.
Both targets are included in the Cargo-metadata impact registry.

No live admin action, native credential acceptance or platform identity policy
is claimed by offline tests. Full native Windows/Linux/macOS and stable/optional
gates remain required before a draft becomes review-ready or is integrated.
