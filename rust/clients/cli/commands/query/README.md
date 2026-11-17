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

## Typed output capability matrix

`encode_single` takes native Arrow tables, never JSON row-object inference or
the legacy CLI table model. It delegates to `fabric-sdk-tabular` directly.
Single-file outputs require exactly one resultset; none is silently selected.
The host retains all native datasets, identities, metadata, and execution evidence.

| Output | Preserved representation | Explicit limitations |
|---|---|---|
| JSON | Ordered declared schema, positional duplicate names, exact integer and decimal strings, nulls, temporal ticks, binary base64 | Shared codec-supported declared types only |
| CSV | Positional columns including duplicates, quoted non-null cells, empty strings distinct from bare null fields | Full schema is not embedded; generic readers can conflate null and empty |
| Parquet | Real Arrow-backed Parquet, declared schema and supported values | Duplicate names, missing decimal precision/scale, unsupported temporal units or metadata fail explicitly |

Encoding failures are not query failures and never authorize resubmission.
Output limits bound retained or encoded data, not scan cost or remote execution.
Local cancellation and dropping a future do not claim remote cancellation.

## Integration status

The package currently provides local route/input preparation, one-shot native
KQL SDK dispatch, and rich single-dataset encoding. The AIHub SDK and strict native
result bridges are separate dependencies awaiting their approved immutable
development releases. Actual root executable dispatch, native host binding,
normal no-export presentation, and standard atomic file delivery remain
integration work; this package alone does not establish command completion.

No live query, endpoint/auth acceptance, caller identity discovery, login,
consent, or production provider enablement is implied. AIHub requires an
explicitly supported caller endpoint, audience, and identity context; a cached
development authentication flow is not production evidence. Typed DataAgent
results are not universal direct-artifact AIHub support.

The offline tests exercise preparation boundaries, actual SDK KQL request and
native-result preservation, and shared JSON/CSV/Parquet encoding with Parquet
readback. Qualification still requires authorized compilation and tests,
executable-boundary provider fixtures, affected reverse consumers, and the
unchanged Dataflow execution regressions.
