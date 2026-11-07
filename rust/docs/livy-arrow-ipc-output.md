<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# One native IPC output stream across Livy chunks

`livy-arrow-ipc` exposes
`fabric_sdk::livy_arrow::ipc::output::NativeIpcOutput`. It accepts native
`RecordBatch` values and uses one Apache Arrow 60.1.0 `StreamWriter` throughout
the output lifetime. It writes one schema, any necessary dictionary messages,
all batches in order, and one explicit EOS when `finish` succeeds. It does not
concatenate the independent IPC streams inside framed-v2 chunks.

This slice is based exactly on coordinator commit
`8fcd0859eacc7a4fea4a29485bd0f46ee6476115`. It changes only the new
`livy_arrow/ipc_output.rs`, a nested module declaration in `ipc.rs`, the existing
IPC integration test, and this document. Dependencies, features, manifests,
lockfile, facade exports and the streaming author's `mod.rs` are unchanged.

## Small caller-owned API

```rust,ignore
use fabric_sdk::livy_arrow::{
    DecodeLimits, NativeIpcDecoder, TypedDecoder, TypedStream,
    ipc::output::{IpcOutputLimits, NativeIpcOutput},
};

// first_chunk is a real observed raw chunk, including a possible schema-only
// stream. The host owns the Write sink and its publication/lifecycle policy.
let schema = NativeIpcDecoder::new().inspect_schema(&first_chunk, decode_limits)?;
let mut output = NativeIpcOutput::new(
    &mut caller_sink,
    schema,
    IpcOutputLimits {
        max_output_bytes: 256 * 1024 * 1024,
        max_rows: 10_000_000,
        max_batches: 100_000,
    },
)?;
let mut typed = TypedStream::new(NativeIpcDecoder::new(), decode_limits)?;
if let Err(error) = typed.decode(&first_chunk, &mut |batch| output.write_batch(batch)) {
    let _ = output.abort(); // Already poisoned if the output itself failed.
    return Err(error);
}
// Repeat the same callback for subsequent replay-filtered chunks.
// On upstream failure/cancellation, abort rather than finishing the output.
// Only the host can validate source terminal + EOF and all source operations.
let local_encoding = output.finish()?;
drop(output); // Releases the mutable borrow of caller_sink.
```

The explicit `NativeIpcSchema` is required at construction, before any batches.
For empty results, inspect an actual schema-only source stream or supply an
already observed schema explicitly. The API has no absent-schema fallback and
never invents a zero-column schema because no remote data arrived. A genuinely
observed zero-column schema is supported.

`write_batch` consumes one batch and returns `Result<(), ArrowError>`, so it fits
the existing typed decoder/stream batch callback. This is also the intended
small sink boundary for a typed result driver. **No `TypedResultDriver`
implementation exists in the original writer-only base.** The additional
`ipc::result::write_ipc_result` consumer below now integrates the real framed
GET driver. The batch-only composer itself does not parse framing, decode IPC, request HTTP,
manage retries/replay, or duplicate native decoder preflight.

The sink is a borrowed `std::io::Write`, including `dyn Write`; no file is opened
or named by the SDK. There is no runtime/global state or background task.
The host must provide a fresh output destination and prevent external interleaved
writes. A generic `Write` cannot inspect/truncate prior bytes; preexisting sink
contents are outside this composer's byte counter. Never append a replacement
composer to failed partial output as a retry.
Writes and final flush apply synchronous backpressure. A host must offload
blocking sinks/encoding to an appropriate caller-owned blocking execution
boundary; do not invoke a blocking file/network sink on an async executor
thread merely because this API is callable from its callback.

## Exact schema and dictionary behavior

The constructor accepts the native decoder's bounded `NativeIpcSchema`.
Before each batch is encoded, the composer reuses that wrapper's exact native
schema matcher: fields, data types, nullability, field/schema metadata, and
recursive dictionary-ordering identity must match. Apache `Schema::eq` alone is
not sufficient because it ignores dictionary ordering inside fields. Root and
nested ordering changes are rejected as `SchemaConflict` before any bytes of
the affected batch are sent to the writer.

