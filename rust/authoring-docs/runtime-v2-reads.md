<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental runtime v2 read head (H1)

This is an additive **library** consumer of the composed native SDK at
`c52a842e082a377e5fc2a179451f07ff521305df`. It executes real SDK reads through an
injected `NativeExecutor`; preparing/printing a request is never remote success.
It does not replace the Python CLI, Rust v1 parser, or existing executable.
It is one incremental H1 slice, not implementation of all 28 grouped commands.

## Selected contracts and source

The selected grouped proposal is Commands
`42ad36bb-98af-4a79-97f1-d741be0e56ed` r30, common get
`f5254c3c-4efc-4fa1-99c3-698968c32887` r16 and list
`d4f58616-f262-4845-a2ca-86adc9dbb905` r12. Notebook facade get
`a8724d31-2637-4543-b683-2464dd430690` r7 and list
`6f2e00e5-6814-4e3a-8cb5-2136260d5716` r4 constrain the same operations.
The source/runtime facet additions below remain **experimental grammar**,
as identified in the complete FNTK and Livy source crosswalks, not an assertion
that every proposed path/backend/parameter has been approved or implemented.
Private specification bodies are not copied into the repository.

Implementation sources:

- FNTK `e250d5d32b04efcc5623a414a361727a93c271ce`, native Notebook content
  and Jupyter session APIs; see [Notebook SDK](notebook-runtime-native.md).
- Livy JSON `10b9db3a20ca1971f8d8a5fff981ff65260b022d`, classic native/Fabric
  session reads and existing statement JSON; see [Livy SDK](livy-native.md).
  The empty target revision of that source PR is not its implementation pin.

## Grammar and operation registration

```text
get <path> [supported options]
list <path> [supported options]
artifact notebook get <path> [same supported options]
artifact notebook list <path> [same supported options]
```

`OPERATIONS` is the single registration table:
`fabric.runtime.get` and `fabric.runtime.list`. Its descriptors generate both
clap spellings and expose `input_schema()` for the **normalized typed input**.
There is one `ParsedInvocation`, one `RuntimeHead`, and one `ReadReceipt`.
There are no per-artifact engines or duplicate MCP registrations. The facade
rejects a non-Notebook typed path before resolution. The head independently
rejects a resolver binding with the wrong owner type before native execution.

Paths are logical resource paths, not Windows file paths or URLs:

```text
/Team.Workspace/Analysis.Notebook/src/content
/Team.Workspace/Analysis.Notebook/sessions
/Team.Workspace/Analysis.Notebook/sessions/<Jupyter-session-id>
/Team.Workspace/Data.Lakehouse/sessions
/Team.Workspace/Data.Lakehouse/sessions/<classic-Livy-id>
/Team.Workspace/Data.Lakehouse/sessions/<classic-Livy-id>/statements/<statement-id>
/Team.Workspace/Data.Lakehouse/sessions/<classic-Livy-id>/statements/<statement-id>/result
/Team.Workspace/Native.LivyService/sessions[/<classic-Livy-id>]
```

The statement paths also apply to `.LivyService`. A `.LivyService` binding
must use the native profile. A Lakehouse's caller-supplied service may explicitly
select the native or Fabric profile; no profile is discovered or retried.
Workspace/artifact suffixes are required; human names are not backend IDs.
The injected resolver alone maps the selected owner to a typed SDK scope.
Session IDs are explicit native IDs validated by the SDK, not display-name
searches or inferred active/newest sessions. Statement IDs are nonnegative
signed-64-bit wire integers. The head validates returned individual IDs.

Paths are at most 4096 UTF-8 bytes, each segment at most 1024. Empty segments,
dot segments, backslashes, escapes, query/fragment delimiters and controls fail.
No relative context, glob, implicit context restoration, URL input or batch.
`sessions/active`, public `livy-history`, HC/REPL ownership, statement lists,
cell selectors, docs, definitions, resources, logs and other facets are not
silently routed to a different engine.

