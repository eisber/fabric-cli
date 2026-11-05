<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Optional native Livy Arrow IPC decoder

`NativeIpcDecoder` implements the existing raw Arrow `TypedDecoder` seam with
Apache Arrow's native `RecordBatch` and `Schema`. This is an opt-in SDK data-plane
decoder, not a schema wrapper, HTTP engine, Python bridge, CLI output adapter,
deployment qualification, or complete streaming transport.

The behavioral source is Fabric Livy SDK Arrow commit
`bded4919a647b3f5f24b723abada69d45f23449c`, assessed in the source-contract
packet's G7/G10 requirements. The native implementation independently wraps
Apache's safe Rust APIs; it does not copy the Python IPC implementation.

## Integration owned by the coordinator

The implementation branch owns only `src/livy_arrow/ipc.rs`, its integration
test, and this document. The coordinator must wire these exact dependencies in
the workspace and SDK manifests, update the lockfile, and export the type:

```toml
# Workspace dependencies. All three are Apache-2.0, minimum Rust 1.88.
arrow-array = { version = "=60.1.0", default-features = false }
arrow-schema = { version = "=60.1.0", default-features = false }
arrow-ipc = { version = "=60.1.0", default-features = false, features = ["lz4", "zstd"] }

# SDK feature:
livy-arrow-ipc = ["livy-arrow", "dep:arrow-array", "dep:arrow-schema", "dep:arrow-ipc"]

# SDK dependencies:
arrow-array = { workspace = true, optional = true }
arrow-schema = { workspace = true, optional = true }
arrow-ipc = { workspace = true, optional = true }
```

In `livy_arrow/mod.rs`, add:

```rust,ignore
#[cfg(feature = "livy-arrow-ipc")]
mod ipc;
#[cfg(feature = "livy-arrow-ipc")]
pub use ipc::NativeIpcDecoder;
```

`lz4` is an explicit `arrow-ipc` feature; `zstd` is its implicit optional
dependency feature, not a separately declared feature table entry. Both are part
of this decoder's dependency contract, not SDK defaults. An IPC build without
those dependency features is not a supported configuration. Apache uses
`lz4_flex` for LZ4 frames and the `zstd` crate (including its native dependency)
for ZSTD; this crate introduces no FFI or unsafe code of its own.

The workspace's declared Rust 1.99.1 exceeds Arrow's minimum. Default JSON and
raw `livy-arrow` consumers must not acquire any Arrow dependency or typed decode.
No facade, CLI, shared HTTP, raw framing, or existing typed seam changes belong
to this slice.

## Public contract

```rust,ignore
use fabric_sdk::livy_arrow::{DecodeLimits, NativeIpcDecoder, TypedStream};

let mut typed = TypedStream::new(
    NativeIpcDecoder::new(),
    DecodeLimits {
        max_message_bytes: 256 * 1024 * 1024,
        max_decoded_bytes: 256 * 1024 * 1024,
    },
)?;
// raw_chunk.payload() already contains exactly one bounded complete IPC stream.
typed.decode(&raw_chunk, &mut |batch: arrow_array::RecordBatch| {
    consume_one_batch(batch)
})?;
```

`Batch = arrow_array::RecordBatch`; `Schema = arrow_schema::Schema`, whose
equality includes fields and all field/schema metadata. `TypedStream` rejects a
different chunk schema before emitting that chunk and poisons failures.
Dictionary IDs are stream-local decoding identifiers, not cross-chunk schema
identity. A native reader is created afresh for every call.

`inspect_schema` performs complete preflight, then decodes and drops every batch
without calling a sink. Thus it validates values, UTF-8, nested offsets, nulls,
and dictionary keys as well as schema and framing. `decode` independently
preflights the entire chunk and then pulls one native batch at a time. It checks
the reader schema and every batch schema against the inspected native schema.
It does not collect output batches. A sink error is returned unchanged and
stops iteration before the next batch is decoded.

The current `TypedStream` seam calls both methods, so native data is decoded
twice. This trades CPU for complete inspection and a stateless, bounded
implementation; it is not a claim of zero-copy or single-pass decoding.
Direct `decode` callers can receive earlier batches before a later malformed
value fails native validation. Sink output can therefore be partial on error.

## Bounds before native decoded allocations

Before creating `StreamReader`, the wrapper walks the **entire** encoded
payload with the verified FlatBuffer root accessor:

1. Require one initial schema, consistent metadata V4/V5, checked little-endian
   lengths, complete metadata/body slices, exactly one explicit EOS, and EOF
   immediately after EOS. Legacy four-byte lengths/EOS and modern continuation
   prefixes are accepted. Missing EOS is truncation even though Apache permits
   implicit EOF. Duplicate schemas, unsupported messages, trailing streams,
   negative lengths, missing dictionaries, and malformed metadata fail.
2. Bound each complete encoded message, including prefix, padded metadata, and
   body, by `max_message_bytes`. The complete raw payload was bounded by the raw
   framing layer; this wrapper does not clone it or retain a message list.
3. Before `try_fb_to_schema`, bound schema expansion, recursion, fields, names,
   timezones, and all field/schema metadata. Reject duplicate or incomplete
   metadata entries rather than silently collapsing them into a native map.
   The safe FlatBuffer verifier also has its upstream finite depth/table/
   apparent-size caps; no unchecked root accessor is used.
4. Require valid nonnegative node lengths/null counts, exact node/buffer
   consumption for the schema, and checked products/sums for logical extents.
   Validate buffer bounds and nonoverlapping nonempty encoded extents. Check
   validity sizes, fixed-width values, nested child lengths, and offset-buffer
   sizes. For uncompressed buffers, also check offset values for nonnegativity,
   monotonicity and child/value-buffer bounds before native decoding.
