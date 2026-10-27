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

The native definition file adapter uses the explicitly selected OS working
directory as its stable owned root. Paths outside that root fail rather than
silently expanding filesystem authority. Native confirmation and workspace export
selection need an injected UI; use the corresponding explicit force/all options
for supported noninteractive branches. Resource notices are buffered in source
order and delivered with the final output, so write failures remain observable.

## Bounded gaps and evidence

Full `deploy --config`, ACL export, access/resource skill binding, recursive
folder transfer metadata, virtual resources other than capacity, local-file
resource copy, workspace removal selection and folder SET metadata are not
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
