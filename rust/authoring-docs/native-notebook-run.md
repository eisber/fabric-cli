<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook run request

`fabric-sdk-jobs::notebook::NotebookRunRequest` validates the native
whole-Notebook release request locally. This is separate from the unchanged
legacy `JobType::Notebook = RunNotebook` route and payload.

The SDK now exposes `notebook::execute(&mut RunSession)` over the existing shared
Run lifecycle. It submits exactly once and optionally observes the returned
scheduler instance. The native binding is
`POST /v1/workspaces/{workspaceId}/notebooks/{notebookId}/jobs/execute/instances?beta=false`.
The validated object is the entire body, not another `executionData` wrapper.
Missing input means zero body bytes; explicit `{}` is retained.

```json
{
  "parameters": [
    {"name": "day", "type": "Text", "value": "2026-11-05"},
    {"name": "batch", "type": "Integer", "value": 500},
    {"name": "enabled", "type": "Boolean", "value": true}
  ],
  "executionData": {
    "compute": "Spark",
    "computeConfiguration": {
      "driverCores": 4,
      "numExecutors": 1
    }
  }
}
```

Parameter arrays retain names, order and JSON types. Names are case-insensitively
unique and at most 256 characters. Text, Integer, Number and Boolean are not
coerced. DateTime requires a real UTC date in `YYYY-MM-DDTHH:mm:ssZ` form; Guid
requires the ASCII hexadecimal `8-4-4-4-12` form. Automatic, VariableReference
and future types are explicitly unavailable until their value semantics are
accepted. Null and unknown fields are not silently omitted.

Spark and Jupyter configuration fields are validated independently. Spark
cores, memory and executor int32/minimum constraints and Jupyter core enums
follow the endpoint-specific schema. DataWarehouse cannot contain
`computeConfiguration`. ById references require explicit workspace and item
GUIDs. Native storage references are inert ABFS/ABFSS paths, never downloaded by
the client. The pool's server-computed `maxClustersToHydrateLimit` is not a
client setting.

The accepted local storage profile rejects password/query-bearing URLs and
checks the documented Python/archive extensions, retaining archive fragments.
Unsupported representations fail explicitly instead of being rewritten.

Local validation does not establish the pool-dependent maximum executor count,
saved Notebook language, parameter-name agreement with code cells, resource
existence, capacity, authorization or downstream identity support. These remain
service checks; the SDK does not fetch definitions/pools or clamp values to
manufacture acceptance. High-concurrency tags can cause the service to reuse
or create compute and are not session identities or warm-variable guarantees.

The standard execution adapter rejects explicit high-concurrency options before
submission. The pure schema validator still describes the full published
request, including high-concurrency configuration, without installing that mode.
No route fallback or parameter conversion to legacy input is allowed.

## Opt-in native CLI

The `fabric-cli` feature `v2-run` routes the existing Run-r13 grammar to a
Notebook-specific native host when `native-host` is also enabled (`native-read`
selects the Windows broker host). Default builds retain the unconfigured Run
frontdoor. Root and facade share the same grammar and preparation:

```text
fab-rs run /<workspace-guid>.Workspace/<notebook-guid>.Notebook --input run.json --wait --timeout 5m
fab-rs artifact notebook run /<workspace-guid>.Workspace/<notebook-guid>.Notebook
```

Only absolute ID-qualified whole Notebooks are bound. Names, cells, borrowed
Livy sessions, UDFs, Beta exit verdicts and high-concurrency modes are not
implemented by this host. Input is at most 1 MiB of strict UTF-8 JSON with
duplicate keys rejected at every depth. Parameters and compute settings retain
native JSON types; unknown fields fail locally.

The executable requires explicit approval of the exact target and decoded input
on its controlling terminal, with an empty answer declining. Structured stdin
is never reused for approval. Noninteractive invocation fails with
`ApprovalRequired` before credentials or submission; no batch consent flag or
headless approval fallback is provided in this preview. A library consumer
injects its own execution policy, credential, transport, clock and interruption
signal rather than prompting inside the SDK.

The installed opt-in root and facade reuse `v2_run::run_command()` rather than
the caller-injected tabular frontdoor's export flags. `--format json` is the
default and only representation. `--output FILE` preflights a no-clobber
destination before approval/credentials and commits the same newline-terminated
JSON receipt once inside the original run budget; `--output -` uses stdout.
CSV/Parquet and legacy `--output_format` are rejected. Default builds and the
separate caller-injected tabular grammar remain unchanged.

Failures go to stderr without claiming successful file delivery. A racing
destination is not overwritten. A failed/interrupted output retains the known
remote handle, possible local output effects and any known partial-file evidence
in a best-effort diagnostic, never repeating submission.

One finite deadline covers input, approval, identity setup, submission and
observation. Defaults are 30 seconds for submission or 5 minutes with `--wait`;
positive integer `ms`, `s`, `m`, `h` values up to 24 hours override them.
Acceptance is not completion. A scoped scheduler handle is observed using the
shared scheduler policy and provider Retry-After minimum. A generic-operation
Location is retained as an operation, never relabeled a run; waiting for its
conversion to a scheduler run is explicitly unsupported.

Timeout and interruption stop local observation only. Unknown native states,
failed/cancelled/deduplicated jobs and local failures have nonzero exits.
Scheduler `Completed` is reported as `completed`, with
`applicationSuccessVerified: false`; Beta application exit-value checking remains
disabled. The receipt retains actual HTTP/handle/response-coverage evidence,
including headers observed before an interrupted body and explicit oversized-body
omission. No remote cancellation, session creation, deletion or automatic replay
is performed.

Source implementation and offline fixtures are not a live-service readiness
claim. A real trial needs a separately approved harmless Notebook definition,
exact target, selected identity/endpoint and compute/data effect boundary.
Tests must distinguish injected HTTP from an actual production invocation.

## Rebuild on the owner's Windows host

The reusable PowerShell 7 helper is independent of an agent's fixed supervision
deadline. It requires the exact committed Rust source, pinned Rust 1.99.1 MSVC
toolchain, installed MSVC x64 tools, restored dependencies and an exclusively
owned output root outside the checkout:

```powershell
pwsh -NoProfile -File .\scripts\Build-RunNotebookStudio.ps1 -ExpectedHead (git rev-parse HEAD) -OutputRoot Q:\my-run-notebook-build
```

An existing nonempty root must have its original matching ownership marker; do
not adopt someone else's cache. The helper uses offline locked Cargo, one job,
isolated cache/target/TEMP, the shared physical-host validation mutex and a fresh
90-minute maximum. An inherited `FAB_RUN_AGENT_DEADLINE` only shortens that bound;
owner terminals do not need an expired agent launcher. Missing dependencies fail
explicitly and require a normal-TLS restore, not automatic mutation/build retries.
Source changes during the build prevent publication. On success, the root's
`bin/fab-rs.exe` and provenance JSON identify actual host, source/tree, feature
set, profile, byte count and SHA256. A build does not authorize Notebook execution.

Contract source: [Notebook release run reference](https://learn.microsoft.com/en-us/rest/api/fabric/notebook/background-jobs/run-on-demand-notebook),
documentation revision `b4d803a1e7f495553fcb7ef509391016e676d91d`.
Legacy behavior remains pinned to the existing independent
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` job oracle.
