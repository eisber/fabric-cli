<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native direct Relevance Engine discovery

`fabric-sdk-relevance` is an SDK-layer, runtime-neutral client for one bounded
direct artifact discovery request. Its concrete `from_api` adapter uses the
finite shared RE origin, an explicit endpoint and caller-injected API/HTTP execution.
An alternative narrow `DiscoveryExecutor` can be injected by another SDK host.
It does not discover credentials, create a runtime, implement HTTP/retries, call
an LLM, execute `ask`, rewrite the utterance, or follow artifact URLs.

## Public interface

```rust,ignore
let request = DiscoveryRequest::new(
    "quarterly revenue",
    10,
    DiscoveryScope::Tenant,
    vec![ArtifactType::Report, ArtifactType::SemanticModel],
)?;
let result = RelevanceClient::new(shared_relevance_executor)
    .discover(&request, max_response_bytes, cancelled)
    .await?;
```

For the concrete shared native execution path, supply the already-configured
`ApiClient` and an explicit `EndpointBinding` for `ServiceOrigin::RelevanceEngine`:

```rust,ignore
let client = RelevanceClient::from_api(api, relevance_endpoint)?;
let result = client
    .discover(&request, max_response_bytes, cancelled)
    .await?;
```

`from_api` performs no credential or network work. Its private adapter constructs
the exact typed request under `Origin::RelevanceEngine`, **always** rebinds it
through `Request::with_endpoint`, and then sends it through `ApiClient::send`.
The shared URL `https://api.powerbi.com/relevanceengine/v1/` is only a request
construction base; it is not a fallback network destination. The actual host is
the caller's explicit endpoint. The shared client checks credential audience and
executor capability before token acquisition. The Power BI/Fabric-audience policy
is explicitly inherited, not live-admission or consent verification.
There is no raw-parser service selector, `ApiClients::get` fallback or supplied
credential.

`RelevanceClient::from_api_with_endpoint(api, endpoint)` is a compatible alias
with the same explicit-endpoint requirement. Both reject other service identities
immediately, retaining shared origin validation rather than rewriting a Power BI
`/v1.0/myorg/` request. Native composition must require an explicit
`FAB_RUST_RELEVANCE_HOST` and explicitly selected inherited Fabric audience policy;
it must not silently fall back to `api.powerbi.com`. The SDK does not read that
environment variable or discover identity/HTTP configuration.

The supplied executor receives only a validated `DiscoveryRequest` and finite
response-byte limit, not a caller-selectable endpoint. The verified controller
route is `POST /relevanceengine/v1/discoverArtifacts` with exactly one
`searchText` URI query parameter. `request.query()` and `request.body()` provide
its exact wire inputs for the shared request builder to encode once.
The host/audience pairing belongs to reviewed shared core/API/HTTP policy, not
this crate. The existing `Origin::PowerBi` is **not** interchangeable: it appends the unrelated
`/v1.0/myorg/` root and preserves the Python Fabric-audience policy.
The finite shared origin and its audience policy are owned by integration;
there is no arbitrary-endpoint, token-scope, host or auth-header escape hatch here.

The concrete adapter implements request construction and shared execution below;
the central host supplies the explicit origin/identity/HTTP bindings. An injected
custom operation adapter must follow the same contract:

1. Select the reviewed finite direct RE service origin and matching explicit
   credential selection; never infer or retry another audience from a response.
   The integration owner may explicitly inherit the existing Power BI policy
   (Fabric scope). Record that pairing as inherited policy with unverified live
   RE acceptance, not as a newly proven service audience.
2. Construct the exact POST using `request.query()` and `request.body()` through
   `fabric-sdk-api::Request`, preserving shared origin validation. If that
   origin's root is `/relevanceengine/v1/`, use `request.action()` as its relative
   endpoint (`discoverArtifacts`), not the full `request.path()` again.
3. Execute once with `ApiClient::send` over the existing bounded HTTP executor.
   The SDK's outer cancellation drops that future, including token acquisition.
4. Return the shared `WorkspaceResponse`/`ApiError` without status coercion or
   inventing empty results. Enforce the supplied byte limit while reading.

