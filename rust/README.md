<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal Rust Fabric API CLI

`fab-rs` is an independent, narrow Rust executable installed **alongside**, not
instead of, Python `fab`. It supports `auth login`, `auth status`, `auth logout`,
raw `api`, and minimal workspace/item CRUDL. It has no REPL, deployment,
installer, updater, or alternative credential providers.

This is implementation-stage source, not an official signed release. Native
validation and minimum supported OS evidence are still required.

## Run

Build from `rust/` with the pinned Rust **1.99.1** toolchain:

```sh
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-cli -- --version
```

Bare `fab-rs` shows a static text welcome screen with getting-started commands.
It does not read local configuration or identity selection. Use
`fab-rs auth status` to check the selected identity.

Help/version and subcommand help do not require authentication or Azure CLI.
Bare `fab-rs auth` lists the available authentication subcommands, just like
`fab-rs auth --help`, without reading configuration or changing identity selection.
Authentication and API calls require Azure CLI, installed separately:

```sh
az login
cargo run --locked -p fabric-cli -- auth login
cargo run --locked -p fabric-cli -- auth status
cargo run --locked -p fabric-cli -- api 'workspaces'
cargo run --locked -p fabric-cli -- auth logout
```

Rust login validates and binds to the current Azure CLI identity; it never
performs interactive sign-in, changes Azure CLI defaults, or accepts tenant,
subscription, account, or credential-mode overrides. Rust logout clears only
Rust's non-secret selection. API requires that selection even if Azure CLI is
already signed in. Identity drift fails closed; select the intended identity in
Azure CLI and run Rust login again.

## Workspace and item commands

```text
fab-rs workspace list
fab-rs workspace get <name-or-guid> [--export-to <dir> [--overwrite]]
fab-rs workspace create [display-name] [--payload <json-or-@file>]
fab-rs workspace update <name-or-guid> (--payload <json-or-@file> | --import-from <dir> [--force])
fab-rs workspace delete <name-or-guid> [--force]
fab-rs item list --workspace <name-or-guid>
fab-rs item get <name-or-guid> --workspace <name-or-guid> [--export-to <dir> [--format <f>] [--overwrite]]
fab-rs item create [display-name] --workspace <name-or-guid> [--payload <json-or-@file> | --import-from <dir> [--type <t>] [--format <f>]]
fab-rs item update <name-or-guid> --workspace <name-or-guid> (--payload <json-or-@file> | --import-from <dir> [--format <f>] [--force])
fab-rs item delete <name-or-guid> --workspace <name-or-guid> [--force]
```

Selectors are single display names or GUIDs; GUID-shaped names are interpreted
as IDs for existing resources. Exact name lookup scans every page and fails on
missing or ambiguous names. All item commands require `--workspace`. GUID
selectors are used directly without lookup. Workspace names are resolved by
exact display name; item names are resolved by exact display name within the
selected workspace's item collection. Workspace commands do not accept
`--workspace`.

Argument errors retain the JSON `Failure` envelope and `InvalidInput` code.
Missing required arguments identify their option or positional placeholder;
for example, `fab-rs item list` reports:

```text
Missing required argument: --workspace <workspace>. Run fab-rs item list --help.
```

Other parser failures identify known arguments or the kind of mistake and point
to the relevant command's help.
Diagnostics never echo supplied argument values or unknown argument names.

Payloads are inline JSON objects or `@` followed by a regular UTF-8 JSON file.
Creation requires `displayName` either positionally or in the payload. If both
are present they must match; a positional GUID is rejected. Item creation also
requires an open-string `type` property. A GUID-shaped payload `displayName`
is a literal name. Update payloads may rename the existing target. `id` and
item `workspaceId` are routing information, not payload properties.

