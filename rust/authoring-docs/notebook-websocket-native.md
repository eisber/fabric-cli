<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Already-upgraded Notebook WebSocket channel

The optional `fabric-sdk-transport/notebook-websocket` feature provides
`notebook_websocket::NotebookWebSocket<S>`, implementing the SDK's existing
`NotebookChannel` on a consumed caller-owned Tokio byte stream. This is a real
RFC 6455 data path, separate from the managed-execution driver. It uses pinned
`tokio-tungstenite = 0.30.0` with default features disabled:
`WebSocketStream::from_raw_socket(Role::Client, Some(config))`. No connect,
handshake, TLS or HTTP client feature is needed. Default consumers are unchanged.

The Jupyter 5.3/Synapse semantics remain owned by the SDK driver/protocol,
translated from FNTK `e250d5d32b04efcc5623a414a361727a93c271ce`.
This adapter neither reparses JSON nor installs another execution engine.

## Caller contract

`NotebookWebSocket::from_upgraded(stream, ready, options, deadline).await`
requires:

- `S: tokio::io::AsyncRead + AsyncWrite + Unpin + Send + 'static`.
- An **already successfully HTTP-upgraded**, authenticated stream. The caller
  must retain any preread bytes within its own I/O implementation.
- An explicit `ReadySession` attesting the exact owner, Jupyter session, kernel
  and established control comm, the correct JSON subprotocol and actual Spark
  readiness. Neither constructing this value nor REST session creation proves
  readiness. No random identifiers or startup ACKs are inferred here.
- Valid finite options and one original `NativeDeadline`. Later per-operation
  deadlines can shorten, but cannot extend, the original budget.
- The caller's existing Tokio runtime with time enabled for I/O operations.
  The feature also implements the SDK `DeadlineAlarm` for the existing
  `TokioTimer`; pass `Box::new(TokioTimer)` to `ManagedExecution`.

The channel exposes its immutable binding and sanitized state/control count,
but no mutable socket, owner or deadline accessor. Do not construct it around
an unupgraded TCP/TLS stream and treat construction as a readiness claim.

## Bounded codec and pull behavior

Options are validated **before** constructing the codec and its eager read
buffer. Invalid input is rejected, not clamped:

| Option | Accepted bound |
| --- | --- |
| `read_buffer_bytes` | 1 through 65,536 |
| `message_bytes` | 1 through 16,777,216 |
| `frame_bytes` | Positive, at most `message_bytes`; incoming payloads |
| `write_buffer_bytes` | Zero through `message_bytes`; every operation flushes |
| `controls_per_receive` | 1 through 1,024 Ping/Pong frames |
| `controls_total` | At least the per-receive cap, at most 1,000,000 |

`max_message_size` and `max_frame_size` are always positive `Some` values.
The finite maximum write buffer is computed with checked arithmetic:
write threshold + maximum message + 14-byte masked frame header +
two maximum masked control frames (139 bytes each). This is strictly larger
than the write threshold and accommodates data plus queued automatic controls.

Outgoing UTF-8 byte length and encoding are checked before copying or sending.
The library applies client masking. Incoming fragments are reassembled by the
library into one complete UTF-8 text message per successful receive. An
oversized declared individual frame is rejected before reserving/reading its
body; since the frame cap cannot exceed the message cap, this also rejects a
single frame declaring an oversized message. For fragmented messages, the
library checks the cumulative message bound before extending the assembled
message; an individually bounded continuation frame may already have been
read. Binary messages, even JSON-looking bytes, are terminal unsupported input.

**Conservative per-call receive contract:** version 0.30.0 exposes no public
configuration setter on `WebSocketStream`. A `receive(max_bytes)` smaller than
the configured `message_bytes` therefore returns `UnsupportedCapability`
**before polling I/O**, permanently disposing the socket. It does not allocate
to a larger cap and then check a smaller budget. A managed driver's shrinking
remaining-total-byte window may hit this restriction; choose the fixed channel
cap deliberately. There is no successful smaller-window fallback or replay.

There are no tasks, queues, background reads or prefetch loops. Only the
driver-requested next text message is returned; Ping/Pong handling is bounded
both per receive and across the channel lifetime, including unrelated controls.
Automatic Pongs are flushed unchanged under the same deadline. Crossing a cap
drops the socket; no response to the excess control is promised.

The codec may buffer bounded **raw bytes**, including bytes for later frames.
This is not a zero-physical-TCP-read-ahead claim. While the managed output sink
is paused, the adapter performs no additional logical receive or I/O poll.
Finite byte-I/O quanta yield between ready reads/writes, preventing an endless
stream of empty continuation frames from monopolizing one operation poll.
Both original and call deadlines are checked at the underlying I/O boundary
and before/after operation/control processing. Caller I/O must obey the
nonblocking async contract; arbitrary blocking caller code cannot be preempted.
These limits do not bound RSS, allocator rounding, caller-owned I/O buffering
or remote application allocations.

## Deadline, lifecycle and errors

Every polled send/receive/close takes the owned socket out of the channel
**before its first await**. It is returned only after a successful send or text
receive. Dropping a polled future drops the owned I/O and poisons continuation;
dropping an unpolled operation does not touch I/O or state. Dropping the
constructor future releases the consumed stream as well.

