<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental native Livy Arrow raw streaming

This is a native Rust **raw framed-v2 codec and in-memory delivery policy**, not
a complete Arrow SDK, HTTP streaming host, CLI feature or native IPC decoder.
JSON execution/paging is independent and unchanged. No Python, PyArrow,
credential discovery, backend discovery, runtime, network engine or durable
checkpoint is installed.

## Provenance and integration prerequisites

- Fabric Livy Arrow source pin:
  `bded4919a647b3f5f24b723abada69d45f23449c`,
  branch `features/5756961/arrow-extensions`.
- Primary behavior: `python/src/microsoft/fabric/livy/arrow.py` (framing,
  media types, budgets, SHA-256 identity), `client.py` (resumption and inline
  fallback), `_common.py` (submission/wait contract), `_ipc.py` (external typed
  decoder requirements).
- Exact golden vectors are retained in the public integration test from
  `conformance/vectors/framing-v2.json` at that pin. They are raw protocol
  vectors: strings such as `arrow-ipc` are deliberately **not valid IPC**.
- The source assessment `livy-arrow-assessment.md`, including G1–G12, was read.
  Its framing contradiction is resolved for this assignment in favor of the
  implemented **20-byte** data header, not the erroneous 16-byte description
  in the pinned `contracts/framing/v2.md`.
- Lane base: `5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b`.
  Common interface commit `5fe13bd171c3bccf76da519b556173570c786603` was
  cherry-picked first (`8e0e510` in this lane).
- Additive runtime enhancement
  `2cca2b509c2e0b7c0f7bf0968f9fbe8e427bfb1f` was subsequently cherry-picked
  (`9cbbb94` in this lane). Arrow request metadata now uses its native
  Content-Type and protocol-header fields directly.

The coordinator owns the optional SDK feature `livy-arrow = ["dep:ring"]`,
the dependency `ring = { workspace = true, optional = true }`, and the SDK
facade export `#[cfg(feature = "livy-arrow")] pub mod livy_arrow;`.
The integration test crate is gated by `#![cfg(feature = "livy-arrow")]`, so
default SDK tests do not import the optional module. The workspace already pins
ring 0.17.14. This lane does not change manifests, locks or facade exports.
Reconcile the SDK's dependency edge in the workspace lock through the normal
integration workflow before running `--locked` checks. SHA-256 uses
`ring::digest`; there is no handwritten crypto, weak hash or substitute integrity
algorithm.

Run the public test after that integration, from `rust/` with an authorized
build slot and the pinned toolchain:

```text
cargo test -p fabric-sdk --features livy-arrow --test livy_arrow --locked --offline
```

Also run package all-target checks, formatting, Clippy and the affected
downstream/architecture checks through the existing validation plan. No Cargo
build/test was run in this lane: Rust tools were absent from the worker PATH
during implementation. The coordinator subsequently restored pinned Rust 1.99.1
tools in its own artifacts, but no worker compilation/build slot was granted.
Static checks do not prove Rust compilation or test execution. Independent
review and combined-candidate validation remain coordinator gates.

Performed static checks: the source worktree HEAD equals the full pin; all 13
embedded golden vector objects equal the pinned catalog; lexical delimiters
balance in the six Rust files; all 28 test declarations are at top level; and
the staged diff passes `git diff --cached --check`. These checks do not replace
rustfmt, compilation, native test execution or independent review.

## Exact raw wire contract

The request Accept is `application/vnd.livy.arrow-chunks`. A successful GET
response must have `Content-Type: application/vnd.livy.arrow-chunks;v=2`
(case-insensitive media/parameter names, optionally quoted version).

All fields are **signed big-endian**:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | Positive encoded payload length |
| 8 | 4 | Nonnegative partition index |
| 12 | 4 | Nonnegative ordinal |
| 16 | 4 | UTF-8 chunk-ID byte length, 1..65536 |
| 20 | ID length | Chunk ID |
| 20 + ID length | Payload length | Raw encoded IPC payload |

A length of **0 alone is an eight-byte successful marker**. Success still
requires actual HTTP EOF; trailing bytes fail. **-1 alone is an eight-byte
service failure**, never success/retry. Other negative lengths, invalid UTF-8,
wrong partition ownership, duplicate IDs and duplicate partition/ordinal pairs
within one response fail explicitly. Missing terminal, partial header/ID/payload
and EOF exactly between data frames are all truncation.

