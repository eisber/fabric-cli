<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Host-neutral search operation

`fabric-sdk-search` is an SDK-layer library for validated search input,
caller-bound Catalog/Relevance client adapters, and pure deterministic federation.
It does not install a CLI or MCP host, select a native RE origin, discover
credentials, authorize effects, acquire a build slot, or create a runtime.

## Contract

`SearchRequest::{Initial, CatalogResume}` separates fresh text/typed filters from
native Catalog continuation. `InitialSearch::new` validates before execution;
`SearchRequest::from_value` uses the same constructors and rejects unknown fields
and null/wrong types. `schemas()` is the sole public schema bundle for SDK, CLI,
and future MCP projections. Its descriptions identify additional semantic checks.

- Catalog is the default mode. Default page size and result limit are 50;
  default serialized/response budget is 1 MiB, with at most two source invocations.
- Catalog page/result bounds are 1–1000. Relevance/hybrid result limits are 1–50,
  a local wrapper policy, **not an asserted native RE server maximum**.
- Scope is explicit tenant or workspace UUID identity. Provider declarations must
  match the selected tenant; this metadata does not verify credential claims.
- Query text is preserved exactly, with no model, expansion or syntax rewrite.
  Ranked modes require nonblank text. Catalog also accepts filter-only or
  workspace-only discovery; an empty global inventory request is rejected.
- Include/exclude filters admit at most 500 total values, each at most 50 Unicode
  scalars. Duplicates and canonical-type contradictions fail. Raw expressions,
  arbitrary workspace lists, content/files/docs search and hybrid resume are absent.
- UUIDs use exact hexadecimal 8-4-4-4-12 shape and canonical lowercase identity.
  Unknown result types survive. Only `AISkill`/`Data Agent` → `DataAgent` and
  `DataWarehouse` → `Warehouse` are mapped.
- `CatalogCursor` retains its original scope and opaque token. Its 64-KiB ceiling
  is local allocation policy, not a published service token limit. A host importing
  a token must preserve and verify its original caller binding; this association
  is not cryptographic proof of the token's embedded scope.

## Execution and policy

`SearchOperation::new(catalog, relevance).execute(&request, cancelled)` borrows
optional `SearchProvider` implementations. Every selected provider and capability
is validated before either source request. Missing relevance in relevance/hybrid
is a failure, never Catalog fallback. Effective admitted types and native
workspace filtering are explicitly injected capabilities, not inferred endpoints.

The host must approve the exact input, scope, providers and effects first.
The low-level `SearchReceipt` is not a shared-core approval token. It owns no timer;
the host supplies one future combining its 30-second total deadline and caller
cancellation. Cancellation is polled first and again after synchronous work.
Both provider futures are borrowed, joined without spawning, and dropped on
cancellation. Providers must not detach work, add runtimes, discover identity,
retry, traverse pages, or ignore streaming byte bounds.

Each adapter receives a one-request allowance and finite response-byte bound.
Hybrid partitions the response allowance equally between its two sources
(rounding down), so the total streaming allowance never exceeds the invocation
budget. Provider failures also retain their measured response-byte consumption.
The operation separately bounds aggregate reported response bytes, serialized
observations and the unprojected receipt. `SourceEvidence.requests` counts provider
invocation attempts, not verified token-service or HTTP-send counts.

`CatalogProvider::new(CatalogClient, Guid)` uses `CatalogRequest`/`CatalogFilter`
and the additive `search_v2_observed` API. It validates entry discriminator,
item/workspace IDs, item workspace hierarchy and actual optional metadata.
Workspace rows have a distinct identity namespace; unknown item type strings
remain unchanged. Resume sends native token only, without fresh query/filter/
page-size mutation. The tenant is the explicit caller binding, not a claim that
the wire response independently proves a tenant.

`RelevanceProvider::new(RelevanceClient, Guid)` admits Report and SemanticModel.
`with_effective_types` accepts a nonempty distinct caller-admitted subset of the
verified SDK request kinds. It does not enable server features. Inclusion and
exclusion are computed over that finite set; an empty set fails pure preflight
before either hybrid dispatch. Native UTF-16 text limits are checked in that
same preflight. Exact workspace UUIDs use native `queryScope: Workspace` and
`artifactObjectIds: [workspaceId]`, as established by the pinned backend query
builder at `67010e65f61d5faf7eeb99ce31247099c98fd105`; no name/global widening or
workspace enumeration occurs. Returned workspace ownership must match the
requested UUID; missing or conflicting evidence fails the source.
Relevance IDs are mandatory; nullable names/workspace metadata remain
nullable. Disabled/unknown provider coverage is an explicit unavailable source,
not empty success. Result order is preserved without score comparisons.

Both adapters use actual observed status/complete body size and safe Retry-After
evidence from their SDK clients, never reserialized approximations.
`ProviderError.response_bytes` and `SourceEvidence.response_bytes()` are
`Option<usize>`: unavailable is `None`, measured success is `Some(n)`, and an
unselected source is `Some(0)`. Aggregate byte summation includes known counts
only; independent provider-enforced partition ceilings still bound acquisition.
No native origin/audience/authentication implementation is supplied by this crate.
Structured native diagnostics actually returned by a provider are retained in
`SourceEvidence.nativeDiagnostics`, with a local 1-MiB metadata ceiling. Known
and forward-compatible diagnostic fields stay outside projected result data;
retaining them does not request debugging or enable a service policy.

