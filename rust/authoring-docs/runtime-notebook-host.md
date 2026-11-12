<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit native Notebook read host

The opt-in `fab-cli-v2` features `headless,runtime-v2,notebook-workload`
compose the existing runtime parser, shared host, Notebook SDK and
`NativeNotebookExecutor`. They do not enable Notebook writes, compute,
discovery or the separately owned session leaves.

```text
fab-cli-v2 get /Team.Workspace/Analysis.Notebook/src/content --output notebook.json
fab-cli-v2 artifact notebook get /Team.Workspace/Analysis.Notebook/src/content -o notebook.json
```

Both spellings return the same bounded JSON receipt. Its `data` is the complete
native IPYNB JSON value: unknown fields, string/line-array source and saved
outputs are retained. The receipt is not a bare byte-identical `.ipynb` export:
JSON formatting may change, and saved outputs are not live execution results.
There is no item metadata or multipart-definition fallback.

## Explicit selection

Keep the existing `FAB_RUST_AUTH_MODE` provider configuration and
`FAB_RUST_FABRIC_HOST` selection (public Fabric remains its existing default).
For this Notebook command only, the same native credential is explicitly opted
into PBI bootstrap. Ordinary Fabric, OneLake and other command behavior is
unchanged. No login, identity fallback or static token configuration is added.

Set `FAB_RUST_NOTEBOOK_BINDING` to the absolute path of one non-secret JSON
file, at most 16 KiB, with exactly this schema:

```json
{
  "schemaVersion": 1,
  "owner": {
    "workspace": "Team.Workspace",
    "artifact": "Analysis.Notebook",
    "kind": "notebook"
  },
  "environment": "production",
  "catalogOrigin": "https://api.fabric.microsoft.com",
  "clusterOrigin": "https://selected-cluster.analysis.windows.net",
  "workloadOrigin": "https://selected-notebook.fabric.microsoft.com",
  "capacityId": "44444444-4444-4444-4444-444444444444",
  "workspaceId": "55555555-5555-5555-5555-555555555555",
  "notebookId": "66666666-6666-6666-6666-666666666666",
  "account": "22222222-2222-2222-2222-222222222222",
  "tenant": "11111111-1111-1111-1111-111111111111"
}
```

These are synthetic placeholders, not usable service configuration. Supply
previously known, authorized origins and IDs; this host does not discover them.
Unknown fields, token/secret fields, relative paths, directories, symlinks,
oversized files and malformed bindings fail before identity or HTTP.
The reader opens the final component with Unix no-follow/nonblocking flags or
Windows open-reparse-point flags, then validates the opened handle before
reading. A final symlink substituted after metadata inspection is rejected.
The selected parent directory namespace must be trusted and stable; this
boundary does not claim containment against hostile ancestor replacement or
hard-link/in-place changes by another writer with access to that namespace.
The typed human owner must match exactly. IDs must be canonical nonzero GUIDs.
The account and tenant must exactly match the existing selected native identity.
The catalog must match both the existing Fabric endpoint and selected
environment (`production`, `msit`, `daily` or `dxt`).

The [foundation](notebook-workload-foundation.md) enforces the supported signed
principal and non-production account policy. Unsupported identity formats or
system-assigned opaque account selectors fail, rather than borrowing a different
identity. Returned cluster/workload/capacity values only confirm the explicit
selection; they never replace it. Fabric's `/v1` executor and its guards are
unchanged. Workload requests use `MwcToken`, never a Fabric bearer.

## Shared connector API and deadlines

`runtime_v2::notebook::NotebookBinding::read` and `parse` validate local
configuration. `check_identity` rejects account/tenant/audience/catalog mismatch
before credential construction. `connect(context, http, options, original_expiry)`
returns a `NotebookConnection` containing the actual `NativeNotebookExecutor`
and `ExplicitResolver` over the existing SDK. Construction performs no network.

The executable connector in `v2_notebook_connect.rs` shares these APIs with
Notebook read leaves. It performs local binding, output preflight, existing
identity capture and PBI-opted session construction through the shared bounded
`setup` worker. It is not a second leaf/protocol/authentication engine.
Do not fork its policy in session-list/get adapters.

One original 30-second monotonic expiry covers local setup, credential
acquisition, signature/JWKS verification, confirming bootstrap, workload body,
receipt serialization and output delivery. `NativeDeadline::until` retains
that exact expiry; it does not start a new timeout from a measured remainder.
One pinned Ctrl+C future is borrowed across all phases. Handler failures,
deadline and cancellation are explicit nonzero local outcomes, with no
remote cancellation or replay.

Output uses the existing owned-stream policy and atomic create-new file commit,
including no-clobber preflight before credentials and a second atomic check at
delivery. Partial/abandoned delivery is not rolled back; local errors retain
typed observation evidence without logging its payload. Runtime shutdown is
bounded. Diagnostics use owned bounded stderr, never a synchronous fallback
after a blocked or expired writer.

## Qualification

The foundation is adopted from the exact published source chain
`0ad30ed47a78f38f49ba88b7364cd0dd97fc1ce5`,
`be268b753e1e44bf96b52703e3d4ea56b2b188d4`,
`dabb1fadfdccd87319eb0d965bdf5d8e5c1123bc`. Source adoption is not a native
qualification receipt or permission to contact a live service.

`runtime_notebook_native` drives the real parser, shared host and SDK through
the production headless credential, RS256 verifier, MWC adapter and scripted
HTTP. It checks full JSON fidelity, exact routes/scopes, rejected bindings,
malformed/denied/partial/oversized responses and original deadline/cancellation.
Actual registered nonzero tests, strict all-feature/all-target lints and
current-head platform/full gates remain required; source-only checks cannot
replace them. No live authentication, Fabric, provisioning or compute is
authorized by this host or its offline tests.
