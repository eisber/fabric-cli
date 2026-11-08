<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI v2 local host

`fabric-cli-v2` adds the `fab-cli` executable without changing Python `fab`,
Rust `fab-rs`, their command grammar, or independent compatibility fixtures.
This is a bounded local-host implementation, not the complete Fabric CLI v2
resource surface or a production authentication/update support claim.

The selected group has exactly six operations:

```text
fab-cli cli configure /.config/<key> --value VALUE
fab-cli cli login /.auth --tenant GUID --account UPN [--client-id GUID] [--no-interactive]
fab-cli cli logout /.auth --tenant GUID --account UPN [--client-id GUID]
fab-cli cli report /.support/feedback --destination DESTINATION --title TITLE --message MESSAGE [--handoff]
fab-cli cli show /<workspace-guid>.Workspace[/<item-guid>.<SupportedType>] [--experience fabric|powerbi] [--uri-only]
fab-cli cli upgrade [--target-version SEMVER]
```

`fab-cli --version` reports the installed build without configuration or provider
access; the same flag is accepted at subcommand levels before required inputs.
Its spelling inside a value or after `--` remains data. There is no `cli rebind`,
`open`, `version`, `completion`, or `serve`
operation and no `client`, `local`, or `tool` group alias. Completion and MCP
deployment are not commands in this head. Unsupported resource roots fail;
this package does not emulate other v2 owners' pending resource operations.

## Offline structural validate

The separate root `validate` checks one explicitly supplied SQLDatabase DACPAC
definition JSON file. It leaves the six `cli` leaves and all v1 roots unchanged.
It consumes the existing neutral operation registration and strict SQLDatabase
codec, not another schema catalogue, validator engine or deployment plan.

```text
fab-cli validate --type SQLDatabase --native-format dacpac --rule-version 1.0 --schema
fab-cli validate --type SQLDatabase --native-format dacpac --rule-version 1.0 --input ABSOLUTE_FILE --max-bytes 4096 --max-decoded-bytes 4096 --max-part-bytes 4096 --max-parts 16 --max-path-bytes 1024 --max-depth 32
```

The numeric values above are **example caller budgets**, not defaults or provider
limits. All six positive limits are required for checking bytes; `--schema`
instead bypasses input acquisition and cannot be combined with input/budgets.
The media is explicitly advertised as `application/json` (`--media-type`).
`--native-format` is distinct from result `--format json|text`.

The input is a bare `{ "format": "dacpac", "parts": [...] }` object, not a wrapped
service response or the binary package itself. Omitted document format uses the
codec's documented DACPAC default; unknown formats are not coerced. Paths,
canonical Base64 and a nonempty primary are checked; package and `.platform`
semantics are not. The SDK report retains rule version, coverage, severity,
safe part-ordinal locations and the existing codec's first-failure policy.

Valid documents exit 0. Invalid documents exit 1 while preserving their
completed local check report (`data.valid: false`) on stdout. The envelope's
`succeeded` means the check completed, **not** that the document is valid.
Unsupported rules/formats, read/path errors, exhausted budgets, cancellation
and deadlines have no verdict, use stderr, and exit nonzero. Invalid command
arguments exit 2. Output failure never repeats the input operation.

Validate uses explicit result format or JSON and never reads configuration,
saved identity, environment target defaults, stdin, remote metadata or schemas.
It never calls authentication, HTTP, a model, SQL, a package builder, or
create/update. No new rule/schema acquisition is implied by `$schema` in input.
The existing `fabric-sdk-discovery` operation-input catalogue is unchanged.

Input must be an absolute ordinary local file beneath nonlinked directories.
Directories, traversal, Windows UNC/device paths, symlinks/reparse points and
Unix hard-linked files are rejected. Windows directory handles prevent ancestor
replacement and the file is held without write/delete sharing during reading.
Unix callers must own a stable directory namespace during acquisition; handle
identity checks do not attest hostile concurrent ancestor retargeting. The host
does not attest mount/cloud-placeholder policy: callers must supply materialized
files on an approved local filesystem. No remote download fallback is attempted.
A 30-second total host deadline is checked at input-read boundaries and before
publishing the bounded synchronous codec result; it does not remotely cancel
anything or preempt a native parser in the middle of an admitted document.

