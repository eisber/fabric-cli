<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native query to shared typed tables

`fabric-sdk-query-tabular` is an SDK-only conversion bridge. It does not execute
queries, discover credentials, fetch resources, create files, replay requests, or
implement another CSV/JSON/Parquet serializer. It depends on the public native
`fabric-sdk-query` model and the Arrow-capable `fabric-sdk-tabular` model/codecs,
not the limited legacy CLI `TabularTable`.

## KQL consumer contract

Call `convert_kql(result, limits, &control)` with an owned `QueryResult`, explicit
finite `ConversionLimits`, and caller-owned absolute deadline/cancellation flag.
The result retains **all** service-order datasets, original table IDs/names and
ordered native columns, plus unchanged metadata, completion and execution
evidence. Duplicate field names and rows remain positional. An empty dataset
vector means no resultset; a dataset with zero rows retains its declared schema.
There is no silent first-dataset selection. Select a dataset explicitly before
calling the existing `fabric_sdk_tabular::encode` or `encode_to_writer`.

Only clean completed, untruncated results with a successful terminal frame and
no diagnostics are consumable. Partial failures, completion warnings, cancelled
responses, local truncation and missing completion fail; forensic partial rows
are not promoted into completed exports. Conversion does not establish live
provider or read-enforcement proof.

| Native declaration | Arrow type | Value contract |
| --- | --- | --- |
| `bool` | Boolean | JSON boolean or null |
| `int` | Int32 | Exact JSON integer in signed 32-bit range |
| `long` | Int64 | Exact JSON integer in signed 64-bit range, including beyond 2^53 |
| `real` | Float64 | Finite JSON number; integer-to-float rounding rejected |
| `string` | Utf8 | Original Unicode, null distinct from empty string |
| `datetime` | Timestamp(Nanosecond, UTC) | UTC `Z`, at most seven fractional digits; exact representable range only |
| `timespan` | Duration(Nanosecond) | Native `[-][d.]hh:mm:ss[.fffffff]`; overflow rejected |
| `guid` | Utf8 with GUID logical metadata | Original validated hyphenated GUID text |
| `dynamic` | Utf8 with JSON logical metadata | Compact JSON of the native `serde_json::Value`, not guessed text parsing |

No schema inference occurs, including for all-null or empty columns. Unsupported
types fail even without rows. KQL `decimal` alone supplies neither precision nor
scale: conversion explicitly rejects it instead of inventing those parameters,
using f64, or reporting successful string conversion. Non-UTC datetime offsets,
leap seconds, finer-than-native precision, and nanosecond-range overflow fail
rather than normalize, truncate or round. Unknown schemas and invalid shapes
also fail with dataset/column/row coordinates and no cell values in error text.

Fields retain their native type through the shared
`fabric.tabular.native_type` metadata key. GUID/dynamic columns additionally use
the shared `fabric.tabular.logical_type` key with `guid`/`json`, respectively.
They do not masquerade as ordinary strings: JSON output retains this metadata
and logical encoding; Parquet retains the Arrow field metadata. Dynamic JSON is
lossless relative to the supplied native `serde_json::Value` model, **not** an
assertion that an upstream JSON parser retained original whitespace, duplicate
object keys or fractional numeric lexemes. The bridge never reparses a dynamic
string as a guessed object. Null source cells remain Arrow nulls.

KQL has no native binary scalar declaration in this source contract; unknown
`binary` schemas fail instead of guessing an encoding. No AIHub wire decoder is
duplicated here; its typed bridge requires the released typed SDK model.

## Typed DataAgent consumer contract

The optional `aihub` feature exposes
`convert_aihub(QueryOutcome, limits, &control)`. It consumes the actual
`fabric-sdk-aihub-query` native model, not an independently decoded MCP body.
Only `QueryOutcome::Complete` with complete, untruncated resource evidence is
consumable. The native SDK owns the required DataAgent routing and typed-result
variants, resource metadata validation and missing-typed-result failure.
Conversion cannot enable a provider capability, turn a Report into a DataAgent,
or fall back to CSV, Markdown or Excel.

