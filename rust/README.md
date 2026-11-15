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

Help/version and subcommand help do not require authentication or Azure CLI.
Bare `fab-rs` runs `auth status`: it validates the existing Rust selection and
reports the current binding, or returns login guidance with exit 4 when none is
selected. It does not select an identity or start a REPL.
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
fab-rs api workspaces --query displayName
fab-rs api workspaces --query '[].displayName'
fab-rs api workspaces --query "[?displayName=='rustcli']"
```

Every nonempty `--query` evaluates the decoded body's top-level `value` when that
key exists, otherwise the body itself (`text.value` or `text` in the unqueried
response). A present `value` is selected even when null, empty, scalar, or an
object; there is no truthiness fallback or recursive unwrapping. On an array,
a bare ASCII field such as `displayName` is shorthand for `[].displayName`.
Missing/null row fields are omitted; empty projections remain empty. Missing
fields on a noncollection input retain `"None"`. Filters require `==`, not `=`;
quote the whole expression for your shell. No automatic pagination is performed.

This intentionally replaces wrapper-based queries, including Python's pinned raw
`api` behavior. Migrate `text.value[].displayName` to `[].displayName`, or
`text.items` to `items` for a body without `value`. Use `@` (or `.`) for the
selected input. Queries cannot access HTTP status/headers; omit `--query` to
inspect the wrapper. An empty query still bypasses projection. Querying is not a
universal security-redaction boundary: selected data can itself be sensitive.

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
There is no `max_attempts` option. `--async` validates this configuration but does
not poll or apply its timing settings. This replaces the earlier
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

Empty/whitespace API response bodies now produce `text: ""`, not the Python
`"(Empty)"` marker. An initial 202 means accepted, not completed. Do not repeat an
accepted POST to collect headers; it may start another job. Inspect existing
jobs in Fabric's monitoring hub or GET a known instance URL. For future intentional
submissions, `--show-headers` exposes the headers the service returns.

JSON is the sole data format. Raw initial HTTP errors remain inspectable
response data; authentication, transport, malformed-response, and LRO failures
are command failures. See the [complete compatibility contract](docs/compatibility.md)
before migrating scripts.

## Preview bundles

The manually dispatched **Rust unsigned unofficial preview** workflow runs the
same four-platform checks as CI and uploads seven-day workflow artifacts. It
does not create tags, publish GitHub releases, sign binaries, or use live identity
secrets. Extract the workflow artifact wrapper first, then verify its inner
target-qualified bundle and sidecars using the
[packaging instructions](docs/packaging.md). An archive is not a promise of zero
OS dependencies. Azure CLI remains an authentication prerequisite.

Official builds must originate in the trusted internal build system, not be
promoted from arbitrary PR artifacts. The
[complete public-safe ADO implementation handoff](docs/official-release-handoff.md)
is a specification, not validated YAML or a compliance certification.

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

Use `python` instead of `python3` on Windows if needed; scripts require Python
3.11+ for development only. `cargo fetch` restores all platform/build/dev
dependencies; the metadata boundary check then operates offline with all
features. Test behavior must use synthetic credentials/HTTP, never ambient
`az` or live services. Dependency restoration itself may contact registries.
See [CI and availability evidence](docs/validation.md) and
[native packaging commands](docs/packaging.md).

## Source provenance

The extraction baseline is upstream Microsoft Fabric CLI
`5b504195c63ef54c7382ca8e8a9d9759717f0ec5`. The selected Rust donor is
`f8cb97e92eab669785954335e0e7276d30a3377e` from this fork.
Reuse is file-allowlisted and adapted to this product slice, not a merge of the
broad donor migration. CI/scripts/packaging/public docs are newly authored for
the reduced graph; no donor migration tooling or broad release schema is reused.
Python source, packaging, and existing workflows remain independent.

The product is MIT licensed; binary bundles include the full product license
and dependency notices. Authentication/core review and upstream contribution
requirements still apply. Neither a green preview nor these documents establish
independent review, official-release approval, or signing compliance.
