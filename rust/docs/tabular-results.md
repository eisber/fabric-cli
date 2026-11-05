<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared tabular execution results

The v2 result contract separates the native execution transport from the
requested output format. Dataflow Execute Query returns Arrow IPC; callers can
serialize its supported typed result as CSV, JSON, or Parquet. Excel is not
implemented in this delivery.

The shared SDK component is independent of the CLI, credentials, transport,
runtime, and engine selection. Other native query/execute adapters should
produce the same ordered typed result instead of implementing their own CSV,
JSON, or Parquet conversions. This does not add SQL, KQL, DAX, GraphQL, or other
query engines that are not already implemented. Existing v1 metadata projection
and output contracts are unchanged.

## Result integrity

- Preserve schema, column and row order, nulls, and supported numeric and
  temporal types. Each format has a documented representation; do not silently
  coerce unsupported types or discard additional datasets.
- A response that exceeds the configured row, batch, or byte budget fails
  explicitly. A clipped result is not reported as complete.
- An empty result is not an absent schema. Preserve the columns even when there
  are no records, where the output format supports this.
- CSV and Parquet require an explicit policy for multiple result sets. They
  cannot silently serialize only the first result.
- HTTP success and a valid Arrow stream are not sufficient proof of application
  success. Dataflow can return error evidence in `PQ Arrow Metadata`. Inspect
  that evidence before reporting successful output.

See the shared `fabric-sdk-tabular` component README for the exact JSON shape,
CSV null/escaping convention, supported Arrow types, and serialization limits.

## Output lifecycle

The command host validates the requested format, local input, and destination
before execution. Existing output files are not overwritten. A destination race
or write failure must return a nonzero outcome and retain execution evidence;
it must never resubmit a query to recreate a missing output file.

Binary output must not be written to a terminal. Diagnostics and receipts must
not contaminate data bytes. A file containing only part of a result must not be
reported as a completed output.

Native execution can consume compute or access external sources. Output row and
byte limits do not bound source scan cost or establish read-only behavior.
Timeout and local cancellation stop observation, not remote execution or
side effects. Reuse an actual returned operation handle; do not manufacture a
scheduler run or replay an uncertain submission.

## Dataflow diagnostics gap

Job instance APIs expose status and structured `failureReason`. Configured
workspace monitoring exposes item-level events through `DataflowGen2Logs`.
Neither is the complete Power Query Mashup diagnostic log.

A supported public REST contract for downloading the detailed refresh-log ZIP
has not been established in the references examined. Native detailed-log
download therefore remains a capability gap. The documented workaround is
**Recent runs > selected refresh > Download detailed logs** in the Fabric portal.
Do not use undocumented portal endpoints or directly access internal staging
storage to fill this gap.

Official references:

- [Dataflow Execute Query](https://learn.microsoft.com/rest/api/fabric/dataflow/query-execution/execute-query)
- [Dataflow Gen2 workspace monitoring](https://learn.microsoft.com/fabric/data-factory/dataflow-gen2-workspace-monitoring)
- [Detailed refresh-log download](https://learn.microsoft.com/fabric/data-factory/dataflows-gen2-monitor#download-detailed-logs-of-the-refresh)
- [Detailed refresh-log format](https://learn.microsoft.com/fabric/data-factory/dataflow-gen2-detailed-refresh-logs)