IDs follow `part-<decimal partition>-<decimal sequence>`. Like the source, this
codec accepts variable widths, not just padded IDs, and does not equate sequence
with ordinal. Unicode decimal recognition is explicitly pinned to Unicode 16.0
Nd blocks; Python source acceptance depends on its Python Unicode version.
Ordinals need not start at zero, be contiguous or arrive sorted. Arrival order
is retained. Internal `application/vnd.livy.arrow-page;v=1` is unsupported.

Raw payloads are not interpreted as IPC: there is no IPC validity, schema,
compression, row-count or native-type claim. HTTP Content-Encoding must be absent
or identity. IPC codec strings are forwarded on submission without negotiation,
decompression, transcoding or a claim that the deployment supports them.

## Pull/drain and allocation boundaries

The shared runtime also defines `NativeStreamExecutor`, `NativeStreamResponse`
and pull-based `NativeStreamBody` contracts. `NativeStreamLimits` independently
bounds transport fragments and total HTTP body bytes. `NativeDeadline` is one
finite absolute observation budget: clones, consumer pauses and reconnects do
not restart it. A host must enforce it during credential acquisition, header
receipt and stalled body reads, not only after those futures return.

These contracts alone do not install a streaming HTTP adapter or connect the
Arrow decoder to a live response. Opening is single-dispatch, and dropping a
body disposes local observation without cancelling remote work. HTTP EOF,
framed-v2 completion, IPC validity and sink completion remain separate gates.

`FramedDecoder::push(fragment)` accepts arbitrary HTTP fragmentation, returns
the consumed prefix length and stops at **one complete chunk**. If that chunk
is pending, another push consumes zero. Retain the unconsumed suffix; call
`drain()`, deliver/process that one chunk, and only then continue. Do not fetch
more transport bytes merely because some of the current fragment remains.

At HTTP EOF, drain every chunk and call `finish()`. A zero marker is provisional
until `finish()` succeeds. Any protocol/limit/allocation error poisons that
response parser; use a fresh parser for a permitted reconnect. An early
`finish()` with a pending chunk reports `InvalidState` so the caller can drain.

The decoder stores one encoded payload, a bounded ID buffer, a fixed 20-byte
header and bounded identity/ordinal sets. It never accumulates the response.
The host must also bound its own read fragment; passing a prebuffered whole
response would defeat host backpressure even though the codec copies one frame.

`StreamLimits` independently bounds:

- Per-frame payload bytes (default 256 MiB).
- Cumulative per-response payload bytes (default 1 TiB).
- Frames per response and unique chunks per delivery ledger (default 100000).
- One ID (default/protocol ceiling 65536 bytes).
- Sum of retained ID bytes (additional native safety bound, default 16 MiB).
- Consumed wire bytes including framing, IDs, replay and terminal (additional
  native safety bound, default 1 TiB + 32 MiB per response).

All bounds are positive and validated. Declared payload/ID lengths, remaining
wire budget and cumulative budgets are checked before their allocation.
Payload/ID vectors use fallible reservation. Metadata entries are bounded by
count and identity bytes. IDs occur in a fixed number of parser/ledger/cursor
copies; allocator overhead, caller-retained chunks/batches, HTTP buffers and
process memory are **not** included in the byte counters. Whole-frame buffering
is intentional, not zero-copy or constant-size memory.

`RawInline::consume` instead passes through a borrowed bounded fragment and
`finish` records HTTP EOF. It does **not** expect framed-v2's eight-byte marker
or assert that HTTP EOF, empty bytes, or eight zeros form valid Arrow IPC.

## Owner-bound request contracts

`StatementOwner` retains the exact caller-owned `NativeEndpoint` and either:

- Session ID, with optional explicitly owned REPL ID.
- HC **execution-session** ID and REPL ID (never an acquisition/management ID).

Native callers select the native base. Fabric callers select the complete
`.../{fabricVersion}/workspaces/{workspaceId}/lakehouses/{lakehouseId}/livyApi/versions/{apiVersion}`
base themselves. SDKs discover neither that endpoint nor native tokens.
`StatementHandle` adds an authoritative nonnegative `u64` statement ID. Rust's
native handle range is finite, unlike Python's arbitrary-size integer.

