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

Only clean completed, untruncated results with actual `http_eof` evidence, a
successful terminal frame and no diagnostics are consumable. The completed flag
cannot substitute for independently observed HTTP EOF. Partial failures,
completion warnings, cancelled responses, local truncation and missing completion fail; forensic partial rows
are not promoted into completed exports. Conversion does not establish live
provider or read-enforcement proof. The original `adapter_capability` is retained,
and `production_verified` is not a conversion gate: honest offline fixtures and
structurally enforced production adapters both remain live-unverified.

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
The version-1 `int64` logical type with `json-number` encoding and native
`Int64` also maps exactly to signed Arrow Int64, preserving the original logical
declaration and native type hint. It does not accept a mismatched native width or
sign, fractional numeric token, overflow, or unknown alias. The existing generic
`integer` mapping is unchanged.
Single/Float32 and Double/Float64 are approximate numbers with their original
declared widths. The known v1 native Kusto `real` alias with logical number and
JSON-number encoding maps to Float64, not SQL REAL/Float32. Nonfinite overflow and
underflow to zero fail. Bare `float` has no proven width in this wire contract and
remains unsupported; an unknown integer/number type never defaults to Int64 or
Float64. Logical boolean/JSON-boolean also accepts the known `bit` alias as
Boolean, without coercing integer or text cells.

`decimal(p,s)` and `numeric(p,s)` declarations explicitly supplying supported
precision (1-38) and scale (0 through precision) map to Decimal128. Coefficient
and exponent arithmetic uses integers only. Values requiring rounding, losing
fractional digits or exceeding precision fail.

For explicitly logical decimal/json-number columns with plain `Decimal` or
`numeric` native type, **native precision and scale remain UNKNOWN**. The bridge
scans every non-null native Decimal lexeme under caller budgets/control and
selects an exact **output representation**, not an inferred SQL schema:
`output_scale = max(0, fraction_digits - exponent)` over all values;
`output_precision` is the maximum of 1, output scale, and all exact rescaled
coefficient digit counts. Leading coefficient zeros do not consume precision;
declared fractional scale is retained, including zero values. Both output
parameters must fit Decimal128 (at most 38). Checked arithmetic rejects exponent,
scale, precision or combined-column overflow without rounding, f64 or
exponent-sized allocations. Values that each fit separately can still fail if
their combined scale/precision cannot fit one exact column. Empty/all-null plain
decimal columns use canonical **output** Decimal128(1,0); no native parameter
claim is made. Original native `Column` declarations and `native_type` metadata
remain unchanged. Explicit native `decimal(p,s)` still takes precedence.

Known SQL `money` and `smallmoney` aliases with logical decimal/JSON-number use
their [documented fixed contracts](https://learn.microsoft.com/en-us/sql/t-sql/data-types/money-and-smallmoney-transact-sql):
Decimal128(19,4) with a signed 64-bit scaled
coefficient, and Decimal128(10,4) with a signed 32-bit scaled coefficient,
respectively. Both precision/scale **and** scaled integer bounds are enforced.
The exact ranges are -922337203685477.5808 through 922337203685477.5807 and
-214748.3648 through 214748.3647. Extra nonzero fractional digits or out-of-range
values fail instead of rounding; unknown monetary labels do not inherit these
contracts. Original native type hints are retained.

Date/DateOnly maps to Date32; Time/TimeOnly to nanosecond time; TimeSpan/duration
to nanosecond duration. Known `datetime2` and `smalldatetime` aliases with logical
datetime/ISO8601 use the same exact representation as DateTime; this does not
assert a missing native SQL precision or expand the Arrow nanosecond range.
For DateTime/DateTimeOffset, the bridge validates
**every** non-null value before choosing the output timezone. Uniform naive
DateTime values map to timestamp without timezone. Uniform explicit `Z` or
`+00:00` values map to timestamp with UTC; uniform nonzero offsets map to a fixed
`+HH:MM` or `-HH:MM` Arrow timezone with checked UTC ticks. The shared Parquet
codec retains this Arrow timezone, so neither the instant nor the fixed offset
is discarded. This is proof about the complete returned column, not a claim
about the native engine's schema or a first-value inference.

Mixed naive/aware values or differing row offsets fail, even if they denote the
same instant. DateTimeOffset requires an explicit offset in every non-null value.
Empty/all-null DateTime columns have unknown timezone and canonical output
`None`; DateTimeOffset uses canonical **output** UTC without claiming a native
offset. Range overflow, leap seconds, finer-than-nanosecond values and RFC3339
unknown offset `-00:00` fail. Canonical output preserves temporal meaning,
precision and fixed offset, not original lexical spellings such as `Z` versus
`+00:00`.

The [synthetic direct consumer](examples/aihub/typed_consumer.rs) demonstrates
plain-decimal and explicit UTC native values. It is not evidence of live
provider compatibility.

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
