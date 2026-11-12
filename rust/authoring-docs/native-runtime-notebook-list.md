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

The impact graph includes both test targets and an additive
`runtime-notebook-list-host` profile with `runtime-v2,native-host` on Windows,
Linux and macOS. Required features prevent default-mode empty test targets from
being reported as feature acceptance. Actual Rust test execution, approved
Notebook scripted-HTTP transport and production executable process evidence
remain gates. Source formatting and passing planner unit tests do not establish
native acceptance. No live Fabric/auth read, provisioning, mutation or compute
is authorized or claimed.
