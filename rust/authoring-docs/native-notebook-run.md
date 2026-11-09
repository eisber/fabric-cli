<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook run request

`fabric-sdk-jobs::notebook::NotebookRunRequest` validates the native
whole-Notebook release request locally. This is separate from the unchanged
legacy `JobType::Notebook = RunNotebook` route and payload.

This prerequisite implements request validation only. It does not add a
submission/wait method or CLI/MCP registration. The eventual native binding is
`POST /v1/workspaces/{workspaceId}/notebooks/{notebookId}/jobs/execute/instances?beta=false`.
The validated object is the entire body, not another `executionData` wrapper.
Missing input means zero body bytes; explicit `{}` is retained.

```json
{
  "parameters": [
    {"name": "day", "type": "Text", "value": "2026-11-05"},
    {"name": "batch", "type": "Integer", "value": 500},
    {"name": "enabled", "type": "Boolean", "value": true}
  ],
  "executionData": {
    "compute": "Spark",
    "computeConfiguration": {
      "highConcurrencyModeOptions": {"enabled": true, "sessionTag": "example"}
    }
  }
}
```

Parameter arrays retain names, order and JSON types. Names are case-insensitively
unique and at most 256 characters. Text, Integer, Number and Boolean are not
coerced. DateTime requires a real UTC date in `YYYY-MM-DDTHH:mm:ssZ` form; Guid
requires the ASCII hexadecimal `8-4-4-4-12` form. Automatic, VariableReference
and future types are explicitly unavailable until their value semantics are
accepted. Null and unknown fields are not silently omitted.

Spark and Jupyter configuration fields are validated independently. Spark
cores, memory and executor int32/minimum constraints and Jupyter core enums
follow the endpoint-specific schema. DataWarehouse cannot contain
`computeConfiguration`. ById references require explicit workspace and item
GUIDs. Native storage references are inert ABFS/ABFSS paths, never downloaded by
the client. The pool's server-computed `maxClustersToHydrateLimit` is not a
client setting.

The accepted local storage profile rejects password/query-bearing URLs and
checks the documented Python/archive extensions, retaining archive fragments.
Unsupported representations fail explicitly instead of being rewritten.

Local validation does not establish the pool-dependent maximum executor count,
saved Notebook language, parameter-name agreement with code cells, resource
existence, capacity, authorization or downstream identity support. These remain
service checks; the SDK does not fetch definitions/pools or clamp values to
manufacture acceptance. High-concurrency tags can cause the service to reuse
or create compute and are not session identities or warm-variable guarantees.

Shared submission/deadline/acceptance/observer integration depends on the
released run foundation. Schema tests alone do not implement a run, establish
CLI/MCP parity or prove live/platform acceptance. Beta application exit-value
checking remains disabled; scheduler Completed is not universal application
success. No route fallback or parameter conversion to legacy input is allowed.

Contract source: [Notebook release run reference](https://learn.microsoft.com/en-us/rest/api/fabric/notebook/background-jobs/run-on-demand-notebook),
documentation revision `b4d803a1e7f495553fcb7ef509391016e676d91d`.
Legacy behavior remains pinned to the existing independent
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` job oracle.