Delete prompts on stderr after resolving the target. An entered blank line,
`y`, or `yes` confirms; `n` or `no` cancels with a failure. EOF and nonterminal
input do not confirm. `--force` bypasses only confirmation. In an embedding
process, a cancelled or timed-out prompt can leave its stdin reader blocked
until a line is entered; do not start concurrent prompts on the same stdin.

### Definition export and import

`--export-to` exports definitions; `--import-from` imports them. Both take an
existing local directory. A leading `~`, `~/`, or (on Windows) `~\` expands to
the home directory like Python `os.path.expanduser`; `~user` is literal.
Absolute and relative paths are used as given.

- `item get --export-to <dir>` calls Get Item Definition and writes each
  base64-decoded part below `<dir>/<displayName>.<type>/`.
- `workspace get --export-to <dir>` calls the beta Bulk Export Item
  Definitions API (`mode: All`) and writes parts below `<dir>` using the
  service paths (for example `<dir>/<name>.<type>/...`).
- `item create --import-from <dir>` reads every file below `<dir>` into a base64
  definition and creates the item. The type comes from `--type` or the
  `.platform` file; a mismatch fails.
- `item update --import-from <dir>` calls Update Item Definition.
- `workspace update --import-from <dir>` calls Bulk Import Item Definitions with
  every file below `<dir>` as `/`-prefixed parts, replacing matching items.
  It fails if any item reports `operationStatus: Failed`. Bulk import never
  creates the workspace.

Export never overwrites an existing file unless `--overwrite` is supplied;
files are written atomically with mode `0600` and directories with `0700` on
Unix. A destination filesystem must support hard links for no-overwrite exports.
The selected directory must not itself be a symlink; symlinks and special files
inside it are rejected. Part paths must be relative, portable, and free of `..`.
Import skips `cache.abf`, rejects `byPath` `definition.pbir`, and is bounded by
the request size limit. Import into an existing target (`update --import-from`)
prompts like delete unless `--force` is supplied. `--format` is forwarded as the
definition format.

All definition operations follow long-running operations by default.
`--async` returns the `Accepted` envelope instead; for exports it adds
`"saved": false` and writes no files. Completed exports return
`{"saved": true, "path": ..., "files": [...]}`.

Resource `--query` runs on the native object or complete array in `result.data`.
For `workspace list` and `item list`, a single field identifier such as `--query id`
is shorthand for `--query '[].{id: id}'`, returning `[{"id":"..."}]`.
Quoted JMESPath identifiers select literal keys, for example `--query '"a.b"'`.
Rows retain their order, missing or null fields produce `{"id":null}`, and
selected values retain their JSON types (including arrays and objects).
An empty list remains `[]`. No unselected fields are returned.
Other expressions retain ordinary JMESPath behavior on the complete list:
`--query '[].id'` returns an array of values, `--query "[?type=='Lakehouse']"`
filters rows, and `--query 'length(@)'` returns the existing scalar string.
Single-resource queries are unchanged: `workspace get <id> --query id` returns
the selected scalar, not a projected object. Empty queries bypass projection;
`.` and `@` select the complete resource data. This list-only shortcut does not
apply to raw `api`, which still requires explicit wrapper paths below.
Resource success envelopes use a non-indented UTF-8 JSON layout. List fetches all
pages and fails if the complete unfiltered compact JSON array
exceeds 32 MiB. Mutations accept `--async` or `--polling-config` with the same
polling rules as raw API. An unresolved HTTP 202 is instead represented as
`{"status_code":202,"status":"Accepted","body":...}` with sanitized `recovery`
when available; it does not claim completion. A bodyless successful HTTP 204
delete returns `null`. Raw `api` retains its existing wrapper and paging behavior.

Successful login/status output reports `logged_in`,
`authentication_mode: "Azure CLI"`, `tenant_id`, and `principal_id`. These are
the validated binding's object IDs, not an account name inferred from token
claims. Status omits `selected`, `mfa_evidence`, and `service_access_verified`.
Login retains `selected: true`, `mfa_evidence: "not_checked"`, and
`service_access_verified: false`. Acquiring a credential is not proof of MFA
or Fabric service access. No access token is
printed; the donor's Windows broker/account/token metadata is not adopted.

Login errors distinguish recognized missing/PATH-unavailable Azure CLI
diagnostics (install Azure CLI 2.54+ or fix PATH) from Azure CLI's explicit
no-signed-in-account diagnostic (run `az login`). Other credential failures
remain generic: they do not prove that Azure CLI is absent or signed out.
All remain `Unauthorized`, exit 4; raw credential diagnostics are never shown.
See the [provider contract](engine/core/auth/provider/README.md) for recognition
limits, including unknown/localized diagnostics.

```text
fab-rs [--verbose] api <endpoint>
  [--method get|post|delete|put|patch]
  [--audience fabric|storage|azure|powerbi]
  [--payload <inline-json-or-file>]
  [--headers <json-object-of-strings>]
  [--query <jmespath>]
  [--show-headers]
  [--async]
