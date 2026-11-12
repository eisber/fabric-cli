<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook Jupyter session collection list

The opt-in `fab-cli-v2` feature `notebook-workload` mounts Notebook session
collection listing. Its `runtime_v2::native_runtime_notebook_list` leaf constrains the
existing [runtime read head](runtime-v2-reads.md), parser and receipt engine.
It does not introduce another command engine or a credential/output stack.
Default/headless-only and v1 builds do not mount this route.
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
`v2_notebook_list_main` mounts that normalized operation before legacy routing,
using the same registered parser to identify its positional owner. Only exact
list collection paths and the two list-help spellings are claimed; options
cannot turn an output destination into a selected owner.

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

**Deployment qualification:** the released helper explicitly does
not qualify a live deployment. The real shared binding/connector is now adopted
from exact foundation commits `0ad30ed47a78f38f49ba88b7364cd0dd97fc1ce5`,
`be268b753e1e44bf96b52703e3d4ea56b2b188d4`,
`dabb1fadfdccd87319eb0d965bdf5d8e5c1123bc`, and connector-only commit
`7c243008ba0c3c2d3946497b9c2a56412dfede80`. No producer Notebook content
mount or peer session leaf is imported.

Build `fabric-cli` with `--features notebook-workload` and select the absolute
non-secret `FAB_RUST_NOTEBOOK_BINDING` file described in the
[shared binding schema](runtime-notebook-host.md#explicit-selection).
It binds exact human owner, environment, catalog/cluster/workload origins,
capacity/workspace/Notebook GUIDs and the selected native account/tenant.
Use the existing explicit `FAB_RUST_AUTH_MODE` provider configuration.
Missing or mismatched binding/identity fails before credential acquisition.
No endpoint/account discovery, alternate credential stack, static-token input,
Python or Azure CLI dependency is added.

`v2_notebook_connect::prepare` performs bounded local binding/target/output
preflight and constructs the existing native session with PBI opt-in only for
this command. The real `NativeNotebookExecutor` verifies signed principal
consistency, confirms the explicit catalog/cluster/workload/capacity selection,
then supplies `MwcToken` authorization to the exact SDK Jupyter route.
The existing production `NativeFabricExecutor` and Fabric `/v1` guards remain
unchanged. Returned service fields never replace the explicit binding.

One original 30-second monotonic expiry and one pinned Ctrl+C future cover
preflight, identity setup, token/JWKS work, bootstrap, workload streaming,
serialization and delivery. A signal-listener failure is local cancellation,
not permission to continue without interruption handling. Shutdown is bounded;
diagnostics use the shared owned stderr policy without blocking fallback.
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

`tests/native_runtime_notebook_list_wire.rs` drives the real parser, leaf, shared
host, SDK, native headless credential, RS256 verifier, MWC exchange and scripted
HTTP. It asserts exact scopes and six-request bootstrap/read sequence, the
bodyless GET without paging query, `MwcToken` rather than Fabric bearer,
canonical/facade equality, failed/unknown state fidelity, zero/singleton arrays,
malformed/202/206/403/partial/continued responses, exact record/data budgets,
stream caps, deadline/cancellation and binding/identity zero-send rejection.
Child-process fixtures also run that full real stack into unread stdout/stderr
pipes, asserting bounded nonzero shutdown, retained completed collection receipts
and no bootstrap/GET replay after timeout, cancellation or output failure.
The signed JWT and private key are existing synthetic offline test fixtures,
not credentials or live-service proof.

`tests/native_runtime_notebook_list_process.rs` invokes the actual executable
for help/version, missing/invalid binding, unsupported input and host ceilings,
preflight no-clobber before identity, and preserved ordinary get routing.
In a headless-only profile it asserts the Notebook route is not mounted.
Positive scripted HTTP is injected at the production stack's library seam;
no production test-only transport or secret bypass is added to the executable.

The impact graph includes all list test targets and an additive
`runtime-notebook-list-host` profile with `runtime-v2,native-host` on Windows,
Linux and macOS, plus `runtime-notebook-list-native` enabling the real
`notebook-workload` stack on each platform. Required features prevent default-mode
empty targets from being reported as feature acceptance. Actual new-head Rust
test execution and complete native platform gates remain required.
Source formatting and passing planner unit tests do not establish
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
Historical `c1dbc9abd55c7661b3ebdd7ea6134ec7392dd250` passed complete Windows
and Linux native checks; macOS was cancelled before runner acquisition, so its
stable gate failed. Those results do not qualify the later foundation,
connector, executable mount or new tests. No current-source pass is claimed
before its ordinary automatic CI finishes.
Shared-connector review identified an additional qualification blocker:
`NotebookBinding::read` checks path metadata before opening, so a substituted
symlink can race that check. This consumer adopts the sole shared owner's exact
two-file correction `7d1578cb33b1e19252b0879eee588cced40dc1d2`, preserving its
provenance and binding-source blob. Unix no-follow/nonblocking and Windows
open-reparse-point flags plus opened-handle validation reject a final symlink
substituted after inspection; the shared module includes a deterministic
replacement regression. The selected parent namespace still must be trusted
and stable, as documented by the shared host; no broader containment is claimed.
The source correction and new regression still require current-head native
execution and review before qualification.

Automatic CI at `a6d44c384a96d4b8fc3ef80df0ed162aee277bbd` exercised the real
stack, but exposed an incorrect new process fixture: `/src/content` is not
mounted by this list-only PR and is not the baseline metadata GET route.
The corrected isolation test uses the unchanged
`get /workspaces/11111111-1111-1111-1111-111111111111` contract, preserving
exit 1 / `NotConfigured` before identity. A separate assertion retains the
unmounted Notebook content exit 2 / `InvalidTarget` contract. No production
GET routing, existing fixture or frozen oracle is changed.
No live Fabric/auth read, provisioning, mutation or compute is authorized or claimed.
