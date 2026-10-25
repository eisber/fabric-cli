<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Complete Python command inventory and Rust implementation gaps

## Source and measurement

Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust assessment: published `a23b4deda07011ea36bb3a635f75266b14b35c80`,
October 25, 2026. Python source is unchanged at this checkpoint. Later additive
integration records below update individual library capabilities, not this
original executable assessment.

The authority is `core/fab_parser_setup.py::create_parser_and_subparsers` and
the registration functions it actually calls, not the help dictionary, command
enum, Swagger, endpoint count or a remembered conversation.
[`inventory.json`](../tests/compatibility/commands/inventory.json) records every
canonical path, alias, source file/line, handler and explicit argument shape,
including cardinality, choices, required flags and defaults.
[`inventory.py`](../tests/compatibility/commands/inventory.py) extracts it using
only the standard-library AST, without importing product code or credentials.

There are **32 canonical root registrations: 26 leaves and six groups**.
The groups contain **26 subcommand leaves**, giving **52 canonical leaves**.
There are **11 alias paths**, not eleven additional implementations. The six
group-only invocations display help and are separate behavior contracts.
Automatic help, flags, batch and REPL modes are additional surfaces below.
These counts are a command denominator, **not a full behavioral denominator**:
each path has resource, option, mode, output and failure variations.

## Complete command table

Every leaf below remains unavailable as a successful live command in the
published executable. `ls`/`dir` and `get` parse but return `NotConfigured`;
other leaves are not registered there, including Python's `version` leaf.
Rust `--help`/`--version` work but are preview contracts, not Python equivalence.

`Partial` means reusable library behavior exists with bounded offline evidence,
not that the command or every resource type is complete. `Missing` means there
is no corresponding end-to-end command handler at the assessed checkpoint.
Crates marked **new** are assigned implementation boundaries, not existing
placeholder crates or claims of delivered code.