| Operation / facet | Actual SDK consumer | Meaning |
|---|---|---|
| get Notebook `src/content` | `NotebookRuntime::get_content` | Full native NBS IPYNB; unknown fields, source line-array/string form and saved outputs preserved |
| list Notebook `sessions` | `NotebookRuntime::list_sessions(executor, false)` | Live Jupyter collection, not public session history |
| get Notebook `sessions/id` | `NotebookRuntime::get_session(executor, &SessionId, false)` | Exact Jupyter session, no readiness claim |
| list Livy `sessions` | `LivyService::list_sessions(ActivityPage)` then `LivyRequest::execute` | One native `size` or Fabric `$top` page |
| get Livy `sessions/id` | `LivyService::session(id)?.get()?.execute(...)` | Exact classic session and separate workload observation |
| get existing statement[/result] | `session.statements().statement(id)?.get(BytePage::default())?.execute(...)` | Same complete statement JSON envelope; no output extraction, cursor traversal or dataframe conversion |

`get /Team.Workspace/Analysis.Notebook` is explicitly
`UnsupportedCapability` in H1. There is no bound metadata adapter and **no
implicit content fetch**. Native item metadata, item multipart definitions and
NBS IPYNB content are not interchangeable. `result` does not mean all retained
byte pages were traversed or the original query's rows were fully recovered.

## Parameters and finite bounds

| Parameter | H1 support |
|---|---|
| `<path>` | One explicit typed resource/representation or session collection |
| `--format json` | JSON receipt only; omitted means json. This never changes the native representation |
| `--output FILE`, get `-o FILE` | Destination passed only to an explicit caller sink; default `-`. No filesystem implementation, overwrite, automatic filename or temporary spill |
| `--max-bytes N` | Positive serialized **data** bytes; default 1048576; checked before delivery, excluding receipt metadata |
| list `--limit N` | Positive whole-record limit; default 100. Native Livy `size` / Fabric `$top`; no server paging parameter invented for Jupyter |
| `--continuation` | Recognized for list, explicitly unsupported: no opaque resume token is fabricated |
| `--timeout` | Explicitly unsupported in this head: no timer installed. The injected executor must enforce the host's deadline/cancellation policy |
| `--profile`, `--project` (get `-q`) | Explicitly unsupported: identity is injected, projection is not implemented |
| get `--wait`, `--recursive`, `--depth` | Explicitly unsupported before resolution/I/O; no poller, traversal or success-shaped stub |

The generic list page does not advertise `-o`; H1 accepts it only on get.
Other options, including unadvertised `--all`, type/filter and recursion flags,
fail parsing. Candidate 30-second timeout semantics are **not** claimed:
explicit timeout input fails rather than being ignored, and this library does
not promise to interrupt a hung executor. The standalone host still needs a
reviewed absolute-deadline implementation.

`ReadOptions` supplies independent finite wire-response, selectable data,
complete receipt and record ceilings. Defaults are 4 MiB wire, 1 MiB data,
2 MiB complete receipt and 1000 maximum selectable records. Zero ceilings fail
at injection. Caller selections above host ceilings fail before resolution.
The SDK retains its own independent Notebook content/cell/output/session bounds.
A shared executor decorator forwards the smaller response bound and rejects a
noncompliant over-budget response. The underlying host must still bound reads
while streaming; post-response checking is not a process-heap or stream cap.

Data is measured by borrowed bounded serialization before output; the complete
receipt is separately serialized into a bounded buffer. Collections never
auto-unwrap. There is no clipped artifact, discarded row or fabricated resume:
an over-budget page is `BudgetExceeded` with no successful output. This strict
all-or-error subset does not implement partial/truncated page delivery. Livy
coverage is `provider_page`, with supplied provider total and null continuation,
never complete history. Notebook SDK rejects advertised continuation rather than
returning an apparently complete empty collection.

## Injection and receipts

The consumer calls:

```text
parse_args(argv including program name)
RuntimeHead::inject(&native_executor, &runtime_resolver, finite_options)
head.run(&parsed_invocation, &mut explicit_output_sink).await
```

`RuntimeResolver::resolve(&TypedPath)` returns `ResolvedTarget::Notebook`,
`Lakehouse`, or `LivyService` containing actual SDK objects. It uses
caller-supplied bindings/snapshots; no resolver implementation discovers
capacity/workspace/endpoints, credentials, audience, tenant or auth metadata.
The host must authorize the executor for that exact selected scope.