Result GET routes are respectively:

```text
/sessions/{session}/statements/{statement}/result
/sessions/{session}/repls/{repl}/statements/{statement}/result
/highConcurrencySessions/{executionSession}/repls/{repl}/statements/{statement}/result
```

`ResultSelectors` forwards `after_chunk`, `parts`, and `from_part`, never JSON
`from`/`size` or internal page cursors. It bounds selector text to 65536 bytes,
rejects controls/negative partitions, and checks ID partition when both cursor
and partition are supplied. `parts` remains opaque; there is no invented
partition-expression grammar. Resume preserves it exactly.

`SubmitOptions` makes exactly this Arrow JSON body:

```json
{"code":"select 1","kind":"sql","format":"arrow","codec":"none"}
```

It targets the corresponding statement collection with native POST/Execute.
Handle mode uses JSON Accept, with omitted or 0..10000 `sync_wait_ms`.
Inline mode requires 1..10000 and accepts
`application/vnd.apache.arrow.stream`, or, **only with explicit opt-in**,
`application/vnd.apache.arrow.stream, application/json;q=0.9`.
Rust's integer API excludes bool/string/fractional wait arguments by type.
Code bytes are bounded before copying; JSON escaping may expand them sixfold.
Kind/codec strings have an additional 256-byte/control-character safety bound.
No live codec/kind/wait support is inferred and no probe, changed POST or
retry-without-wait occurs.

The host supplies a validated canonical-shaped UUID correlation header
`x-ms-livy-resource-id`. It is stored using `NativeRequest::with_header`;
`native().headers()` retains it for callers using the common request contract.
Native Accept and Content-Type retain the response representation and JSON body
media type. `ArrowRequest::headers()` exposes the same metadata as one convenience
iterator: exact Accept, POST Content-Type `application/json`, and protocol headers.
Apply **all** of them. Authorization, Host and transport-security headers remain
host-controlled; Arrow never attempts to override them. This UUID is not durable
idempotency, and no mutation is replayable.

`ArrowRequest::validate_response` validates actual status, expected media/version,
identity HTTP encoding, and optional declared body length. Enforce actual byte
budgets too. Non-2xx statuses remain HTTP errors; **404 is nontransient**.

An opted-in successful inline JSON response can be read through the bounded
`InlineFallback`. It parses body `id` and/or the **`statementId`** header, matching
them if both exist, and returns a handle bound to the original owner. There is
deliberately no argument for HC's `id` header (REPL identity). Missing, malformed,
negative, bool/fractional or conflicting IDs yield `OutcomeUnknown`. A fallback
handle causes no implicit poll, result fetch or submission. HTTP failures,
malformed JSON, a binary stream error and lack of deployment are **not** reasons
to perform fallback or repeat the POST.

## Replay and finite recovery

Use a fresh response parser but the **same `DeliveryState`** for one logical
iteration. `deliver` compares ID -> (partition, ordinal, SHA-256(payload)) and
occupied partition/ordinal pairs. Identical replay across responses is suppressed
before the sink; changed payload/identity or a new ID at an occupied position
fails closed. Duplicate IDs in a *single* response are still protocol failures,
even if identical. Unique byte/chunk/identity budgets span reconnects; each
response separately bounds replay and wire bytes.

The sink callback runs only for a new chunk. Cursor advances only after it
returns success, retaining full owner/statement, chunk ID, partition and initial
`parts`. Partial frame bytes never advance it. The host should use that exact
cursor for subsequent GETs. Callback failure poisons the ledger because output
may already be partial. A successful callback is **not a durable/transactional
acknowledgment**. No state serialization, restart-safe resume, retention/expiry
discovery or exactly-once file/stdout delivery is implemented. Supplying an
external initial cursor does not import historical digest/schema/sink state.

`RetryPolicy` is a finite decision function, not an actual HTTP retry engine:

- Only result GET transport interruption, truncation, HTTP 408/429/500/502/503/504
  may reconnect within the explicitly supplied maximum.
- Maximum N reconnects means at most N+1 openings when a host follows the plan.
- All POSTs stop without retry regardless of transport/status.
- 400/401/403/**404**/501, protocol/replay/limit errors, -1, typed failures,
  sink errors, local cancellation and operation timeout do not reconnect.
