<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit native read host

Python source pin: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Prerequisites: published `e218c981bc5f27712084ef662cae49548f4fc50c`
plus the explicitly integrated context, text and Windows identity checkpoint
`9c57018e7b703a03f5c7442dc3f98d498412c3b6`.
This is a bounded executable host, not full `ls`/`get`, live-service, installation
or cross-platform acceptance.

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

After configuration checks, the host silently acquires **Fabric only** for the
explicit Windows account. This is read connection/refresh, not login: it cannot
open sign-in UI or request unrelated OneLake/ARM consent. The separate explicit
`auth login` continues acquiring all three audiences in source order.
Interaction-required, account mismatch, unavailable provider, cancellation and
other broker failures retain distinct sanitized diagnostics.

The CLI owns one current-thread Tokio runtime, Ctrl+C handling and an overall
240-second command deadline. Ctrl+C is exit 2; timeout and setup failures are
nonzero diagnostics on stderr. Dropping credential/HTTP futures requests
cooperative cancellation, not rollback or guaranteed native cleanup completion.
Ordinary SDK/path/query failures retain the existing command envelopes: JSON on
stdout, Unauthorized exit 4 and other command failures exit 1.
Writer and rendering failures never become successful results.

The fixed REST origin is `https://api.fabric.microsoft.com`; TLS uses platform
trust with no added roots. The host selects direct connections, **not** proxy
environment discovery or a fallback backend. Redirect, retry, paging, byte/row
limits and continuation-origin policy belong to the existing transport/shared
executor/SDK. No duplicate policy is introduced. MSIT portal URLs do not establish
a different REST hostname.

## Complete bounded composition

`ReadSession` is the runtime-neutral production invocation path shared by the
native binary and offline executable consumer. It asynchronously loads complete
SDK metadata scopes, then reuses the pure `Resolver`/`Catalog`, compact
`FolderGraph`, `ReadHost`/`FolderReadHost`, `PythonQuery` and output library.
Synchronous `Lookup` never performs network I/O. A missing scope is an internal
request to load metadata, not a service `NotFound` or successful empty snapshot.
At most 128 scope-loading passes are allowed for each resolution.

Supported branches include root workspace LS/dir (including paging, long capacity
enrichment and hidden collection names), named workspace flat item LS,
folder-context LS, source-routed supported item metadata GET and enriched
workspace metadata GET. Current/home, quoted/tokenized paths, queries, known-type
filtering and ordering reuse the existing contracts. Default workspace presentation
is flat; library callers may explicitly select folder presentation in `ReadConfig`.
Discovery and handler reads are distinct command-local snapshots: folder path
discovery can therefore list folders again during rendering. This is not Python
TTL-cache/request-count parity. A failure on any page never emits partial rows as
complete success.

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