`OutputSink::check` rejects unavailable/unsupported/existing destinations before
HTTP without creating output. `write` must enforce no-clobber atomically at
delivery too, and receives one complete bounded JSON receipt plus newline.
`-` is only a destination label, not implicit access to process stdout.
The library opens no files, creates no directories, renders no binary,
fetches no external media and invokes no model.

The public serde receipt has `schemaVersion: "0.1"`, canonical `operationId`,
typed human/resolved native `target`, `effect: "read"`, `status`, bounded `data`
and provenance/coverage in `meta`. `nativeUrl` is actual post-read provenance,
not data representing a request plan. Provider/source content is untrusted and
can be sensitive; receipts must not be automatically logged.

`read_complete` means that one read returned valid bounded data.
`accepted` retains actual HTTP 202 and is not read completion, readiness or
workload success. Notebook convenience methods that reject a 202 representation
remain explicit errors; H1 does not invent an LRO fallback.
Livy `observe()` results are separate `workloadObservation` metadata. GET of a
failed session/statement still succeeds as a read, with raw `state`, error/output
and unknown fields retained. Notebook states remain raw with unknown workload
classification; neither SDK nor head invents Spark readiness from Jupyter GET.
Persisted Notebook outputs are never labeled live execution results.

Native errors retain real HTTP status when available and never print provider
bodies, source/code or output error details. Transport timeout/cancellation
never issues remote cancellation or replay. `Delivery` retains the completed
receipt and actual target after a failed/partial sink write. That receipt is
remote observation evidence, **not** proof output was delivered. There is one
HTTP dispatch and one output attempt, no automatic retry/replay and no
cross-process exactly-once delivery promise.

## Historical H1 integration gates and validation

This lane exclusively adds `src/runtime_v2/**`, `tests/runtime_v2.rs` and this
document. The coordinator must add the optional manifest feature
`runtime-v2 = []` and the feature-gated library export:

```rust,ignore
#[cfg(feature = "runtime-v2")]
pub mod runtime_v2;
```

The integration test crate has 30 test functions, starts with
`#![cfg(feature = "runtime-v2")]` and denies `unnameable_test_items`.
Tests use the actual parser and SDK plus a
single-response fake implementing `NativeExecutor`, not a mock replacement
command engine or a production network. They cover content fidelity,
canonical/facade equality, Jupyter/native/Fabric reads, failed/unknown states,
existing result JSON, accepted responses, owner/ID/path/type errors, budgets,
unsupported options, no-clobber preflight and partial sink-delivery failure.

**Actually checked in this lane:** rustfmt parsing/formatting of owned Rust
sources and source/ownership/whitespace checks. Initial lightweight rustc
probes were blocked by stale cached SDK artifacts lacking `livy`. A subsequent
metadata-only compile of the **exact base SDK source**, reusing existing external
dependencies, succeeded. The H1 module and feature-enabled integration test
source then passed metadata-only type checks and direct Clippy-driver checks
with missing-docs/unsafe-code denied and `clippy::all`/warnings denied. The
isolated wrapper exposes only this module; it is not the existing CLI crate.
These checks performed no code generation, linking, dependency build or install.

**Not run:** Cargo check/test/clippy, Rust test execution, the actual composed
CLI all-target check or combined/native validation. No heavy build slot or
manifest/export changes were authorized. Source-ready/type-checked tests are
not passing runtime tests. After adding the feature/export, the coordinator's
serialized validator must run the focused `fabric-cli` runtime-v2 test target,
all-target checks/lints and affected consumers, then independent reviews.

At the original H1 checkpoint there was **no standalone v2 HTTP/auth/output
host or executable dispatch**.
The default CLI is untouched. Caller-injected fakes prove neither deployed
Notebook/Livy qualification nor real executable remote completion. HTTP
authorization/redirect/deadline/streaming behavior, actual metadata-backed
resolver, atomic no-clobber file sink, terminal stream policy, native platform
acceptance and credential backend remain explicit host gaps. No WS/network
streaming, credential/settings discovery, live/auth operation, PR, push or
merge is part of this slice.

