<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Compatibility contract

Reference: Python Fabric CLI at
`5b504195c63ef54c7382ca8e8a9d9759717f0ec5`; selected Rust donor
`f8cb97e92eab669785954335e0e7276d30a3377e`. This is a deliberately narrower
product with approved extensions, not full Python flag/command parity.
These are intended acceptance criteria; combined-revision tests remain the
authority for implementation evidence.

## Surface and authentication

- The executable is `fab-rs`, not `fab`; no Python runtime bridge or changes to
  Python installation/auth files. No resource commands, shell, workspace
  navigation, paging, deployment, import/export, installer, or updater.
- Only argument-free `auth login`, `auth status`, `auth logout` and raw `api`.
  Azure CLI is the **sole** credential provider. No native browser/broker/device
  code, service principal, managed identity, developer chain, or fallback.
- Rust login validates the current Azure CLI identity, not interactive sign-in.
  Users run `az login` themselves. Rust never runs `az login`, `az logout`, or
  `az account set`. No tenant/subscription/account overrides.
- Rust-specific non-secret binding is required before API. Missing/stale binding,
  unreadable/malformed selection, missing required tenant/principal metadata, or
  identity drift fail closed. Subscription-only changes with the same identity
  are not drift. Re-login to adopt an identity change. Status validates binding;
  logout is idempotent and leaves Azure CLI signed in. Tokens are never persisted
  by Rust.
- `--help`, `--version`, and ordinary subcommand `--help` use stdout, exit zero,
  and do not load credentials or send HTTP. Use two ASCII hyphens.
- Bare `fab-rs` shows a plain-text welcome screen on stdout, then exits without
  entering a shell. The static text contains getting-started commands and exits
  zero with empty stderr when stdout is writable, regardless of identity
  selection or configuration availability. There is no configuration read,
  credential acquisition, Azure CLI invocation, network access, or selection mutation.
  This replaces the previous automatic `auth status` execution and its JSON
  success/`Unauthorized` output; explicit `auth status` and other command JSON
  contracts are unchanged.
- Login/status successes report `logged_in: true`,
  `authentication_mode: "Azure CLI"`, `tenant_id`, and `principal_id` from the
  validated binding. Status omits `selected`, `mfa_evidence`, and
  `service_access_verified`; login retains `selected: true`,
  `mfa_evidence: "not_checked"`, and `service_access_verified: false`.
  Credential validation does not establish MFA or service authorization.
  No account name, token, or donor broker capabilities
  are inferred; the existing JSON envelope and logout response are unchanged.
- Bare `fab-rs auth` shows the same human-readable help as `fab-rs auth --help`,
  with stdout text, empty stderr, and exit zero. It needs no Azure CLI or
  configuration and does not change selection. This replaces its previous JSON
  `InvalidInput` failure and exit one; scripts relying on that failure must adapt.
  Unknown commands, subcommands, or flags and missing required API arguments
  remain sanitized JSON failures on stdout with exit one.

## Request and output

- Default method `get`; supported methods are `get`, `post`, `delete`, `put`,
  `patch`. Long API options only: no `-A`, `-X`, `-H`, or `-i`.
- Use `--show-headers`, **not** Python's `--show_headers`; no underscore alias.
- Rust's global `--verbose` is a diagnostic extension, not Python `get`'s
  property-display option. It is off by default and writes only sanitized,
  grouped stderr messages. No `-v`, `--debug`, file sink, or environment-driven
  logging is added. Welcome/help/version and bare `auth` stay effect-free with
  the flag. Existing progress output and stdout result contracts are unchanged.
- Response `Authorization` and `Proxy-Authorization` headers are always removed
  case-insensitively before `--show-headers` output or query evaluation, with or
  without verbose mode. Outgoing authentication and response bodies are unchanged.
  Diagnostic headers use a stricter allowlist: parsed content metadata and
  retry delays, canonical UUID request IDs, and templated Locations. Queries,
  resource IDs, arbitrary hosts, bodies, SDK errors, and recovery objects never
  enter debug events. Required recovery remains in ordinary stdout failures.