| Python command | Alias | Existing Rust library / remaining gap | Crate ownership |
| --- | --- | --- | --- |
| `ls` | `dir` | Partial workspace/item/capacity/folder reads; missing other hidden collections, OneLake files/tables, live context and full text output | Existing `fabric-cli`, `fabric-sdk-{workspaces,items,capacities,folders}`; new OneLake/catalog SDKs |
| `get` | - | Partial ordered item/specialized/workspace metadata and projection; missing other resources, complete enrichment and live host | Existing read host and focused SDKs |
| `cd` | - | Partial immutable path/context resolver; missing command persistence and interactive state | Existing `fabric-cli-context`; new `fabric-cli-local` |
| `pwd` | - | Partial context types; missing command/output/session integration | `fabric-cli-context`, new `fabric-cli-local` |
| `exists` | - | Partial lookup primitives; missing command, resource/error truthiness and OneLake branches | Existing focused SDKs/context, new OneLake SDK |
| `open` | - | Missing source URL selection and injected platform browser handler | New `fabric-cli-local`, existing context/SDK |
| `mkdir` | `create` | Partial Notebook create in SDK; missing full workspace/item/folder/file/hidden-resource routing and command | Existing focused SDKs; new OneLake SDK and `fabric-cli-mutations` |
| `rm` | `del` | Missing full resource deletion, selection/prompts, hard/recursive modes and cleanup | Same mutation owners |
| `mv` | `move` | Missing cross-workspace/item/file move, recursive traversal, confirmations and reconciliation | Mutation owners plus deployment/definitions |
| `cp` | `copy` | Missing item/file recursive copy, overwrite and path-collision behavior | Mutation owners plus deployment/definitions |
| `set` | - | Missing property-specific mutations and dispatch; read metadata is not a setter | Existing focused SDKs and `fabric-cli-mutations` |
| `ln` | `mklink` | Missing OneLake and external shortcut requests/types/overwrite behavior | New `fabric-sdk-onelake`, mutation CLI |
| `start` | - | Missing capacity ARM and mirrored-resource lifecycle; distinct from `job start` | Existing capacity SDK, new resource lifecycle SDK |
| `stop` | - | Missing capacity/mirrored-resource stop and completion handling | Same lifecycle owners |
| `assign` | - | Missing capacity/domain workspace assignment and routing | Existing workspace/capacity SDKs; new catalog SDK |
| `unassign` | - | Missing capacity/domain unassignment and routing | Same assignment owners |
| `export` | - | Missing full item-definition retrieval, transforms and filesystem output | Existing `fabric-deploy`; new `fabric-cli-transfer` |
| `bulk-export` | - | Missing recursive workspace/folder export, structure and binding preservation | Same transfer owners |
| `import` | - | Partial Notebook preparation/create/update/LRO; missing complete CLI and item-type branches | Existing `fabric-deploy`/SDK; transfer CLI |
| `deploy` | - | Partial native single-Notebook library; missing full Python command/config/planner/publication orchestration | Existing `fabric-deploy`; transfer CLI |
| `clear` | `cls` | Missing terminal/platform command; not a Fabric API | New `fabric-cli-local` |
| `find` | - | Missing OneLake catalog search/filter/paging and command output | New `fabric-sdk-catalog`, `fabric-cli-resources` |
| `desc` | - | Missing support-matrix report, extension/path and `all` behavior | New `fabric-cli-local`, existing context |
| `api` | - | Partial shared HTTP foundation only; missing raw request CLI and explicit audience/origin policy for Fabric/storage/ARM/Power BI | Existing `fabric-sdk-http`/transport; new `fabric-sdk-api`, resource CLI |
| `extension` | - | Python itself emits unsupported warning; preserve it, do not invent a plugin loader | New `fabric-cli-local` |
| `version` | - | Missing Python-named leaf; Rust version flag is not the same registration | Existing CLI dispatcher, local CLI |
| `auth login` | - | Explicit unsupported factory on shared branch; experimental optional Windows broker forward-port `d8ae051` submitted separately | Existing `fabric-identity`, `fabric-identity-windows`, CLI auth |
| `auth logout` | - | App-scoped logout in unintegrated experimental lane; missing integrated/default/other-backend parity | Same identity owner |
| `auth status` | - | Silent status in unintegrated experimental lane; missing integrated complete backend coverage | Same identity owner |
| `config set` | - | Missing typed config defaults/validation/persistence command | New `fabric-cli-local` |
| `config get` | - | Missing config lookup and output command | Same local owner |
| `config ls` | `config dir` | Missing ordered config listing/defaults and output | Same local owner |
| `config clear-cache` | - | Missing scoped cache invalidation; must not delete broker credentials implicitly | Same local owner with identity boundary |
| `acl ls` | `acl dir` | Missing resource-specific ACL enumeration, projection and paging | New `fabric-sdk-access`, resource CLI |
| `acl get` | - | Missing principal/resource-specific ACL lookup | Same access owner |
| `acl set` | - | Missing principal/role validation, prompts and mutations | Same access owner |
| `acl rm` | `acl del` | Missing ACL removal, errors and confirmations | Same access owner |
| `label set` | - | Missing local label lookup plus service application/permissions | Same access owner plus local config |
| `label rm` | `label del` | Missing sensitivity-label removal | Same access owner |
| `label list-local` | - | Missing local label definitions/output; registered even though easy to omit from summaries | Same access/local-config owners |
| `job start` | - | Partial typed library submission/accepted handle and source output; binary host integration missing | Existing `fabric-sdk-jobs`, `fabric-cli-jobs` |
| `job run` | - | Partial real status/wait/timeout/cancel policy and input conversion; text/front-door and numeric boundaries remain | Same jobs owner |
| `job run-list` | - | Partial instances/schedules collection with shared `fabric-sdk-paging`; binary consumer missing | Same jobs owner |
| `job run-cancel` | - | Partial accepted cancellation and actual status wait; generic-operation reconciler wiring missing | Same jobs owner |
| `job run-sch` | - | Partial source schedule creation/validation library handler; host integration missing | Same jobs owner |
| `job run-update` | - | Partial GET-before-PATCH schedule update library; host integration missing | Same jobs owner |
| `job run-rm` | - | Partial confirmed/forced schedule deletion library; host integration missing | Same jobs owner |
| `job run-status` | - | Partial run/schedule status and source-shaped output library; host integration missing | Same jobs owner |
| `table schema` | - | Missing Lakehouse table schema/format contract | New `fabric-sdk-tables`, `fabric-cli-tables` |
| `table load` | - | Missing source/file validation, load submission/completion and output | Same table owner plus OneLake boundary |
| `table vacuum` | - | Missing retention/dry-run behavior, submission and completion | Same table owner |
| `table optimize` | - | Missing optimize/V-Order/Z-order request and completion semantics | Same table owner |

### Groups, modes and options are not optional extras

