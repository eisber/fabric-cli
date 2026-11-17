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

No schema inference occurs, including for all-null or empty columns. Unsupported
types fail even without rows. KQL `decimal` alone supplies neither precision nor
scale: conversion explicitly rejects it instead of inventing those parameters,
using f64, or reporting successful string conversion. Non-UTC datetime offsets,
leap seconds, finer-than-native precision, and nanosecond-range overflow fail
rather than normalize, truncate or round. Unknown schemas and invalid shapes
also fail with dataset/column/row coordinates and no cell values in error text.

`dynamic`, GUID and binary do not currently have an approved native-to-shared
logical metadata mapping in this bridge. In particular, shared tabular currently
rejects all nonempty Arrow metadata; dynamic JSON must not masquerade as an
ordinary string. These types return explicit unsupported errors pending a
released shared logical metadata contract. No AIHub wire decoder is duplicated
here; its typed bridge requires the released typed SDK model.

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
retained array bytes are independently bounded, not an allocator-wide RSS quota.
UTF-8 offsets require a byte budget no larger than `i32::MAX`. Value nesting is
caller-bounded up to 128. Cancellation/deadline is checked during preflight,
cell conversion, and before returning. Output codecs need separate finite
`OutputLimits`, and callers own atomic/no-clobber file publication.

The offline contract tests construct actual public native results, convert
through this API, encode using the existing shared codecs, and read actual
Parquet back to assert Arrow schema, widths, values, Unicode and null fidelity.
No tests dispatch to a provider.
