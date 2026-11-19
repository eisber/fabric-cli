<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Dataflow execution diagnostics

The existing root `execute` command adds advisory `guidance` to its JSON error
record on stderr. A diagnostic is still a failure, never a successful receipt or
partial dataset. JSON, CSV, and Parquet output all use the same failure path;
unclassified `PQ Arrow Metadata` prevents result publication in every format.
This does not change request preparation, approval, execution, or polling.

## What the evidence establishes

The SDK exposes
`fabric_sdk_dataflow::diagnostics::{Diagnostic, DiagnosticCondition,
DiagnosticBasis, ServiceCode}`. `Diagnostic::from_query_error` and
`Diagnostic::from_operation_error` inspect existing typed evidence only.
They perform no I/O and do not modify the original failure.

| Existing evidence | Advisory condition | Limits |
| --- | --- | --- |
| Exact `DataflowExecuteQueryError` | `query_execution_failed` | Broad query-name, mashup, or evaluation failure. Does not establish a connection fault. |
| Exact `GatewayNotReachable` | `gateway_unavailable` | Service-reported reachability condition. Does not establish its cause or gateway type. |
| Exact `DM_GWPipeline_Gateway_DataSourceAccessError` | `gateway_source_access` | Gateway cannot access its source. Does not prove invalid credentials or a specific missing permission. |
| Shared `OperationErrorKind::Credential` | `execution_credential_unavailable` | Failure of the selected execution credential, not the data source's saved credentials. |
| HTTP 401/403 alone, unknown codes, cancellation, uncertain dispatch, or PQ metadata | `unknown` | No specific connection, credential, permission, or Arrow-version cause is established. |

Codes are exact and case-sensitive. Human-readable messages, source text,
connection strings, parameters, and arbitrary nested objects are not searched.
A known top-level code may be refined by at most eight immediate, documented
`moreDetails` entries. Unknown, conflicting, malformed, or excessive details
produce `unknown`; an unknown top-level code cannot be promoted by a familiar
detail. Conflicting header and typed body codes also remain unknown. No recursive
error-envelope parser is added. A known header cannot classify a retained JSON
body that failed typed error decoding. Immediate details validate documented
field types without interpreting their text; optional parameter arrays have the
same eight-entry bound.

The REST contract identifies the `PQ Arrow Metadata` column but does not publish
its value schema. JSON-looking strings, malformed JSON, deeply nested content,
and familiar code names inside that column remain unclassified failures. No
metadata value is interpreted as success.

## Manual checks, not automatic repair

Unknown failures include conditional checks for the saved connection binding,
gateway availability, and the data source's credentials and permissions. These
are possible checks, not a confirmed diagnosis or a promise of repair. Dataflow
execute permission and applicable `Dataflow.Execute.All` or `Item.Execute.All`
delegated scope are separate from data-source access.

The Execute Query contract states that Arrow version 2 does not support
on-premises data gateways and version 1 does. However, the reviewed public
contract does not identify a specific structured failure code for that
combination. This implementation therefore neither classifies an Arrow/gateway
incompatibility nor recommends switching versions based on HTTP status, message
text, or a generic gateway failure. Callers still select the version explicitly;
there is no implicit fallback or re-execution.

There is no automatic connection creation, binding, deletion, gateway discovery,
credential probe, definition retrieval/update, save, publish, refresh, scope or
identity change, backend switch, retry, or extra network call.

## Disclosure and reconciliation

`guidance.classification` contains only typed enum values and allowlisted static
service codes. Source-sensitive service messages and unknown code strings are
not presented. `operation.serviceError` is null when absent and otherwise a
`{"redacted":true}` marker. `metadataEvidence` retains safe location and
classification fields, redacts values and nonstandard keys, and presents at most
eight entries; `metadataEvidenceCount` records the original count.

This is an intentional tightening of the previous native error-output contract,
which included raw service and metadata values. The original SDK
`ExecuteQueryError::operation_error()` / `response()` and prepared host
`ExecutionFailure` fields retain native status, code, error, response bytes,
operation ID, and `mutation_may_have_occurred` for explicit caller diagnostics.
Those raw objects remain sensitive and must not be logged indiscriminately.
CLI diagnostics retain the actual HTTP failure status when present, validated
operation ID, and mutation uncertainty. An HTTP-200 in-stream or terminal
operation failure does not acquire a fabricated HTTP error status.
Local cancellation does not prove remote cancellation, rollback, or safe replay.

## Source authority and gaps

Sources checked on 2026-11-18:

- [Execute Query REST contract](https://learn.microsoft.com/rest/api/fabric/dataflow/query-execution/execute-query),
  source revision `b4d803a1e7f495553fcb7ef509391016e676d91d`: endpoint,
  permissions/scopes, broad `DataflowExecuteQueryError`, Arrow versions, and the
  in-stream diagnostic column.
- [Troubleshoot refresh scenarios](https://learn.microsoft.com/power-bi/connect-data/refresh-troubleshooting-refresh-scenarios#gatewaynotreachable-when-setting-credentials),
  revision `953d121cf54d9e4916983e2085a004c2200a3cfc`:
  `GatewayNotReachable`.
- [Troubleshoot on-premises data gateways](https://learn.microsoft.com/power-bi/connect-data/service-gateway-onprem-tshoot),
  revision `28d6a27caf6624882e6bede71eab7e3bf4fd1169`:
  `DM_GWPipeline_Gateway_DataSourceAccessError`.

The gateway documentation describes Power BI scenarios, not a guarantee that
Execute Query emits these codes. The helper recognizes their documented meaning
only if the existing native structured error actually supplies them. A dedicated
Execute Query connection-binding, saved-credential, execute-permission, or
Arrow/gateway code list was not verified. These gaps deliberately remain unknown;
community examples alone do not add codes to the allowlist.

Upstream
[`FabricDataflowService.cs` at `171647e617cab34b1ae440f9b68afd4b139e2d29`](https://github.com/microsoft/DataFactory.MCP/blob/171647e617cab34b1ae440f9b68afd4b139e2d29/DataFactory.MCP.Core/Services/FabricDataflowService.cs)
calls the same Execute Query endpoint. Its separate
`AddConnectionsToDataflowAsync` retrieves the definition, obtains cluster IDs
through `GetClusterId`, changes bindings, and calls `updateDefinition`.
That is explicit mutating authoring, not an execution prerequisite to copy into
diagnostic handling. No upstream Arrow summary or success-shaped error fallback
is ported.

The offline test source covers exact-code precedence, malformed/bounded details,
misleading messages, raw-evidence retention, redaction, actual injected CLI
error consumers, all three formats, terminal operation IDs, and no extra calls.
Test authoring does not establish a compiler, runtime, live authentication, or
service acceptance result; native validation requires the shared-host admission.
