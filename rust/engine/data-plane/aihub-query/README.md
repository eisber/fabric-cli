<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# AIHub DataAgent query SDK

`fabric-sdk-aihub-query` is an independently callable, runtime-neutral SDK for
`execute_query` through an explicitly bound AIHub MCP endpoint. It does not depend
on the CLI or an identity implementation, create a runtime, discover credentials,
prompt, write files, or query a live service in its examples and tests.

## Start here

Run the complete synthetic consumer from `rust/`:

```text
cargo run --locked -p fabric-sdk-aihub-query --example injected_query
cargo test --locked -p fabric-sdk-aihub-query --all-targets
cargo clippy --locked -p fabric-sdk-aihub-query --all-targets -- -D warnings
```

The [example](examples/injected_query.rs) calls the real
`AiHubQueryClient::execute_query` API over injected `HttpClient`,
`TokenCredential`, and `ExecutionTimer` fixtures. Its HTTP exchanges initialize
MCP, acknowledge initialization, discover `execute_query`, and call it once.
It asserts the actual variant metadata and exact numeric result without export.
The host must configure its existing Azure HTTP transport for streaming,
**no redirects and no automatic retries**. The SDK cannot undo retries inside
an arbitrary caller-supplied transport.

Construct `AuthBinding` with the exact HTTPS endpoint, token scope, tenant and
account. The caller must qualify this combination before live use. Opaque
`TokenCredential` tokens cannot prove their account/tenant/audience to this
library. No default endpoint, audience, ring, account or credential provider is
selected. A scope is requested exactly once, with no identity fallback.

Construct `DataAgentTarget` from **DataAgent ID and datasource ID**, not a
workspace/item pair. Both IDs are nonempty GUIDs; their semantic ownership is
verified by the service, not guessed locally. Select `Production` or `Sandbox`
explicitly. `QueryRequest::new(..., TypedJson::Required)` makes typed-result
opt-in explicit; legacy CSV is not a typed-result mode.

## Capabilities and evidence limits

| Surface | Implemented behavior | Qualification |
| --- | --- | --- |
| MCP | Streamable HTTP POST; JSON or bounded SSE response; initialize and negotiated `2025-06-18` or `2025-03-26`; initialized notification; bounded paged tool discovery; one tool call | Synthetic HTTP fixtures; no live endpoint qualification |
| Authentication | Caller-injected credential and exact scope/account/tenant labels | Labels are caller attestations, not token inspection; supported live caller contract remains unverified |
| Production auth | No automatic user-token or broker composition | Available source evidence describes S2sObo; user CLI token compatibility is **not established** |
| Typed execution route | Explicit DataAgent/datasource with both routing and typed-result variants | Source-snapshot contract only; opt-in is not routing eligibility or authorization |
| Read policy | Always request the existing positive SQL/GQL policy variant; require discovery `readOnlyHint: true` and `destructiveHint: false` before query dispatch | Requested policy and an untrusted discovery contract, not cryptographic or live enforcement proof |
| Direct Report/SemanticModel | Not advertised as a typed SDK route | No universal typed-result coverage claim |
| Stage | Explicit Production (Sandbox token absent) or Sandbox (existing token present) | Never inferred from typed opt-in; no alternate-stage fallback |
| Templates | Explicit `QueryMode::Template`, `language: "jmespath"` and existing template variant; positive tool-schema enum or quoted selector advertisement required | A selector advertisement is not evidence that a particular template exists or that the caller is eligible |
| Results | Embedded `application/json` **and** resource `_meta["fabric.queryResult"]`; schema version 1 | No Markdown, CSV, URI-extension, or `structuredContent` inference |
| Source freshness | Contract derived from an available October 2026 source snapshot | Not verified current or deployed; private implementation/source material intentionally omitted |
| Files/CLI/Arrow | No local export or CLI composition | Other consumers explicitly convert the public native result model |

All fixtures, identities, endpoints, query text and resource contents are
synthetic. No live query, sign-in, consent, credential discovery or service write
is part of this package's evidence.

## Variants and arguments

Each lifecycle/discovery/call request consistently carries the comma-separated
string at `params._meta.variants`. This is the supported primary body form; the
SDK does **not** also send `FABAIHUB-X-Variants`, which is a server-side fallback.

| Existing variant | Purpose | Selection |
| --- | --- | --- |
| `Fabric.Routing.Documents.DataAgent` | DataAgent route opt-in | Always for this explicit route |
| `Fabric.Routing.Documents.DataAgent.TypedQueryResults` | Versioned JSON resource opt-in | Always, because typed JSON is required |
| `Fabric.Routing.Documents.SqlReadOnlyEnforce` | One-way positive SQL/GQL read-policy request | Always, including lifecycle and discovery |
| `Fabric.Routing.Documents.DataAgent.Sandbox` | Draft configuration stage | Only explicit Sandbox |
| `Fabric.Routing.Documents.QueryTemplates` | Curated template selector | Only explicit Template mode |

These are existing request variants, not new service flights, rollout changes,
permission grants, or a request to modify shared settings.