This narrow adapter boundary lets shared integration own the endpoint change
without hardcoding an unverified audience or bypassing existing origin checks.
The finite origin's selected default is repository policy, not proof of live
admission or consent. The caller supplies credentials and explicit identity
selection; audience and endpoint mismatches must fail before credential
acquisition in the shared authentication boundary. Do not select or retry another
resource merely because a hostname or service name looks similar. No new Power BI
`.default` mapping or resource fallback is defined here.

The source model mounts the RE gateway at HTTPS port 443 under `relevanceengine/`;
the forwarding controller adds `v1/discoverArtifacts`. Its host derives from
`InternalWebApiGatewayHost`, which does not establish a fixed public authority
such as `api.powerbi.com`. The shared bearer authenticator's accepted audiences
are the configured `PowerBiApiResourceId` plus `AdditionalResourceIds`, not an
audience inferred from this route. Source gateway validation tests use the
standard resource `https://analysis.windows.net/powerbi/api`, but test literals
are not evidence of deployment-specific additional audiences. No live settings
or tenant data were read, and this crate makes no deployment-acceptance claim.

The JSON body contains only:

```json
{
  "discoveryScope": {
    "queryScope": "Global",
    "allowedArtifactTypes": [3, 2]
  },
  "top": 10,
  "userIntent": "search-intent"
}
```

Request type filters are **numeric registry IDs**, whereas response `documentType`
values are **strings**. Supported request filters are SemanticModel (2), Report
(3), AISkill (5003), OrgApp (16000), App (16001) and Ontology (21005).
Paginated-report and workspace types can be decoded in related metadata, but are
not accepted as request filters. Unknown types, duplicate types and an empty list
are rejected; an empty list would make the service expand the scope of types.

The direct `DiscoveryOptions.userIntent` property uses `StringEnumConverter`.
The pinned `UserIntentType` defines `Unknown = 0` with `EnumMember` value
`unknown-intent`, and `SearchQuery = 1` with value **`search-intent`**. Therefore the
SDK emits the explicit direct wire string `search-intent`, not an inferred enum
name or the workload wrapper's numeric encoding. The property has no initializer,
so omission leaves the CLR zero/Unknown value; a service helper can normalize it
to SearchQuery, but this SDK does not rely on that normalization. It never emits
the AnswerQuery intent (`answer-intent`, enum value 2).

### Input and scope boundaries

- Search text must be explicitly supplied, nonblank and at most **500 UTF-16 code
  units**, matching the .NET service limit. Accepted text is sent unchanged.
- `max_results` is **1–50**, an SDK/host safety bound. The service's verified
  `top` validation is 1–200; 50 is not claimed to be its maximum.
- Tenant scope means the selected tenant's accessible artifacts, not every
  artifact in that tenant and not an administrative tenant inventory.
- Exact workspace-ID scope requires a nonempty GUID. It emits
  `queryScope: "Workspace"` with `artifactObjectIds: ["<workspace-id>"]`;
  the pinned backend query builder explicitly interprets those IDs as workspace
  IDs, and `WorkspaceIdSearchOptionHandler` adds an exact-ID restriction.
  No workspace name is resolved and no global top-k response is fetched for local
  filtering. A top-level response row outside that exact workspace fails explicitly.
- Other scope kinds are not represented by this API and must not be widened
  by adapters. The service's app/artifact-list scope variants are outside this
  provider's bounded tenant/workspace interface.
- No experimental, debug, metadata-workspace, conversation, arbitrary/answer
  intent override or internal app-only workload options are emitted. SearchQuery
  intent is fixed explicitly as described above.

### Results, failures and cancellation