- `auth`, `config`, `acl`, `label`, `job`, `table` with no subcommand have
  source help behavior. Nested parser error/help handling also needs comparison.
- No-command starts the Python REPL. Preserve prompt/context changes, history,
  completion, `help`, `exit`/`quit`, cancellation, error recovery and shutdown.
  The current Rust no-argument error is not parity.
- Root `-c`/`--command` is repeatable batch execution; command splitting,
  separators, summaries and exit-code propagation are observable.
- Root `-v`/`--version`, automatic `-h`/`--help`, custom `-help`, per-parser
  `--output_format {json,text}` and hidden `--skill` belong in acceptance.
  Rust's preview `-V` spelling does not cover Python's `-v`.
- Positional token cardinality and attached/separate/repeated options,
  quoting, abbreviation and platform aliases must come from registration and
  actual source dispatch, not an idealized Clap schema.
- Auth login includes interactive, username/password service principal,
  managed identity, certificate, federation and `--azure-cli`. The approved
  no-az/Python dependency and explicit identity rules are intentional product
  constraints; unavailable methods must be explicit gaps, not silent fallbacks.
- Resource support in `core/fab_config/command_support.yaml`, individual
  handler dispatch and hidden collections multiply the cases behind each row.
  A generic item CRUD endpoint does not finish an entire filesystem verb.

## Implementation waves and ownership

Keep `fabric-cli` as the single front door and composition owner, not a service
monolith. Reuse `fabric-cli-context/output/query`, existing focused SDKs,
`fabric-sdk-http/transport`, identity and `fabric-deploy`.
Extract new crates only with working behavior, documented public APIs and tests.
They use existing `cli`, `sdk`, `deploy`, `identity`, `core` layer rules, not new
cross-layer shortcuts; SDK never depends on CLI/identity/deployment.

| Wave / lane | Complete assigned surface | First reviewable deliverable |
| --- | --- | --- |
| Identity (existing session) | Auth methods/lifecycle and optional integration | Review exact `d8ae051` forward-port, default/opt-in contracts; live proof separately scoped |
| CLI host/local (existing SDK/CLI session) | Dispatcher, live read-host composition, config/navigation/help/version/clear/extension/desc, REPL/batch/text integration | Real explicit dependencies/runtime/context read host and local command library; coordinate parser with identity rather than competing writers |
| Resource mutations / OneLake (new session) | `mkdir/rm/mv/cp/set/ln/start/stop/assign/unassign/exists` and OneLake branches of reads | Workspace/item/folder mutations with full failure/cleanup consumers, then OneLake and resource lifecycle; no duplicate SDK/retry stack |
| Transfer/deployment (existing session) | `export/bulk-export/import/deploy` | Full Python command mapping and real definition export/import around existing Notebook pipeline, followed by breadth/planner/bulk semantics |
| Jobs (new session) | All eight `job` leaves | Real run/list/status/cancel and schedules using shared execution/LRO policies, parameter and polling contracts |
| Tables (new session) | All four `table` leaves | Schema and real load/maintenance submission/completion consumers |
| Access/catalog/raw API (new session) | All ACL/label leaves, `find/api`, catalog/hidden resource support | ACL/label service and CLI consumers, then catalog/raw API with explicit origin/audience binding; no relaxing Fabric-only executor policy globally |
| Independent validation (existing session) | Entire registry, modes, resource/option matrices; offline and opt-in online evidence | Source-derived per-family expectations and actual native command adapters; protect original oracles |
| Coverage/live setup (existing session) | Current test evidence and supported account/test-environment handoff | Non-secret documented setup/scope for `marcozo@microsoft.com`, not copied credentials |

This is a multi-wave port, not a promise that launching workers completes it.
Only an exact reviewed commit plus executed consumer evidence changes a row's
status. Shared parser/dispatcher/manifests/lock are coordinated; lanes never
annex peer drafts or force-update `feature/rust`.

### Shared service prerequisites

`HttpExecutor::execute_fabric` exposes the existing fixed-origin Fabric request
engine to additional handwritten adapters; `wait_for` exposes its existing
bounded async timer without shadowing `NotebookExecutor::wait`. Neither adds a
second HTTP/retry/polling implementation, custom headers, another audience, or
permission to treat 202 as completed. Normal Fabric origin, no-replay, decoded
byte and deadline guards still apply.

