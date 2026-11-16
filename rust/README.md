<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal Rust Fabric API CLI

`fab-rs` is an independent, narrow Rust executable installed **alongside**, not
instead of, Python `fab`. It supports only `auth login`, `auth status`,
`auth logout`, and raw `api`. It has no REPL, resource commands, pagination,
deployment, installer, updater, or alternative credential providers.

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
fab-rs api <endpoint>
  [--method get|post|delete|put|patch]
  [--audience fabric|storage|azure|powerbi]
  [--input <inline-json-or-file>]
  [--headers <key=value,...>]
  [--query <jmespath>]
  [--show-headers]
  [--async]
```

Quote endpoints and URL-encode query values yourself. For example:

```sh
fab-rs api 'workspaces?continuationToken=TOKEN'
fab-rs api workspaces/.../items --method post --input request.json --async --show-headers
fab-rs api operations/OPERATION_ID --async --show-headers
fab-rs api workspaces --query 'text.value[].displayName'
fab-rs api workspaces --query "text.value[?displayName=='rustcli']"
```

`--input` cannot be combined with `--method delete`. Any supplied input,
including an empty value or a file containing JSON `null`, fails with
`InvalidInput` (exit 1) before input loading, authentication, or HTTP requests.
DELETE without `--input` remains supported; other methods are unchanged.

`--query` evaluates the response object placed inside `result.data`, matching
Python's raw `api` command: `status_code`, decoded `text`, and optional `headers`.
Do not include a `result.data` prefix. Use `text.value` for Fabric workspace
collections, `text` for an array body, and explicit paths such as `text.items`
for other body structures. There is no automatic body/value unwrapping or
bare-field row shortcut: migrate `[?displayName=='rustcli']` to
`text.value[?displayName=='rustcli']` and `displayName` to
`text.value[].displayName`. Use `@` (or `.`) to select the whole response object.
Headers are queryable only when `--show-headers` is supplied.

Filters require `==`, not `=`; quote the whole expression for your shell. Missing
fields retain `"None"` and empty projections remain empty; neither falls back
to the unselected response. This nondisclosure protection intentionally differs
from Python's falsey-result fallback. An empty query bypasses projection. No
automatic pagination is performed. Querying is not a universal security-redaction
boundary: selected data can itself be sensitive.

Recognized Fabric, Azure, and Storage operations and Fabric job instances poll **by default**.
`--async` returns the actual initial response, without polling or starting background
work. Fabric LROs and job instances use a 2-, 7-, then 20-second client schedule (20 seconds
thereafter), never earlier than a valid `Retry-After`;
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
`home-cluster-uri` is not a polling target. Job polling honors `Retry-After`,
including the initial wait before the first status/progress update.
With a valid `x-ms-operation-id`, a safe non-status `Location` supplies only the
origin for `/v1/operations/{id}`. Result-header Locations likewise select that
operation's `/result` route, not an arbitrary advertised resource path.
Queries are retained on matching status/result routes, not copied from origin
hints. Conflicting operation IDs and unsafe URL components still fail.
Polling uses fresh bodyless GETs with the Fabric credential scope, without
forwarding the original request's custom headers. HTTP redirects are not followed.

Once a valid operation/job enters polling, the CLI immediately prints and flushes
`Operation accepted (HTTP 202). Polling for completion...` (or `Job accepted`,
with the actual HTTP acknowledgement code) to
**stderr**, before the first polling delay (including a long `Retry-After`).
Before each status GET it prints `Checking operation status...` or
`Checking job status...`. An interactive terminal refreshes one waiting line
**every second**, starting with the next check's actual countdown.
Redirected stderr prints a waiting heartbeat after **5 seconds without an
update**, repeating every 5 seconds until another update. Heartbeats
include elapsed polling time and the **last reported** service status/percentage, if any;
they distinguish waiting for the next check from waiting for a status response
or final result. For example:

```text
Job accepted (HTTP 202). Polling for completion...
Checking job status...
Job instance status: InProgress
Waiting for job completion; next status check in 2s; Last reported status: InProgress (7s elapsed)...
Checking job status...
Job instance status: Completed
Job completed.
```

These are local liveness messages, not new service observations or estimated
progress. They do not send extra requests or shorten `Retry-After`.
Display refreshes are independent of `poll_interval`. The countdown comes from
the engine's selected delay, including `Retry-After`, not an assumed interval.
While a status GET is pending, the display reports how long that check has been
waiting instead of suggesting that a new service status has arrived.
Real checks and service statuses remain permanent lines; terminal waiting output
is cleared before those lines and on completion, failure, timeout, or cancellation.
A final completion message follows successful result retrieval/conversion;
`Deduped` retains its distinct outcome rather than being called completed.
Updates continue while credentials, a status request, retries, or result retrieval
are pending. They begin only after the initial response establishes a valid
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
`NO_COLOR` disables colors but keeps in-place refresh on an interactive terminal.
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
they use `Service`. Service error messages are not forwarded. See the
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