```

Quote endpoints and URL-encode query values yourself. For example:

```sh
fab-rs api 'workspaces?continuationToken=TOKEN'
fab-rs api workspaces/.../items --method post --payload request.json --async --show-headers
fab-rs api operations/OPERATION_ID --async --show-headers
fab-rs api workspaces --headers '{"Accept":"application/json","If-None-Match":"*"}'
fab-rs api workspaces --query 'text.value[].displayName'
fab-rs api workspaces --query "text.value[?displayName=='rustcli']"
```

`--payload` cannot be combined with `--method delete`. Any supplied input,
including an empty value or a file containing JSON `null`, fails with
`InvalidInput` (exit 1) before input loading, authentication, or HTTP requests.
DELETE without `--payload` remains supported; other methods are unchanged.

`--payload` replaces the former `--input` name; no alias is provided.
`--headers` accepts only an inline JSON object of string values, not `key=value`
pairs. Names are case-insensitive; duplicates (including case variants) are
errors. The only allowed names are `Accept`, `Content-Type`, `If-Match`,
`If-None-Match`, `Prefer`, and `Range`. Values retain commas, equals signs, and
whitespace. Authentication, cookies, routing, framing, version, User-Agent, and
managed correlation fields cannot be overridden. Invalid headers fail locally
before payload file loading or authentication, without echoing their values.

The shared Rust HTTP executor adds
`User-Agent: fabric-agent-cli/<version> (<command>; <os>; <arch>; <agent-kind>)`.
The final component is omitted when no single agent family is recognized;
direct engine callers default to command `http`. The CLI reads only known
inherited agent hints, never agent files, sockets, or APIs. If known session
variables contain exactly one distinct complete GUID, it sends the canonical
value as `x-ms-client-request-id`, reused across the invocation's requests,
retries, polling, and results. Invalid IDs are ignored; conflicting valid IDs
omit correlation. Prefixes are not stripped and IDs are not synthesized.
These environment hints are forgeable and may be absent, inherited, or stale:
they are neither authenticated provenance nor universal agent coverage.
Session correlation and agent kind reveal operational metadata to the target
service; neither raw hints nor session IDs enter local debug messages.

`--query` evaluates the response object placed inside `result.data`, matching
Python's raw `api` command: `status_code`, decoded `text`, and optional `headers`.
Do not include a `result.data` prefix. Use `text.value` for Fabric workspace
collections, `text` for an array body, and explicit paths such as `text.items`
for other body structures. There is no automatic body/value unwrapping or
bare-field row shortcut: migrate `[?displayName=='rustcli']` to
`text.value[?displayName=='rustcli']` and `displayName` to
`text.value[].displayName`. Use `@` (or `.`) to select the whole response object.
Headers are queryable only when `--show-headers` is supplied.
`Authorization` and `Proxy-Authorization` response headers are always excluded
case-insensitively, before query evaluation. This does not redact response bodies.

Filters require `==`, not `=`; quote the whole expression for your shell. Missing
fields retain `"None"` and empty projections remain empty; neither falls back
to the unselected response. This nondisclosure protection intentionally differs
from Python's falsey-result fallback. An empty query bypasses projection. No
automatic pagination is performed. Querying is not a universal security-redaction
boundary: selected data can itself be sensitive.

### Local debug logging

Use the global `--verbose` flag before or after a command:

```sh
fab-rs --verbose api workspaces
fab-rs auth status --verbose
```

Readable, grouped debug messages go to stderr; stdout remains the command's
JSON result. Each group starts with `[verbose] [<timestamp>] <log>`, using a UTC
RFC 3339 timestamp with millisecond precision, such as `2026-11-16T15:57:50.794Z`.
Logs describe command stages, credential timing, HTTP attempts and
responses, retries, and polling decisions. Progress messages remain enabled
without verbose mode. Help, version, bare `auth`, and bare `fab-rs --verbose`
remain effect-free and produce no debug messages.

Logs never contain authorization headers (even masked), credentials, bodies,
local paths, query strings, identity IDs, raw exceptions, or SDK/process output.
Only parsed, allowlisted header metadata is logged. Full canonical UUIDs from
response headers `requestId` and `x-ms-request-id` are the explicit ID exception.
Locations and request destinations use fixed service origins and route templates;
dynamic segments are replaced, unknown paths are redacted, and other accepted service
hosts appear as `service-advertised`. Invalid or ambiguous metadata is omitted
with a fixed marker.

Color is used only on terminal stderr when `NO_COLOR` is absent and `TERM` is
not `dumb`. Redirected output is plain text. Each group is flushed before work
continues. An output failure stops local execution without replaying requests;
remote work may already have succeeded or still be running.
Console writes are synchronous; a blocked output stream can delay cancellation
until its current write returns.

Treat logs as potentially sensitive: request IDs, timings, and sizes expose
operational metadata. Ordinary stdout responses and recovery details are not
covered by diagnostic sanitization. No log files are created; file logging is
not implemented, and `RUST_LOG` does not enable these diagnostics.

### Operation polling

Recognized Fabric, Azure, and Storage operations and Fabric job instances poll **by default**.
`--async` returns the actual initial response, without polling or starting background
work. Fabric LROs and job instances wait 2 seconds before the first check and
7 seconds before the second, ignoring `Retry-After` for those two waits.
Subsequent waits use the minimum of 20 seconds and a valid `Retry-After`;
absent/invalid hints use 20 seconds. This explicitly permits polling earlier
than the service requests and can increase throttling.
Azure/Storage operations use each response's `Retry-After`, or 10 seconds when absent or
invalid, without exponential backoff. There is no overall polling timeout by
default: a backend that never completes can keep the command waiting indefinitely.
Individual poll/result requests, including their retries, remain bounded to 240
seconds. Ctrl+C cancels local waiting, not the backend operation; do not blindly
repeat an uncertain mutation.

Use `--polling-config '{"poll_interval":5,"timeout":900}'` to override polling for a
single command. Both optional values are positive numbers in **seconds**, including
fractions. `poll_interval` replaces the client schedule for either protocol, but
cannot shorten a server `Retry-After`. `timeout` bounds the entire reconciliation
phase (waits, status GETs, retries, and result retrieval), not the initial request.
Omitting a key or setting it to `null` uses the corresponding default. Unknown
keys, invalid types, and nonpositive/out-of-range durations fail before sending.
Use `poll_interval`, not `polling_interval`; values must be JSON numbers, not
quoted strings. Invalid values identify the affected field.
There is no `max_attempts` option. `--async` cannot be combined with
`--polling-config`, even an empty object: async mode returns only the initial
response and does not support polling. This conflict fails before input loading,
identity acquisition, or sending a request. This replaces the earlier
`--lro-config` spelling; no alias is retained. Power BI responses still
return raw: timing configuration does not add support for new polling protocols.

Storage (OneLake) uses the same polling protocols as Azure: HTTP 201/202 with
`Azure-AsyncOperation` takes precedence over `Location`. Status JSON drives
`Azure-AsyncOperation`; Location-only polling continues on 201/202 and completes
on 200. Polling stays on the selected audience's fixed origin and token scope:
Storage uses `onelake.dfs.fabric.microsoft.com` and the Storage scope, not ARM.
Completion preserves the initial body/headers and normalizes its status to 200.
Without either header the response stays raw; `--async` skips polling.

Job polling recognizes a safe `Location` of the form
`/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobInstanceId}`.
It waits through `NotStarted`/`InProgress`, returns the actual terminal job JSON
for `Completed`/`Deduped`, and fails explicitly for `Failed`/`Cancelled` or unknown
states. HTTP 200 on a status GET is not itself proof of job completion. Collection
Locations without an instance ID remain raw; no job ID or polling URL is invented.
See [Run On Demand Item Job](https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/run-on-demand-item-job)
and [Get Item Job Instance](https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/get-item-job-instance).

Fabric operation/job Locations trust service-advertised hostnames without a
hostname allowlist, matching Python's shared Fabric LRO client. This includes
regional and MSIT hosts without per-cluster exceptions. Only validated HTTPS
operation status/result and exact job-instance routes may use additional hosts;
raw user-supplied endpoints remain fixed-origin. TLS certificate verification,
credential/fragment, port, and unsafe-path checks remain enabled.
Origin-relative result Locations retain the operation's validated host.
`home-cluster-uri` is not a polling target. Job polling uses the same default
timing policy as Fabric operations.
With a valid `x-ms-operation-id`, a safe non-status `Location` supplies only the
origin for `/v1/operations/{id}`. Result-header Locations likewise select that
operation's `/result` route, not an arbitrary advertised resource path.
Queries are retained on matching status/result routes, not copied from origin
hints. Conflicting operation IDs and unsafe URL components still fail.
Polling uses fresh bodyless GETs with the Fabric credential scope, without
forwarding the original request's custom headers. HTTP redirects are not followed.

Once a valid operation/job enters polling, the CLI immediately prints and flushes
`Operation accepted. Polling for completion...` (or `Job accepted`) to
**stderr**, before the first polling delay.
Before each status GET it prints `Checking operation status...` or
`Checking job status...`. Each scheduled wait prints one fixed line with the
engine's selected delay, with no countdown, elapsed time, or terminal refresh.
If a status request or result retrieval takes more than 5 seconds, one additional
waiting line identifies that phase and the last reported status, if known.
It does not repeat while that phase remains unchanged. For example:

```text
Job accepted. Polling for completion...
Waiting for job completion; next status check in 2s...
Checking job status...
Job instance status: InProgress
Waiting for job completion; next status check in 7s...
Checking job status...
Job instance status: Completed
Job completed.
```

These are local liveness messages, not new service observations or estimated
progress. They do not send extra requests or change the engine's timing policy.
All messages are permanent lines in both terminals and redirected output.
A final completion message follows successful result retrieval/conversion;
`Deduped` retains its distinct outcome rather than being called completed.
The delayed waiting notice also covers credentials and retries.
Messages begin only after the initial response establishes a valid
polling handle, not during the initial request or its credential acquisition.
The CLI stays in the foreground while monitoring a server-side background job.

Every recognized pending or terminal polling status is printed and flushed to
**stderr** immediately, for example `Operation status: Running (50%)` or
`Job instance status: InProgress`. Numeric `percentageComplete` values from
0 through 100, including zero and fractions, are shown when supplied. Invalid
percentage metadata is omitted with a safe warning; missing/null values leave
status-only output. These updates are enabled by default, unlike Python's
debug-gated shared polling logs. Terminal progress is cyan, completion statuses
green, failures red, cancellation yellow, and waiting text subdued gray.
Redirected stderr and `TERM=dumb` use append-only plain text.
`NO_COLOR` disables colors.
Stdout remains a single final JSON response.
`--async`, ordinary raw responses, and help/version do not emit progress.

When running a shell alias such as `fabrs`, use `type -a fabrs` to check which
executable it names. Rebuilding in a different checkout does not update that
binary. Use the freshly built executable from the intended checkout to see its
progress behavior; see the [build instructions](CONTRIBUTING.md#build-and-run-locally).

Empty/whitespace API response bodies now produce `text: ""`, not the Python
`"(Empty)"` marker. An initial 202 means accepted, not completed. Do not repeat an
accepted POST to collect headers; it may start another job. Inspect existing
jobs in Fabric's monitoring hub or GET a known instance URL. For future intentional
submissions, `--show-headers` exposes the headers the service returns.

JSON is the sole data format. Raw initial HTTP errors remain inspectable
response data; authentication, transport, malformed-response, and LRO failures
are command failures. Failed operation status/result responses expose a
service error code in `result.error_code` when available and valid; otherwise
they use `Service`. Failed/cancelled Fabric jobs also read
`failureReason.errorCode` (or `code`) and expose a nonempty string
`failureReason.message` as `result.failure_reason.message` in the final JSON.
This service-provided text may contain sensitive details; it is JSON-escaped
and never copied into progress, verbose logs, or error debug formatting.
For other failed operations and status or result HTTP failures, the service's
error message is appended to the safe summary after sanitization (control
characters removed, whitespace collapsed, URL query strings dropped, capped at
512 characters). Unrelated response fields are not forwarded. A valid request
ID is appended when present.
The safe summary distinguishes failed jobs from cancelled jobs.
Recovery retains the safe operation ID and status URL, but no public uncertainty
boolean. When the final status is unknown, the error message warns that the
operation may still be running and directs you to check `status_url` before
retrying. A confirmed terminal status suppresses that warning, even if subsequent
result retrieval or output processing fails.
See the
[complete compatibility contract](docs/authoring/compatibility.md)
before migrating scripts.

## Preview bundles

The manually dispatched **Rust unsigned unofficial preview** workflow runs the
same four-platform checks as CI and uploads seven-day workflow artifacts. It
does not create tags, publish GitHub releases, sign binaries, or use live identity
secrets. Extract the workflow artifact wrapper first, then verify its inner
target-qualified bundle and sidecars using the
[packaging instructions](docs/authoring/packaging.md). An archive is not a
promise of zero OS dependencies. Azure CLI remains an authentication prerequisite.

Official builds must originate in the trusted internal build system, not be
promoted from arbitrary PR artifacts.

## Development and validation

Start with the [Rust contributor guide](CONTRIBUTING.md) for local setup,
debug and release builds, running `fab-rs`, and source-level debugging.

The workspace has explicit retained package leaves under `engine/` and
`clients/cli/`. Engine libraries accept injected credentials/HTTP/time and must
not depend on CLI packages or select ambient identities. The composition root
owns runtime, streams, signals, and local selection storage.

Run these commands from `rust/`. On a shared local host, **obtain the integration
owner's explicit heavy-build lease first**, and run only one heavy job at a time:

```sh
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo fetch --locked
python3 scripts/check_boundaries.py
cargo check --offline --locked --workspace --all-targets
cargo clippy --offline --locked --workspace --all-targets -- -D warnings
cargo test --offline --locked --workspace
cargo test --offline --locked --workspace --doc
cargo doc --offline --locked --workspace --no-deps
cargo run --offline --locked -p fabric-cli -- --help
cargo run --offline --locked -p fabric-cli -- --version
```

The product is MIT licensed; binary bundles include the full product license
and dependency notices. Authentication/core review and upstream contribution
requirements still apply. Neither a green preview nor these documents establish
independent review, official-release approval, or signing compliance.
