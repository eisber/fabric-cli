<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# V2 metadata Exists

The explicit `fab-cli-v2` head now accepts `exists <path> [--max-bytes N]
[--timeout SECONDS]`. The Python `fab`, Rust v1 `fab-rs`, discovery-only head
and separate local `fab-cli` package are unchanged.

```text
fab-cli-v2 exists /Sales.Workspace/Forecast.Notebook
fab-cli-v2 exists /workspaces/<workspace-uuid>/items/<item-uuid>
fab-cli-v2 exists /workspaces/<workspace-uuid>
```

This slice observes workspace and generic item **metadata only**. Physical
OneLake files/directories, catalog namespaces/tables, Fabric folders, definition
parts, `docs/` representations, jobs and hidden/admin resources are deferred.
Generic item request support is not proof that every provider type supports
that endpoint. An unsupported provider response is an error, never absence.
Names use the current v2 Get resolver's ASCII-case-insensitive display-name
comparison with exact type spelling. Root-item name resolution explicitly uses
`recursive=false` and excludes rows with a containing folder. It does not
silently search other folders or pick a different type.

## Results and evidence

The SDK's `fabric_sdk::exists` operation and CLI deliver the same canonical
newline-terminated JSON report. `exists` is a native boolean or null, not the
v1 string message. Both determinate values exit 0. Null observations exit 1;
invalid arguments exit 2. This preserves successful boolean-result semantics
without falsely reporting a failed lookup as absence.

| Observation | Result | Evidence |
| --- | --- | --- |
| Present | `true` | Valid exact metadata response with matching IDs/type/name; HTTP 200 |
| Authoritative item absence | `false` | Exact item-ID Get returns HTTP 404 with the documented `ItemNotFound` code |
| Denied | `null` | HTTP 403; does not prove either presence or absence |
| Ambiguous | `null` | Multiple distinct IDs match the scoped name/type after complete paging |
| Incomplete | `null` | Page/row/response/aggregate/retention bound or invalid continuation |
| Unknown | `null` | Visible-inventory miss, unverified 404, changing target, timeout/cancellation or transport uncertainty |
| Failed | `null` | Authentication, malformed response, type/identity mismatch or other protocol failure |

Workspace listing returns workspaces the principal can access. Even completed
paging does not prove that a missing name is absent tenant-wide. Workspace Get
does not have a verified missing classification in this slice, so its 404 is
unknown. A named item found during enumeration that disappears or moves before
Get is `ChangedDuringProbe`, not authoritative absence. A later page's 404 is
never mistaken for a singleton item's missing response. Duplicate rows for the
same UUID are not distinct identities. Partial lists cannot establish uniqueness.

The operation reuses `MetadataHost`, workspace/item SDK Get/List, bounded paging,
caller credential injection and the shared `v2_outcome` native evidence container.
It does not add a Get library, pager, HTTP engine, authentication provider, token
cache or runtime. The shared contract's `Verb::Exists` and
`exists_descriptor()` identify one operation, `fabric.metadata.exists`.
Preparation binds the real tenant collection and the complete immutable request;
unresolved names never become fabricated item/workspace IDs. Immediate host
policy must authorize exactly that request's RemoteRead effect and no mutation
preconditions. SDK client tenant/audience and approved input are checked before
credential/HTTP access.

Native response statuses, bodies and allowlisted headers remain available through
explicit SDK getters. Automatic JSON/Debug omits raw metadata, provider messages,
credentials, continuation tokens and header values. The CLI reports bounded
status/byte summaries, IDs and reason categories; it does not hide errors behind
an empty array. Completed negative observations retain the actual 404 rather
than synthesizing HTTP success. Existing shared receipt-success validation is
unchanged.

## Bounds, cancellation and freshness

Omitted byte bounds reuse the existing SDK workspace response/report bound;
omitted timeout reuses the explicit Get head's total timeout. Page/row/retention
defaults remain the existing SDK defaults. `--max-bytes` caps native response
retention in aggregate and the whole serialized output, including LF. No clipping
or success-shaped fallback occurs after bounds are exceeded.

`ExistsHost::execute_until` takes a caller-owned typed stopping future. The same
latched future covers every discovery/probe read and preserves cancellation
versus deadline expiry. No SDK timer, runtime or signal handler is installed.
The native head borrows its existing explicit `NativeSession` setup and provides
one deadline from before identity setup. The unit-cancellation `execute` entry
remains available to embedding callers.

Each invocation uses fresh reads without a persistent existence cache. Credential
token caching remains the injected provider's responsibility. Reads are sequential
observations, **not an atomic snapshot, mutation precondition, lock, reservation,
connectivity test or permission grant**. Create/Update/Delete must enforce their
own documented native conditions. There are no writes, implicit sign-in, credential
fallbacks, retries of uncertain mutations or remote cancellation calls here.
Output writer failures propagate without repeating the probe.

## Source and verification boundaries

- [Get Item][item] documents item Read permission, generic/type-specific delegated
  scopes and `ItemNotFound`.
- [List Items][items] documents Viewer workspace access, paging, folder selection
  and the default recursive behavior.
- [Get Workspace][workspace] documents primary metadata and Viewer+ access.
- [List Workspaces][workspaces] explicitly limits results to principal-accessible
  workspaces.

Fabric metadata uses the Fabric audience, not OneLake Storage. Users/service
principals/managed identities still require the provider's endpoint-specific
permissions and tenant policies. This implementation does not change scopes or
settings, and makes no live capability/platform acceptance claim.

Offline tests cover one-GET success/absence, all other HTTP classifications,
visibility-limited misses, late ambiguity, partial pagination, response identity
and type checks, mid-probe changes, output/native bounds, no invalid-input
credential/HTTP, latched cancellation and deadline, and exact SDK/CLI projection
and writer failure. Actual binary tests exercise help, invalid/physical input
rejection and unconfigured identity without live services. Native three-platform,
stable and applicable optional gates remain required before review-ready.

[item]: https://learn.microsoft.com/en-us/rest/api/fabric/core/items/get-item
[items]: https://learn.microsoft.com/en-us/rest/api/fabric/core/items/list-items
[workspace]: https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/get-workspace
[workspaces]: https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/list-workspaces