- No `--params`. Put URL-encoded query parameters in the quoted endpoint.
  Already-encoded values must not be double-encoded. `--query` instead applies
  JMESPath locally to the response object stored inside `result.data`, matching
  Python raw API's HTTP wrapper input: `status_code`, decoded `text`, and optional
  `headers`. The outer envelope and `result.data` array are not query inputs.
  Unlike Python, a nonempty Rust query
  that yields an empty collection or string does not fall back to the full
  response, avoiding unintended disclosure. An explicitly empty query still
  bypasses projection.
- All expressions use that response object, with no automatic `text`/`value`
  unwrapping, bare-field projection, evaluation retry, recursive traversal, or
  service pagination. This restores the pinned Python raw API query input.
  Use `fab-rs api workspaces --query "text.value[?displayName=='rustcli']"` for
  matching workspace rows, `text.value[].displayName` for their names,
  `text[?displayName=='rustcli']` for an array body, and `text.items` for a body
  object's `items` field. Migrate earlier implicit body queries to these explicit
  paths. Body fields named `text`, `status_code`, or `headers` likewise require
  a `text.` prefix. Query `@` or `.` for the whole response object.
  `headers` exists only with `--show-headers`. JMESPath equality uses `==`, not `=`.
  Missing/null row fields are omitted by array projections; scalar/list/object
  values and empty projections retain existing output rules. Missing/null fields
  produce `"None"` rather than selecting the full response.
  An empty/missing projection never reveals the unselected full response.
  `--query` is not a universal security-redaction boundary.
- JSON inline/file input and non-JSON-file multipart input retain the source
  behavior for non-DELETE methods. `--method delete` rejects any supplied
  `--input` with structured `InvalidInput` and exit 1, before input loading,
  authentication, or HTTP requests. This CLI-specific restriction intentionally
  replaces the previous acceptance of DELETE inputs, including empty values and
  files containing JSON `null`; it is not a universal HTTP restriction.
  DELETE without `--input` remains supported.
  Missing input files fail explicitly (a donor safety constraint),
  rather than silently sending a bodyless request. Headers split at the first
  `=`; authorization/host/hop-by-hop overrides are forbidden.
- Raw requests bind credentials to the selected audience/origin. Unsafe origins,
  URL credentials/fragments, traversal, redirects, and header injection are
  rejected before credential acquisition. Fabric polling has the narrow
  service-advertised operation-host exception described below, not a general
  cross-origin token-forwarding policy.
- Audience `fabric`: base `https://api.fabric.microsoft.com/v1`, scope
  `https://api.fabric.microsoft.com/.default`.
- Audience `storage`: base `https://onelake.dfs.fabric.microsoft.com`, scope
  `https://storage.azure.com/.default`.
- Audience `azure`: base `https://management.azure.com`, scope
  `https://management.azure.com/.default`.
- Audience `powerbi`: base `https://api.powerbi.com/v1.0/myorg`, deliberately
  **Fabric's scope**, matching the pinned Python implementation. Do not silently
  substitute a Power BI-specific scope.
- Only the Python-shaped JSON envelope: `timestamp`, `status`, `command`, and
  `result.data` response objects with `status_code` and `text`. Four-space
  indentation and trailing newline; optional `headers` only with
  `--show-headers`. Empty/whitespace bodies become `""`, an approved departure
  from Python's `"(Empty)"` marker; nonempty invalid
  JSON is an error, not a text fallback. Initial response JSON is validated before
  default polling can replace it; invalid accepted bodies retain safe recovery
  information and do not trigger a status GET. Diagnostics do not contaminate stdout.
- Initial raw HTTP 4xx/5xx remain inspectable response data. This is not a
  universal fail-on-HTTP-status client. Credential, transport, malformed-body,
  and automatic-LRO failures are structured command errors with nonzero exits.
  Source exit mapping is retained, including Unauthorized exit **4**.

## Default polling is an extension

Python raw `api` returns before shared LRO processing and has no `--wait` flag.
Rust automatically waits for recognized Fabric/Azure/Storage operations and Fabric job
instances.
`--async` is an approved escape hatch: one initial request, unchanged response,
no sleeps, status/result fetches, or background process.
No `--wait`, `--wait=false`, `--no-wait`, or individual timing flags.

Fabric operation/job client schedule: 2, 7, then 20 seconds repeatedly. Azure/Storage operations
use each response's `Retry-After`, or 10 seconds when absent/invalid, without
staged backoff. Valid server delta-seconds
or HTTP-date `Retry-After` is a not-before constraint, never permission to poll
earlier; invalid values use fallback with safe diagnostics. Delays, credential
acquisition, retries, status requests, and final-result retrieval all share the
remaining budget when one is configured. Initial request timeout is separately
240 seconds; each status/result request and its retries also has a 240-second
bound, shortened by any remaining overall budget.

