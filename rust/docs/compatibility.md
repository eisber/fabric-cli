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

## Request and output

- Default method `get`; supported methods are `get`, `post`, `delete`, `put`,
  `patch`. Long API options only: no `-A`, `-X`, `-H`, or `-i`.
- Use `--show-headers`, **not** Python's `--show_headers`; no underscore alias.
- No `--params`. Put URL-encoded query parameters in the quoted endpoint.
  Already-encoded values must not be double-encoded. `--query` instead applies
  JMESPath locally to the full response object. Unlike Python, a nonempty query
  that yields an empty collection or string does not fall back to the full
  response, avoiding unintended disclosure. An explicitly empty query still
  bypasses projection.
- JSON inline/file input and non-JSON-file multipart input retain the source
  behavior. Missing input files fail explicitly (a donor safety constraint),
  rather than silently sending a bodyless request. Headers split at the first
  `=`; authorization/host/hop-by-hop overrides are forbidden.
- Credentials bind to the selected audience/origin; unsafe origins, URL
  credentials/fragments, traversal, redirects, header injection, and cross-origin
  token forwarding are rejected. Validate input and finite allocation bounds
  before credential acquisition.
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
  `--show-headers`. Empty/whitespace bodies become `"(Empty)"`; nonempty invalid
  JSON is an error, not a text fallback. Initial response JSON is validated before
  default polling can replace it; invalid accepted bodies retain safe recovery
  information and do not trigger a status GET. Diagnostics do not contaminate stdout.
- Initial raw HTTP 4xx/5xx remain inspectable response data. This is not a
  universal fail-on-HTTP-status client. Credential, transport, malformed-body,
  and automatic-LRO failures are structured command errors with nonzero exits.
  Source exit mapping is retained, including Unauthorized exit **4**.

## Default polling is an extension

Python raw `api` returns before shared LRO processing and has no `--wait` flag.
Rust automatically waits only for recognized Fabric/ARM operations.
`--async` is an approved escape hatch: one initial request, unchanged response,
no sleeps, status/result fetches, or background process.
No `--wait`, `--wait=false`, `--no-wait`, `--lro-config`, or timing flags.

Fixed policy: 600-second monotonic polling budget, initial fallback 1 second,
factor 2, no separate attempt limit or interval cap. Valid server delta-seconds
or HTTP-date `Retry-After` is a not-before constraint, never permission to poll
earlier; invalid values use fallback with safe diagnostics. Delays, credential
acquisition, retries, status requests, and final-result retrieval all share the
remaining budget. Initial request timeout is separately 240 seconds.

Fabric 202 with validated operation Location polls even without an operation ID.
ID-only responses construct `/v1/operations/{id}`. Conflicting ID/URL fails
explicitly. Job-instance `/jobs/instances/` Locations retain Python's exclusion.
`Succeeded`/`Completed` fetch a validated advertised result, otherwise return the
actual terminal response. Only documented `OperationHasNoResult` suppresses a
result-fetch error; unrelated errors remain failures. No fabricated 200 copy of
the original body for result-less Fabric completion.

ARM 201/202 prefers `Azure-AsyncOperation` over Location and interprets terminal
JSON status. Location-only polling continues for 201/202 and completes on 200.
ARM preserves the shared Python normalized completion status/shape where
applicable; `--async` never normalizes the initial response.

Ordinary responses, accepted responses without a recognized handle, Storage,
and Power BI remain raw; acceptance is not proof of completion. Polling/result
URLs get the same origin checks, without forwarding original mutation bodies
or sensitive custom headers. Cancelled/Canceled are failures; malformed/unknown
states are protocol errors. Mutations are sent once; uncertain sends are not
replayed. Safe GET retries remain inside the operation deadline.

Timeout, local Ctrl+C, terminal failure, and uncertain transport outcomes preserve
known safe operation ID/status URL and uncertainty in structured recovery data.
Ctrl+C does not cancel the remote operation or imply rollback. Inspect the known
operation before deciding whether another mutation is safe.
