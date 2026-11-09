<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# MLModel native input and output contracts

The focused item SDK exposes additive MLModel request and payload contracts.
They describe documented native APIs over existing injected execution. They
are not an embedded model runtime, a CLI implementation, or live-service
acceptance evidence.

## Parameter transport

The SDK consumes native typed JSON models. The common host owns explicit
file/stdin input, strict decoding, byte budgets, schema discovery and local
diagnostics. This follows the published-UDF run separation between parameter
transport and the service-specific request. It does not reuse UDF's endpoint,
audience, parameter names or synchronous response protocol for ML scoring.

For scoring, required `inputs` is an array of native JSON values. Optional
`formatType` currently admits `dataframe`; optional `orientation` admits
`split`, `values`, `record`, `index`, and `table`. The service spells `record`
singular. Omitted options are not replaced with guessed defaults. False, zero,
null, strings and nested data inside the native input remain values, not
stringified command arguments. Model-specific signatures remain authoritative;
valid JSON alone does not establish compatibility with a model.

Item creation accepts metadata, not trained weights or an item definition.
Item patch accepts `description`, not a new display name. Endpoint patch
configures its default version; version patch configures its scale rule. These
are distinct effects, not registry-stage or model-registration updates.
Sensitivity-label creation settings use `labelId`, not a source convenience
helper's undocumented `sensitivityLabelId`.

## Native result versus data delivery

Scoring returns the native prediction representation. False, zero, null,
empty arrays and unknown native response fields must not disappear through
truthiness checks or generic list normalization. A successful operation-state
GET is not a prediction response. An accepted operation is not completed
scoring and has no fabricated predictions.

The shared output owner supplies the common schema-bearing table model and
CSV/JSON/Parquet encoders. Commands adapt only explicitly tabular native
results to that model; they must not invent columns or flatten arbitrary
model/UDF output. An SDK-to-CLI dependency is not permitted.

The intended shared host convention is an explicit dataset format and local
destination, with an execution receipt retained separately. Existing output
defaults remain unchanged when no dataset export is requested. This SDK does
not register those options or claim that a released common export host exists.
Parameter parsing, schema-directed table conversion, file no-clobber checks,
finalization and conversion/output-failure receipts remain host integration
prerequisites. A failed local write must never rerun scoring.

## Identity and operation lifecycle

The [MLModel item APIs][items] document user-only support, whereas the
[endpoint APIs][endpoints] document users, service principals and managed
identities. Support is per operation, not inherited from the artifact type.
An already resolved endpoint target does not need another user-only item read.
Caller-selected identity metadata is not proof of token claims or permission.
All requests use the caller's existing Fabric context and transport.

Item metadata uses the documented `mlModels` route. Most endpoint operations
document `mlmodels`; default scoring documents `mlModels`. Each binding
preserves its operation-specific spelling. Version names are opaque, not
integers or aliases for "latest." An omitted version does not imply
deactivate-all. No arbitrary service URL, alternate audience or registry
fallback is inferred.

The native create, activate, deactivate, deactivate-all and score APIs can
return a real Fabric 202 long-running operation. A caller chooses acceptance
only, result-bearing observation, or no-result completion as appropriate.
The existing shared executor owns deadlines, safe operation links,
`Retry-After`, result retrieval, cancellation and uncertainty evidence.
Request builders do not add their own HTTP client, retry loop or poller.

The serving-version list builder describes one native page. It does not
silently fetch all pages or claim exhaustive coverage. The shared pager/host
must preserve complete records, correct continuation, finite bounds, and
partial progress on failure. Registry version pagination is a different
contract and is not enabled by this serving-page primitive.

## Integration and capability gates

The [serving feature][serving] is preview and has flavor/schema/capacity
restrictions. Activation changes compute consumption; deactivation changes
availability; scoring executes a model and requires write permission.
None is automatically a read-only query. No cancellation endpoint or safe
mutation replay contract is invented from the CLI spelling.

The following are not established by these SDK contracts:

- CLI/SDK/MCP operation-registry and facade composition.
- A released common UDF/tabular input/output host or column schema adapter.
- Full-list traversal from a single-page builder.
- Registry raw transport, audience, name-to-item mapping, filtering or paging.
- Model training, weight upload, definition export, automatic activation, or
  universal scoring against arbitrary endpoints.
- Live feature availability, identity/permission acceptance or model quality.

Native tests and independent review must validate the exact source revision.
Passing request/payload tests does not establish command-boundary acceptance,
output-file fidelity or live service support. Existing Python/Rust v1 contracts
and independent oracles remain unchanged.

## API provenance

Public API version: Fabric `v1`. The official references were reviewed in
preparation; API/source declarations remain distinct from runtime acceptance.

| Native contract | Source |
| --- | --- |
| Item list/get/create/update/delete | [MLModel Items][items] |
| Endpoint get/patch and serving-version get/list/patch | [MLModel Endpoint][endpoints] |
| Exact-version activate/deactivate and all-version deactivation | [MLModel Endpoint][endpoints] |
| Default and exact-version scoring, inputs/predictions and LRO | [MLModel Endpoint][endpoints] |
| State/result observation and permission inheritance | [Fabric LRO][lro] |
| Preview, model signatures, supported flavors and compute effects | [Model endpoints][serving] |

[items]: https://learn.microsoft.com/en-us/rest/api/fabric/mlmodel/items
[endpoints]: https://learn.microsoft.com/en-us/rest/api/fabric/mlmodel/endpoint
[lro]: https://learn.microsoft.com/en-us/rest/api/fabric/articles/long-running-operation
[serving]: https://learn.microsoft.com/en-us/fabric/data-science/model-endpoints