See the [SDK structural profile](../../../sdk/sql/definition/README.md#local-structural-validation)
for pinned API/source provenance and exact nonclaims. Other types/native grammar,
SQL project compilation, complete report/model semantics and live probes remain
unsupported, not successful empty finding lists.

## Results and host effects

The default is one newline-terminated JSON envelope on stdout. `--format text`
uses the existing shared key/value renderer. Errors use stderr and are nonzero.
Invalid arguments exit 2 without echoing rejected supplied values. Help and
version run before settings, credentials, metadata, or installation setup.

The local-host envelope has `schemaVersion: "0.1"`, `command`, `status`, `data`,
`effects`, and `error`. It does not replace SDK result types or claim to settle
every future v2 resource schema. Effect evidence survives output failure where
stderr remains writable; the operation is never replayed to repair output.

`prepared` is not submitted. `handoff` requires a caller action. `accepted` means
the requested submission was acknowledged, not that installation or another
downstream action completed. Partial and unknown outcomes are nonzero. A timeout
or cancellation is not rollback, remote cancellation, or permission to replay.

Libraries accept caller-owned control and effect adapters. The synchronous
local host supplies a total deadline; terminal termination does not undo a file
write or browser launch. Authentication and update hosts retain their existing
bounded cancellation/cleanup semantics.

## Local preferences

Only configure writes preferences. `--config-root ABSOLUTE_DIRECTORY` selects
an isolated explicit root. Otherwise the host selects
`%LOCALAPPDATA%\Microsoft\FabricCliV2` on Windows, or
`$XDG_CONFIG_HOME/microsoft/fabric-cli-v2` / `$HOME/.config/microsoft/fabric-cli-v2`
on Unix. Missing or relative platform roots fail. No v1 config or auth selection
is discovered or migrated; help, show and report do not initialize settings.
When `--format` or show's `--experience` is omitted, the head reads its declared
presentation preference without creating a root, lock, defaults or migrations.
Explicit options win; both explicit presentation options let show bypass the
store entirely. Corrupt settings are errors, never silently replaced defaults.
Help/version and compiled-out or unsupported-platform providers do not read
the store.

The advertised preference allowlist is `output_format`, `default_open_experience`,
`output_item_sort_criteria`, `show_hidden`, `folder_listing_enabled`,
`debug_enabled`, and `cache_enabled`. Values are validated before filesystem
effects. Explicit invocation options remain authoritative. Format and viewer
experience are consumed by this head; sort/hidden/folder/debug/cache preferences
are stored for their explicitly supported resource-host consumers, not applied
as hidden remote target or authority defaults. Configure's own response uses
explicit `--format` or JSON and never requires a second preference read.

Identity/workspace/capacity/ARM defaults, plaintext encryption fallback,
cancel-on-timeout and arbitrary remote settings are not writable through this
surface. Existing unrelated keys are preserved, not interpreted as authority.

## Viewer

Show initially supports explicit GUID-addressed workspace/item resource paths
with known source-backed Fabric viewer routes. Names, ambiguous targets,
representations, folders, tables, files and unknown types are not guessed or
resolved by hidden live calls. Each actual supported route is tested.

`--experience fabric` is the declared default; `powerbi` changes the URI only.
No token or account hint is appended. The browser has its own authorization
context; opening a URI neither transfers CLI credentials nor establishes that
the browser authenticated or displayed the resource.

`--uri-only` returns an explicit handoff without launching anything. Otherwise
the explicitly invoked system-browser adapter uses separate process arguments.
The result reports only the actual launcher observation, never successful
display. Missing/failed/possibly-started launchers have distinct failure evidence.
`--timeout-seconds` bounds local launch work, defaults to 30 and accepts 1-600.

## Login and logout

The optional `windows-broker` feature uses the existing coordinated Windows
provider. This profile requires explicit account and tenant, with the existing
registered public-client identifier as the declared client default. It does not
select from environment variables, saved bindings, or an account picker, and it
does not add a v2 plaintext token/selection cache.

Login acquires Fabric, Storage/OneLake, then ARM before publishing success.
`--no-interactive` forbids broker UI; generic failures do not select a different
provider. Logout invalidates the selected application's coordinated credentials
and signs out its account on this device. It does not delete the OS account,
clear browser cookies, revoke already returned tokens globally, or reset v1
settings. `--timeout-seconds` defaults to 120 and accepts 1-600.

Without the feature or on an unsupported platform, authentication fails
explicitly. Synthetic broker tests do not establish MFA, Conditional Access,
registration, guest-account, native cleanup or live service acceptance.

## Feedback

Report prepares an exact caller-supplied title/message draft. Destinations are
`github:OWNER/REPO`, `ideas`, and `security`. `--handoff` supplies the actual
provider's manual route and submission instructions; it does not open a browser,
send a request, upload diagnostics, or return a submission receipt.

The title is limited to 256 UTF-8 bytes and the message to 16,384 UTF-8 bytes.
No logs, screenshots, environment, tenant data, repository configuration,
credentials or local history are collected. Payload text is not put in a
prefilled URL. GitHub repository visibility is unverified; a confidentiality
label cannot make a public issue private.

Automated feedback delivery is **not advertised or implemented by this profile**.
A future sender needs an actual independently injected provider credential,
destination/visibility policy, exact-payload approval and durable uncertain-send
reconciliation. Microsoft security reports use the repository's Microsoft
security route, not public GitHub or Ideas.

References: [Fabric feedback](https://learn.microsoft.com/en-us/fabric/fundamentals/feedback),
[GitHub issue creation](https://docs.github.com/en/rest/issues/issues#create-an-issue),
[Microsoft security reporting](https://aka.ms/SECURITY.md).

## Installation update

The optional `self-update` adapter reuses the existing signed update engine and
its custody/helper protocol. Bare upgrade requests installation; it is not
silently replaced by an inspection-only check. Only canonical target versions
in the supported signed channel are eligible; a target version does not grant
major-version upgrade permission.

Missing trust, a non-v2 variant, unknown/managed custody, or unavailable installed
helper fails before an unsafe installation. The v2 host must not install a v1
binary merely because both belong to the Rust CLI publication family. Helper
submission, installed bytes, receipt completion, restart and health are separate
facts. The session lifetime stays owned by the outer host until output and
diagnostics finish.

No signing key, trusted release, installation receipt, privilege, or signing
account is created by this package. Native signing/notarization and production
release/platform acceptance remain release-owner prerequisites. This repository
does not claim those inputs exist or that a live installation was exercised.
See the adapter's public API and the existing
[updater protocol](../update/README.md).

Standalone v2 and v1 installations must use separate installation directories.
The reused protocol intentionally keeps the existing `.fab-rs-install.json` and
`fab-rs-update-helper` sibling names; its executable/variant pins are single-tenant
per directory. A receipt for another executable or variant fails closed rather
than being shared or overwritten by this adapter.

## Offline validation

Use an explicitly allocated shared-host validation slot. Keep fixtures in an
absolute project-owned `FAB_CLI_V2_TEST_ROOT`, never a real user profile.

```text
cargo test --offline --locked -p fabric-cli-v2
cargo test --offline --locked -p fabric-cli-v2 --features windows-broker
cargo test --offline --locked -p fabric-cli-v2 --features self-update
cargo test --offline --locked -p fabric-cli-v2 --features windows-broker,self-update
cargo clippy --offline --locked -p fabric-cli-v2 --all-targets --all-features -- -D warnings
```

Tests use synthetic roots, injected launchers/brokers/transports and trusted
test-only update fixtures. They do not authenticate, access credential stores,
launch a browser, send feedback, download a production release or run an installer.
The dependency-impact graph includes all three optional feature profiles;
unexecuted OS/architecture acceptance is not inferred from compilation.
