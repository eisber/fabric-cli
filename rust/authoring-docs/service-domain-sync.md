<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Grouped Domain Contributor synchronization

This bounded v2 binding implements:

```text
service domain sync /.domains/<domain>.Domain --role Contributor
```

The role is required explicitly; no numeric or role default is introduced.
`fabric_cli_resources::service::service_command()` mounts only this owned subtree
in a separately composed v2 host. Its isolated `command()` and `parse_args()` are
available for embedded/offline consumers. The opt-in `v2-service-domain` feature
now mounts that same schema on the existing `fab-cli-v2` head:

```text
cargo build --locked -p fabric-cli --features headless,v2-service-domain --bin fab-cli-v2
fab-cli-v2 service domain sync /.domains/Finance.Domain --role Contributor
```

Default/legacy `fab-rs` and discovery parsers, identity providers and frozen
fixtures are unchanged. There are no new command flags, role defaults, `--force`
or unattended approval switches. Root and leaf help require no authentication.
The existing explicit headless credential configuration and
`FAB_RUST_FABRIC_HOST` selection apply; no ambient credential chain is installed.

The November 4 selected studio capture described the conceptual
`/.domains/<domain>/acl` target, but did not select exact role flags or body.
Its sync documentation link incorrectly addressed Lakehouse file sync. This
executable deliberately uses the already committed `service.rs` grammar and
`DomainTarget` contract from integration
`345226c61f56448f6f0a6993679a816fb44aa1be`, not that unrelated file-sync grammar.
The bounded typed `.Domain` spelling is not a claim of exact studio-spec parity.

## Executable resolution and approval

`fabric_cli::v2_service_domain_host::BoundedDomainResolver` requires one explicit
UUID tenant, Fabric audience/origin, and an identically bound shared
`HttpExecutor`. Invalid roles and paths fail before credential acquisition or
HTTP. A display-name selector performs one admin-domain catalog read, limited to
1 MiB and 1,000 rows, with exact case-sensitive matching and ambiguity rejection.
Continuation-bearing, malformed, oversized or incomplete metadata fails closed;
no pages, other tenants or fuzzy names are searched. A UUID selector skips the
catalog. Both routes require a bounded singleton admin-domain GET with matching
UUID and, for names, unchanged display name before preparing the mutation.

The [list](https://learn.microsoft.com/en-us/rest/api/fabric/admin/domains/list-domains)
and [get](https://learn.microsoft.com/en-us/rest/api/fabric/admin/domains/get-domain)
admin APIs independently require Fabric administrator authority, with
`Tenant.Read.All` or `Tenant.ReadWrite.All` for delegated access
(MicrosoftDocs source `d8c6c468593295835ddf30cc5d2066ec25330ab2`). A successful
read is observed admin-resource access for this identity/context, not a local
OAuth scope assertion or a promise that subsequent write authorization cannot
change. Denied reads never become absence or mutation permission.

`PreparedDomainSync` binds those observations to the exact credential instance,
account, tenant, audience, endpoint and immutable prepared operation. It is
single-use. Context swaps fail before mutation credentials or approval. The
native head requires an interactive terminal and explicit confirmation of the
path, verified Domain UUID, identity, endpoint, `RemoteWrite`, `ExistingOnly` and
`UnguardedConcurrency`. Piped stdin is not approval. Refusal, interruption or the
120-second overall deadline prevents the POST. Approval is immediate and cannot
be saved as a reusable grant. The existing native session, runtime, timer and
HTTP transport are reused; no authentication is performed by help or tests.

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

`fabric-cli/tests/service_domain_host.rs` additionally exercises the actual new
host with the injected shared HTTP executor: public/daily endpoint resolution,
exact singleton verification, one exact POST, zero-send/zero-token local
preflight, denied admin reads, duplicate names, continuation/row/byte bounds,
approval denial, credential/account/tenant/audience/endpoint swaps, cancellation
before and after dispatch, response loss, throttling, unsupported acceptance,
write/flush failure with retained receipt, and executable help/grammar/nonterminal
approval refusal. The impact catalog includes this consumer and the unchanged
command/catalog `service_sync` suites under the opt-in headless profile.

No live admin action, native credential acceptance or platform identity policy
is claimed by offline tests. Full native Windows/Linux/macOS and stable/optional
gates remain required before a draft becomes review-ready or is integrated.
