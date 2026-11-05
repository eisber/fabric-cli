<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Dataflow query execution (v2 preview)

This additive `fab-rs` command binds an explicitly addressed Dataflow named
Power Query to the public Execute Query API. It does not change Python `fab`,
v1 metadata `get -q`/JMESPath projection, or item `job` commands. The source
baseline is `5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b`; Execute Query is an
owner-approved addition, not a claim of existing Python command parity.

```powershell
fab-rs execute "/<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/Orders" --wait --format json
fab-rs execute "/<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/Orders" --wait --format csv --input request.json --output orders.csv
fab-rs execute "/<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/Orders" --wait --format parquet --output orders.parquet --timeout 5m
```

Replace both IDs with explicit UUIDs. The named query is a single path segment;
names containing `/`, backslashes, percent escapes, control characters, or the
traversal segments `.` and `..` are not accepted in this first slice. Workspace
and item name discovery, context-relative paths and cross-engine text-language
guessing are not implemented. There is no `--text` requirement: the request
names a Power Query, not SQL. SQL, KQL, DAX, and GraphQL execution engines are
not implemented in the source baseline and are not fabricated here. Their future
adapters can use the engine-neutral `fabric-sdk-tabular` component.

## Input and execution lifecycle

`--input FILE` or `--input -` accepts one bounded UTF-8 JSON object:

```json
{"customMashupDocument":"section Section1; shared Orders = ...;","arrowVersion":1}
```

Input is optional. Omitted input and `{}` use Arrow version 1 without a custom
Mashup document. `arrowVersion` accepts only numeric `1` or `2`. Unknown or
duplicate fields, null values, empty/invalid input and an invalid custom
document are errors. Input is limited to 1 MiB. Repeated input options do not
permit multiple stdin reads. Version 2 has service/gateway restrictions; the
client does not silently retry version 1 if version 2 fails.

Execution can consume compute and access external data sources. Before selecting
or acquiring credentials, the native host validates the target, reads and
validates input, preflights the output path and requests explicit approval bound
to the immutable target and complete structured input. The prompt shows that
input, so do not put secrets in it. There is no `--force` bypass. A redirected
process without an interactive approval capability fails closed; embedders and
offline tests inject an `ExecutionApproval` instead of prompting. Structured
stdin is read only once and is not interpreted as approval: an attached console
is used separately where available.

`native-read` or `headless` supplies the existing explicit native credential and
HTTP host. The default executable recognizes valid arguments but returns
`NotConfigured`. The command does not log in, discover a different identity,
install another credential chain or create a runtime in SDK/library code.
See [native host configuration](native-read-host.md) for supported credential
selection and limitations. Headless authentication does not bypass execution
approval. Permissions, tenant policy, capacity, data source credentials, gateway
support and endpoint availability are service prerequisites; offline tests are
not live acceptance evidence. The REST contract requires execute permission on
the Dataflow and delegated `Dataflow.Execute.All` or `Item.Execute.All` scopes;
it lists users, service principals and managed identities as supported identity
types. Identity support does not waive those prerequisites.

The host submits exactly one
`POST /v1/workspaces/{workspaceId}/dataflows/{id}/executeQuery` over the shared
operation engine and requests native Arrow media, independent of output format.
An immediate 200 response and an actual generic 202 operation/state/result path
are supported. Accepted is not completed, and no scheduler handle is invented.
`--wait` is required in this slice.

The service's 90-second query evaluation window is distinct from the total
client budget. `--timeout` defaults to `5m`, accepts positive integer `ms`, `s`,
`m` or `h`, and is bounded at 24 hours. The same deadline spans local preparation,
approval, credentials, submission, polling and output preparation/publication.
Local cancellation and deadline expiration stop observation; they do not issue
remote cancellation, undo source effects, or replay a query. Diagnostics retain
the actual operation ID when one was returned and indicate uncertain effects.
An interrupted OS input/output worker can still finish after local waiting
stops. Blocking OS writes cannot be rolled back.

## Result formats and finite output

