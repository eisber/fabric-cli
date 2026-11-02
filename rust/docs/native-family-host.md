<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Central family composition

The experimental `fab-rs` root parser mounts the released local/configuration,
resource, access/catalog/raw API, TABLE and definition-transfer command schemas alongside
the existing read, job and optional authentication commands. These are the Python
names from source `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, not new namespaces.
Registration is not a successful command or live-service acceptance.

## Local commands

Supply an absolute `FAB_RUST_CONFIG_ROOT` for non-secret `config.json` and
`context-<session>.json`. `FAB_RUST_SESSION_ID` selects an explicit shell/session;
without it persistence is process-scoped. Named `version` needs neither selector
and does not create configuration. Configuration, `label list-local`, root
navigation, support descriptions, terminal clearing and the extension warning do
not construct credentials or a runtime. `pwd` retains its stderr output.

`label list-local` reads `local_definition_labels` before remote host setup.
Configuration remains usable without credentials even if the selected session
contains a remote navigation context. Browser and terminal effects occur only for
the corresponding explicit command. Tests inject those effects instead of opening
a browser or clearing the test runner's terminal.

The executable keeps complete metadata snapshots for one command only. There is
no persistent metadata cache. Clearing that cache never deletes settings, session
files or broker data. Remote command results keep their original cache effects
for embedded consumers; executable snapshots expire at command exit.

## Native read and job settings

On Windows, the native host also loads the explicitly selected
`FAB_RUST_CONFIG_ROOT` through the same local `ConfigStore::initialize` facility
before credential connection. This preserves existing default and `fab_` key
migration rules; it does not discover or import a Python profile. A selected
missing configuration file receives the existing local-family defaults, including
text output. An omitted root retains the read/job preview defaults: JSON,
hidden names off, name sorting, flat workspace listing and timeout cancellation
enabled. An empty or relative root, unreadable/malformed store, or invalid
consumed value fails; it is not silently replaced by preview defaults.

| Existing setting | Native consumer |
| --- | --- |
| `show_hidden` | `ReadConfig::show_hidden`; explicit `ls --all` still enables hidden names |
| `output_item_sort_criteria` | `ReadConfig::item_sort`, `byname` or `bytype` |
| `folder_listing_enabled` | Existing folder-aware read presentation |
| `output_format` | Read and all job leaves; explicit `--output_format` takes precedence |
| `job_cancel_ontimeout` | `JobConfig::cancel_on_timeout` for the existing synchronous run handler |

These settings retain the existing schema's string types and case-sensitive
allowed values. In particular JSON `false` is not the string `"false"`. Invalid
values produce a typed selection failure naming only the setting, not its value.
Configuration is a command-boundary snapshot, not mutable global state. Other
families still require their existing store/setup and use their own released
configuration consumers.

There are no saved job timeout or polling-interval keys in the pinned source.
`--timeout` and `--polling_interval` remain unchanged explicit arguments, including
the family's validation and signed-timeout errors. `cache_enabled` cannot create
a persistent cache in this command-local host; mandatory metadata hydration
remains unchanged. Debug, identity, provider, endpoint and HTTP request policies
are not selected by this mapping.

The pure selector and `native_settings` offline regressions track Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`:
`core/fab_constant.py` (keys/defaults), `core/fab_state_config.py`
(initialization), `parsers/fab_jobs_parser.py` (explicit wait arguments),
`commands/jobs/fab_jobs_run.py` (timeout cancellation), and
`utils/fab_cmd_job_utils.py` (integer elapsed accounting and poll overshoot).
Tests compose selected settings with the existing production read/job sessions,
synthetic HTTP, time and credentials. They cover saved hidden names, explicit
format precedence, completion, Unauthorized, local cancellation, timeout with
and without the cancellation POST, invalid values and local-store migration.
They do not establish native broker or live-service acceptance.

## Remote commands

