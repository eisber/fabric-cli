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
```

Recognized Fabric/ARM operations poll **by default**. `--async` returns the actual
initial response, without polling or starting background work. Polling has a
fixed 600-second budget and a 1-second doubling fallback, honors server
`Retry-After`, and has no tuning flags. Ctrl+C cancels local waiting, not the
backend operation; do not blindly repeat an uncertain mutation.

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
