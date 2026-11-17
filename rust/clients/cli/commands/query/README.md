<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native query command composition

`fabric-cli-query-commands` is the query-specific CLI capability package. It
consumes authoritative native bindings and the existing root query argument
shape. It does not own another parser, authentication engine, HTTP stack, clock,
output framework, or SDK implementation.

The compatibility source is the root query parser at integration commit
`61c8221914152e231e5e64f7bd38418f66264999`. Existing inline text, `--file`,
`--parameters`, `--engine`, and explicit export options remain parser-owned.
No provider, resultset, language, or namespace flags are added.

## Preparation and provider selection

The native host supplies one `QueryTarget` snapshot containing the requested
artifact, actual execution artifact, and supported exact bindings. A real
DataAgent datasource binding includes its DataAgent identity, datasource identity,
bound artifact, and explicit production or sandbox stage. It selects the AIHub
typed route by default. A bare Report, SemanticModel, or other artifact does not
become a DataAgent. An explicit advertised engine selects exactly one binding.
Missing or ambiguous bindings fail before input, credentials, or query dispatch.

Artifact-native KQL remains an explicit route using the resolved Kusto
endpoint, database, audience, account, tenant, and Fabric owner. The statement is
opaque; its text never chooses a language, management route, or provider.
`PreparedKql` constructs the real SDK request before acquiring credentials and
consumes it once through `fabric_sdk_query::query_kql`. The SDK's capability
guard and hardline read policy remain intact. Failure does not select another
provider or authorize replay.

`prepare_input` resolves files or stdin through one caller-owned bounded reader.
It preserves exact UTF-8 text, including line endings, and does not echo it in
diagnostics. Both current providers lack an accepted native parameter schema:
`--parameters` remains parseable but is explicitly rejected before input or
credential I/O. It is not interpreted as output projection or template selection.

`PreparedAiHub` constructs the real AIHub SDK request from that resolved
DataAgent datasource, requires typed JSON, and consumes a caller-configured
`AiHubQueryClient` once. The SDK sends
`Fabric.Routing.Documents.DataAgent` and
`Fabric.Routing.Documents.DataAgent.TypedQueryResults`, plus the positive
`Fabric.Routing.Documents.SqlReadOnlyEnforce` policy on the MCP lifecycle.
The fixed `execute_query` contract uses initialize (ID 1), initialized
notification, and one tool call (ID 2): three HTTP exchanges. It does not list
tools, discover a schema, inspect annotation hints, or retry a missing tool.
This intentionally removes discovery latency while retaining local argument
validation and the positive read-policy variant. Remote missing-tool or argument
errors retain dispatched/uncertain execution evidence and never cause fallback.
Sandbox stage adds its separate stage token. Missing typed resources fail
explicitly; legacy CSV, Markdown, prose, or auxiliary resource URLs are not
queried or parsed as a fallback.
Schema version 1 accepts either the full validated resource marker or its compact
`schemaVersion`/`resultId`/`format` marker with authoritative schema, counts and
positional rows in the JSON body. Explicit signed `int64` logical declarations
retain their native identity and exact values, including values beyond the
JavaScript safe-integer range; unknown aliases and inconsistent metadata fail.

## Typed output capability matrix

`encode_single` takes native Arrow tables, never JSON row-object inference or
the legacy CLI table model. It delegates to `fabric-sdk-tabular` directly.
Single-file outputs require exactly one resultset; none is silently selected.
The host retains all native datasets, identities, metadata, and execution evidence.
`encode_presentation` renders all datasets through the same rich JSON codec for
normal no-export output. Its aggregate row, batch, and envelope-inclusive byte
limits fail without returning a clipped prefix.

| Output | Preserved representation | Explicit limitations |
|---|---|---|
| JSON | Ordered declared schema, positional duplicate names, exact integer and decimal strings, nulls, temporal ticks, binary base64 | Shared codec-supported declared types only |
| CSV | Positional columns including duplicates, quoted non-null cells, empty strings distinct from bare null fields | Full schema is not embedded; generic readers can conflate null and empty |
| Parquet | Real Arrow-backed Parquet, output schema, native type hints and supported values | Duplicate names, unrepresentable Decimal128 layouts, unsupported temporal units or metadata fail explicitly |

Encoding failures are not query failures and never authorize resubmission.
Output limits bound retained or encoded data, not scan cost or remote execution.
Local cancellation and dropping a future do not claim remote cancellation.

