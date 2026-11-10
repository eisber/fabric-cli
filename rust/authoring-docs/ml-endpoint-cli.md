<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native ML endpoint commands

The additive `fab-rs artifact ml-endpoint` family operates on the exact owning
MLModel, using `--workspace-id GUID --model-id GUID`. These are item IDs, not
registry versions or a standalone endpoint. Existing Python and v1 commands
are unchanged. Native execution requires the existing `native-host` identity
and transport composition (`native-read` on Windows for silent broker reuse).
Default builds register arguments but return `NotConfigured` for valid service
requests. Invalid input fails before identity selection.

| Leaf | Selection and effects |
| --- | --- |
| `settings get` | Read the native endpoint settings object |
| `serving-version list` | Read **one** page; optionally pass `--continuation-token TOKEN` |
| `serving-version get --version NAME` | Read the exact opaque serving version |
| `serving-version get --default` | Read settings, then the exact configured default; no latest fallback |
| `settings patch --body JSON --force` | Change documented native default settings |
| `serving-version patch --version NAME --body JSON --force` | Change native scaling settings |
| `serving-version activate --version NAME --force` | Provision serving compute |
| `serving-version deactivate --version NAME --force` | Change serving availability |
| `deactivate-all --force` | Explicit model-scoped deactivation of every serving version |
| `score (--default \| --version NAME) --body JSON --force` | Execute potentially billable model inference |

Use `--wait` on activate, deactivate, deactivate-all and score to observe the
existing Fabric LRO. Without it, HTTP 202 returns `status: accepted`, not completed
activation or predictions. Waiting never replays a mutation. Cancellation and
timeout stop local observation, not remote work. Retain the operation ID and
uncertainty receipt; do not blindly repeat an uncertain mutation.

`--body` is inline native JSON, limited to 1 MiB. Request types reject unknown
fields, invalid finite enums and wrong shapes. Settings patch accepts
`defaultVersionAssignmentBehavior` and/or `defaultVersionName`. Version patch
requires `scaleRule` (`AlwaysOn` or `AllowScaleToZero`). Score requires an `inputs`
array; optional `formatType` and `orientation` retain the SDK contract. There is
no automatic model upload, training, activation, schema conversion or input-file
reader. Approval is explicit `--force`; no unattended yes or UI prompt is inferred.

Output is JSON (`--output_format json`; text is rejected locally). A completed
response contains `status`, actual `httpStatus` and the unflattened native `data`.
Unknown response fields and enum strings survive known-field shape validation.
List retains continuation fields and never follows a provider-supplied URL.
Pass the opaque token explicitly for another bounded page. A page is not an
exhaustive-list claim. The shared executor bounds requests, responses, retry
delays and LRO links. `--timeout` bounds the complete native operation to 1-3600
seconds (default 240), including authentication and polling.
Native JSON publication uses owned output handles and the same absolute deadline.
On blocked/failed output, the host stops rather than reusing a possibly active
writer or replaying a remote operation. A best-effort diagnostic has at most one
additional second of cleanup; runtime shutdown is also bounded to one second.
JSON Unicode escapes preserve native values while allowing safe ASCII output
through owned Windows console handles.

Set `FAB_API_ENDPOINT_FABRIC=msitapi.fabric.microsoft.com` explicitly for MSIT.
The host remains paired with the Fabric audience
`https://api.fabric.microsoft.com/.default`; it does not select another identity,
fallback to production, accept tokens, or launch Python/Azure CLI. Reuse an
existing supported saved binding or explicitly configure its same non-secret
selectors as documented in [native host setup](native-read-host.md).

Contract provenance is the common source
`701620ba30b0a1309a33ef27998315fff54643f2` and the official
[Fabric MLModel Endpoint v1 reference](https://learn.microsoft.com/en-us/rest/api/fabric/mlmodel/endpoint)
(read definitions reviewed at docs commit
`b4d803a1e7f495553fcb7ef509391016e676d91d`). In particular, default-version GET
resolves settings instead of inventing a `versions/default` endpoint.
Build and live acceptance are separate from these documented contracts; mutating
live tests require separate effect approval.