5. For each nonempty compressed buffer, inspect the signed eight-byte decoded
   size first. `-1` means uncompressed bytes following the header; a zero-sized
   buffer can omit the header. Other negative sizes are invalid. Sum declared
   decoded sizes with checked arithmetic **before any decompressor is called**.
   Compression method must be BUFFER and codec must be LZ4_FRAME or ZSTD.
6. Charge the schema estimate, cumulative dictionary costs, encoded body,
   decoded buffers rounded to 64-byte alignment plus per-buffer allowance,
   per-node allowance and logical validity extents against
   `max_decoded_bytes`. Record batches release this charge after handoff.
   Dictionary replacements never refund their charge within this IPC stream.

The hard caps are 1 MiB per metadata message, 1 MiB schema-expansion estimate,
1024 schema fields/nodes, depth 32, 3072 buffers per batch, and 4096 metadata
entries per metadata vector. Schema expansion is also capped by both caller
limits. A logical node/batch extent multiplied by 64 must fit `isize`.
These conservative caps can reject otherwise valid IPC before the caller's
nominal payload budget is exhausted.

Apache 60.1.0's LZ4 path bounds reads to the advertised output length and probes
one extra byte without growing the output; ZSTD bulk decompression receives the
advertised maximum output capacity. The native default full array validation
remains enabled. Compressed offset **values**, dictionary key ranges, UTF-8 and
actual null counts are validated by Apache after decompression into the
preflight-bounded output, not falsely described as readable without decoding.
Forged larger declared sizes fail preflight; forged smaller sizes must fail the
native bounded decompressor or array validation.

These are conservative decoded-data accounting bounds, not a peak RSS or exact
allocator limit. They exclude codec workspaces (for example LZ4 frame scratch),
allocator rounding/overhead beyond the stated allowances, the already-owned
raw payload, and batches retained by the application. Allocation failures
reported by Apache map to `Allocation`; the Rust allocator's process-abort OOM
behavior is not converted into a recoverable error. Errors are sanitized and
never include payloads or native error text.

## Explicit capability limits

Supported layouts are Null, Boolean, signed/unsigned integers, Float16/32/64,
binary/string and large binary/string, positive fixed-size binary, dates,
times, timestamps, durations, intervals, Decimal32/64/128/256, List/LargeList,
positive FixedSizeList, Struct and standard Map. Scalar-valued dictionaries
can appear inside those nested containers. Dictionary replacements are supported
and cumulatively counted, including unused dictionaries.

The wrapper returns `UnsupportedCapability` before constructing an allocating
reader for unions, binary/string/list views, run-end encoding, unknown future
types/features/codecs, big-endian schemas/hosts, non-scalar dictionary values,
shared dictionary IDs declared by multiple fields, and dictionary deltas.
Deltas would invoke Apache's concatenate path, whose transient copies and
new offsets need a separate retained-size model. View/run-end/union layouts
likewise require their own checked structural accounting. This is intentionally
not unqualified support for every Arrow type or every valid dictionary stream.
The wrapper requires dictionary definitions even for all-null dictionary
columns, rather than Apache's optional empty-dictionary synthesis.

## Offline fixtures and truthful validation status

`tests/livy_arrow_ipc.rs` is gated by `livy-arrow-ipc` and denies
`unnameable_test_items`. It writes real IPC with Apache `StreamWriter`, exercising
uncompressed/LZ4/ZSTD row counts, nulls, nested arrays, dictionaries, schema and
field metadata, schema-only/empty batches, cross-chunk schema mismatch,
truncation/EOS/trailing streams, corrupt FlatBuffers, duplicate schemas, buffer
bounds, nested offsets, integer overflow, per-message encoded budgets, decoded
budgets, cumulative dictionary replacements, dictionary scope, explicit
unsupported deltas/views, schema amplification/depth, and sink failure.

The compression-bomb fixtures replace an actual compressed buffer's declared
size with 1 MiB, corrupt its compressed bytes, and demand `Limit` under a 64 KiB
budget. The malformed-codec error must not precede the limit. Separate fixtures
forge smaller decoded headers and matching raw offsets, so structural preflight
passes and the native decompressor must reject the oversized actual output.
Those error assertions do not independently measure allocator behavior; the
bounded-output implementation is an upstream API/source prerequisite, not a
claimed executed allocation-instrumentation result.
The sink fixture corrupts UTF-8 in the second batch: inspection must reject it;
a failing first-batch sink must stop before decoding that invalid second batch.

**No build, package/toolchain install, or test execution was performed in this
lane, as explicitly assigned.** Cargo/rustfmt were unavailable on this session's
PATH; formatting also remains for the coordinator. An independent source review
covered allocation/failure behavior and public APIs/fixtures. It found an
underreported-size fixture that failed too early in preflight; the fixture was
repaired to keep the raw offsets consistent with the forged decoded length.
The repair still requires execution and independent validation. Source
inspection, fixture presence and lexical
checks are not a test pass. The coordinator must first wire manifests/exports,
then run the serialized pinned-toolchain all-target check and actual tests, for
example from `rust/`:

```text
cargo +1.99.1 check -p fabric-sdk --all-targets --features livy-arrow-ipc --locked
cargo +1.99.1 test -p fabric-sdk --features livy-arrow-ipc --test livy_arrow_ipc --locked
```

Default/raw-only dependency isolation and existing raw tests also require
coordinator validation. G7/G10 are not fully signed off by an uncompiled lane;
HTTP streaming, CLI output and deployed service qualification remain outside
this A1 typed-decoder slice.
