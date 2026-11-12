<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook Jupyter session collection list

The `runtime-v2` leaf `runtime_v2::native_runtime_notebook_list` constrains the
existing [runtime read head](runtime-v2-reads.md), parser and receipt engine.
It does not introduce another command engine or a credential/output stack.
The [native Notebook SDK](notebook-runtime-native.md) source remains FNTK
`e250d5d32b04efcc5623a414a361727a93c271ce`. The integration baseline for this
leaf is `ced983722015e440e989f07d1bf866821d27fe90`.

## Grammar and ownership

```text
list /Team.Workspace/Analysis.Notebook/sessions [OPTIONS]
artifact notebook list /Team.Workspace/Analysis.Notebook/sessions [OPTIONS]
```

Both spellings normalize through `runtime_v2::parse_args` to
`fabric.runtime.list`. `claims(&ParsedInvocation)` matches **only** the list
operation, Notebook owner and session collection facet. `run` rejects foreign
invocations before resolution, output preflight or native execution, then
delegates to the same `RuntimeHead::run`.

Get, Notebook content, exact sessions, artifact metadata, Lakehouse/Livy
collections and statements are not claimed. Neither spelling selects public
Livy history, `/sessions/active`, a cached/newest session or Spark readiness.
No session creation, stopping, kernel control or compute is performed.

The existing experimental H1 grammar and options are unchanged:
`--format json`, `--output FILE|-`, `--max-bytes N` and `--limit N`.
List does not accept `-o`. Continuation, timeout, profile, projection and
unadvertised options fail before I/O rather than being ignored.

## Native route and complete-page semantics

The real SDK call is `NotebookRuntime::list_sessions(executor, false)`.
The exact request is one bodyless GET with no invented paging query:

```text
{explicit-workload-origin}/webapi/capacities/{capacity}/workloads/Notebook
/Data/Automatic/api/workspaces/{workspace}/artifacts/{notebook}
/jupyterApi/versions/1/api/sessions
```

The native host must supply an actual explicit immutable workload
origin/capacity/workspace/artifact/account/audience binding and authorized
executor. A public Fabric `/v1` adapter or a fallback token is not that binding.
This leaf does not discover endpoints, resolve identity or authorize HTTP.
Missing binding is an explicit prerequisite, not an empty collection.

Data is always an array, including zero and one records. Full provider records,
unknown fields, null/missing distinctions, raw failed and unknown states are
retained. A successful read does not classify collection rows as ready or
successful workloads. Coverage is `returned_collection`, not historical
completeness. The SDK rejects advertised continuation; no token is fabricated.

`--limit` is a whole-record collection bound, **not** server-side Jupyter paging.
An over-record or over-byte collection fails in full without clipping rows,
unwrapping a singleton, projecting away provider fields or writing a partial
success receipt. Wire, data and complete receipt bounds remain independent.
Malformed rows, HTTP 202/206, denied access and unavailable service do not
become successful empty collections. Native errors preserve observed HTTP
status and sanitized diagnostics. Local cancellation/timeout or delivery
failure never retries HTTP or replays output.

## Composition and validation boundary

The leaf uses the exact released shared helper
`37483c7f63ca2ab86894fb2f7b0777f1601368ce`, consumed with its cherry-pick
provenance intact. It retains caller-injected `RuntimeHead`, `RuntimeResolver`,
`ReadOptions` and `OutputSink`. With `runtime-v2,native-host`, leaf `acquire`
first rejects foreign invocations and then delegates to `runtime_v2::host`.
The shared host supplies original-deadline/cancellation handling and atomic
no-clobber output; no private replacement host is installed by this leaf.
Pin one cancellation future across preflight, acquisition and delivery, and
bound runtime shutdown as required by the shared host.

The corrected helper source is adopted through two exact owner-released packets:

| Packet | Bytes | SHA256 |
| --- | ---: | --- |
| Shared helper correction through `67e16216017cf85560d1f30234a015335c6764dd` | 40055 | `0b5fba40419ff9bfbee0de050527794273406309b00d686e3c766279416181fc` |
| Legacy-output compatibility fix through `e88526a1a0feb38b882b687783b28c6d189bf3cf` | 5191 | `8e938602a6df32547d813d1755bd38e7f2a907a304faed317fd4467c0acf66a6` |