Only stream-local dictionary IDs are deliberately ignored. Apache's writer
assigns its own output IDs by schema traversal, rather than copying incoming
chunk IDs. The writer is kept alive across chunks and uses explicit `Resend`
dictionary handling: equal dictionaries can be reused; changed dictionaries
produce replacement messages with the proper output ID before their batch.
It does not assume that independent chunks have identical dictionary values or
keys, and does not enable delta dictionaries. Nulls, native values, metadata,
nullability and ordered/unordered dictionary semantics are preserved.

Output is uncompressed IPC V5 using Apache's default alignment. Input chunks can
use any codec/layout supported by the existing native decoder. Output is a
standard Arrow **stream**, not the Arrow file format with a footer, not a
framed-v2 body, and not a complete `.arrow` file host.

## Finite accounting, errors and lifetime

All three `IpcOutputLimits` fields must be strictly positive. Invalid limits
write nothing. Counts and byte sums use checked arithmetic. Every batch's row
count must also fit IPC's signed 64-bit length before Apache encodes it.

- `max_output_bytes` includes the initial schema, all dictionary messages,
  metadata/padding/bodies, and the final EOS. Before forwarding each write, the
  adapter checks the entire offered slice against the remaining budget. A slice
  that would cross the cap is rejected before that slice is copied to the sink.
  Earlier slices may already have been written; this is not transactional output.
- `max_rows` and `max_batches` are checked cumulatively before native encoding.
  Zero-row batches consume the batch budget but not the row budget.
- `bytes_written()` reports accepted bytes, including partial writes. It is
  progress accounting, not success, durability, source completion, or an artifact.

Legal positive short writes are completed through the standard `write_all`
contract. A zero write on nonempty input, any real sink error (including
`Interrupted`), or a final flush error becomes sanitized `SinkFailed`. Byte
budget errors remain `Limit`; native non-I/O errors use existing sanitized Arrow
error mapping. Original sink/native error text, data and paths are never
returned. The internal adapter distinguishes its own byte-limit error from a
sink's real `FileTooLarge` error; the latter still reports `SinkFailed`.

All failed operations poison the composer permanently, including pre-write
schema/count failures. Calls after failure/abort/finish return `InvalidState`
without retrying or appending more output. State is set failed before entering
the writer, so an unwinding sink/encoder panic does not leave an apparently
reusable open composer. Panics are not caught or converted to SDK errors.

Construction writes the schema immediately and can leave partial schema bytes
on error. `abort` and dropping an unfinished composer never append EOS or flush,
delete, truncate, roll back or publish anything. The caller retains the sink and
partial bytes, and chooses their disposal policy. A caller-owned buffered sink
can have its own eventual drop/flush behavior independently of this component.
Some IPC readers accept implicit EOF; readable batches without an explicit EOS
are not evidence of this composer's successful finish.

`finish` succeeds once, only after writing EOS and successfully flushing the
sink. Its `LocalIpcCompletion` contains accepted output bytes and successfully
encoded row/batch counts. It means **local encoding completion only**: not
workload success, framed terminal/EOF validation, HTTP completion, durable
storage, atomic publication, or remote cancellation. A flush failure returns
failure even if a valid EOS was already accepted by the sink. On an upstream
failure not observed by this batch-only component, the host must abort it; the
composer cannot infer a failed HTTP operation from the absence of another batch.

Composer `Debug` reports only state and counts, never the sink, schema, data,
metadata, paths or underlying errors.

## Memory scope and remaining host work

The composer stores one schema, counters, and one native writer. It consumes
and drops each record-batch handle after its synchronous write; no result vector,
input chunk archive, or list of encoded output batches is collected. Apache may
retain reusable writer scratch and dictionary-tracker array/buffer references
for each schema dictionary, including keys/values or shared input body storage.
These are writer state, not a promise that every prior input buffer is
immediately freed.

