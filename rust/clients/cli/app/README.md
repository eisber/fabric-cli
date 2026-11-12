<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI application and command host

`fabric-cli` provides the `fab-rs` executable, root argument parser, reusable
command sessions and native host composition. Command behavior lives in focused
libraries; this crate connects them to explicit runtime capabilities.

The additive `get_metadata` library entry composes the SDK's explicit metadata
operation with preflighted, no-clobber exact local output. It does not replace
the existing executable GET or register v2 command syntax. See the
[explicit retrieval contract](../../../authoring-docs/explicit-get.md).

The separate opt-in `fab-cli-v2` binary (existing `headless` feature) mounts
the [experimental v2 get surface](../../../authoring-docs/v2-get.md). The default run target
remains `fab-rs`. Service credentials and providers are explicitly selected;
the v2 head does not activate private FabricAIHub APIs.

The additional default-off `notebook-workload` feature enables the
[explicit native Notebook content connector](../../../authoring-docs/runtime-notebook-host.md)
for typed `get .../src/content` and `artifact notebook get`. It requires an
owner-selected non-secret binding and opts the existing identity into PBI/MWC
only for that Notebook read; no discovery or default-host behavior change.

## Run the application

From `rust/`:

```text
cargo run --locked -p fabric-cli -- --help
cargo run --locked -p fabric-cli -- --version
cargo run --locked -p fabric-cli --features native-read -- deploy --help
```

Help, version and local command setup do not authenticate. Default and
broker-only builds do not install a remote execution host.

The offline legacy read probes accept only read invocations. Job and Dataflow
`execute` invocations remain explicit errors in those probes; their dedicated
command tests cover execution separately.