Only HTTP 200 is a completed discovery response; additional 2xx statuses use the
shared `UnsupportedStatus` classification, not empty success. Non-success HTTP
responses decode the source-backed nested `error.code`/optional `error.message`
Power BI envelope, including typed validation details and optional inner-error
metadata. The verified legacy-compatible top-level key is `code`, **not** Catalog's
`errorCode`. Error messages remain untrusted data and are excluded from the SDK
error's display/debug output. A malformed nonempty failure body produces
`InvalidErrorResponse` with the actual HTTP status. Bodyless failures preserve
their failing status without fabricating a code or an empty result.
`ServiceFailure` retains `Retry-After` and header error codes separately from the
body; no automatic retry is initiated. Shared execution failures retain their
typed cause. Malformed success JSON, missing required keys, incorrect known field types,
invalid query GUIDs, excess rows and out-of-filter top-level types fail explicitly.
Required nullable C# reference fields retain JSON null as `Option`, rather than
inventing IDs, names, URLs or matched-signal arrays. Known optional fields are
typed, and unrecognized optional metadata is preserved without executing it.
Scores and row order are preserved; duplicates are not silently removed.

`DiscoveryCoverage::RankedTopK` is always **non-exhaustive**, including an empty
result. It does not assert an authoritative total, a continuation cursor, or a
complete search of accessible artifacts. The disabled controller returns
`Guid.Empty` and no rows; this becomes `DisabledOrUnknown`, never authoritative
zero. An empty query GUID with nonempty rows is rejected as inconsistent.
There is exactly one logical POST, no paging sweep, and no SDK POST replay.
The caller supplies a finite response-byte bound enforced by shared execution
and checked again before decoding.

Cancellation drops local work through the shared client. The pinned controller
uses `CancellationToken.None` for its backend call, so dropping the client request
does **not** establish server-side cancellation.

### Observed response facts

The additive `discover_observed(&DiscoveryRequest, NonZeroUsize, cancelled)` API
returns `Result<DiscoveryObservation, DiscoveryObservationError>`. The original
`discover` remains unchanged and discards only this extra observation metadata.
`RelevanceObservationError` is an exported compatible alias for the same failure
type, for neutral-provider consumers.

- `DiscoveryObservation` contains the validated `result`, actual `http_status`
  and actual complete raw body length as `response_bytes`. Bytes are counted
  before JSON decoding; no result reserialization is used to invent a measurement.
- `DiscoveryObservationError` contains `error`, optional `http_status`, optional
  `response_bytes` and optional `retry_after_seconds`. Complete HTTP status,
  decode and local body-bound failures retain observed status/body length,
  including a measured zero-byte response. Credential/executor/stream failures
  before a complete raw response reached this client retain **None**, not a
  guessed status, byte limit or partial-byte estimate.
- `retry_after_seconds` is set only for a valid nonnegative integer Retry-After
  delta. Overflow, malformed values and HTTP-date values are not converted or
  guessed; date conversion would require a caller-owned clock. The original
  header remains available on structured service failures. No retry is initiated.

## Pinned contract evidence

Implementation was checked with the owner's authorized, authenticated source
reader at Power BI source commit
`67010e65f61d5faf7eeb99ce31247099c98fd105`. No private source bodies are included.
The succinct API facts above come from:

- `RelevanceEngineService/Contracts/RelevanceEngineServiceRoutes.cs`
- `RelevanceEngineService/Service/Controllers/RelevanceEngineController.cs`,
  direct `DiscoverArtifactsAsync`, not `DiscoverArtifactsFromWorkload`
- `RelevanceEngineService/Contracts/{DiscoveryOptions,DiscoveryScope,`
  `DiscoveryQueryScope,DiscoveryConstants,ArtifactRegistryTypes,`
  `RelevanceConfidence,UserIntentType}.cs`
- `RelevanceEngineService/Common/Extensions/DiscoveryOptionsExtensions.cs`
- `RelevanceEngineService/ArtifactIndex/Indexing/IndexClient/QueryBuilders/`
  `Builders/ArtifactSearchQueryBuilder.cs`, especially the workspace-ID mapping
- `ASGateway/PowerBI/RelevanceEngine/{RelevanceEngineApiHost,`
  `RelevanceEngineApiForwardingControllerBlock}.cs`
- `/Sql/CloudBI/Common/ModelCommon/Model/BIAzureServiceModel.cs`,
  the HTTPS `relevanceengine/` gateway endpoint declaration
- `/Sql/CloudBI/Common/Gateway/Common/Authentication/`
  `AzureActiveDirectoryTokenAuthenticator.cs`, the configured audience union