The sink byte cap is not a preallocation cap on Apache's schema/batch encoder.
Encoding the current batch can allocate writer buffers before the cap rejects
a subsequent sink write. Input arrays, caller-retained batches, dictionary
buffers, allocator overhead, encoder workspace, sink buffering and total RSS
are not bounded by the output byte counter. Keep decoder limits and host
input/output memory controls separate; there is no total-process-memory claim.

No CLI command, SDK file access, file naming/publication, async host adapter,
backend operation, authentication or deployment
qualification is provided. The host must coordinate source completion, abort
on upstream failure, offload blocking work, and implement any partial-output
cleanup/publication policy.

## Offline fixtures and unexecuted validation

The existing feature-gated IPC test crate still denies
`unnameable_test_items`. New real Arrow writer/reader fixtures exercise two
independently encoded framed-v2 chunks through raw parsing, typed decoding and
one output writer; dictionary replacement, metadata/nulls/types; a single schema
and explicit EOS with no trailing stream; local dictionary-ID remapping;
known-schema empty output; root/nested ordering, metadata and nullability
mismatch; exact/tiny schema/batch/EOS byte budgets; row/batch budgets and integer
overflow; legal short writes, real sink errors, zero writes, partial failures,
flush failure after EOS, double finish, abort/drop without finish, and redacted
diagnostics.

**No build or tests were run in this lane, as directed.** Fixtures, source
inspection and `git diff --check` are not an execution pass. Independent
review and the coordinator's serialized pinned-toolchain check/test remain
required, for example from `rust/`:

```text
cargo +1.99.1 check -p fabric-sdk --all-targets --features livy-arrow-ipc --locked
cargo +1.99.1 test -p fabric-sdk --features livy-arrow-ipc --test livy_arrow_ipc --locked
```

## Integrated framed GET consumer

`fabric_sdk::livy_arrow::ipc::result::write_ipc_result` consumes a fresh
`FramedResultStream`, a caller-owned `Write`, `DecodeLimits` and
`IpcOutputLimits`. It performs actual asynchronous response pulls through the
stream's injected executor. It uses the existing stream delivery ledger:
identical acknowledged replays never reach decoding or the output writer.
There is no second ledger, hidden submission, file open, or new retry engine.

The first real IPC chunk establishes an observed schema, including an empty
schema-only stream. Subsequent chunks are checked against exact logical schema
identity before writing their batches. A schema-free framed terminal returns
`IpcResultCause::MissingSchema` without writing output; it does not invent an
empty schema. The original source codecs, dictionary replacements, nulls and
metadata are handled by the bounded native decoder and the single writer.

Every chunk is acknowledged only after all of its IPC batches were successfully
validated and written. Schema, decode, sink, bound and framing failures close
local observation without finishing the writer. Errors retain the original
structured stream/native/Arrow cause, accepted output bytes, owner binding,
acknowledged cursor and partial progress. The error never substitutes a generic
sink failure for a schema or protocol error. Dropping the consumer future also
closes the response and drops unfinished output; no remote cancel or local
rollback is implied.

Only a valid framed terminal, actual HTTP EOF, valid complete source IPC and
successful local output finish/flush yield `IpcResultReceipt`. This certifies the
observed representation, not overall remote workload success, durable storage
or atomic publication. Flush or deadline failure can occur after EOS bytes were
accepted. The original absolute observation budget is checked around decoding,
acknowledgement and finalization; synchronous decoding or `Write` cannot be
preempted, so blocking sinks still require a caller-owned execution boundary.

Offline regressions exercise fragmented compressed dictionary chunks, one
standard output stream, recovery with replay suppression, known empty versus
missing schema, corrupt IPC and framing, schema changes, invalid limits before
network access, and dropping a pending consumer with partial output. These
instrumented executor tests are not live backend/identity qualification.