`--polling-config` accepts an inline JSON object with optional `poll_interval` and
`timeout` keys: positive finite numbers in seconds, including fractions, within
the duration/clock range. Omitted/null keys use defaults; unknown keys or invalid
values fail locally before sending a mutation. No `max_attempts` is exposed.
`poll_interval` replaces the client schedule for either protocol, but never
overrides a server not-before constraint. `timeout` is a total monotonic
reconciliation budget, starting after the initial response, including waits,
GET retries, and final-result retrieval. It has **no default**; omitted/null
means indefinite overall waiting, still cancellable and with bounded individual
requests. `--async` cannot be combined with `--polling-config`, including an empty
object, because async mode is initial-response-only and does not support polling.
The conflict fails before input loading or host effects, even if configuration
is invalid. Invalid duration errors name the field; the known `polling_interval`
typo suggests the supported `poll_interval` key without accepting an alias.
The former `--lro-config` spelling is no longer accepted. Power BI stays
raw; configuration does not add protocol recognition.

Fabric 202 with validated operation Location polls even without an operation ID.
ID-only responses construct `/v1/operations/{id}`. Conflicting ID/URL fails
explicitly.
For a valid operation ID and a safe non-status Location, the Location supplies
only the origin of the canonical `/v1/operations/{id}` status route. Terminal
Location headers similarly select `/v1/operations/{id}/result` on their validated
origin. Matching status/result routes retain their queries; resource-shaped
origin hints do not contribute paths or queries to GETs. This URL-selection
behavior follows Python main at `5b504195c63ef54c7382ca8e8a9d9759717f0ec5`,
without changing the approved job/ARM protocols. Known contradictory operation
IDs remain errors.
Fabric polling trusts hostnames advertised in service Location headers without
a hostname allowlist, matching Python main's shared Fabric LRO client
(`_handle_fab_long_running_op` and `_fetch_operation_result`). Regional and MSIT
hosts require no per-host exceptions. This accepts the service's advertised host
as a bearer-token destination; TLS certificate verification remains enabled.
The `home-cluster-uri` header does not select a polling origin.
This exception is restricted to recognized `/v1/operations/{id}` status and
`/v1/operations/{id}/result` routes and exact job-instance routes. It does not
allow credentials/fragments, unsafe paths, non-443 ports, or redirects.
These URL-component guards are retained Rust safeguards, not a claim of exact
Python URL-normalization parity.
Raw user endpoints and other audiences retain fixed-origin validation.
Discovery and authenticated polling share this policy; only bodyless GETs
without custom headers may use polling execution. Tokens are freshly acquired
for the unchanged Fabric scope, not forwarded from the mutation.
Origin-relative result Locations resolve against the validated status origin.
The JSON `resourceLocation` extension still selects a validated advertised
resource URL rather than serving as a canonical-route origin hint.
`Succeeded`/`Completed` fetch the selected validated result, otherwise return the
actual terminal response. Only documented `OperationHasNoResult` suppresses a
result-fetch error; unrelated errors remain failures. No fabricated 200 copy of
the original body for result-less Fabric completion.

Fabric job-instance Locations of the exact form
`/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobInstanceId}`
are reconciled separately, using their validated Fabric service URL rather than
`/v1/operations/{id}`. A valid additional operation ID does not change the job
protocol selected by that Location. Jobs use the same 2-, 7-, then repeated
20-second client schedule as Fabric operations, respecting `Retry-After`
(delta-seconds or HTTP date) as a minimum and diagnosing invalid hints safely.
This shared schedule is a user-approved difference from Python's job polling.
Jobs share the optional overall polling budget,
GET retry bounds and local cancellation behavior, without remote cancellation.
Job GETs must return HTTP 200 and a recognized JSON status: `NotStarted` and
`InProgress` continue; `Completed` and `Deduped` return the actual terminal
body/headers; `Failed` and `Cancelled` fail. Unknown/malformed status is a
protocol error. Job terminal Locations are not general-LRO result handles.
Collection Locations without an instance ID and other unsupported job routes
remain raw. This job reconciliation is an approved extension, not Python raw
API parity or a port of the full job-command subsystem.