`native-read` selects the same credential, single-thread runtime, native HTTP
client, shared executor, clock and cancellation boundary already used by reads
and jobs. It retains the explicit account/tenant/client selectors and silent
Windows credential host. Linux credential execution remains unsupported, not an
automatic alternate identity provider. No live authentication is established by
offline composition tests.

`fabric_cli::families::FamilySession` also accepts those existing dependencies
from an offline or embedded caller. Its context adapter asynchronously hydrates
the existing read snapshot before using the existing pure path resolver. Capacity
IDs use the released capacity API; OneLake properties use storage-audience
credentials and the released OneLake/shared HTTP adapter. Resource, access and
transfer behavior stays in its family library, including paging, reconciliation,
partial writes and accepted-operation uncertainty.

Family output retains original resource versus storage result/error types.
Optional mutable confirmation and resource-file capabilities are reborrowed for
each command; their caller-owned lifetimes do not extend command-local clients,
settings or metadata snapshots.
Transfer results retain completed files, actual create IDs and deferred import
cache effects. Delivery failure is not completion and never replays an operation.
An embedded host applies deferred import cache effects only after delivery.

TABLE delegates to `TableSession`, the existing context snapshot/resolver,
OneLake HTTP reads/properties, shared Fabric load reconciliation and generic
maintenance jobs. It retains original table/operation failures, observed IDs,
mutation uncertainty and warnings separately from output delivery. A bare 202
prints only the started message; cancellation and failed delivery never replay
the POST or imply rollback. Shared text output retains schema headers, job
key/value output and source progress on stderr. See the
[TABLE matrix](table-compatibility.md) for precise source and validation limits.

The optional authentication adapter and offline consumers use the same public
`session::run_auth`/`auth_commands` path. Its injected boundary delegates the
existing whole-login/sign-out operation, not a second broker engine. Offline
metadata/selection/stream proof does not establish native WAM, MFA, Conditional
Access or cross-platform authentication; see
[auth command evidence](auth-command-evidence.md).