## Shared native host composition

With `runtime-v2,native-host`, `runtime_v2::host` adds an explicit composition
seam, not a second SDK engine:

- `ExplicitResolver::new(OwnerPath, ResolvedTarget)` binds one exact typed human
  owner to the caller's existing SDK object. Foreign owners fail before execution.
- `prepare_destination(label, cwd, deadline, cancelled)` checks output on an
  owned worker, without creating files. `Destination::check` is the synchronous
  equivalent for callers already on a blocking worker.
- `acquire(invocation, executor, resolver, options, destination, deadline,
  cancelled)` calls the existing `RuntimeHead` and SDK, retaining one complete
  bounded receipt. `PendingRead::deliver(destination, deadline, cancelled)`
  consumes the observation once. It atomically uses `create_new` for files.
- The caller supplies **one original `Instant` deadline**, including preflight,
  credential acquisition, HTTP streaming, serialization and delivery. Pin one
  cancellation future and borrow it across phases; do not reinstall a signal
  listener or restart the timeout between phases. Cancellation is local only.

Delivery reuses the native host's owned-handle worker policy. Workers do not
hold Rust's global stdout/stderr locks. Raw UTF-8 output to a Windows console is
rejected by that existing policy; redirect stdout or select a file. Partial
delivery and abandoned workers are not rolled back or replayed; byte counts are
snapshots, not a promise that an abandoned OS write has stopped. Front ends must
bound runtime shutdown. Small ASCII diagnostics use owned stderr and the original
deadline, with no synchronous fallback when it is blocked or time has expired.
`HostError::receipt` retains the full completed observation on failed delivery,
including an output-path race. Its `Debug`, `Display` and diagnostic output
never print the retained source/output/URL. Only the caller may explicitly
inspect that typed evidence; it does not authorize replay.
An accepted-only receipt retains HTTP 202 and its `accepted` status, while the
host diagnostic's `readCompleted` remains false even if output delivery fails.

Executable Fabric-profile child modules can reuse the existing
`v2_main::capture_fabric_selection()` and `NativeSession::new(...)` parent
composition. Ordinary GET consumes the same capture helper, so the existing
auth mode, selected account/tenant and Fabric-host knobs have one unchanged
implementation. No new configuration framework or credential provider is added.
Construct `NativeFabricExecutor` from that exact session context and HTTP
executor; the SDK scope must use the same explicitly selected endpoint.
This supports the existing approved Fabric-profile boundary, not MWC.

The host **does not authorize Notebook MWC execution**. The production
`NativeFabricExecutor` still accepts only its exact Fabric `/v1` binding.
The current endpoint/audience primitives do not establish an approved Notebook
workload origin/audience/identity binding. In particular, never substitute a
Fabric token for a `pbidedicated.windows.net` endpoint or relax the adapter's
guard. Enabling production Notebook reads requires an authoritative workload
authorization contract and a separately reviewed, explicitly selected adapter.
No configuration, token acquisition or endpoint discovery is installed here.

`runtime_native_host` tests the real parser, shared host and SDK. Notebook
fidelity uses a scripted `NativeExecutor` and is **not** production MWC proof.
A separate supported Fabric-profile test injects an Azure credential and HTTP
client into the actual `RequestContext` / `NativeFabricExecutor` / `HttpExecutor`
stack; negative Notebook tests verify that stack refuses before requesting a
token or HTTP. This evidence must not be relabeled as native Notebook acceptance.

The opt-in `fab-cli-v2` build (`headless,runtime-v2`) now recognizes precisely
Notebook `get .../src/content` and its `artifact notebook get` equivalent.
Until that production prerequisite is supplied, both return nonzero
`NotebookWorkloadBindingRequired` before reading authentication configuration,
constructing credentials or making HTTP requests. This is a truthful
capability boundary, **not an implemented production Notebook read**.
Unsupported flags and excessive caller budgets remain argument errors.
Other owners/facets and update facades retain their existing dispatch.
The host does not mount the five session/statement sibling leaves.

The available selected specification index still references Commands r30,
consistent with the pins above; no newer private specification body was
available through the current tools. No private specification is copied here.