ARM 201/202 prefers `Azure-AsyncOperation` over Location and interprets terminal
JSON status. Location-only polling continues for 201/202 and completes on 200.
Like Python's shared ARM handler, an advertised URL containing the Azure endpoint
is reduced to the suffix after that endpoint and polled on the fixed Azure HTTPS
origin; the advertised hostname is not used as a bearer-token destination.
Origin-relative headers remain supported as a Rust extension. Existing URL
component and path checks still apply to the resulting polling URL.
ARM preserves the shared Python normalized completion status/shape where
applicable; `--async` never normalizes the initial response.

Storage is a user-approved extension of these Azure polling semantics, not a
claim that Python Storage/raw API performs automatic LRO reconciliation.
HTTP 201/202 recognizes `Azure-AsyncOperation` before `Location`, with identical
status handling, timing, cancellation, timeout, progress, and normalized completion.
Header suffix resolution uses the fixed OneLake endpoint rather than ARM;
GETs retain the Storage audience and `https://storage.azure.com/.default` scope.
It does not add blob-copy or other header protocols.

Ordinary responses, accepted responses without a recognized handle,
and Power BI remain raw; acceptance is not proof of completion. Polling/result
URLs get the shared operation-origin checks, without forwarding original mutation
bodies or custom headers. Cancelled/Canceled are failures; malformed/unknown
states are protocol errors. Mutations are sent once; uncertain sends are not
replayed. Safe GET retries remain inside the operation deadline.

Failed status/result responses expose their service error identifier in
`result.error_code` when it is a bounded ASCII identifier; otherwise the code
remains `Service`. Raw service error messages are not emitted.

Entering validated reconciliation emits and flushes
`Operation accepted (HTTP 202). Polling for completion...` or `Job accepted`
(using the actual initial HTTP status)
to stderr before the first polling delay, including any `Retry-After`. This
static local notification does not assert a backend status or percentage.
Each logical status GET emits `Checking operation status...` or
`Checking job status...` after its required delay, before credentials or HTTP.
Interactive terminals show a waiting line immediately and refresh it every
second; redirected stderr emits a heartbeat after 5 seconds without an update,
repeating every 5 seconds. Waiting text shows elapsed polling time, an actual
countdown supplied by the engine's selected delay, and any **last reported**
status/percentage. It never says no status was received merely because the first
status GET has not returned. Pending status requests show time since that check
started. Heartbeats distinguish
a scheduled wait, an unanswered status request, and final-result retrieval.
They remain active during credentials/retries and never send requests or change
server not-before constraints. The display interval is fixed, separate from API
polling configuration. No heartbeat covers the initial request.
All output is flushed. Real checks/statuses remain permanent lines; transient
terminal waiting output is cleared before permanent output and all local exits.
Successful result retrieval/conversion prints a final completion line; `Deduped`
remains distinct. Native terminal checks use cyan, successful statuses green,
failures red, cancellations yellow, and waiting text gray.
Redirected streams and `TERM=dumb` use append-only plain text; `NO_COLOR` disables
color independently of interactive refresh.
Recognized pending and terminal status GETs emit live, flushed stderr progress
through an injected observer; engine libraries never write to the console.
`Operation status: Running (50%)` and `Job instance status: InProgress` are
example lines. Only canonical recognized status and finite numeric
`percentageComplete` metadata in the inclusive 0-100 range reach the observer.
Zero and fractions are retained; missing/null metadata produces status-only
progress, and invalid supplied metadata produces a safe static warning.
No body text, error text, headers, URL queries, or arbitrary percentage strings
are printed as progress. Stdout remains one final JSON envelope. Async/raw,
initial/result responses, failed HTTP attempts, and unknown/malformed statuses
emit no service-status observations. A pre-request line is not evidence that a
response arrived. Heartbeats stop with local completion, failure, cancellation,
or timeout; output failure stops local reconciliation with safe recovery, not
the remote job. Default-enabled status/percentage reporting intentionally
differs from Python's debug-gated shared polling logs.

Timeout, local Ctrl+C, terminal failure, and uncertain transport outcomes preserve
known safe operation ID/status URL and uncertainty in structured recovery data.
Ctrl+C does not cancel the remote operation or imply rollback. Inspect the known
operation before deciding whether another mutation is safe.
