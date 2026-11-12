<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Exact Notebook Jupyter session GET

The opt-in `notebook-workload` feature mounts only the experimental
**individual-session GET** in `fab-cli-v2`:

```text
fab-cli-v2 get /Team.Workspace/Analysis.Notebook/sessions/jupyter-1 --output session.json
fab-cli-v2 artifact notebook get /Team.Workspace/Analysis.Notebook/sessions/jupyter-1 -o session.json
```

Both spellings use the existing `ParsedInvocation`, `fabric.runtime.get`
operation and version-0.1 `ReadReceipt`. The native SDK calls
`NotebookRuntime::get_session(executor, &SessionId, false)` once. The request
has no `detailed` query and never substitutes a kernel ID, Livy ID or display
name. The returned Jupyter session ID must exactly equal the explicit ID.
No new command-line flags, implicit active/latest lookup or name search is added.

The mount claims only `get` of a typed `.Notebook` individual-session path.
Notebook metadata, `src/content`, session collections, `list`, public Livy
history, foreign owners and other artifact facades remain with their existing
handlers. This branch does not import the producer's content command mount.
The default `fab-rs` executable and builds without `notebook-workload` retain
their existing dispatch. Exact-session help uses the selected path with
`--help` and reads no binding, configuration, identity or network.

## Binding and execution

Use the existing explicit native provider and the absolute, non-secret
`FAB_RUST_NOTEBOOK_BINDING` file described in
[the shared Notebook host](runtime-notebook-host.md#explicit-selection).
The typed human owner, account, tenant, catalog, workload origin and
capacity/workspace/Notebook IDs must match that binding. Construction is local.
The shared connector performs output preflight before credential construction;
it never searches for endpoints or restores an implicit session.

The released [native foundation](notebook-workload-foundation.md) verifies
the signed Fabric/PBI principal, confirms the exact selected cluster and
workload grants, and sends `Authorization: MwcToken` only to the scoped
Notebook route. A Fabric bearer is not sent to that route. The existing
Fabric `/v1` adapter and its guards are unchanged.

One original 30-second monotonic expiry covers local setup, credential
acquisition, signature/JWKS verification, confirming MWC exchange, session
response streaming, serialization, file/stdout delivery and diagnostics.
One latched Ctrl+C future is reused across phases. Runtime shutdown is bounded;
cancelled or blocked OS writes may have delivered a prefix and are not rolled
back. There is no synchronous diagnostic fallback, retry, remote cancellation
or replay. HTTP exchange POSTs only obtain authorization; no session, kernel
or compute lifecycle request is made.

## Data, errors and finite bounds

The receipt preserves the complete native session JSON, unknown provider
fields, error information and raw state. A successful GET of a failed session
is `read_complete`, not workload success or Spark readiness. Notebook workload
classification remains `unknown`, including for unfamiliar or structured states.
HTTP 202 is not read completion: the existing SDK rejects it as an invalid
individual-session representation. Malformed, mismatched and denied responses
remain explicit errors, with actual HTTP status when available.

Supported options remain the runtime library's JSON-only `--format json`,
`--output`/`-o` and positive `--max-bytes`. The default data ceiling is 1 MiB,
the independent wire ceiling is 4 MiB and the receipt ceiling is 2 MiB.
Over-budget responses are rejected, never clipped into successful output.
`--timeout`, projection, wait and recursive options remain explicitly unsupported.
No metadata/content fallback is used after a session error.

Files use create-new delivery and cannot overwrite an existing destination.
No-clobber is checked before authentication and atomically again at delivery.
Local delivery failure retains the completed receipt for explicit reconciliation;
that observation is not proof of output delivery and is not logged automatically.

## Offline evidence and remaining gates

`runtime_notebook_session` exercises both invocation spellings through the
real runtime parser, shared host, SDK, production headless credential, RS256
verifier, MWC exchange and scripted HTTP. It checks the exact route, methods,
headers, identity/scope context, raw failed/unknown states, response ID/shape,
202/denied/partial/oversized responses, exact data/receipt budgets, invalid
session syntax before credential dispatch, signed principal/audience denials,
no-clobber races and deadline/cancellation without replay. Process cases check
help/configuration admission with the feature both enabled and disabled.
The shared host suite supplies blocked-writer and partial-delivery evidence.

Registered nonzero tests, impact checks, strict all-target lints and full native
platform gates must apply to the actual submitted head. Source adoption and
older foundation/helper checks are not that evidence. These tests use only
synthetic keys, tokens and HTTP; no live authentication, Fabric access or compute
is authorized or claimed.