Both packets were hash-verified and sequence-checked in an isolated index before
application. The first is three shared core files plus the original
`c4d6a8396ac09cf45beaa26e11455df97d315e45` shared identity-capture extraction
in `v2_main.rs`. It excludes the producer's Notebook content root hook and leaf.
The second preserves legacy `write_owned` admission/detachment. The subsequent
exact two-file helper correction
`674bed1f205c71a4f7aec0df15b9862de1e66954` removes the unused stoppable wrapper:
legacy `write_owned` calls the same `write_owned_with_policy` engine with `None`,
while runtime setup/delivery explicitly pass `Some(stop_latch)`. Both adopted
files match that helper commit's blobs exactly. Completed observations
remain in `HostError::receipt` after delivery failure, with sanitized diagnostics;
an already-ready cancellation does not admit a runtime output worker.
These source identities do not qualify the combined consumer executable.

**Remaining executable prerequisite:** the released helper explicitly does
not authorize Notebook workload MWC HTTP. An authoritative explicit immutable
Notebook workload origin/capacity/workspace/artifact/account/audience binding
and reviewed authorized adapter are still needed from the shared host owner.
The existing production `NativeFabricExecutor` only accepts its Fabric `/v1`
binding; it must not be widened or supplied fallback tokens. Executable
registration/help and successful Notebook HTTP/process acceptance must wait
for that contract. A caller-injected script is not a production binding.
Default/v1 behavior remains unchanged; native composition is explicitly opt-in.

`tests/native_runtime_notebook_list.rs` exercises the real shared parser,
leaf, runtime head and Notebook SDK with one scripted `NativeExecutor`.
It covers exact GET/no query, canonical/facade equality, unknown/failed states,
empty/singleton arrays, malformed/accepted/partial/denied/continued responses,
whole-page record and exact data-byte bounds, independent response/receipt caps,
foreign owners, missing binding, invalid input, preflight failure, cancellation,
timeout and partial delivery without replay.

`tests/native_runtime_notebook_list_host.rs` additionally composes the leaf with
the released host, checking real create-new file delivery and its no-clobber
race, original deadlines/cancellation, wrong bindings and bounded child
processes with unread stdout/stderr pipes. The child process is an isolated
test fixture, **not** the production executable. Negative tests use the actual
SDK credential/HTTP adapter and prove that Notebook workload routes and wrong
audiences cannot access credentials or send HTTP through the Fabric adapter.
They do not invent a successful Notebook HTTP binding.
Leaf regressions also assert retained collection receipts on no-clobber failure
and already-cancelled delivery without file creation, provider-data diagnostics
or HTTP replay.

The impact graph includes both test targets and an additive
`runtime-notebook-list-host` profile with `runtime-v2,native-host` on Windows,
Linux and macOS. Required features prevent default-mode empty test targets from
being reported as feature acceptance. Actual Rust test execution, approved
Notebook scripted-HTTP transport and production executable process evidence
remain gates. Source formatting and passing planner unit tests do not establish
native acceptance. Historical automatic CI at
`7afd6c9b9f4925b2d9a01a87c6eb606b18170163` executed the 10-test collection suite
and 6-test host suite successfully on Windows, Linux and macOS, but each overall
native job failed the then-current shared `runtime_native_host` fixture.
Those passes are not evidence for the corrected helper or the added regression.
The old intermediate helper also failed a frozen legacy-output cancellation
test in its own hosted validation; the second packet addresses that regression
without changing the frozen test. New-head automatic CI must qualify this
combined source. No further local heavy allocation, budget reset or manual CI
replay is authorized. Independent read-only adoption review flagged an unused
private stoppable entry point in the legacy executable's separate
`native_read::output` module copy. Automatic Linux CI at consumer head
`2aa684e30fcdfb60bd071b3dc279f3b6c0f8015b` confirmed that `-D dead-code` failure
in `fab-rs`. The shared owner released `674bed1` to remove
that entry point without changing the policy engine or weakening lints.
This source correction still needs new-head native/Clippy qualification.
No live Fabric/auth read, provisioning, mutation or compute is authorized or claimed.
