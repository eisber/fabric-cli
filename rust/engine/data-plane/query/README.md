<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded KQL read-intent binding

`fabric-sdk-query` implements a caller-injected KQL request/result adapter,
not JMESPath projection or a new production CLI host. It consumes the existing
`NativeStreamExecutor`, `NativeStreamBody`, `NativeDeadline` and cancellation
primitive. `KustoQueryExecutor` composes the caller's `TokenCredential`, streaming
Azure `HttpClient` and shared `ExecutionTimer`; it installs no HTTP client,
identity provider, timer, runtime or retry.

## Exact binding seam

`EngineBinding::kusto` retains the resolved owner, binding ID/version, exact
HTTPS origin, database, explicit audience scope, account and tenant. None is
selected from query text. `EngineExecutor` binds the injected stream stack to
that same immutable descriptor; `query_kql` rejects any mismatch before opening.
The descriptor is reusable by Execute for its separately approved management
request. It is not a write approval, query safety certificate or token proof.

The existing Fabric-only native HTTP adapter cannot authenticate this Kusto
route. Use `KustoCredentialContext::new(credential, account, tenant, scope)` and
`KustoQueryExecutor::new(binding, credentials, http_client, timer)` instead.
Construction checks exact account, tenant and audience equality without I/O.
The caller must bind the injected provider to those selectors; the shared
credential trait does not expose verifiable account/tenant claims. The adapter
requests precisely the supplied supported scope, never a Fabric default or a
hostname-derived scope, and never selects another identity after failure.

`ReadOnlyKustoV2` denotes structural adapter qualification: exact binding,
fixed query/read policy, explicit audience and single-dispatch streaming. It
does **not** certify live authorization, service read enforcement or an
independently attested transport implementation. `QueryEvidence.adapter_capability`
records that actual promise; the legacy `production_verified` field stays
`false`, even after a successful production exchange. No live query, login or
tenant-enforcement test was performed.

Arbitrary `EngineExecutor` implementations still default to
`UnverifiedProduction` and fail before dispatch. Public trait implementations
are responsible for any admitted capability they explicitly promise; this is
not cryptographic proof. `ScriptedOffline` remains an offline-only promise and
must never label a network implementation. There is no production-enable boolean.

## Native read policy and results

`KqlQuery` always creates one `POST /v2/rest/query`. It sets KQL language,
`request_readonly`, `request_readonly_hardline` and disables callouts, remote
entities, external data, impersonation and sandbox execution. There is no
user-supplied options object, language flag, parameter substitution, management
prefix routing, SQL compatibility fallback or identity fallback.

An explicit server timeout must be positive, millisecond-representable, at most
one hour and within the remaining local deadline. Production calls require
`KqlQuery::with_server_timeout`; no timeout is inferred or silently clamped.
The fixed route, method, exact body shape, database, read-policy options and
correlation metadata are validated before credentials and again before dispatch.
Duplicate or unknown request fields and extra request headers are rejected.
Credential acquisition can exhaust the deadline or leave less than the explicit
server timeout: that fails before dispatch instead of weakening the request.
The same absolute deadline covers credentials, opening, all reads and result
validation. The shared stack enforces pending I/O; no query-local clock is
installed. Local cancellation drops local work; it neither cancels the remote
request nor implies rollback or replay.
Cancellation is observed at each open/read and native frame/row validation
boundary, including immediately ready streams; it is not checked only when I/O
happens to suspend. Cancellation during validation retains bounded provider bytes
and already validated tables as incomplete forensic evidence, never completion.

Success requires JSON media, actual HTTP EOF, a non-progressive `v2.0` header,
well-formed tables and terminal completion without errors/cancellation or native
completion warnings.
All primary tables remain in native order, with declared types, duplicate column
names and positional JSON values intact. Unknown native type names remain
unknown to the adapter; they are not inferred from sample values. Non-primary
frames remain execution metadata. Progressive and unknown frame protocols fail
explicitly rather than being partially interpreted.

`QueryLimits` has no new CLI defaults. Request/response byte and shape limits
are explicit. The row limit applies across primary tables; exceeding it returns
`QueryErrorKind::RowLimit` with validated partial data and terminal metadata.
Native terminal failure/cancellation likewise preserves partial results in an
error, not a success envelope. In-data `OneApiErrors` bags are separate
diagnostics, never positional rows. `QueryCompletionInformation` uses the native
v2 `Level`, `ClientRequestId` and `Payload` schema: Level 2 or smaller is a
failure; Level 3 is a warning/non-clean result; informational/statistics Levels
4 through 6 remain metadata and can complete successfully. Unknown/malformed
status schemas fail explicitly, with no v1 Severity fallback or message guessing.
Original status rows, error bags and terminal metadata remain inspectable.
Malformed tables or missing terminal frames retain prior structurally validated
observations. Every failed response retains its bounded raw provider bytes;
interrupted streams retain the bytes received before the interruption.
Retained data after a native partial query failure is forensic evidence only:
Kusto can flush incorrect values, so consumers must not use it as query results.
Debug/Display omit input, response data, endpoints and identity selectors.

