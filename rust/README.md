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
fab-rs workspace get <name-or-guid>
fab-rs workspace create [display-name] [--payload <json-or-@file>]
fab-rs workspace update <name-or-guid> --payload <json-or-@file>
fab-rs workspace delete <name-or-guid> [--force]
fab-rs item list --workspace <name-or-guid>
fab-rs item get <name-or-guid> [--workspace <name-or-guid>]
fab-rs item create [display-name] --workspace <name-or-guid> [--payload <json-or-@file>]
fab-rs item update <name-or-guid> [--workspace <name-or-guid>] --payload <json-or-@file>
fab-rs item delete <name-or-guid> [--workspace <name-or-guid>] [--force]
```

Selectors are single display names or GUIDs; GUID-shaped names are interpreted
as IDs for existing resources. Exact name lookup scans every page and fails on
missing or ambiguous names. Item list and create require `--workspace`, as do
get, update, and delete when the target is a name. For a GUID target without
`--workspace`, the CLI scans all accessible workspace and item pages to find
its workspace before issuing the requested operation. This can be slow, needs
permission to list those workspaces and items, and fails if any scan fails or
the ID is missing or ambiguous. Supplying `--workspace` avoids that scan.
Workspace commands do not accept `--workspace`.

Argument errors retain the JSON `Failure` envelope and `InvalidInput` code.
Missing required arguments identify their option or positional placeholder;
for example, `fab-rs item list` reports:

```text
Missing required arguments: --workspace <workspace>. Run fab-rs item list --help.
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
input do not confirm. `--force` bypasses only confirmation.

Resource `--query` runs on the native object or complete array in `result.data`.
Resource success envelopes use a non-indented UTF-8 JSON layout. List fetches all
pages and fails if the complete unfiltered compact JSON array
exceeds 32 MiB. Mutations accept `--async` or `--polling-config` with the same
polling rules as raw API. An unresolved HTTP 202 is instead represented as
`{"status_code":202,"status":"Accepted","body":...}` with sanitized `recovery`
when available; it does not claim completion. A bodyless successful HTTP 204
delete returns `null`. Raw `api` retains its existing wrapper and paging behavior.

Successful login/status output retains `selected` and adds `logged_in`,
`authentication_mode: "Azure CLI"`, `tenant_id`, and `principal_id`. These are
the validated binding's object IDs, not an account name inferred from token
claims. Like the `feature/rust` auth summary, it explicitly reports
`mfa_evidence: "not_checked"` and `service_access_verified: false`: acquiring
a credential is not proof of MFA or Fabric service access. No access token is
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

Recognized Fabric/ARM operations and Fabric job instances poll **by default**.
`--async` returns the actual initial response, without polling or starting background
work. Fabric non-job LROs use a 2-, 7-, then 20-second client schedule (20 seconds
thereafter), never earlier than a valid `Retry-After`;
ARM operations and job instances use each response's `Retry-After`, or 10 seconds when absent or
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
`--lro-config` spelling; no alias is retained. Power BI/Storage responses still
return raw: timing configuration does not add support for new polling protocols.

Job polling recognizes a safe `Location` of the form
`/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobInstanceId}`.
It waits through `NotStarted`/`InProgress`, returns the actual terminal job JSON
for `Completed`/`Deduped`, and fails explicitly for `Failed`/`Cancelled` or unknown
states. HTTP 200 on a status GET is not itself proof of job completion. Collection
Locations without an instance ID remain raw; no job ID or polling URL is invented.
See [Run On Demand Item Job](https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/run-on-demand-item-job)
and [Get Item Job Instance](https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/get-item-job-instance).

Fabric operation/job Locations may use the public API host or regional
`wabi-<cluster>-redirect.analysis.windows.net` hosts. Only validated HTTPS
operation status/result and exact job-instance routes may use this regional
exception; arbitrary hosts and raw user-supplied regional endpoints remain
rejected. Origin-relative result Locations retain the operation's validated host.
With a valid `x-ms-operation-id`, a safe non-status `Location` supplies only the
origin for `/v1/operations/{id}`. Result-header Locations likewise select that
operation's `/result` route, not an arbitrary advertised resource path.
Queries are retained on matching status/result routes, not copied from origin
hints. Conflicting operation IDs and unsafe origins/components still fail.
Polling uses fresh bodyless GETs with the Fabric credential scope, without
forwarding the original request's custom headers. HTTP redirects are not followed.

Every recognized pending or terminal polling status is printed and flushed to
**stderr** immediately, for example `Operation status: Running (50%)` or
`Job instance status: InProgress`. Numeric `percentageComplete` values from
0 through 100, including zero and fractions, are shown when supplied. Invalid
percentage metadata is omitted with a safe warning; missing/null values leave
status-only output. These updates are enabled by default, unlike Python's
debug-gated shared polling logs. Stdout remains a single final JSON response.
`--async`, ordinary raw responses, and help/version do not emit progress.

Empty/whitespace API response bodies now produce `text: ""`, not the Python
`"(Empty)"` marker. An initial 202 means accepted, not completed. Do not repeat an
accepted POST to collect headers; it may start another job. Inspect existing
jobs in Fabric's monitoring hub or GET a known instance URL. For future intentional
submissions, `--show-headers` exposes the headers the service returns.

JSON is the sole data format. Raw initial HTTP errors remain inspectable
response data; authentication, transport, malformed-response, and LRO failures
are command failures. See the
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