## Canonical approved SDK operation

`search_descriptor()` produces the one operation descriptor, `catalog.search`,
using `schemas()` for both schemas. It is local and needs no request, identity
or runtime, so a head can inspect the descriptor without initializing a provider.
The advertised output schema is the canonical shared-core receipt, not the
low-level `SearchReceipt` envelope.

`prepare_search(&SearchRequest, canonical_path)` returns shared
`PreparedOperation`. Tenant scope requires `/`; workspace scope requires exactly
`/workspaces/<canonical-guid>`. Real tenant/workspace collection identities are
used without fake items. Preparation requires RemoteRead and selects
ModelExecution for explicit relevance/hybrid modes. There is no metering claim.
The caller supplies its own immediate `ApprovalPolicy`.

`execute_approved_search(&ApprovedOperation, &SearchRequest, &SearchOperation,
cancelled)` returns `ExecutionReceipt<SearchData>`. It rejects mismatched
serialized input, target, canonical path, availability or effects before provider
access. It never approves on behalf of the caller. A host may select the declared
LocalWrite effect only with one exact LocalOutput precondition; the host remains
responsible for no-overwrite preflight, absolute local paths and delivery. Other
preconditions are unsupported rather than being presented as enforced guards.

Canonical source evidence lives at `meta.sources`. Its `details` is serialized
`SearchSourceDetails`, retaining the full original source status, coverage, byte
measurement, safe retry/error fields, mode and explicit scope. Executable
continuation is present only in Catalog source details when genuinely resumable;
global discarded-row counts remain outside projected data. Partial/limited
sources produce shared Partial + Incomplete; all-source failures produce shared
Failed with original sources and **no fabricated empty data**. Federation failures
also retain original source evidence. Canonical encoding is bounded before
projection, and build/source/output-budget errors retain the available receipt.
Successfully completed requested top-k retrieval instead produces Succeeded
with shared Unknown coverage: operation completion is separate from exhaustive
inventory completeness. The original RankedTopK marker remains in source details.

## Federation and output

`federate(&request, catalog_observation, relevance_observation)` is pure.
It joins only matching tenant + resource namespace + UUID, with compatible
canonical type and nonconflicting workspace evidence. Workspace entries retain
a distinct identity namespace. Conflicts fail explicitly; names never join.
Foreign-tenant/out-of-scope/filter-violating observations fail that source and
discard all of its candidates, retaining its original status/byte evidence.

Catalog owns placement of overlaps; both original metadata/provenance/ranks
survive. Stable alternation starts with Catalog, retaining source order after
deduplication and applying one global cap. Scores are neither accepted nor compared.
Display name/description are optional and preserve null without inventing a name
from an ID. A candidate still requires authoritative valid identity coordinates.

`SearchReceipt` keeps `sources`, `status`, `scope`, native `continuation`, and
`droppedCandidates` outside `data`. `project` maps only data, retaining the original
typed receipt even on projection failure. Hosts apply JSON/JMESPath projection;
the SDK does not acquire a CLI query dependency.

Failed or explicitly limited sources make the receipt partial; both sources
failed is an error. Completed empty observations remain usable. Successfully
completed requested ranked top-k is successful with unknown inventory coverage,
never a claim of exhaustive inventory. A terminal Catalog
page means only the accessible requested query finished, not tenant completeness.
Native Catalog continuation is exposed as executable resume only in Catalog mode
when no fetched distinct candidates were discarded. Raw native source continuation
remains evidence in hybrid, **not a common or lossless hybrid cursor**.

## Integration and validation

The public facade exposes these contracts through `fabric_sdk::search`, native
clients through `fabric_sdk::{catalog,relevance}`, and injected request
execution through `fabric_sdk::api`. The explicit `fab-cli-v2` resource head
composes the same operation; its local `search --schema` projection starts no
identity or service work. This library does not install a standalone MCP server.

Synthetic `tests/contracts.rs` covers validation, schemas, identities, aliases,
ordering, conflicts, provenance, source failure, partial/cursor semantics,
projection, finite bounds, lazy dispatch and cancellation/drop behavior. Tests
require no credentials, HTTP, runtime, local files or new external dependencies.
`tests/providers.rs` additionally uses test-only credentials and injected shared
executors to check exact request mapping, real body measurements, error/unknown
measurement semantics, strict row normalization, preflight zero-send cases,
native continuation, ranked coverage, federation and client-future cancellation.
Run under the coordinator's authorized validation slot:

```text
cargo test --offline --locked -p fabric-sdk-search
cargo clippy --offline --locked -p fabric-sdk-search --all-targets -- -D warnings
```

These are validation commands, not a claim that they ran. Full consumer/native
composition and independent reviews remain separate integration gates.