Table schema owns interpretation of Delta commit logs, not a second OneLake
filesystem client. The resource/OneLake lane supplies list/read/properties;
the jobs lane supplies generic `TableMaintenance` run/status/cancel. Table-load 202 cannot be interpreted from the command handler alone: Python's
shared client defaults `wait=True` and polls Fabric operation responses before
returning when operation headers exist, except for job-instance locations.
Without operation headers it returns the original 202; `raw_response` bypasses
this processing. The load handler's "operation started" branch applies only when
202 reaches that handler. Preserve these complete client/handler branches rather
than treating all 202 responses as immediate acceptance or always polling.
Generic resource LRO
extraction must retain one policy implementation and preserve existing public
Notebook APIs and independent contracts.

Keep focused SDK executor traits independent of the facade-dependent HTTP
adapter, avoiding a facade -> service -> HTTP -> facade cycle. Narrow composition
adapters can bind the shared HTTP implementation after service interfaces are
committed. `HttpExecutor::execute_bound` separately supplies fixed public
Fabric/OneLake/ARM/Power BI origins and validated bounded non-auth headers through
that same engine. The caller must bind credential selection/acquisition to the
explicit origin's audience; Power BI preserves the pinned Python Fabric scope.
Authentication, cookies, Host, proxy and framing/hop-by-hop overrides are rejected.
The existing Fabric-only method never infers token audience or expands its
allowed hosts. Typed raw-command and OneLake service adapters still need wiring
and consumer evidence; the shared primitive alone is not command completion.

`TokenPager::with_collection_field` selects source root arrays such as
`accessDetails` or `domains` without reserializing their responses into `value`.
Byte limits apply to original decoded response bytes before parsing; token
precedence, ordering, cancellation, partial errors and bounds are unchanged.

Some worker launch branches preserve newer upstream `8ad4c70` ancestry. Its item
type changes are not the pinned Python oracle. Preserve those commits and
unrelated work, but derive source expectations from exact `cf67fd52` objects or
a read-only pinned export. Integrate Rust-only lane deltas, not the unrelated
upstream Python feature. Record source drift separately.

## Offline and online completion gates

### Job and paging library integration

The coordinator reviewed the exact jobs handoff `f75b7fe`, additive path coverage
`e4faed4` and limitation clarification `4cf9d71`, applying only their Rust changes
without the worker's unrelated upstream Python ancestry. The SDK has real
submit/status/cancel/wait and schedule CRUD, and the CLI library owns all eight
handlers over an actual shared `HttpExecutor` adapter. `TokenPager` is a reusable
transport-neutral SDK collector first consumed by job listing; existing service
pagers and protected oracles remain unchanged. Direct TableMaintenance consumers
exercise submission, status and cancellation without a CLI/identity dependency.

The existing front-door owner still must integrate registration, explicit host
configuration, shared text output, batch/REPL and custom help. Generic Fabric
operation reconciliation is an injected capability, not a copied job LRO engine;
responses requiring it fail explicitly until that shared adapter is wired.
Skill headers, bounded numeric/conversion-error differences and exact main-level
unexpected-error envelopes remain documented gaps. See the
[job compatibility matrix](job-compatibility.md); these rows are not complete
binary or online parity.

Every row needs independent Python-derived expected requests, outputs/streams,
exit codes, errors, side effects and cancellation/cleanup. Validate options,
supported/unsupported resource branches, pagination and accepted-versus-complete
operations. Additive source safety exceptions remain individually classified;
do not waive or rewrite existing fixtures. All native runners must execute
feature-enabled consumers where the default graph excludes the implementation.

Online validation supplements these contracts. The owner approved
`marcozo@microsoft.com` and requested the existing test session's setup.
The coordinator requested its supported sign-in entry point, non-secret
tenant/cloud/client registration and designated test resources. No credentials
or cache files are requested or copied. The exact scope/setup handoff remains
pending at this checkpoint.

Keep offline CI secret-free and network-free at the test boundary. An opt-in
online harness must bind the exact account, audience, tenant, registration and
resource allowlist; reject missing settings before any service request. Start
with read-only acceptance. Mutating commands need a dedicated disposable test
workspace, permitted resource types, unique names, bounds and cleanup
authorization; account approval does not authorize deleting or altering existing
content. Do not execute both Python and Rust mutations against the same object
as a shadow comparison, persist bearer tokens, bypass TLS/CA policy or discover
the first available account. Report online outcomes as executed, blocked or
unexecuted with exact reasons; offline green is not a substitute.