In a `native-read` build, `fab-rs auth login` uses the saved selection or opens
WAM's work-account picker. Successful login saves only the account, tenant and
client ID; `fab-rs ls /` then reuses that selection with silent WAM credentials
and native HTTPS transport. Explicit argument/environment selectors take
precedence. Tokens stay in WAM. See the [selection and storage
contract](../../../authoring-docs/native-read-host.md#selection-and-execution).

Authentication supports `--output_format text` and `--output_format json`.
An explicit format overrides the saved `output_format`; without a saved value,
auth defaults to text. JSON retains its existing auth result/error shape for
scripts. Both formats keep success on stdout and failures on stderr, without
printing access tokens or changing timeout/cancellation behavior.

With the optional `interactive` device-code feature on macOS, explicitly supply
`FAB_RUST_PROTECTED_STORE_HELPER` (absolute root-custodied installation path) and
`FAB_RUST_PROTECTED_STORE_HELPER_SHA256` (independently trusted 64-digit SHA-256).
The CLI captures these inputs once for the session store; it never discovers a
helper or calculates trust from an arbitrary executable. All legacy Keychain
secret operations run in that isolated helper, not the SDK process. See the
[protected-store contract](../../../engine/core/auth/provider/protected-store-macos.md)
for SDK injection, installation requirements and remaining native validation.

```powershell
fab-rs auth status --output_format text
fab-rs auth status --output_format json
```

Listing queries operate on an array of children. Use
`fab-rs ls 'Demo.Workspace' --long -q '[].id'` for the item IDs, or
`fab-rs get 'Demo.Workspace' -q id` for the workspace's own ID. A bare `id` applied
to a listing array returns the source-compatible scalar string `None`; it does
not select each item's ID. Short workspace-item listings expose names only;
`--long` makes their IDs available to the query.

Local commands and remote families share the same default user configuration
directory: `%LOCALAPPDATA%\Microsoft\FabricCLI` on Windows, the application
support directory on macOS, or the XDG configuration directory on Linux.
`FAB_RUST_CONFIG_ROOT` remains an explicit override, not a required setup step.
Help and version do not create configuration; settings initialize when needed.

For an all-feature development executable and its production updater helper:

```text
cargo build --offline --locked -p fabric-cli --all-features --bin fab-rs
cargo build --offline --locked -p fabric-cli-update --features bounded-helper --bin fab-rs-update-helper
```

This includes Windows broker, native transport, headless and Windows
certificate-store credentials, REPL and self-update code. It does not configure
release verification keys, establish installer custody or make an unsigned
development build eligible for updates. The helper is a separate executable;
test-fixture helper binaries are not part of this development pair.

Mounted command renderers use the embedded source-compatible Unicode width
provider for tables, including catalog search truncation, job status tables,
local configuration and transfer output. Renderer-generated ellipses and
non-ASCII service values use the same complete scalar coverage; ASCII-only
measurement is not a production default. JSON-only failure diagnostics remain
independent of width lookup. The immutable Unicode policy is documented by the
[output component](../output/README.md#unicode-text).
`ReadSession::dispatch_with_evidence_and_width` and its source-operation
counterpart accept an explicit caller-owned character-width capability without
changing discovery or execution. The frozen `read_session_probe` uses the
original ASCII-only layout profile to retain its unsupported-character negative
case; ordinary sessions and mounted native commands still use production
Unicode, and JSON does not require width lookup.

| Feature | Host capability |
| --- | --- |
| Default | Arguments, local commands and unconfigured-host diagnostics |
| `windows-broker` | Explicit experimental Windows login, status and logout |
| `native-read` | Windows service command host, including reads, jobs, transfers and deployment |
| `headless` | Cross-platform service host with explicitly selected unattended credentials |
| `repl` | Optional Reedline shell for terminal-only, no-argument invocation |
| `self-update` | Optional signed-release checking and standalone update host |

See [native host configuration](../../../authoring-docs/native-read-host.md) and
[Windows authentication](../../../README.md#windows-authentication) for selectors,
platform limits and live-operation requirements.

## Bounded Ontology v2 preview

The separately gated `v2-preview` source mounts the released
`fabric-cli-ontology` public adapter. It does **not** replace the v1 root parser,
change ordinary `get`/`ls`, install another global registry, or implement the
rest of the v2 command groups. Only an outer, first-position `--v2-preview`
marker selects this grammar; batches and the REPL keep their existing parser.
Default help and version remain unchanged, including in preview-enabled builds.

The preview remains an opt-in path in `fab-rs`; it does not replace the
`fab-cli` local host, `fab-cli-v2` native get entry, or `fab-cli-workspace`
Workspace preview. The impact graph validates both `v2-preview` alone and
`v2-preview-combined` with the existing Workspace, native, cancellation,
copy, runtime, discovery, identity, REPL, and update features.

The preview uses these entries in this crate's manifest
(the shared workspace dependency already exists):

```toml
[features]
v2-preview = ["dep:fabric-cli-ontology"]

[dependencies]
fabric-cli-ontology = { workspace = true, optional = true }
```

Do not add `v2-preview` to `default`, `native-read` or `headless`. No new binary
target, direct core dependency, direct Ontology SDK dependency, transport,
credential backend, runtime or test dependency is needed. The SDK facade already
reexports the Ontology and neutral contract APIs. The integration owner owns
manifest/lockfile changes and the serial validator.
The preview fixture target requires the feature; executable entry tests also
run without it to verify the default grammar.

The two forms use the same capability argument schemas, operation descriptors,
SDK execution and receipts:

```text
fab-rs --v2-preview get /Demo.Workspace/Knowledge.Ontology
fab-rs --v2-preview artifact ontology get /Demo.Workspace/Knowledge.Ontology
fab-rs --v2-preview create /Demo.Workspace/Knowledge.Ontology --input metadata.json
fab-rs --v2-preview artifact ontology update /Demo.Workspace/Knowledge.Ontology --mode patch --input metadata-patch.json
fab-rs --v2-preview delete /Demo.Workspace/Knowledge.Ontology
fab-rs --v2-preview get /Demo.Workspace/Knowledge.Ontology/src/definition
fab-rs --v2-preview artifact ontology get /Demo.Workspace/Knowledge.Ontology/src/model
fab-rs --v2-preview update /Demo.Workspace/Knowledge.Ontology/src/model/minimized --mode replace --input model.json
fab-rs --v2-preview artifact ontology update /Demo.Workspace/Knowledge.Ontology/src/definition --mode replace --input definition.json
```

These are invocation shapes, **not live execution instructions or claims**.
The standalone binary, including native-read/headless builds, returns exit 1
with sanitized `NotConfigured` JSON on stderr for a valid preview operation.
It does so before settings, credential selection, runtime creation, stdin or
output-file access. Help succeeds without a host; malformed arguments return
exit 2 without echoing supplied values. A configured native credential is not
proof of Ontology public endpoint/feature readiness or exact-effect approval.
No native policy has been invented to turn those conditions into an automatic
yes. This is the remaining native executable binding blocker.

### Injected root host

`v2_preview::parse_args` parses argv after removing the outer marker, retaining
the program name. Pass its `PreviewCommand` to `v2_preview::dispatch_command` with:

- The existing `ReadSession` and its same shared executor, SDK context,
  caller-owned runtime and selected endpoint.
- The caller's canonical `Registry`, with the five public descriptors and
  `item.metadata.create`, `item.metadata.patch`, `item.delete` registered, then
  `fabric_cli_ontology::{bind_public_operations, bind_metadata_operations}`
  applied. This mount does not define alternate descriptor schemas or identities.
- A `PreviewHost` that binds one finite total deadline/cancellation policy,
  approves scoped metadata discovery, reads bounded explicit input, atomically
  reserves output and verifies the exact prepared operation's conditional
  prerequisites, effects, identity, input and preconditions.
- Explicit `AdapterLimits` and a cancellation future observing that same
  deadline throughout discovery, metadata preflight, dispatch and observation.

`dispatch_command` selects the same operation for generic and facade requests.
Its `PreviewReport` retains either the original public `ExecutionReport` or
metadata `CapabilityReport`, including typed failures and output evidence.
`receipt()` borrows the original receipt and `exit_code()` maps actual outcomes
without treating acceptance as completion. Existing `dispatch` callers keep
their public Get/replacement API; `dispatch_metadata` is the metadata-only API.

`dispatch_with_native` additionally accepts a `NativePreviewHost` with an
explicit private Digital Operations `NativeContext`. Its registry also needs
the seven `ontology.entity.*` descriptors and `bind_native_operations`.
This is a distinct injected workload executor, routing, identity and audience
binding, never a public endpoint override. The ordinary `PreviewHost` and
`dispatch_command` do not silently acquire this capability.

The read session's existing `Snapshot`/`Resolver` resolves existing owner
segments. The preview then reads authoritative public Ontology metadata
for type/generation before invoking the adapter. This metadata preflight is
explicitly covered by discovery approval, not a hidden source fallback. The
adapter performs its own approved request, including another metadata request
for metadata Get; discovery is not presented as the operation's result. The
deadline is established once and is not reset between these phases.
Owner discovery retains the existing snapshot's finite SDK list limits;
`AdapterLimits.operation` bounds the subsequent public operation, not a
combined discovery-plus-execution request count. The caller's total deadline
and discovery policy cover both phases.

Metadata Create instead resolves only the actual workspace. Its neutral target
is a `WorkspaceCollection` with a real workspace ID, no fake item ID, and unknown
generation. The adapter calls the existing SDK
`ItemMutations::create_with_completion`; returned item identity/generation stays
in the actual provider data. The root verifies that input `displayName` equals
the addressed item name before approval. Fresh-resource create does not promise
atomic display-name reservation: policy must refuse any stronger unavailable
namespace compare-and-swap requirement.

Creation input is exactly `{"displayName":"Knowledge","description":"optional"}`.
Patch accepts supplied `displayName`/`description` strings only, preserving
omitted fields; explicit null, definition, type, generation and creationPayload
are rejected, not transformed into defaults. Patch and Delete use the existing
SDK `ItemMutations::*_with_completion` APIs, not a copied CRUD/LRO engine.
Delete uses the generic items route with the exact optional `--permanent`
intent; no native definition reset or permanent fallback is substituted.
The prepared operation binds body, input/output source, selected SDK context,
bounds and native conditions to host approval. Metadata mutations require
RemoteWrite/RemoteDelete, declared LocalRead for input, and LocalWrite for
explicit file output. The host supplies no blanket approval.

The bounded path grammar is an optional leading slash followed by exact
`Name.Workspace/Name.Ontology` segments, with no navigation segments, escaped
slashes, folders or arbitrary subresources:

| Suffix | Representation | Execution |
| --- | --- | --- |
| None | Public item metadata JSON | Get, metadata-only Create, explicit Patch, or Delete |
| `/src/definition` | Public native multipart envelope | Get or explicit replacement |
| `/src/model` or `/src/model/expanded` | Expanded public model | Get only |
| `/src/model/minimized` | Authored/minimized public model | Get or explicit replacement |
| `/src/schema/entities` | Private V4 entity collection | List or strict Create |
| `/src/schema/entities/<lineage>` | Private V4 entity by stable lineage | Get, explicit Patch, Delete, or Move |
| `/src/schema/entities/<lineage>/binding` | Existing entity source binding | Link |

Native schema paths require `dispatch_with_native`. A lineage is one opaque
segment, not an entity display name. This bounded mount does not decode encoded
IDs or support lineage IDs containing path separators. The SDK's injected
resolved-target API remains distinct from this limited root path grammar.
Move is a name-only patch preserving the original lineage identity, not a
transfer or copy/delete. Link accepts a backing-table patch, never a graph or
ACL grant. Deletion requires explicit permanent intent and approval of affected
references. These native writes do not imply graph publication.

Model source requires provider-established generation 2; native definitions
retain generation 1/2 applicability. Unknown, malformed and future generation
values are not coerced or defaulted for existing-target dispatch. The current
capability validation also requires known generation for existing metadata
execution; unknown-generation existing metadata remains a fail-closed limitation.
Metadata-only Create deliberately leaves the resulting generation unknown. Type is
verified from actual metadata, not trusted solely from the path suffix.

Both entry points reject unsupported options through the same adapter. The
preview additionally rejects `--profile` and `--project` because no root
profile binding or projection implementation is installed. Get, source replacement
and metadata-only Create/Patch/Delete execute through injected public hosts.
The separately injected native entity host supplies all seven selected aliases.
`/docs`, OWL/RDF, internal stored models, non-entity native schema resources at
this root, writable graph, context authoring, query/model
execution and publication are not mounted. Such paths never reach OneLake
folder resolution. No private transport, token exchange or credential fallback
is configured.

Successful/accepted receipts go to the reserved stdout/file destination;
provider failure/unknown receipts go to stderr unchanged. Use
`PreviewReport::exit_code` for the unified route, or `execution_exit_code` for
the original public-only API: accepted is exit 0 as submission,
not completion, while failed/partial/unknown is exit 1. Local typed
`PreviewError`s expose their exit and a sanitized diagnostic renderer. Preserve
`AdapterError::Output` or `CapabilityError::Output`'s original report if presentation fails; do not render a
replacement remote-failure result or replay the request. The host owns cleanup
of an unused reserved output file after refusal or failure; no rollback or
transactional local-output publication is promised.

### Offline acceptance and validator handoff

`tests/v2_preview.rs` authors real root parser -> existing owner discovery ->
actual Ontology adapter -> shared `HttpExecutor` -> injected Azure HTTP fixtures.
It covers generic/facade metadata equivalence, native definition and both model
forms, selected endpoint propagation, actual replacement input/output files,
atomic no-clobber reservation, policy refusal, unsupported facets/options,
generation/type denial, acceptance versus completion, denied source, accepted
poll failure, cancellation before/after acceptance, result bounds and writer
failure with original remote evidence. Metadata root cases exercise real
workspace-only Create, generic/facade consistency, existing-owner Patch/Delete,
path/body name binding, explicit permanence, exact-policy refusal and no-clobber
through the same HTTP fixture and operation engine. Its registry and approval implementations
are explicitly offline fixture policies, not production defaults.

`tests/v2_preview_entry.rs` authors subprocess contracts for unchanged v1
help/version/grammar, absence of the marker in default builds, preview help,
sanitized arguments, and `NotConfigured` before native configuration.

Public metadata/source root fixtures, executable opt-in/opt-out cases, existing
argv/CLI contracts and architecture fixtures have been executed offline on
Windows, with strict all-target Clippy. This does not establish other platform
or live-service acceptance. Native root fixtures additionally cover the seven
aliases, private-binding refusal, partial/accepted outcomes and writer failure.
After acceptance, a failed public HTTP poll remains `Unknown` with its original
handle and possible mutation, not `Failed`. The root keeps a regression for that
contract without reclassifying or concealing the upstream receipt. The new
package `tests/metadata_execution.rs` additionally authors metadata-only wire,
input, receipt, cancellation, acceptance/poll failure and output-error contracts.

Validation must run
formatting, all-target checks for `fabric-cli` with the preview feature, these
two integration-test targets, the capability metadata test target, and affected existing root argv/CLI contracts
in both default and preview/native feature configurations. Preserve the existing
v1 oracles. No live Fabric/authentication/deployment acceptance is implied.

## Headless service commands

Build with `--features headless` and explicitly set `FAB_RUST_AUTH_MODE`.
Supported modes are `client-secret`, `client-assertion`, `workload-identity`,
`managed-identity-vm` and `managed-identity-app-service`. Select a public-cloud
tenant and the documented provider inputs; the presence of a secret never
selects a credential. Service commands acquire tokens lazily, without interactive
login. Service-principal identity labels are client IDs, not user UPNs or
independently verified claims.

See the exact [environment bindings and host contracts](../../../authoring-docs/native-read-host.md#headless-credentials).
Credentials are not command arguments or saved settings. The
[identity API](../../../engine/core/auth/provider/README.md)
documents renewal, redaction and unsupported providers. Offline tests do not
establish live platform support, service authorization or GA readiness.

## Optional release updates

The `self-update` feature adds outer-only update commands:

```text
fab-rs update --check
fab-rs update --apply
```

Release builds pin the `microsoft/fabric-cli` publisher and an independently
approved verification public key. Discovery uses the `rust-vSEMVER` namespace;
other product releases in the repository are not update candidates. Missing
publisher trust fails before networking. No GitHub credential, Fabric credential,
downloaded verification key or alternate repository is used.

Automatic checking or installation is opt-in through `FAB_RUST_UPDATE_POLICY`
(`off`, `check` or `install`) and an explicit absolute
`FAB_RUST_UPDATE_STATE_ROOT`. Checks are rate-limited across processes. Help,
version, authentication, batch, redirected and CI execution do not start an
automatic check. Explicit updates cannot run inside a batch or interactive shell.

Installation requires standalone build and installer receipts. Unknown and
package-manager-owned installations are not overwritten. Automatic installation
stays within the current major version; major upgrades require explicit opt-in.
After a successful automatic replacement, the process exits with status **75**
and states that the original command did not run and must be rerun. Pre-commit
update failures warn without suppressing the requested command; uncertain native
replacement stops for recovery. Commands are never replayed automatically.

See the [update component contract](../update/README.md)
for signed metadata, exact target/feature matching, bounded downloads, Windows
working-directory/helper restrictions, retained recovery files and acceptance
limits. Update-manifest signatures do not establish native package signing,
notarization, helper cleanup or post-install health. Production trust setup and
native update/recovery acceptance remain release gates.

## Optional interactive shell

Build with `--features repl` to start the interactive shell when no arguments are
supplied. Combine it with `native-read` to use the configured remote host:

```text
cargo run --locked -p fabric-cli --features repl
cargo run --locked -p fabric-cli --features native-read,repl
```

The terminal editor and shell loop live in the separate
[`fabric-cli-repl`](../repl/README.md) package. They are not SDK
dependencies and are excluded from the application's default dependency graph.
Removing the optional feature and private entry module does not require changing
shared command, batch or SDK libraries.

The shell delegates commands to the existing parser and dispatcher, retains
navigation in memory and does not restore or write command-line context files.
History is bounded and memory-only. `help`, `version`, `quit` and `exit` are
interactive aliases. Input is tokenized with quoting and escapes, not evaluated
by an operating-system shell. No completer or persistent history is configured.
Both stdin and stdout must be terminals; redirected input fails before editor,
configuration or credential setup. Use command arguments or `-c` for automation.
Help and version arguments never enter the editor.
Unclosed quotes and trailing escapes end the session after a diagnostic with a
normal session exit status. Resource-limit and terminal-editor failures instead
return a nonzero status; output failures propagate.

Real-terminal editing, key bindings, signal handling and terminal-state
restoration require native Windows, macOS and Linux acceptance. Fake-input tests
and dependency-graph checks do not establish that platform support.

## Embed command execution

`parser::parse_args` accepts argv with the executable name.
`ReadHost::dispatch` returns a render plan; `ReadHost::run` writes it to supplied
streams. Inject the SDK client, executors, immutable current/home context,
metadata, configuration, timestamp and query provider.

`with_default_projection` selects the built-in `PythonQuery` provider, backed by
the JMESPath library. `NoQuery` explicitly disables query and path discovery.
Empty LS queries use default columns; GET with no expression lists property
paths, while `"."` retains the metadata value.

`ReadSession` composes asynchronous discovery and command execution.
`FolderReadHost` adds folder-aware listing without changing the base host's
public inputs. `JobSession`, `TableSession` and `FamilySession` compose their
focused handlers over the same execution and context policies.

Libraries do not discover credentials, create a runtime, install transport,
read global state or terminate the process. Cancellation is a supplied future;
partial pages never become successful complete results.

## Command batches

Repeat `-c` or `--command` to execute ordered command strings through the same
per-command parser and dispatcher:

```text
fab-rs -c "config set show_hidden true" -c "config get show_hidden"
fab-rs --output_format json -c version -c version
```

Batch input is bounded command tokenization, not a shell-language interpreter.
The execution contract is:

- Split on Unicode whitespace, including U+001C through U+001F. Quotes
  remain in tokens; semicolons and pipes are ordinary data, not separators.
- Skip empty entries. After a returned nonempty entry, write the 30-hyphen
  separator to stderr unless that entry has the last **raw list index**.
- Continue after handled command errors; return the last executed command's exit
  status. Stop on parser misuse, explicit help exits, uncaught failures or
  cancellation; do not print the final summary after stopping.
- After more than one executed entry, print two stdout newlines and a success
  summary using the **raw list length**, even when earlier commands failed.
  The JSON summary's `command` is the complete original string list.
- The outer output override applies only to the summary. Each inner command
  parses its own override or reads configuration at its execution point. The
  summary reads the configuration after all commands return.
- A normal subcommand following root batch options replaces the command list.
  Root version does not replace a batch;
  root help still exits before dispatch. Nested `-c` is rejected.

Invalid arguments use the sanitized error envelope. Unknown batch commands stop
with exit 1 and a sanitized `UnexpectedError`. Diagnostics and debug formatting
of batch invocations omit supplied command strings.
Explicit tabular exports preserve the native command's terminal or cancellation
disposition; adapting an error to an export receipt does not authorize the next
batch command. Handled export failures retain the ordinary last-command rule.

`parser::batch::run` takes a `BatchInvocation` and caller-owned `BatchHost`.
`execute` receives split arguments and returns `CommandOutcome::Returned(u8)` or
`Stop(u8)`. `separator` and `summary` are ordered output callbacks; any callback
error stops without replay. `parse_command` reuses the existing parser and
distinguishes explicit help exits from returning bare-group help. No library
runtime, process exit, prompt, environment access or mutable global is added.

The binary captures the initial context selection once at batch entry and borrows
one caller-owned `NavigationState` across native family commands. Remote `cd`,
relative reads/jobs and `pwd` use the updated in-memory path even when context
persistence is disabled. Settings still use the existing file store; enabling
persistence or changing settings later does not reload the initial context.
Context persistence remains opt-in: the batch host does not force context-file
writes or modify environment variables. A completed context change is not rolled
back by a subsequent output failure.

Authentication uses the same explicit broker selection and command engine as a
single invocation. Its typed outcome distinguishes cancellation from a handled
provider failure even though both retain exit 1. Cancellation, timeout and
signal-handler failure stop before the next command and suppress the summary.
Invalid authentication configuration (exit 2) and failure to initialize its
runtime also stop before the next command; ordinary handled selection failures
still preserve the last-command exit rule.
No new credential backend, acquisition rule or token input is introduced.

Default local execution has no installed Ctrl+C handler. The existing Tokio
signal support is feature-gated with authentication; a portable default/local
batch signal host still needs a reviewed dependency/runtime contract. Synthetic
cancellation tests do not establish OS-signal handling. Portable signal
acceptance remains a release gate, independent of the shared native context hook.

Offline contract fixtures and their independent checks are maintained under
`tests/`. Recheck them without rewriting expected outputs:

```text
python clients/cli/app/tests/batch_reference.py --check clients/cli/app/tests/batch_source.json
cargo test --offline --locked -p fabric-cli --test batch_commands --test batch_parser --test argv_contract --test local_mounted
```

The same executable selectors and an all-target check are also required with
`--features native-read`. Contract captures do not establish actual native
cancellation, remote context reuse or platform acceptance.

## Deployment and transfers

`DeployHost` loads configuration, templates and parameters before preparing an
owned repository snapshot. Its generic workflow handles selected item types,
scoped parameter resolution, publication, bulk operations and orphan removal.
Results retain nonfatal warnings and partial or uncertain service effects.

`FamilySession::dispatch_with_deploy_inputs` accepts explicit parameter
environment values. Default `dispatch` supplies an empty map. The native binary
alone captures process values, validates Unicode and keeps diagnostics redacted.
Those values are not credential inputs.

`TokioTransferFiles` and `TokioDeployFiles` run bounded local file phases on the
caller's blocking executor. Deferred path selection preserves classification
order and limits local authority to explicit arguments or trusted local
configuration documents.

## Native Dataflow query execution

The additive v2 `execute` root binds an explicit
`/<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/<query-name>` target
with required `--wait`. `--format json|csv|parquet` uses the shared native Arrow
tabular result component; Parquet requires a new file destination. Optional
`--input FILE|-` supplies strict JSON for `customMashupDocument` and
`arrowVersion` (1 or 2), not SQL text.

The host requires target/input-bound approval before credentials, performs
no-clobber output preflight, and preserves real operation evidence on local
cancellation, timeout or output failure without replay. Noninteractive native
processes without an approval capability fail closed; embedders inject
`execute::host::ExecutionApproval`. The default binary returns `NotConfigured`.
Result decoding or serialization failure stops a batch before subsequent
commands, even when the query already completed; the query is never replayed.
Authorization diagnostics retain the actual HTTP status alongside operation
evidence, including empty HTTP 401 responses with `Unauthorized` exit 4.
See the [complete execution contract](../../../authoring-docs/dataflow-execution.md) for
finite limits, exact format references, permission/gateway prerequisites,
unimplemented providers and the detailed refresh-log API gap. Existing v1
JMESPath metadata projection is unchanged. Excel output is deferred.

## Output and execution boundaries

Use returned plans and original evidence until output delivery completes.
Writer errors propagate without replay. HTTP acceptance, local cancellation and
completed remote operations remain distinct outcomes.

The native binary owns one asynchronous runtime, cancellation handler and
command-selected watchdog. Explicitly unbounded job waits retain per-request
limits and Ctrl+C handling without an arbitrary whole-command cutoff.

Windows credentials require explicit account, tenant and client selection.
Other platforms can use injected library credentials; native credential
execution there fails explicitly. Resource/access skill headers and several
resource-specific capabilities remain unavailable.