`--format json|csv|parquet` selects the shared tabular serializer; default is
`json`. `--limit` is a maximum complete row budget, not a clipping instruction:
default 100,000, hard ceiling 1,000,000. `--max-bytes` bounds each native response
and complete serialized output: default 16 MiB, hard ceiling 64 MiB. Shared
tabular schema and decoded-memory bounds also apply, with at most 4,096 record
batches, including empty batches. Exceeding any budget
is a nonzero failure, never a silently clipped successful result.

See the [shared result contract](tabular-results.md) and
[`fabric-sdk-tabular` README](../capabilities/tables/fabric-sdk-tabular/README.md)
for the exact schema-bearing JSON representation, CSV quoting/null convention,
Parquet types, unsupported/lossy types and multiple-result-set policy. Native
`PQ Arrow Metadata` failure evidence is inspected before output can succeed.
An HTTP 200 response containing native failure evidence is not a successful
table. Non-null values in that reserved diagnostic column fail closed without
guessing an undocumented payload schema. An absent or all-null diagnostic column
is allowed by the explicit integration policy; the REST documentation does not
define null payload semantics, so this allowance alone is not service proof of
application success. Structured metadata evidence is retained
in the stderr error diagnostic, with unknown classification where appropriate;
it can contain source-sensitive service information and should be handled
accordingly. No output format may silently drop additional datasets.

CSV and JSON default to stdout; `--output FILE` selects a new local file.
`--output -` explicitly selects stdout. Parquet requires a file: binary stdout,
including terminals, is refused in this first slice. The `--format` result
contract is separate from v1 `--output_format`; the latter is rejected for root
execute. Excel output is deferred.

Diagnostics and prompts go to stderr. Data stdout contains only the requested
serialized result, without navigation notices, receipts or an outer CLI status
envelope. A successful file publication is quiet. Existing files, directories
and symlinks are refused before submission; `create_new` repeats the no-clobber
check at publication to prevent destination races. Preflight creates, writes
one byte to and removes a uniquely named `.fab-execute-preflight-*` probe in the
destination's parent; probe creation, write or cleanup errors prevent submission.
It does not reserve the final path. Parent-directory or
permission changes after preflight can still fail publication. A failed
write/sync returns nonzero and explicitly identifies possible partial output;
it never reports an incomplete file as complete, deletes a potentially raced
path, overwrites another result, or resubmits the query. If the query completed
but output failed, investigate the retained evidence rather than replaying
automatically.

## Detailed refresh-log download: explicit API gap

Execute Query produces fresh query evaluation results, not persisted refresh
data and not the full diagnostic log of a refresh.

Public job-instance APIs can expose run status and structured `failureReason`.
Configured workspace monitoring can expose item-level `DataflowGen2Logs`.
Neither is the complete detailed Power Query Mashup diagnostic log ZIP.
The public references examined do not establish a supported REST endpoint to
download that ZIP. This host therefore does not implement a pretend log
download, undocumented portal endpoint, or internal staging-storage access.

For detailed refresh logs, use **Recent runs > selected refresh > Download
detailed logs** in the Fabric portal. Availability, retention and permissions
remain subject to the product. The current monitoring documentation says logs
become available a few minutes after completion, remain available for up to 28
days, require at least the workspace Viewer role, and require gateway diagnostics
admin consent for gateway-backed refreshes. A future native downloader needs a
documented public API plus separately reviewed permissions, bounds and artifact
handling.

Authoritative references:

- [Execute Query REST contract](https://learn.microsoft.com/rest/api/fabric/dataflow/query-execution/execute-query)
- [Dataflow Gen2 workspace monitoring](https://learn.microsoft.com/fabric/data-factory/dataflow-gen2-workspace-monitoring)
- [Download detailed refresh logs](https://learn.microsoft.com/fabric/data-factory/dataflows-gen2-monitor#download-detailed-logs-of-the-refresh)
- [Detailed refresh-log format](https://learn.microsoft.com/fabric/data-factory/dataflow-gen2-detailed-refresh-logs)