Every template resultset becomes its own `AihubDataset`, retaining URI,
schema/result IDs, ordered original columns, row count, truncation and optional
maximum/total count evidence. Auxiliary resources, messages, aggregate
completeness and negotiated protocol remain separate from Arrow rows. Opaque
URIs are never fetched, and missing totals remain unknown.

Mapping is from **both** declared logical type/encoding and a supported native
type. Integer widths/signs follow the explicit Byte/SByte/Int16/UInt16/Int32/
UInt32/Int64/UInt64 families (including the documented native SQL aliases).
Single/Float32 and Double/Float64 are approximate numbers with their original
declared widths; nonfinite overflow and underflow to zero fail. A service's
unknown integer/number engine type never defaults to Int64 or Float64.

`decimal(p,s)` and `numeric(p,s)` declarations explicitly supplying supported
precision (1-38) and scale (0 through precision) map to Decimal128. Coefficient
and exponent arithmetic uses integers only. Values requiring rounding, losing
fractional digits or exceeding precision fail. Plain `Decimal` without schema
parameters is unsupported even for empty/all-null columns. The
[synthetic direct consumer](examples/aihub/typed_consumer.rs) demonstrates an explicitly
parameterized schema; it is **not** evidence that a live provider supplies those
parameters.

Date/DateOnly maps to Date32; naive DateTime to nanosecond timestamp **without**
timezone; Time/TimeOnly to nanosecond time; TimeSpan/duration to nanosecond
duration. No first-value timezone or precision inference occurs. Offset-bearing
datetime values and DateTimeOffset columns without a declared fixed column
timezone explicitly fail rather than normalize/drop offsets. Range overflow,
leap seconds and finer-than-nanosecond values also fail.

Boolean, text, base64 binary and GUID have explicit mappings. Unknown engine
types are accepted only when the source explicitly declares logical string/text,
not guessed as strings from arbitrary values. GUID uses the shared logical
metadata convention; decoded binary preserves bytes, including empty versus
null. Native enum variants must match their declarations. Scalar row values
have no nested-value representation in this native model; opaque auxiliary
metadata is retained as bounded text, not reparsed through a lossy JSON model.

## Formats and limits

The shared JSON codec emits ordered schema and positional rows, with exact
integer/decimal/timestamp encodings. Shared Parquet is real Arrow-backed Parquet;
its duplicate-name, zero-column and other output restrictions still apply.
Conversion preserves duplicate names, but does **not** rename columns to bypass
Parquet's explicit rejection.

Shared CSV quotes every non-null cell (including `""`) and writes null as a bare
empty field. Recovering that distinction requires a schema-aware reader that
retains quotedness; generic CSV readers often conflate null and empty. CSV has
no complete type schema and is not universally lossless.

Rows and batches are aggregated across all datasets; columns are bounded per
dataset. Each dataset consumes one batch, even when empty. Source payload,
metadata and nested values are checked before Arrow allocation. Source bytes and
retained Arrow schema/metadata and array bytes are independently bounded, not an
allocator-wide RSS quota. The retained-byte budget is aggregated across all
datasets, including empty tables.
UTF-8 offsets require a byte budget no larger than `i32::MAX`. Value nesting is
caller-bounded up to 128. Cancellation/deadline is checked during preflight,
cell conversion, logical JSON encoding, and before returning. Output codecs need separate finite
`OutputLimits`, and callers own atomic/no-clobber file publication.

The offline contract tests construct actual public native results, convert
through this API, encode using the existing shared codecs, and read actual
Parquet back to assert Arrow schema, widths, values, Unicode and null fidelity.
No tests dispatch to a provider.

The [direct SDK example](examples/native_kql.rs) exercises conversion and all
three shared codecs with a synthetic native result, without authentication or
file/network access.
