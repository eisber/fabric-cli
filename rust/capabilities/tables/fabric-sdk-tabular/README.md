<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared tabular results

`fabric-sdk-tabular` is an engine-neutral SDK-layer component for the
owner-approved native CLI v2 execute/query contract. Its source baseline is
`5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b`. This is an additive contract, not
a claim of Python/v1 output parity or live Dataflow acceptance.

The crate owns ordered Arrow datasets, bounded IPC stream decoding, and real
CSV, JSON, and Parquet serialization. It has no identity, HTTP, runtime,
deployment, CLI, filesystem, or global-state dependencies. Callers provide bytes,
schemas, batches, limits, and optional output writers. Excel is deferred.

## API

```rust
use std::sync::Arc;
use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, TabularResult, encode};

let limits = OutputLimits::new(1_000, 1_048_576, 20)?;
let schema = Arc::new(Schema::new(vec![Field::new("count", DataType::Int64, true)]));
let batch = RecordBatch::try_new(
    schema.clone(),
    vec![Arc::new(Int64Array::from(vec![Some(i64::MAX), None]))],
)?;
let result = TabularResult::try_new(schema, vec![batch], limits)?;
let json = encode(&result, ResultFormat::Json, limits)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`decode_arrow_stream(&bytes, limits)` returns the same `TabularResult` type.
`encode_to_writer(&result, format, limits, &mut writer)` first serializes
completely, then writes and flushes the caller's sink.

### Single dataset and schema policy

One result has exactly one explicit `SchemaRef` and an ordered vector of
`RecordBatch` values. Every batch must match that exact schema, including
nullability and metadata. No schema inference, type coercion, row clipping,
first-dataset selection, or implicit merging occurs. Duplicate names remain
positional in CSV/JSON. Parquet rejects them because reader name resolution
cannot guarantee their meaning.

IPC input must contain one schema, zero or more record batches, and an explicit
end-of-stream marker. Modern continuation prefixes and legacy length prefixes
are accepted. Missing/truncated end markers, second schemas, concatenated
streams, and *any* trailing bytes fail. This explicit-termination requirement
is stricter than producers that use transport EOF alone; callers must not
silently append a terminator to an unverified/truncated response.

### Application metadata is not success

Dataflow PQ Arrow Metadata semantics are not guessed from undocumented names
or JSON substrings. Any nonempty schema, field, or IPC message custom metadata
fails closed with `TabularError::Metadata` and ordered `MetadataEvidence`.
Each entry preserves its location, key, and value and is classified `Unknown`.
Evidence is not logged and may require redaction before display.

This conservative rule also refuses benign-but-unrecognized metadata (for
example pandas metadata). No metadata allowlist or application-error parser is
claimed. Dictionary/tensor/other unsupported messages fail rather than being
silently skipped. Diagnostics encoded as a second dataset also fail the
single-dataset policy; they are not discarded as trailing successful data.
The application adapter remains responsible for verified Dataflow status,
documented diagnostic interpretation, operation IDs, and partial outcomes.
An HTTP 200 response or a structurally valid stream alone proves neither.

## Supported types

| Arrow type | JSON cells | CSV non-null cells | Parquet |
| --- | --- | --- | --- |
| Signed/unsigned 8-, 16-, 32-, 64-bit integers | Exact base-10 strings | Base-10 text | Original type |
| Float32 / Float64 | Finite JSON numbers | Shortest round-trip text | Original type |
| Boolean | JSON booleans | `true` / `false` | Original type |
| Utf8 / LargeUtf8 | JSON strings | UTF-8 text | Original Arrow type |
| Binary / LargeBinary | Standard padded base64 strings | Standard padded base64 | Original bytes/type |
| Timestamp, all four units | Signed tick strings | Signed tick text | Millisecond/microsecond/nanosecond units only |
| Decimal128 | Exact fixed-point strings | Exact fixed-point text | Nonnegative scale only |

JSON's ordered schema includes the Arrow type description (including timestamp
unit/timezone or decimal precision/scale), nullability, name, and a named cell
encoding. All integer widths use strings so generic JSON consumers cannot
silently round integers through IEEE-754 doubles. Finite floats retain their
Arrow width in the schema; readers must parse into that width. Negative zero
is preserved. A decimal's trailing zeros reflect its scale. Negative scales
are exact decimal strings with appended zeros. Timestamp cells are ticks,
**not** formatted or timezone-converted date strings.

NaN/infinities, out-of-precision decimals, nulls in non-nullable columns, nested
types, dictionaries, Decimal256, date/time/interval types, and every type not
listed above are explicit errors. Decimal precision must be 1–38, with
`abs(scale) <= precision`. Parquet additionally rejects second-unit
timestamps (it cannot store that unit natively), negative decimal scales,
duplicate names, and zero-column schemas, rather than doing lossy conversion.
No cast to strings is used to fake Parquet's typed data.

### JSON

The UTF-8 output is a single newline-terminated object:

```json
{"schema":[{"name":"count","type":"Int64","nullable":true,"encoding":"integer-string"}],"rows":[["9223372036854775807"],[null]]}
```

Rows are positional arrays aligned with the ordered schema. Duplicate names
do not overwrite cells. Null is JSON `null`; empty text/binary is `""`.
An empty result preserves its schema and has `"rows":[]`. Zero-column
schema-only results are supported by JSON.

### CSV

CSV is UTF-8 without a BOM, comma-delimited, and uses CRLF record separators.
It always includes a quoted header. Every non-null cell is quoted; embedded
quotes double, and embedded commas, CR/LF, and Unicode are preserved.

- Null: bare empty field.
- Empty string or empty binary: `""`.
- Literal `"null"`: `"null"`, not null.

The distinction is lexical, not a standardized CSV type system. Generic
readers that discard quotedness can conflate null and empty; schema-aware
consumers must retain it. CSV preserves column names and order but not full
Arrow type information. A schema-only result emits its header. A zero-column
CSV schema is rejected.

### Parquet

Output is a complete, uncompressed Parquet file with embedded Arrow schema
metadata, not a renamed JSON payload. Rows/columns/nulls and supported Arrow
types retain their order and meaning. The writer bounds row groups to 1,024
rows and flushes at batch boundaries. Schema-only files retain the schema.
Independent `ParquetRecordBatchReaderBuilder` tests compare actual decoded
schemas, arrays, values, and nulls; file magic alone is not validation.

## Bounds and output ownership

`OutputLimits::new(max_rows, max_bytes, max_batches)` has no unlimited default.
Byte and batch maxima must be nonzero; zero rows permits schema-only results.
The byte maximum applies **independently** to:

1. Supplied IPC bytes, before parsing or Arrow batch allocation.
2. Declared schema/evidence storage and retained array memory (conservatively
   counted per column/batch, even when buffers are shared).
3. Encoded output bytes, including headers, escapes, and Parquet footer.

IPC framing, FlatBuffer verification, declared total rows/batches, and body/
buffer ranges are checked before allocating decoded batches. Compressed IPC is
explicitly rejected, avoiding decompression-amplification allocations. Repeated
metadata references are charged before copying evidence or converting schema.
FlatBuffer verification also limits nesting to 16, tables to the byte budget,
and expanded apparent size to four times that budget, bounding aliased-reference
verification work before metadata is copied.
Negative or overflowing lengths fail. Empty batches count against the batch
budget. Unsupported/lossy cases fail before output delivery.

These are payload/work bounds, **not an allocator-wide resident-memory quota**:
input and output may coexist, and Arrow/Parquet have working/schema overhead.
The caller should use appropriately small finite limits and bounded transport
collection. Parquet encoding buffers at most one bounded row group at a time.

`encode` returns no partial vector on error. Serialization/budget errors in
`encode_to_writer` leave the sink untouched. Sink write/flush failures return
`TabularError::Output` and may leave a prefix; the crate cannot roll back an
arbitrary writer. The host must preflight paths before dispatch, refuse unsafe
binary terminal output, enforce no-clobber, stage/atomically finalize local
files, remove incomplete artifacts, and keep receipts/diagnostics separate.
This crate neither dispatches remote work nor marks final files successful.

## Offline validation

Tests generate synthetic Arrow fixtures in memory. They cover typed JSON,
independently parsed CSV, exact numeric boundaries/decimals/timestamps, Unicode,
embedded quotes/newlines, null versus empty, duplicate names, empty schemas,
Parquet schema/array readback, multi-batch ordering, malformed/truncated IPC,
multiple datasets, unknown metadata, unsupported types/values, finite budgets,
and writer/flush failures.

From `rust/`, after the integration owner supplies centralized dependencies
and grants the shared-host build slot:

```powershell
cargo test --locked -p fabric-sdk-tabular --all-targets
cargo clippy --locked -p fabric-sdk-tabular --all-targets -- -D warnings
cargo test --locked -p fabric-sdk-tabular --doc
```

The integration owner also owns workspace architecture, downstream adapter,
combined-candidate, and native-host checks. Component tests do not establish
CLI binding, HTTP/LRO/media behavior, filesystem atomicity, live permissions,
or service acceptance.
