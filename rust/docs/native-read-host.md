<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit native read host

Python source pin: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Prerequisites: published `e218c981bc5f27712084ef662cae49548f4fc50c`
plus the explicitly integrated context, text and Windows identity checkpoint
`9c57018e7b703a03f5c7442dc3f98d498412c3b6`.
This is a bounded executable host, not full `ls`/`get`, live-service, installation
or cross-platform acceptance.

The same explicitly selected host also composes all eight source `job` leaves;
see [native job composition](native-job-reads.md) for their bounded branches,
production/offline boundary and explicit remaining API gaps. Mutations use
the released `JobHost` with the same credential, metadata and runtime setup.
Interactive removal confirmation requires an injected UI; native removal
currently requires explicit `--force`, never implicit consent.

## Selection and execution

Default and `windows-broker`-only builds retain `NotConfigured` for reads.
`cargo build --locked -p fabric-cli --features native-read` opts into the
Windows broker and existing Hyper/Rustls transport. The SDK still has no default
credential or transport dependency. On other platforms the host fails explicitly;
it never tries another provider.

Native reads require **all three** non-secret environment selectors:
`FAB_RUST_TENANT` (public-cloud tenant GUID), `FAB_RUST_ACCOUNT` (explicit UPN),
and `FAB_RUST_CLIENT_ID` (approved desktop public-client GUID).
No client ID or identity is inferred for reads. Missing selectors return
`NotConfigured`; invalid selectors return a sanitized configuration failure.
Help/version and malformed arguments are handled before reading host selectors
or contacting credentials. Token/secret arguments and static-token environment
fallbacks are not supported.