## Native connection and limits

The app's additive `v2-query` feature composes this package into `fab-rs` and
the headless `fab-cli-v2` entrypoint. `fab-rs` also needs an existing supported
native host profile, such as `native-read` on Windows or `headless`. Default
builds, the six-local-command `fab-cli`, Python/v1 behavior, and Dataflow
`execute` approval, M input, named-query and Arrow semantics are unchanged.

The native resolver uses the existing absolute typed metadata paths, for example
`/<workspace>.Workspace/<database>.KQLDatabase` or an actual `.DataAgent` owner.
Names use complete bounded SDK discovery, never first-match selection.
KQL uses typed database metadata, its exact query origin and native database
name, the explicit public Kusto audience, and the same selected account/tenant.
`--engine kql` is advertised for that native route. DataAgent metadata does not
advertise an engine override; the native host rejects one rather than inventing
an `aihub` engine selector.

AIHub requires an optional, non-secret `query_connection` object in the existing
caller-owned `config.json`. The feature only reads it; it does not initialize,
write, migrate, or change settings or identity selection:

```json
{
  "query_connection": {
    "protocol": "mcp",
    "service_binding": "dataagent-typed-query",
    "endpoint": "https://api.fabric.microsoft.com/v1/mcp/fabricaihub/integrations/m365",
    "audience_scope": "https://api.fabric.microsoft.com/.default",
    "data_agents": [
      {
        "workspace_id": "11111111-1111-4111-8111-111111111111",
        "agent_id": "22222222-2222-4222-8222-222222222222",
        "datasource_id": "33333333-3333-4333-8333-333333333333",
        "stage": "published"
      }
    ]
  }
}
```

The endpoint above demonstrates the validated shape, **not a production
availability or authorization claim**. The caller must supply its supported
connection. Its origin must exactly match the existing selected trusted
Fabric/Power BI service descriptor; its audience must match that descriptor's
allowed scope. The MCP profile path is exact. Ordinary REST/write paths,
normalized or escaped alternatives, arbitrary HTTPS hosts, alternate audiences,
identity/secret fields, and ambiguous agent mappings fail before query token
acquisition. No ring, endpoint, audience, account or tenant is inferred.

One datasource and `published` or `staging` stage must be configured for each
addressed real agent. The existing stage API verifies the datasource and its
exact supported Fabric item reference within eight pages, 1,000 records and a
shared metadata byte/request budget. Missing references, duplicates, incomplete
continuations and arbitrary continuation URIs fail closed. There is no reverse
DataAgent discovery from a bare artifact. Missing AIHub configuration does not
disable artifact-native KQL.

Native output uses the strict `fabric-sdk-query-tabular` bridge and shared rich
codecs. DataAgent plain declared decimal types can use a validated exact Arrow **output**
precision/scale chosen across all values; the original native type hint remains
unchanged and unknown native SQL precision/scale is not invented.
Unrepresentable Decimal128 layouts, malformed declarations, differing per-row
timezone offsets, overflow and other unsupported types remain typed errors.
This is not a claim that every native provider type has an Arrow representation.

Normal no-export presentation is rich JSON and retains all datasets. Exports
use a preflighted standard `FileSink` and atomic no-clobber publication; encoding,
file or receipt failure retains completed-query evidence without resubmission.
The native KQL server policy is explicitly 30 seconds, independent of the
existing overall native command budget. Metadata, authentication, transport,
conversion and output share the original invocation deadline. Row/byte limits
are output bounds, not scan or compute-cost quotas.

No live query, endpoint/auth acceptance, caller identity discovery, login,
consent, or production provider enablement is implied. AIHub requires an
explicitly supported caller endpoint, audience, and identity context; a cached
development authentication flow is not production evidence. Typed DataAgent
results are not universal direct-artifact AIHub support.

The offline tests exercise preparation boundaries, actual SDK AIHub and KQL
requests and native-result preservation, and shared JSON/CSV/Parquet encoding
with Parquet readback. Injected native-context fixtures exercise real metadata,
credentials, MCP and production KQL adapters; process fixtures exercise both
actual entrypoints without credentials. Source integration is not validation:
compilation, these tests, default/headless/Windows-native profiles, reverse
consumers and unchanged Dataflow regressions must pass under an explicit
validation allocation before this development candidate is qualified.