The positive read-policy variant takes precedence over an operator disable
flight in the available source contract, but an explicit in-process server
override can still take precedence over it. The SQL/GQL policy does not validate
DAX or KQL. Discovery hints are server-supplied and not an independent enforcement
mechanism: missing, malformed, false read-only, or true destructive hints fail
closed before query HTTP dispatch. These checks do not prove a trusted server,
deployed policy, or safe live target; caller qualification and the documented
authentication/source limits still apply. The SDK offers no write-approval bypass,
client SQL-text heuristic, disable-policy token, or alternate query provider.

Tool arguments are `artifactId`, `query`, optional `filename`, optional
`language` (only `jmespath`), and `max_rows`. Native mode omits `language`; the
datasource owns its engine dialect. Arbitrary dialect/parameter injection is not
exposed. Filename bases are restricted to 1..=50 ASCII letters, digits, hyphens
and underscores. The default row cap is 1,000; callers can explicitly request
1..=10,000. Invalid bounds are rejected, not silently clamped.
**An output row cap is not a server compute, cost, or read-only enforcement bound.**

## Native result contract

`QueryOutcome::Complete(TypedQueryResult)` requires valid typed resources that
all report `truncated: false`. `QueryOutcome::Incomplete(...)` explicitly retains
truncated data; it is not completed data. Unknown total row count stays `None`,
not a fabricated total. Missing typed JSON fails with `MissingTypedJson` and
actionable routing/eligibility/opt-in guidance. A legacy direct-artifact string
never becomes a typed success.

Every `QueryResource` retains `schema_version`, `result_id`, `uri`, ordered
`columns`, positional `rows`, `row_count`, `truncated`, `max_rows`, and
`total_row_count`. Columns retain exact `name`, `engine_type`, `logical_type`,
and `encoding`. Empty schemas, all-null columns and duplicate column names
remain intact. Every typed resultset is retained in content order. Auxiliary
embedded template resources (including metadata) and prose are retained
separately; none is silently selected as the first resultset or downloaded.
CSV/XLSX blocks without typed descriptors fail rather than conceal untyped
secondary results.

The decoder validates matching descriptor/body IDs, version/schema/counts/cap/
truncation, positional arity, exact null coordinates, duplicate JSON properties,
duplicate resource URIs and duplicate result IDs. It enforces configured byte,
depth, node, resource, column, row and cell budgets. The defaults are 32 MiB per
decoded HTTP response, 16 MiB combined resource text/metadata, 128 content
blocks, 32 resultsets, 1,024 columns, 10,000 aggregate rows and 1,000,000 cells.
Bounds may be lowered, never raised above these envelopes.

Duplicate/depth validation borrows subtrees from the bounded input instead of
copying large payloads at every nesting level. SSE frames are parsed as chunks
arrive; the matching terminal JSON-RPC response completes observation even if
the transport remains open. Releasing that body is local disposal, not a remote
cancellation request.

`NativeValue::{Integer, Decimal, Number}` holds the **original JSON number
lexeme**, without a floating-point intermediary. Wide signed/unsigned values,
decimal digits, scale and exponent survive; no decimal precision/scale is
invented. Temporal values preserve their original offsets/fractional text;
GUIDs and base64 binary are validated and retained exactly. Unknown native
engine types remain attached to their declared portable representation,
including exact text for `string`/`text`. Unsupported logical-type/encoding
pairs, scalar mismatches and structured cells fail explicitly rather than
being coerced.

Syntax validation uses `serde::de::IgnoredAny` and requires end of input.
An SDK-private bounded span decoder captures original cell tokens; it does not
enable `serde_json`'s `raw_value` or `arbitrary_precision` features. This avoids
changing unrelated workspace JSON consumers through Cargo feature unification,
including objects with serde-private-looking property names.

## Failure and lifetime contract

One caller-supplied `NativeDeadline` covers credential acquisition, all MCP
exchanges, streaming, discovery, parsing and decoding. The caller supplies
asynchronous time through the existing `ExecutionTimer` seam. Cancellation
is checked around dispatch/body/decode boundaries and wins a simultaneous ready
result. Dropping the future releases local credential/HTTP/body/timer futures.
Neither drop nor local timeout proves remote cancellation, rollback, or that
server compute stopped.

HTTP errors (including auth/permission/429), JSON-RPC errors, MCP `isError`,
unsupported Tasks/lifecycle, accepted/unknown/malformed results and byte/shape
failures are not completed data. Every POST is single dispatch; there is no
hidden retry, replay, reconnect, redirect, provider/identity fallback, URI
download, fabricated task, polling handle, or remote cancel request.
Only a session ID actually returned by initialization is propagated.
`QueryError::query_dispatched` records whether query HTTP dispatch began:
an error after that point can mean the remote outcome is unknown and never
authorizes replay. Diagnostics and `Debug` redact queries, identities, URLs,
provider prose and data; callers own the sensitive typed result itself.

This package is additive. Existing KQL, Dataflow, CLI grammar/output, shared HTTP,
identity and clock implementations are unchanged. SDK/fixture evidence alone
does not qualify production authentication, a deployed service, a CLI command,
or an export format.