- `/Sql/CloudBI/Common/ConfigurationCommon/`
  `AzureActiveDirectoryAuthenticationConfiguration.cs`, the source property types
- `/Sql/CloudBI/AS/test/ComponentTests/PowerBIASGateway/`
  `JsonWebSecurityTokenHandlerFactoryComponentTests.cs`, standard resource test
- `RelevanceEngineService/Service/RelevanceEngineServiceWebApiHost.cs` and
  `ServiceCommonWebApiHost/PowerBIWebApiHost.cs`, the inherited exception filter
- `/Sql/CloudBI/AS/src/WebApiCommon/{WebApiExceptionFilterAttribute,`
  `ValidateModelAttribute}.cs`, the actual error production paths
- `/Sql/CloudBI/Common/ServiceContractsCommon/{PowerBIServiceException.cs,`
  `API/APIErrorResponse.cs}`, nested and legacy-compatible error contracts
- `PowerBIServiceContracts/RelevanceEngineService/{PublicDiscoverArtifactsResult,`
  `PublicArtifactDocument,PublicSearchDiagnostics}.cs`

RE and service-contract paths are relative to `/Sql/CloudBI/AS/src/PowerBI/`;
the gateway paths are relative to `/Sql/CloudBI/AS/src/` in that source revision.
The direct action applies grouped delegated-read authorization, user throttling
(60/minute), and tenant multi-workspace read filtering. These are service-side
policies, not permissions granted or bypassed by this SDK.

For an ordinary delegated principal, the controller's four groups require one
scope from **each** group:

| Group | Accepted scopes |
| --- | --- |
| App | `App.Read.All`, `Item.Read.All`, `Item.ReadWrite.All` |
| Dataset | `Dataset.Read.All`, `Dataset.ReadWrite.All`, `Item.Read.All`, `Item.ReadWrite.All` |
| Report | `Report.Read.All`, `Report.ReadWrite.All`, `Item.Read.All`, `Item.ReadWrite.All` |
| Data agent | `DataAgent.Read.All`, `DataAgent.ReadWrite.All`, `Item.Read.All`, `Item.ReadWrite.All` |

Consequently `Item.Read.All` occurs in all four groups; the dataset/report scopes
alone do not satisfy the ordinary grouped check. Literal values are from
`PowerBIServiceContracts/Api/Scopes.cs`; grouped evaluation is from
`Security/AuthorizationRequireAnyScopeFromEachGroupAttribute.cs` and
`Security/AuthorizationHelper.cs` at the pinned revision. These paths are relative
to `/Sql/CloudBI/AS/src/PowerBI/`. The service also implements its own first-party,
service-principal and other authorization cases. The SDK does not introspect a
token, infer principal type, request broader permissions, or reproduce those
exceptions locally.

## Offline validation

Synthetic tests cover route/query/body operation inputs, request bounds, enum wire
types, strict required and optional fields, nullable fields, metadata preservation,
rank/order, disabled semantics, exact workspace-ID request and response checks,
unsupported types, response-byte bounds,
HTTP failures, propagation of shared authentication/execution errors and local
cancellation. Error checks include nested/legacy/bodyless failures, malformed
error fields, metadata preservation, Retry-After and sanitized display/debug.
Central integration must additionally test its actual HTTP
method/URL encoding, audience selection and pre-token origin validation.
The additional `api_binding` tests exercise concrete `from_api` construction,
exact shared request URL/body, inherited Fabric scope, caller-selected endpoint
preservation, pre-token service/audience/capability/cancellation failures and
structured throttling without POST replay. Mock success verifies the adapter
contract, not acceptance by a live RE gateway.
Observed API tests separately cover original whitespace-bearing JSON bytes,
complete-response success/decode/status/bodyless/body-bound failures, failures
without raw responses, pre-cancellation and valid/invalid Retry-After deltas.

Run the targeted test and all-target lint only in the integration owner's granted
shared-host build slot:

```text
cargo test -p fabric-sdk-relevance --all-targets --offline
cargo clippy -p fabric-sdk-relevance --all-targets --offline -- -D warnings
```

These are offline contract checks, not live tenant or production auth acceptance.
