<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Execution time and local cancellation

`fabric-core::execution` owns the reusable monotonic scheduling boundary.
`ExecutionClock` supplies both `now() -> Duration` and asynchronous `sleep` in
one domain. It consolidates the former jobs `RunClock`; the old import remains
an alias, so existing caller implementations and `JobFuture` signatures work.
The LRO `deadline::{MonotonicDeadline, DeadlineError}` imports also remain
compatibility reexports, not a second deadline implementation.

Scheduling never uses an output timestamp. Existing `fabric-cli-output::Timestamp`
and GET's `GetOutputClock` remain separate, validated wall-time presentation
inputs. Moving wall time forwards or backwards must not add execution budget.

## One original budget

Create `MonotonicDeadline` at the operation's existing start boundary, then
reuse it and the same injected clock/cancellation future through credentials,
HTTP headers/body reads, retry and polling waits, progress and output.
`new` retains the jobs/native positive, inclusive 24-hour limit. `after` performs
checked positive arithmetic without adding that policy to legacy HTTP callers.
Neither retries nor nested phases restart a watchdog.

`ExecutionContext` borrows that clock, absolute deadline and pinned cancellation
future. `guard` checks cancellation and expiry before and after each operation
poll. At the exact deadline no new effect is polled. Cancellation wins a tie;
completed cancellation and invalid/expired time are latched. Clock readings
earlier than the origin, or backwards between context observations, fail closed.
`wait` provides the same budget for Retry-After, polling and heartbeat delays:
exceeding the remaining budget expires at the deadline, never becomes an early
retry. Overflow is rejected rather than saturated into a successful wait.

Keep actual accepted-operation handles, response prefixes and local output
effects in caller-owned evidence before returning from an operation poll.
Rejecting a late result does not erase those facts. Cancellation and expiry
drop losing local futures; they do not prove remote cancellation, authorize a
mutation replay, or convert an accepted operation into a completed operation.

## Migrated and compatibility boundaries

Jobs `RunDeadline::guard` uses the common context. Existing `RunSession` phases
therefore share its pre/post-poll semantics through resolution, credentials,
submission, polling and output, without moving the original timeout boundary.

`NativeDeadline::with_clock` injects the same monotonic source across cloned
native observation/stream budgets. Expiry and invalid backwards time are shared
terminal facts across its clones. Existing `NativeDeadline::new` remains the
production compatibility boundary using `std::Instant` and a caller-supplied
alarm; it is not virtual-time evidence. Injected consumers must schedule their
alarms on `NativeDeadline::clock()`, not on an unrelated runtime clock.
The native Notebook driver's channel and sink supervision now does exactly
that; its `DeadlineAlarm` argument remains the legacy-constructor fallback.
Its stalled send/receive/sink, close and original-expiry tests advance virtual
time instead of waiting for system timers.

The separately owned HTTP engine/transport extraction and CLI command/output
ownership migration are coordinated follow-up seams. Until their exact released
commits are adopted, the legacy HTTP `ExecutionTimer` is sleep-only, native
streaming retains an external timer adapter, and CLI native
entrypoints retain host-specific `Instant`/Tokio supervision. These are explicit
remaining seams, not a universal clock-injection claim.

Opaque Azure credential implementations, OS broker/keystore and subprocess
internals, socket/TLS watchdogs and native managed runtimes are external
boundaries. A guard can drop their asynchronous local future and reject a late
poll result; it cannot preempt synchronous blocking code or certify an external
implementation's cleanup. Native host tests remain necessary for those adapters.

## Deterministic evidence

Core `tests/execution.rs` advances a caller-owned clock without wall sleeping:
exact expiry/ties, late and pending credentials, transport/body cleanup,
cancellation before/during/after a send, retained acceptance evidence, finite
Retry-After not-before waits, heartbeat rendering cutoff, backwards time and
unrepresentable durations. SDK `tests/native_deadline.rs` covers the actual
cloned native budget and unchanged duration policy. Existing independent
fixtures and compatibility gates are not modified.

Formatting and Cargo metadata planning do not establish compilation, test
registration or native behavior. Run the existing dependency impact plan and
its selected all-target/default/optional-feature checks under the integration
owner's current shared-host grant; record unrun checks explicitly.