The separate unprojected `QueryEvidence` records binding, language/transport,
version, policy, adapter capability, correlation IDs, actual HTTP EOF and native
completeness. HTTP EOF alone does not establish a successful query. A correlation ID is not an
async handle. No continuation, LRO, polling or automatic remote cancel is
invented. Output bounds do not bound scans, memory on the service or capacity
costs. Arrow/CSV/Parquet conversion, file input/output, facade/path resolution,
native parameter schemas and production CLI/MCP composition are not implemented
by this SDK.

## Direct SDK use and support boundaries

The public consumer example `examples/kusto_query.rs` runs the production
adapter with synthetic HTTP and credential fixtures. It does not discover
credentials, connect to its illustrative endpoint or query tenant data:

```text
cargo run --locked -p fabric-sdk-query --example kusto_query
```

In a real host, inject its existing streaming client and timer and an explicitly
selected credential. The client must disable redirects and automatic retries
and release local resources when dropped. The SDK cannot inspect an arbitrary
trait implementation to prove those promises. Do not wrap a retrying HTTP client
or use an ambient/default credential chain. An HTTP 200 response still requires
valid media, actual EOF, all ordered primary tables and a clean terminal frame;
401/403/429/transient failures never cause token/identity refresh or replay here.

| Surface | Supported contract | Not established or unsupported |
| --- | --- | --- |
| Platforms | Runtime-neutral Rust on Windows, Linux and macOS with caller-provided HTTP/TLS/time | Native platform/live acceptance must be qualified separately |
| Audience | Exact public Kusto `https://api.kusto.windows.net/.default` | Fabric REST, ARM, SQL, arbitrary inferred and sovereign scopes |
| Endpoint | Exact caller-resolved HTTPS origin on port 443, one database and owner | Endpoint discovery, ingestion, management, redirects and route fallback |
| Identity | Injected `TokenCredential`, explicitly matching account and tenant selectors | Token-claim proof, login, consent, ambient credential discovery and provider fallback |
| Permissions | Caller supplies a credential authorized to query the exact database; Kusto Database Viewer or a suitably scoped equivalent must be provisioned by its owner | No permission is granted or tested by construction; service policy/tenant proof remains external |
| Runtime | Existing shared streaming HTTP engine and caller timer, one absolute `NativeDeadline` | New runtime, transport, global timer or retry stack |
| Protocol | KQL v2 nonprogressive query, finite limits, all native primary results and diagnostics | SQL compatibility, progressive response, management, remote cancellation and output codecs |

Database query rights do not imply that every table or query is authorized.
The hardline flags are sent on every request but are not a cost quota or a
substitute for service-side authorization. A write/management-looking string is
sent unchanged to the same query route under the same policy, never rerouted.
Keep provider bodies, positional rows, diagnostics and serialized binding
metadata out of logs: only sanitized error formatting is safe by default.

## Source and offline evidence

This additive slice starts at Rust checkpoint
`cc183dd43140350eb1f23af0636332150d1f6d50`; existing v1 and frozen oracles are
unchanged. Direct offline public-consumer tests exercise exact POST/policy,
binding mismatch and production rejection, native ordered typed tables,
terminal failure after rows, row/byte caps, broken streams, media errors,
no replay, cancellation and local stream disposal.
Independent review exposed inline-error retention and completion-warning gaps.
New native-shaped regressions reproduced both against the initial decoder
before repair; the existing v1/frozen oracles are unchanged.

Native contracts:

- [Kusto HTTP request](https://learn.microsoft.com/en-us/kusto/api/rest/request?view=microsoft-fabric)
  separates query from management endpoints.
- [Request properties](https://learn.microsoft.com/en-us/kusto/api/rest/request-properties?view=microsoft-fabric),
  documentation revision `4634221e42e6d708beec152c2343ab49dc38a446`,
  documents strict `request_readonly_hardline` and disallows overriding it with
  a query `set` statement.
- [Kusto v2 response](https://learn.microsoft.com/en-us/kusto/api/rest/response-v2?view=microsoft-fabric),
  revision `85351b30116da41b75c5a4e8a5b7f5730d81ab08`, documents ordered
  primary tables and HTTP-200 partial errors in the terminal frame.
- [Query limits](https://learn.microsoft.com/en-us/kusto/concepts/query-limits?view=microsoft-fabric)
  documents native truncation and timeout restrictions; these are not local
  emitted-output defaults or a cost guarantee.
- [Partial query failures](https://learn.microsoft.com/en-us/kusto/concepts/partial-query-failures?view=microsoft-fabric),
  revision `85351b30116da41b75c5a4e8a5b7f5730d81ab08`, specifies in-data
  property bags and warns that partial data can be incorrect.
- Microsoft SDK fixture shapes at [azure-kusto-python
  3f53425f](https://github.com/Azure/azure-kusto-python/tree/3f53425f7ff8d02de8fc0ea250d02dde30c7d258/azure-kusto-data/tests/input):
  `query_partial_results_defer_is_false.json` contains inline OneApiErrors;
  `query_partial_results_defer_is_true.json` has Level 2 completion failure
  despite terminal HasErrors=false; `deft.json` distinguishes Info/Stats.
- [Microsoft SDK v2 status severity mapping](https://github.com/Azure/azure-kusto-node/blob/d1319e5a1ad220926e700c5c682b10f8d4bf0635/packages/azure-kusto-data/src/response.ts)
  uses `Level` and defines Error=2/Warning=3. No SDK implementation is copied
  or imported at runtime; regression values are synthetic.