The default definition file adapter uses the explicitly selected OS working
directory as its stable owned root. Import and export instead bind their exact
explicit path arguments and selected roots, including absolute paths
outside the working directory and Windows tilde expansion. Selection is lexical:
it does not canonicalize individual files or bypass traversal/reparse guards.
The lazy constructors do no filesystem I/O. Target/format validation and
Fabric-versus-local classification precede activation on the serialized worker,
so rejected remote imports never cause local source access and remote export
never activates its unused local candidate. Import owns a selected file parent
or directory; export requires an existing selected directory.
The asynchronous family capabilities
accept the CLI-owned `families::transfer_files::TokioTransferFiles`, which awaits
the existing runtime's blocking executor for root checks, bounded directory
enumeration, import preparation and ordered export writes. Native setup awaits
that adapter's construction on the current runtime and retains its typed native
or worker errors through the existing local-family error mapping. After capturing
the command result, the runtime owner waits at most one second for shutdown,
rather than waiting indefinitely for a blocked OS syscall. Cancellation and
watchdog diagnostics explicitly retain uncertainty about unfinished effects.
Cancellation is not syscall cancellation
or rollback; an in-flight write can still complete after its future is dropped.
The [transfer contract](../capabilities/deployment/fabric-cli-transfer/README.md#asynchronous-local-filesystem-boundary-td03)
records task/byte bounds, partial progress and the independent source pin.
Deployment mounts a separate zero-I/O `TokioDeployFiles` capability pinned to
the explicit configuration argument. Configuration, parameter and template reads
may select only paths supplied by trusted local documents. Repository preparation
seals further selections and returns a complete owned bounded snapshot before
remote work, including the legacy-to-generic fallback on the same checked root.
The adapter exposes no write or drive-wide repository capability. It shares the
serialized worker, stop checks and host/caller limits without changing import
or export state. No borrowed filesystem or worker guard escapes across awaits.
Original preflight failures and later partial/uncertain service effects remain
intact. Synchronous `LocalTransferFiles` remains for explicitly worker-owned
embedded callers, never as a blocking native async substitute.
Resource notices are buffered in source order and delivered with the final
output, so write failures remain observable. Resource and access confirmations
remain separate host capabilities; the transfer adapter does not supply them
implicitly.

### Terminal transfer UI

The Windows native host implements `TransferUi` with stdin answers and stderr
prompts, keeping stdout available for results. Confirmation accepts an entered
empty line as yes. Workspace selection displays configuration-sorted numbered
names: enter comma-separated numbers, `all`, or an empty line for no items.
Names retain their original values and display order; control characters are
escaped only in prompt labels. Export execution retains discovery order.
This is a line-based adapter, not an arrow-key or checkbox interface.

| Workspace export flags | Selection | Confirmation |
| --- | --- | --- |
| Neither flag | Rejected in command-line mode | Not asked |
| `--force` | Ask which items to export | Skipped |
| `--all` | Every supported item | Ask before export |
| `--all --force` | Every supported item | Skipped |

For single-item export, import and bulk export, `--force` skips the existing
confirmation prompts. Scripts should use explicit flags. Redirected stdin is
read as explicit input, never inferred consent. EOF, unterminated or invalid
answers, input limits and failed prompt delivery produce errors. An explicit
empty selection is distinct from cancelled or unavailable input.

Input runs on the existing runtime's blocking worker. The adapter releases stderr
before waiting for input, so cancellation and error output remain deliverable.
Dropping a prompt future cannot interrupt an OS read; bounded runtime shutdown
and cancellation uncertainty still apply. No REPL or authentication change is
implied.

### Terminal removal UI

The Windows native removal entry point supplies the existing resource
`RemovalUi` with fallible numbered selection and confirmation on the same
blocking-worker boundary. Workspace removal without `--force` selects supported
items and confirms before deleting only those items; it never deletes the
workspace. Forced workspace removal still reads and counts the complete item
catalog before deleting the workspace, without a selection or confirmation.
Tenant removal, including `--force`, displays `Select workspaces:` and always
requires explicit selection and confirmation. `Select items:` and
`Select workspaces:` use the same bounded line reader as transfer selection.
EOF, invalid input, failed prompt delivery and worker errors are not empty
selections or declines. Notices are flushed before input without retaining a
stderr lock while waiting. Partial outputs, cache effects and operation
uncertainty remain owned by the resource family; dropping input waits does not
interrupt an OS read or replay a deletion. These offline contracts do not prove
live-service or all-platform acceptance.

## Bounded gaps and evidence

The generic deployment CLI route, recursive transfer metadata and configuration
parameter inputs are implemented candidates, not a complete native validation
or live-service claim. The latest source-matched schema, lifecycle and profile
cases must pass before publishing a parity milestone.

ACL export, access/resource skill binding, virtual resources other than capacity, local-file
resource copy and folder SET metadata are not
implemented by this central adapter. These fail explicitly. Workspace SET uses
the existing typed metadata API; it does not claim the source-synchronous
workspace GET accepted-operation branch. REPL/batch and native
Windows/macOS/live-service acceptance remain separate work.

`local_mounted` executes actual subprocess contracts with isolated configuration
and no inherited credentials. `family_mounts` runs root argv through the same
central production session and shared HTTP adapter, asserting requests,
audiences, output/writer failures, file contents and uncertainty. Existing read,
job, family source comparisons and architecture contracts remain required.
Their presence and the 52-leaf inventory are not a measured passing percentage.
`table_mounted` adds real root TABLE HTTP, literal source request bytes, output,
malformed-value, cancellation and uncertainty contracts. `auth_commands` passes
root auth arguments through the shared production session with injected
credential effects and time. Only actual exact-head passing receipts may add
their canonical leaves to the numerator; test registration alone adds none.
