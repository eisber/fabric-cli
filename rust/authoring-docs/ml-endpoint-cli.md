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
| `score (--default \| --version NAME) (--body JSON \| --input FILE \| -- key=value) --force` | Execute potentially billable model inference |

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
no automatic model upload, training or activation. Approval is explicit `--force`;
no unattended yes or UI prompt is inferred.

## Scoring inputs

Choose exactly one input mode. These interfaces intentionally extend the native
studio; they do not change Python or the native SDK's omission semantics.

| Mode | Contract |
| --- | --- |
| `--body JSON` | Raw native object. `inputs` remains an arbitrary JSON array, including zero, false, null and nested data. No model/tensor compatibility is inferred. |
| `--input FILE --input-format json` | UTF-8 array of record objects, not a native request envelope. JSON is the decoder default. |
| `--input - --input-format json` | The same record array from one bounded stdin read. Do not reuse stdin for confirmation. |
| `--input FILE --input-format csv` | Unique header and JSON-valued cells inside normal RFC 4180 CSV quoting. No type/null guessing. |
| `--input FILE --input-format parquet` | Primitive typed Parquet, using the existing shared Arrow/Parquet dependencies. Unsupported codecs, decimals, timestamps, binary, nested/logical annotations and metadata conversions fail locally. |
| `-- x=0 flag=false nothing=null text=hello` | One schema-aware record. Values are JSON, except unquoted text for signature string columns is literal text, never evaluated. Use a JSON string to distinguish literal `"null"` from null. |

`--format-type dataframe` is a native representation selector, **not**
`--input-format` or `--output_format`. `--orientation` accepts exactly
`split|values|record|index|table` (`record` is singular). Raw mode leaves omitted
selectors omitted. Flags may add absent fields but conflict with fields already
present in the body, even if equal; they never override native JSON.

File/stdin/record conveniences explicitly default to `dataframe` plus `record`.
`--orientation values` constructs ordered row arrays in signature order.
The other native orientations are available with `--body`; their service-side
structures are not guessed from a local table. Convenience modes require a
nonempty record array, with at most 1 MiB encoded/decoded input, 10,000 rows,
1,024 columns and 100,000 cells. Encoded native requests are also capped at
1 MiB. Parquet row-group decoded allocation is capped before reading batches.

Before convenience scoring, the CLI reads the exact selected serving version's
`inputSignature`. With `--default`, it first reads settings, resolves that exact
name, then reads its signature and **POSTs to that same exact version**, not the
potentially changed default route. A mismatched response name fails closed.
Missing, unknown and duplicate columns/keys are rejected. Supported named
primitive signatures are `boolean`, `integer` (i32), `long` (i64), `float` (exact
f32), `double` (finite f64) and `string`; unknown/tensor schemas are rejected.
Every column must be present. Null is accepted only with `required:false`.
Integer-to-floating conversions outside the exact range and lossy narrowing
fail before POST. This conservative local contract is not universal model
support. Raw `--body` remains the native escape hatch.

Local file/stdin reads run on a bounded worker before identity selection. If
cancelled or expired while an OS read may remain active, the host stops rather
than returning to the REPL or consuming another queued command.

CSV cells use explicit JSON typing: `0`, `false` and `null` are distinct.
The string `a,b` occupies CSV text `"""a,b"""`; an empty string is `""""""`.
An empty cell is ambiguous and rejected. Existing CSV *output* uses a different,
lossy null convention; it is not automatically a lossless scoring-input file.

### PowerShell argument transport

Serialize hashtables explicitly:

```powershell
# PowerShell 7.3+ Standard argument passing:
$body = @{ inputs = @(0, $false, $null) } | ConvertTo-Json -Depth 100 -Compress
$PSNativeCommandArgumentPassing = 'Standard'
& $fab artifact ml-endpoint score --version 1 --workspace-id $workspace `
    --model-id $model --body $body --force --wait
```

Windows PowerShell 5.1 and PowerShell Legacy transport can remove embedded JSON
quotes. Legacy-specific quote escaping must **not** be applied in Standard mode.
Prefer a JSON record file/stdin to avoid this transport issue. Malformed JSON
diagnostics explain serialization and transport without echoing rejected data.
Options such as `--force`, `--wait` and target IDs can precede or follow `--body`;
they must precede the trailing-record `--` separator.

## Failure evidence and follow-up

The original owner's `ExecutionFailed` with null HTTP status/operation ID cannot
be retrospectively diagnosed from that envelope. The previous native ML path
used a 30-second socket-read budget independently of the 240-second command
deadline; a cold-start timeout is plausible, not proven for that invocation.
Scoring now uses its explicit command timeout for both HTTP execution and the socket-read budget, still
bounded by the single absolute command deadline. Ordinary reads retain 30 seconds,
connect/write budgets are unchanged, and POST is never retried.

Errors retain actual classification, service status, operation/uncertainty
evidence, elapsed time, latest exchange phase/method/request count and allowlisted
request/activity/correlation IDs. Status/IDs survive a response-body failure;
each new exchange clears older HTTP IDs so metadata reads are not mistaken for
score acceptance. Scoring service text, arbitrary fields and parameters are not
printed because they can echo inputs or secrets. Safe service codes/request IDs
remain visible. Transport kinds report only what is known; generic I/O is not
mislabelled as a proven timeout.

`nextActions` supplies actual read-only `fab-rs api` argv for a known operation
and its result (retrieve the result only after `Succeeded`), using the existing
same host/identity. No operation ID or log URL is invented. Without an ID,
reconciliation by ID and recovery of that score result are unavailable from the
retained evidence. Preserve redacted stderr JSON, its timestamp, elapsed duration,
target, actual request IDs and binary/source build receipt for authorized Fabric
support/service-log correlation. No persistent scoring log or log-download
endpoint is claimed. A settings/version read can check control-plane state but
cannot prove inference success. Do not repeat inference merely to gather logs.
Post-execution observation/output failure never authorizes rescore.

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