The same absolute lifetime budget includes caller pauses, controls, sends,
receives and close. Runtime alarms race stalled I/O without a thread or global
runtime. Clock checks after a ready poll reject late outcomes. An attempted
send error/timeout/drop leaves `SendUnknown`, and a later send fails without
touching I/O: it must never replay potentially dispatched code. Preflight
size/UTF-8/deadline rejection instead returns `Limit`, `InvalidInput` or
`TimedOut` before dispatch.

An idle channel has no background deadline watcher: caller pauses consume the
budget, and the next polled operation rejects expiry and disposes the socket.
The caller can always release idle I/O immediately by dropping the channel.

For receive, capacity/control violations return `Limit`; malformed UTF-8 or
RFC protocol violations return `InvalidResponse`; binary returns
`UnsupportedCapability`; raw EOF/reset without a Close handshake is
`Transport`, not clean completion. Actual peer Close flushes the library's
automatic response and returns `None`. Stalled control/close flushing is still
deadline-bound. All errors are existing sanitized `NativeError` variants, and
channel `Debug` never delegates to caller I/O or emits owners, IDs, URLs or
payloads. Applications must independently choose safe third-party log levels.

Local close is best-effort RFC Close plus flush followed by unconditional
disposal, **not** a wait for a remote kernel acknowledgement. Close failure or
future drop still releases I/O. Repeated local close is a no-op. No close/drop
cancels, interrupts, deletes or restarts sessions/kernels; no rollback,
reconnect, credential refresh or resubmission is claimed.

## Declared offline checks and validation handoff

The new integration test file is
`engine/core/http/transport/tests/notebook_websocket.rs`, with
top-level `#![deny(unnameable_test_items)]`. Its real tungstenite server peers
use `tokio::io::duplex`; instrumented byte streams observe polls, masking,
drop and stalls. Malformed-wire tests supply explicit RFC frame bytes, not a
second codec. The `ReadySession` fixture is an offline caller attestation,
not REST-derived Spark readiness or a synthetic startup ACK.

Declared top-level tests:

- `sends_real_masked_client_text_and_counts_unicode_bytes`
- `receives_one_text_message_and_reassembles_fragmented_unicode`
- `rejects_declared_oversize_frame_or_message_before_body_poll`
- `fragmented_message_sum_is_independently_bounded`
- `smaller_per_call_budget_is_terminal_and_rejected_before_io`
- `invalid_options_reject_before_codec_or_io`
- `send_preflight_checks_bytes_and_utf8_without_copying_to_io`
- `binary_is_terminal_unsupported_not_json_even_when_bytes_are_json`
- `malformed_utf8_protocol_and_masked_server_frames_are_redacted`
- `automatic_pong_is_flushed_unchanged_under_the_receive_deadline`
- `ping_flush_stall_times_out_and_drops_the_socket`
- `ready_control_flood_has_per_receive_and_lifetime_limits`
- `genuine_close_flushes_response_but_raw_eof_and_reset_are_transport_errors`
- `local_close_sends_only_rfc_close_and_disposes_without_remote_ack`
- `stalled_send_receive_and_close_race_the_existing_runtime_timer`
- `fresh_call_deadline_does_not_extend_original_and_pauses_consume_budget`
- `late_ready_io_is_rejected_after_poll_not_reported_as_success`
- `unpolled_operations_preserve_io_but_polled_drop_poison_disposes`
- `constructor_future_and_channel_drop_release_consumed_io`
- `owner_binding_is_immutable_and_debug_does_not_delegate_to_io_or_owner`
- `ready_empty_fragment_flood_yields_within_a_finite_io_quantum`
- `actual_managed_execution_observes_native_success_error_and_abort_over_real_peer`
- `managed_sink_backpressure_prevents_another_logical_receive_or_io_poll`
- `managed_negative_correlation_cannot_finish_and_wrong_parent_session_fails_closed`
- `managed_peer_loss_and_dropped_sink_future_never_replay_execution`
- `managed_constructor_rejects_another_notebook_owner_before_any_io`
- `expired_constructor_receive_and_close_dispose_without_io`
- `finite_nonzero_write_threshold_and_read_buffer_boundaries_work`

The managed-flow tests use actual `ManagedExecution`, SDK public constructors
and codecs, and this real channel. The peer receives the actual execute request
before sending correlated output and execute replies. Workload error/abort are
distinguished from observation success. A counting delegation wrapper observes
logical receives during sink backpressure; it does not replace the transport
or protocol engine.

This authoring lane is restricted to formatting/static inspection. It has not
run compilation, tests, Clippy, dependency resolution, feature-graph checks or
backend acceptance. The integration owner owns serialized feature/all-target
validation and exact-head evidence; declarations are not passing results.

## Explicitly undelivered

Authentication/bootstrap, HTTP upgrade, TLS composition, concrete MWC policy,
subprotocol negotiation, Spark startup/readiness discovery, reconnect/resume,
full Notebook facade and executable composition remain undelivered. This
slice does not modify SDK protocol/driver, identity/core, HTTP/TLS machinery,
manifests, lockfile, impact graph or protected certificate validation oracles.
Existing component-test results do not validate this new channel.