`FAB_RUST_CONTEXT` optionally supplies an absolute Fabric path, default `/`.
`FAB_RUST_HOME` optionally supplies a full `Workspace`/`Personal` name, such as
`Demo.Workspace`. There is no implicit personal-workspace or persisted current
context discovery. Omitted home fails explicitly when `~` is used.
No Python configuration/cache is read or written.
An explicit `FAB_RUST_CONFIG_ROOT` selects the existing Rust local-family
settings snapshot, including read display and output defaults; an omitted root
retains JSON preview defaults. See the [native settings mapping](native-family-host.md#native-read-and-job-settings).

After configuration checks, the host silently acquires **Fabric only** for the
explicit Windows account. This is read connection/refresh, not login: it cannot
open sign-in UI or request unrelated OneLake/ARM consent. The separate explicit
`auth login` continues acquiring all three audiences in source order.
Interaction-required, account mismatch, unavailable provider, cancellation and
other broker failures retain distinct sanitized diagnostics.

The CLI owns one current-thread Tokio runtime, Ctrl+C handling and an overall
240-second read command deadline. Synchronous jobs with an explicit finite
timeout use the [derived job watchdog](native-job-reads.md#command-watchdog-versus-job-timeout)
so family timeout/cancellation policy can finish. Ctrl+C is exit 2; timeout and setup failures are
nonzero diagnostics on stderr. Dropping credential/HTTP futures requests
cooperative cancellation, not rollback or guaranteed native cleanup completion.
The host captures the original command result before a bounded, one-second
runtime shutdown wait. A running blocking filesystem call can outlive that wait
and still finish with effects; cancellation and watchdog diagnostics state that
uncertainty explicitly. The grace does not extend the family job timeout or
certify that mutation work stopped.
Ordinary SDK/path/query failures retain the existing command envelopes: JSON on
stdout, Unauthorized exit 4 and other command failures exit 1.
Writer and rendering failures never become successful results.

The default REST origin remains `https://api.fabric.microsoft.com`.
The optional `FAB_API_ENDPOINT_FABRIC` selector supplies a hostname, not a URL,
using the shared [endpoint policy](endpoint-context.md). For example,
`msitapi.fabric.microsoft.com` and `dailyapi.fabric.microsoft.com` select those
exact HTTPS hosts while retaining the Fabric token scope
`https://api.fabric.microsoft.com/.default`. Only the binary reads the selector;
SDK libraries continue to accept explicit immutable configuration.
An empty, non-Unicode or invalid hostname fails with sanitized
`invalid_configuration` before identity selectors, credentials or network access.
No arbitrary origin, public-host fallback or token-audience inference is added.
Help/version and argument errors still precede endpoint configuration.

The same binding configures SDK workspace/item reads, folder discovery,
long-LS capacity enrichment, the shared HTTP executor and source-synchronous
workspace GET initial/poll/result requests. Active continuation/result links
cannot switch back to public or another Microsoft host. Non-public native
endpoints currently support only `ls`/`dir`/`get`; other remote families fail
explicitly with `NotSupported` before credentials instead of routing silently
to their not-yet-migrated public defaults. Unset/public configuration preserves
existing family behavior. This is not a migration of every SDK family or any
new live-operation permission.

TLS uses platform trust with no added roots. The host selects direct connections, **not** proxy
environment discovery or a fallback backend. Redirect, retry, paging, byte/row
limits and continuation-origin policy belong to the existing transport/shared
executor/SDK. No duplicate policy is introduced. A portal URL is not an endpoint
selector; the caller must explicitly supply the eligible REST hostname.

## Complete bounded composition

`ReadSession` is the runtime-neutral production invocation path shared by the
native binary and offline executable consumer. It asynchronously loads complete
SDK metadata scopes, then reuses the pure `Resolver`/`Catalog`, compact
`FolderGraph`, `ReadHost`/`FolderReadHost`, `PythonQuery` and output library.
Synchronous `Lookup` never performs network I/O. A missing scope is an internal
request to load metadata, not a service `NotFound` or successful empty snapshot.
At most 128 scope-loading passes are allowed for each resolution.

The native host and `read_session_probe` explicitly select
`ReadSession::with_source_operations(&executor)`. This borrowed wrapper uses
that same `HttpExecutor` and its existing timer through `OperationExecutor`.
Workspace primary GET selects the shared `PythonSynchronous` policy before
the existing three enrichment steps. A remaining non-200 primary response,
including headerless 202, emits no result or enrichment requests; it does not
fabricate completed metadata. Completed 200 metadata is validated and handed to
`enrich_workspace_metadata`, which reuses the existing optional-read policies.
Operation failures use the existing borrowed CLI formatter without normalizing
its message a second time.
Only that explicit GET operation opts into bounded opaque operation IDs:
one to 256 ASCII letters, digits, hyphens or underscores. This preserves IDs
such as `op` without rewriting them to synthetic GUIDs; unsafe path/query,
origin and response inputs remain errors. Default operations and strict SDK
metadata consumers do not acquire this compatibility mode.

Default `ReadSession::dispatch`, its public fields and custom `ReadExecutor`
implementations keep their existing contracts. In particular, the generic SDK
metadata API still rejects accepted primary responses rather than installing
operation polling implicitly. No new executor supertrait, runtime or transport
is required by old library consumers. Dispatch retains the original
current-thread lookup contract; it does not promise a Send future.

Supported branches include root workspace LS/dir (including paging, long capacity
enrichment and hidden collection names), named workspace flat item LS,
folder-context LS, source-routed supported item metadata GET and enriched
workspace metadata GET. Current/home, quoted/tokenized paths, queries, known-type
filtering and ordering reuse the existing contracts. Default workspace presentation
is flat; library callers select folder presentation in `ReadConfig`, and native
callers can select the existing `folder_listing_enabled` local setting.
Discovery and handler reads are distinct command-local snapshots: folder path
discovery can therefore list folders again during rendering. This is not Python
TTL-cache/request-count parity. A failure on any page never emits partial rows as
complete success.

Absolute/root and explicitly configured home targets resolve from the tenant
without first discovering the unrelated current workspace, item or folder.
Relative and omitted paths still reconstruct the supplied current path.
The current path must remain absolute even when the target does not use it;
tenant/audience checks and cancellation remain unchanged.

JSON is the native preview default. Explicit `--output_format text` uses the
shared renderer with no colors and fixed pure-renderer terminal defaults.
ASCII table width/key/repr properties are supplied; non-ASCII requiring missing
Unicode properties fails explicitly. JSON has no such text-property restriction.
This is not full terminal/Unicode parity.

Definitions, exports, `--verbose`/`--force`, OneLake and hidden-collection reads,
folder GET, unsupported item types, service-principal/managed identity, sovereign
clouds, persisted sessions/context, proxy configuration, batch/REPL and the other
command families remain unsupported. Listing hidden names is not reading those
resources. Existing SDK safety and workspace-enrichment omission contracts remain
unchanged; metadata-helper parity is not full specialized GET parity.

## Offline evidence versus live setup

`read_session_probe` is a separate development executable with synthetic HTTP,
credential and clock injected into the **same production `ReadSession`** and real
shared `HttpExecutor`. It is not a shipped fake mode. Its consumer asserts actual
requests, scopes, query/output streams and exits; native `fab-rs` subprocess
contracts separately cover help, malformed arguments and missing/invalid native
configuration. Existing source-pinned read, query, folder, workspace GET, extended
context and text comparisons stay independent and unchanged.

No actual native sign-in or Fabric read was performed for this implementation.
Before live proof, the registration owner must verify that the selected client
permits the requested scopes and broker redirect
`ms-appx-web://microsoft.aad.brokerplugin/<client-id>`.
Readiness review and separate owner-approved live scope are required. If silent
read acquisition needs interaction, an authorized operator must explicitly run
`auth login` with the same tenant/account/client selectors before retrying reads.
The host does not change registrations, consent, policy, trust or service data.

### Source-synchronous workspace GET evidence

The bounded GET202 recovery was exercised on October 26, 2026, from exact
`c72860db65e821ada2ec3cbac8f34c4bc7458042`, using Rust 1.99.1 on
`x86_64-pc-windows-msvc`. The actual `read_session_probe` matched all 15 workspace
observations frozen independently at
`5ff98d8360cbd6f2939790b3aa8c7f2b4cd90781` and
`075375250875c0c1b0a43de5993c4c673dd9d0c1`, including both previously failing
202 cases. The comparator asserted exit, exact stdout/stderr, request order and
Fabric scope calls. It did not compare Python return objects, sleeps, events,
files or request parameters.

The same production-session executable passed its existing 15 compositions.
Affected-package tests/doctests, the complete existing read parity runner,
12 operation, 44 error and 290 continuation comparisons, strict default and
native-feature all-target Clippy, native-feature CLI tests and formatting passed.
Existing SDK primary-202 rejection and default `ReadSession` behavior remained
covered. All 218 external registry lock records and 1,796 protected Python,
oracle, workflow, root manifest and toolchain files remained byte-identical.
The default CLI graph still excludes the broker and native transport; the SDK
graph excludes CLI and identity implementations.

Private receipts `gate-get202-01.json` and `gate-get202-02.json` retain the actual
outcomes. The first batch passed every product check but failed its final
artifact-only graph assertion, which incorrectly prohibited ordinary
`windows-sys` dependencies along with the broker. The corrected custody-only
check passed in the second receipt; this is not a single all-green batch.
No product code or protected expectation changed for that correction. No live
service, sign-in, Linux/macOS execution, remote CI or broader command-family
acceptance is claimed by these local checks.

### Separately approved live setup

After that separate approval, the operator setup is:

```powershell
$env:FAB_RUST_TENANT = "<approved-tenant-guid>"
$env:FAB_RUST_ACCOUNT = "<approved-account-UPN>"
$env:FAB_RUST_CLIENT_ID = "<verified-public-client-guid>"
cargo run --locked -p fabric-cli --features native-read -- auth login --tenant $env:FAB_RUST_TENANT --account $env:FAB_RUST_ACCOUNT --client-id $env:FAB_RUST_CLIENT_ID
cargo run --locked -p fabric-cli --features native-read -- ls /
```

The first command is explicit live authentication and may show UI; the second
is a real Fabric read. Neither is run by the offline validation suite.

## Executed local evidence

Final targeted checks ran on October 26, 2026 on
`x86_64-pc-windows-msvc`, Rust 1.99.1 (`58cf8f9c3`), using the common serialized
validation gate, a private target, one build job and one test thread.

| Boundary | Observed result |
| --- | --- |
| Native-feature CLI/identity | 100 tests passed, including 15 session consumers, nine executable contracts and the home-selector escaping/case regression; strict all-target Clippy passed |
| Offline production-session executable | All 15 compositions passed: named workspace/item/folder discovery, enriched workspace GET, current/home, queries, long/hidden data, service/paging/cancellation failures and JSON/text streams |
| Optional Hyper/Rustls transport | 20 existing tests passed, including actual offline wire consumers |
| Protected source runner | All 16 stages passed with default and broker-only feature builds; existing exact/safety/snapshot/unsupported classifications remain distinct |
| Extended context/text | 93 extended source cases and 14 negative guard checks passed; actual Rust extended example ran; all 83 Rust text observations matched source and three text guard tests passed |
| Earlier default checkpoint | Full workspace tests/doctests, strict workspace all-target Clippy and broker-only CLI/identity tests passed before the final native-only home-selector change |
| Custody and graph | 1,682 protected source/oracle/manifest/toolchain blobs byte-identical to the prerequisite; all 218 external dependency records unchanged; only internal CLI-to-transport edge added; default CLI graph excludes native identity and transport |

Final code evidence is retained in `native-final-log-5.txt`: broker source runner,
native tests/Clippy, transport tests and all 15 executable cases passed. That
attempt's **receipt remains failed** because the artifact-only custody script
resolved Git paths relative to `rust/`, then tried to read them from the repository
root. The script was corrected to run Git at the repository root; the subsequent
`release-proof-receipt-6.json` completed custody, formatting and toolchain checks.
No product code or protected expectation changed for that correction. Together
these are local bounded evidence, not a single all-green receipt or remote CI.

Earlier failed/stopped attempts remain preserved: a new synthetic cancellation
driver omitted its runtime wake; a new executable test was nested; a new-file
Black check needed formatting; an artifact script selected a nonexistent context
test filename; and one gate wait expired without starting validation. Only new
tests or artifact drivers were corrected, never an existing oracle.
The renewed final grant used approximately five minutes and 29 seconds of acquired
gate time across attempts five and six. No live account, broker-token acquisition
or Fabric request was exercised, and no Linux/macOS or remote native-CI result is
claimed.