- One host-supplied monotonic absolute deadline includes opens, reads,
  reconnects **and caller pauses**. No response resets it.
- The host closes resources on every path, applies remaining time to actual I/O,
  rejects redirects, bounds buffers and handles local cancellation. There is no
  automatic remote cancel, borrowed-session cleanup, Retry-After or backoff engine.
- After dispatched submission loses an authoritative outcome, report unknown
  execution outcome rather than cancelled/safely retryable. The request contract
  alone cannot determine what a remote server executed.

## Typed decoding and output are deliberately distinct

`TypedDecoder` is an explicit external **native Arrow IPC** prerequisite, not a
decoder implementation. It must validate IPC messages, preflight encoded/
decoded limits and compressed buffer sizes before allocations, account for
nested/dictionary buffers, enforce exact schema metadata equality and emit one
batch at a time. `TypedStream` retains one such decoder/schema across chunks and
reconnects, compares schema **before** batch delivery, and poisons on failure.
`UnsupportedDecoder` returns `UnsupportedCapability`. The test `SchemaProbe`
only exercises trait/schema-state sequencing; it is not IPC acceptance evidence.

No Arrow crate/decompression implementation is selected in this lane. Therefore
compression-bomb protection, native row/type fidelity, real schema equality,
dictionary handling and typed inline IPC remain **unimplemented** until a native
decoder and its tests are integrated. Do not advertise a complete Arrow SDK.

`write_frame` and `write_terminal` support an explicitly labeled **raw Livy
framed-v2 output**, not one `.arrow` IPC file. Independent IPC chunk streams must
not be concatenated and called a single standard IPC stream. Only write the
success terminal after source EOF was validated and all sink processing passed.
Write failures may leave a partial artifact. No CLI binary stdout, atomic file
publication, overwrite/append or cleanup policy is installed.

## Assessment gates and remaining qualification

| Gate | Native evidence implemented here | Still not established |
| --- | --- | --- |
| G1 | No edits/dependency on JSON worker; Arrow body is separate | Combined JSON regression suite |
| G2 | Exact owner-bound classic/REPL/Fabric/HC POST/GET route tests | Actual host/async/cancel integration |
| G3 | Optional/zero/1/10000 wait and invalid-bound tests; numeric Rust API | Server honoring wait |
| G4 | All 13 source vectors; every split/fragment width; signed fields, ID UTF-8, terminal, truncation, duplicates, limits | Execution of Rust tests after export/dependency integration |
| G5 | Partial-frame reconnect, cursor, identical/conflicting replay, occupied ordinals, owner and logical-budget tests | Real streaming HTTP reconnect engine; durable sinks |
| G6 | Enumerated finite retry decisions, 404/permanent failures and no-POST-retry tests | Backoff/Retry-After host implementation |
| G7 | Encoded frame/total/chunk/ID/wire budgets before reserve; explicit raw/typed distinction | Native IPC preflight, compression/dictionary/nested allocation tests |
| G8 | Fake-time absolute deadline/caller-pause tests; sink-failure poison | I/O interruption, response-close/cancellation host tests |
| G9 | Explicit fallback, bounded fragments, matching statement IDs, HC owner preservation | Streaming host closes response before returning handle |
| G10 | Raw framed writer round-trip and partial-write tests, clearly not a single IPC file | Typed output/type fidelity, CLI terminal/file policy |
| G11 | In-memory-only cursors; no checkpoint serialization or restart guarantee | Durable transactional restart delivery, if ever approved |
| G12 | No Python/PyArrow/native IPC dependency; only existing workspace ring is requested | SDK export/dependency integration, native decoder distribution matrix |

These are **test cases present**, not test-pass receipts. No live Spark,
authentication, installation or deployment experiment was performed.

The pin's `docs/release-gates.md` records classic Arrow route **404** failures,
despite recorded JSON successes. These are upstream recorded results, not
qualification performed here. Relay/server flags and shared result storage are
server prerequisites, not client-discoverable capabilities. Native Apache Livy,
HC isolation/retention, arbitrary codecs, tenant permissions, server rollout and
remote cancellation remain unqualified. Expose this as experimental and
deployment-gated; never mask a 404 with automatic JSON execution or fallback.
