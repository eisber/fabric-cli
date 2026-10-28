<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Deployment configuration and transform compatibility

The initial slice inventories reference behavior and adds synthetic offline
fixtures. The owner subsequently approved a native single-Notebook implementation,
described below. It is not the full `fab deploy` command, a planner, unpublication,
or a Python runtime bridge.

## Source checkpoints and consumers

| Source | Exact checkpoint | Inspected boundary |
| --- | --- | --- |
| `microsoft/fabric-cli` | `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` | `src/fabric_cli/commands/fs/fab_fs_deploy.py`, `commands/fs/deploy/fab_fs_deploy_config_file.py`, `utils/fab_util.py`, `utils/fab_jmespath.py`; `tests/test_commands/test_deploy.py`, `tests/test_utils/test_fab_deploy_bulk_publish.py` |
| `microsoft/fabric-cicd` | `5ca096c00a9a878c646c59fe537b883eba951329` | `src/fabric_cicd/_parameter/_parameter.py`, `_parameter/_utils.py`, `_common/_config_utils.py`, `_common/_config_validator.py`, `_common/_check_utils.py`, `_common/_exceptions.py`, `constants.py`, `fabric_workspace.py` |
| Rust integration base | `eccfca1d3b1cb4df71d0fa65a76221a588095290` | Includes scaffold ancestor `407c595095ca326ee4a7d8355232346f219b6912`; Python source/tests unchanged from the CLI checkpoint |

The public CICD checkout was acquired separately into session artifacts and its
exact commit verified. There is no source-availability gap. Fixtures do not
vendor upstream code or copy real tenant snapshots. The
[fixture README](../tests/fixtures/deploy/README.md) describes provenance,
reproduction, and the AST-extraction boundary.

The Python CLI prompts unless forced and delegates to `deploy_with_config`.
`get_dict_from_params(..., max_depth=1)` retains dotted parameter names at the
top level, strips backslashes during input normalization, and produces strings;
the deploy wrapper then tries `json.loads` on each string, retaining the string
only on `JSONDecodeError`. This is JSON typing, **not YAML typing**. It overwrites
`host_app` with the CLI user agent/version, supplies an MSAL-bridge credential,
configures logging, enables identity-print suppression, and optionally enables
experimental bulk publishing. Its `finally` removes command-scoped bulk flags
on success or failure. Errors become CLI deployment errors; successful result
messages are printed only when the result is truthy.

Existing CLI deployment tests include prompts/cancellation, invalid paths and
environment selection, config overrides, tilde paths, host-app behavior, and
bulk-flag cleanup. They are integration/wrapper evidence, not a transform oracle:
several use workspace fixtures or mock CICD calls. This slice does not run them,
invoke auth, or claim their behavior is ported.

## Reference behavior inventory

| Boundary | Source behavior to preserve | Fixture evidence |
| --- | --- | --- |
| Config YAML | `ConfigValidator._validate_yaml_content` calls plain `yaml.safe_load`. Repeated identical keys take the last value; differently cased keys remain distinct at this parsing boundary. Structural validation is a later step. | `yaml.json`: config duplicate and merge cases |
| Parameter YAML | `_DuplicateKeyLoader` traverses mapping nodes and mappings in sequences. String keys compare with Python `lower`, not `casefold`; non-string keys compare using Python equality, so boolean/integer keys can collide. Diagnostics include variants and counts. This rejects duplicates; it does not lowercase stored keys. | `yaml.json`: parameter duplicate, sequence child, bool/int, Unicode cases |
| YAML merges | Plain safe loading expands anchors/merges and explicit keys override inherited values. The pinned parameter duplicate constructor calls `construct_object` on merge keys before SafeLoader flattens them, so its merge tag raises `ConstructorError`. Do not silently make the parameter loader accept merges as a porting improvement. | Separate config merge success and parameter merge failure |
| YAML types | SafeLoader resolves YAML-1.1-style boolean spellings, nulls, octal/hex/sexagesimal integers, dates, and non-finite floats. `1e3` remains a string in the pinned oracle; quoted scalar spellings remain strings. | `yaml.json`: typed scalar list |
| Config environment lookup | `get_config_value` uses exact dict keys; a dict is treated as an environment mapping. Missing keys/environments return `None` at this leaf, not a complete validation result. `extract_workspace_settings` strips the environment and omits missing optional settings. | `config.json` |
| `_ALL_` | `process_environment_key` mutates a one-key mapping whose sole key equals `_all_` after `lower`. It does not merge defaults when other keys exist; upstream parameter validation rejects the mixed form. | `config.json`; mixed-form case is leaf-only, not accepted config |
| Logical IDs | `_replace_logical_ids` iterates repository items, skips the all-zero placeholder, and performs case-sensitive global substring replacement. Referenced items with empty deployed GUIDs raise `ParsingError`; it is not a token-aware UUID parser. | `ids.json`: replacement and undeployed failure |
| Workspace IDs | Only default-zero values in the source regex's case-sensitive `default_lakehouse_workspace_id`, `workspaceId`, or `workspace` spellings are replaced. Non-default IDs and unrelated keys remain untouched. Reflex has an additional escaped-action regex. This is raw text processing, not general JSON traversal. | `ids.json`: ordinary and escaped Reflex cases |
| Dynamic variables | Current workspace `$id` and legacy `id` forms resolve to the target ID; name encoding uses `urllib.parse.quote(..., safe="")`. Item names can contain dots; supported item types are exact-case, attributes are lowercased; legacy item attribute syntax remains accepted. Missing deployed items fail. Item lookup refreshes deployed metadata before access. | `ids.json`: resolution with synthetic data/callbacks; named/cross-workspace parsing only |
| JSONPath mutation | `replace_key_value` uses **`jsonpath_ng.ext.parse`**, finds matches, and mutates each `match.full_path`. Extended filters, wildcards, and recursive descent are sampled. Values retain JSON types and string values can resolve dynamic variables. Missing paths are not created. | `transforms.json`: filter, wildcard typed object, recursive descent, no-match cases |
| Serialization | With no replacement, return the exact original content. After replacement, JSON uses `json.dumps` defaults (spaces and ASCII escapes); YAML uses `yaml.dump(default_flow_style=False, allow_unicode=True)` (key sorting, scalar quoting, comments lost). Empty YAML returns unchanged. | Exact string expectations in `transforms.json` |
| Python regex | Regex find patterns must compile and contain exactly one capture, even if a file filter excludes them or there is no match. The first match's empty capture fails. Replacements affect only group 1 within every full match, via a callback; backreferences in replacement strings are **literal**, not expanded. Python lookbehind and named captures are sampled. | `transforms.json`: successful captures and invalid/empty/zero/two-group failures |
| Literal replacement | Case-sensitive uses `str.replace`; ignore-case uses escaped pattern plus callback and `re.IGNORECASE`. An empty resolved replacement is a no-op in find/replace, unlike typed key/value replacement. | Literal metacharacter/backreference and empty-value cases |
| Transform order | `_replace_parameters` applies key/value rules before find/replace rules regardless of dict insertion order. JSON is recognized first; YAML is considered only for `.yml`/`.yaml` structured files. Rules operate sequentially. | Order and non-YAML-extension cases |

The usual text publication path applies item preprocessing, logical IDs,
parameters, then workspace IDs (`fabric_workspace.py`, `_publish_item`).
VariableLibrary definitions apply parameters only; `.platform` files and
non-text files bypass this processing. Lakehouse shortcut processing has a
separate consumer/order. Bulk publication and item-specific preprocessing are
not interchangeable with this leaf pipeline and remain unported.

### JSONPath is not CLI JMESPath

Deployment `find_key` uses JSONPath expressions such as
`$.items[?(@.n >= 2)].name`. CLI `fab_jmespath.search` uses `jmespath.search`
for projection, with special empty-expression path listing and `"."` handling;
scalar results are stringified. CLI `replace` is a separate, restricted
explicit-path helper that rejects wildcards. Do not route either language
through the other parser or treat a JSONPath mutation test as CLI projection
parity. No JMESPath implementation or crate is added in this slice.

## Evidence and parity status

The 44 fixtures have source-derived fixed expectations. The development oracle
executes actual pinned leaf definitions, not a rewritten replacement algorithm;
its limitations are explicit in the fixture README. Integration validation
passed all 44 cases with the pinned dependency versions; see
[validation baselines](validation-baselines.md). The native slice now consumes
five config and three logical/non-Reflex workspace-ID cases from that unchanged
oracle; other categories are not declared equivalent.

**Blocked parity, not waived:** full configuration validation/overrides and
filesystem path resolution; full parameter schema/template validation; environment
variable expansion; named/cross-workspace metadata resolution; bulk-mode caches;
wildcard/file filters; all JSONPath operators and malformed-path diagnostics;
Python regex Unicode/optional-capture edge cases beyond this sample; item-specific
transforms beyond the bounded Notebook path; deployment planning/dependency
ordering; broad publish/unpublish; shared retry/throttling and rollback policies;
and platform-specific emission/path behavior.
The upstream Python 3.10/jsonpath-ng 1.7.0 dependency branch is also unexecuted.
These are future gates, not claims of complete semantic coverage.

**Approved architectural exceptions:** the eventual runtime is native and has
no Python/CLI subprocess dependency; libraries use caller-supplied SDK/core
interfaces and credentials rather than mutable CLI state. Libraries must not
prompt, exit, initialize a global logger/runtime, or discover ambient credentials.
These exceptions do not approve changes to the sampled transform semantics.
No new behavioral waiver or YAML/regex/JSONPath crate equivalence is approved.

## Initial adapter proposal (superseded milestone)

Subject to coordinator agreement, implement a small pure config/transform adapter
inside the existing `fabric-deploy` crate, consuming these immutable fixture
inputs/expected outputs. Begin with exact environment selection and explicit
literal/logical/workspace-ID transforms; keep unsupported YAML/JSONPath/regex
behavior as typed failures, not success-shaped unchanged data. Expand only after
comparing each supported operation with the independent oracle. This is a
proposal, not authority to edit shared files or select crates.

Request shared dependencies/interfaces from the integration owner:

- Workspace-approved JSON representation (`serde`/`serde_json`) and structured
  core error classification that distinguishes invalid configuration, unsupported
  compatibility, and missing/deferred metadata.
- A pure injected resolution boundary for target workspace/item attributes,
  retaining refresh/failure semantics without embedding SDK I/O in text helpers.
- A YAML compatibility adapter decision covering duplicate-key comparison,
  implicit types, merge rejection versus expansion, and exact emission.
- Separate extended-JSONPath mutation and Python-regex compatibility adapters
  chosen against demonstrated cases, not library names or advertised features.

This was the initial fixture-only proposal, not the current implementation goal.
The owner selected native Notebook deployment and explicitly requested implementation
in this session on October 23, 2026, followed by branch/session synchronization.
That expands this lane's native slice, not live-operation or remote-publication
authority. **Accepted is not completed; cancellation is not rollback.**

## Implemented native Notebook slice

The owner-selected milestone is implemented in `fabric-deploy` and the additive
`fabric-sdk::notebooks` module. SDK handoff
`c07228b5ed5266b899ff3d2b37c7a3778cb855b8` was fetched from the published owner
branch and ancestry-aware merged, preserving the initial local fixture commit.
The workspace-list implementation, its models/tests, core and identity were not
rewritten. See the [native consumer guide](../crates/fabric-deploy/README.md).

The path loads a stable local Notebook directory, parses `.platform`, orders
definition files, normalizes universal newlines, applies ordered explicit
logical/literal/default-workspace-ID transforms to non-platform content, and
base64-encodes native request parts. Full bounded item discovery selects exact
Notebook name/type; the SDK sends create or metadata-updating definition requests.
Accepted operations must reach a known terminal state, and create retrieves the
actual result item before returning a completed deployment.

This extends the SDK's operation-local injected-executor pattern, not a second
production HTTP pipeline. Credentials stay injected. Executors own deadlines,
streaming byte limits, real transport and async poll waits; caller cancellation
drops in-flight work. No mutating POST is automatically replayed. Errors preserve
an observed operation ID and whether mutation may have occurred. Request/item/
response/poll-delay bounds are explicit. Live Fabric, production HTTP/TLS,
interactive identity, a CLI deploy command, general deployment YAML, JSONPath,
Python-regex transforms, folder moves and unpublish are not implemented.

The default operation paths and wire shapes were additionally checked against
Microsoft Learn REST contracts on October 23, 2026:

- [Create item](https://learn.microsoft.com/en-us/rest/api/fabric/core/items/create-item)
  (documentation source commit `5df184c567fafea7284357607ebac79d6301a698`).
- [Update item definition](https://learn.microsoft.com/en-us/rest/api/fabric/core/items/update-item-definition)
  (documentation source commit `b4d803a1e7f495553fcb7ef509391016e676d91d`).
- [Get operation state](https://learn.microsoft.com/en-us/rest/api/fabric/core/long-running-operations/get-operation-state)
  and [result](https://learn.microsoft.com/en-us/rest/api/fabric/core/long-running-operations/get-operation-result)
  (documentation source commit `9feaea0cb4ae5c7942ca78399640061545c3df95`).

Native safety/subset differences are explicit: service IDs are validated UUIDs,
operation/continuation links must match the expected HTTPS Fabric resource,
ambiguous matching items fail, content is allocation-bounded, non-regular or
unsupported files fail rather than disappearing, and supported Jupyter notebooks
must be v4. These are not a claim of complete Python behavior or new implicit
compatibility waivers; they are review gates before broad product rollout.

Only existing dependency versions are reused: the new exercised deploy-to-SDK
edge, existing serialization/test dependencies, and already-locked base64 0.22.1.
The lock update changes workspace edges, not registry package versions. No
toolchain, core, identity, CLI source, or independent oracle expectations change.

Local offline validation and platform limits are recorded with the implementation
handoff. The tests consume real temporary local definition files through native
request serialization and scripted service responses. This proves native offline
execution, not production transport or live tenant deployment.

On October 23, 2026, the owner granted one serial ten-minute validation session.
Checks completed and the slot was released at 17:00:20Z on Windows x64,
Rust/Cargo 1.99.1 (`x86_64-pc-windows-msvc`):

```text
cargo test --offline --locked -p fabric-sdk -p fabric-deploy -- --test-threads=1
cargo clippy --offline --locked -p fabric-sdk -p fabric-deploy --all-targets -- -D warnings
cargo fmt -p fabric-sdk -p fabric-deploy -- --check
```

Result: 24 native deployment integration tests, 30 existing SDK unit tests,
one existing credential-example test and two compiled doctests passed, with no
skips or lint waivers. The new tests cover both notebook formats, create/update
and metadata payloads, pagination, actual LRO create results, failed/unknown
states, malformed results, identity failures, allocation/request/response bounds,
unsafe links/paths, cancellation disposal/reuse, mutation uncertainty, and the
unchanged reference cases. The complete command log is a session artifact
(`native-notebook-validation.txt`); forum sync is recorded at `tk:post/12/4/30`.
Full workspace integration, independent-lane acceptance, macOS/Linux execution
and live Fabric remain unexecuted for this implementation.

## Config-driven checkpoint (October 27)

Base: `03fab20577a1016178de227eac0ee178921bb8d0`. Python and CICD source
pins above, the original 44 fixtures and their AST oracle are unchanged.
This is the first bounded config-to-production-host implementation, **not full
deployment parity or a whole-product command coverage claim**. Source code is
written; native checks and their immutable source/runner identities must be
recorded before treating this checkpoint as validated.

`fabric_cli::parser::Invocation::Deploy` routes actual root `deploy --config`
arguments to `fabric_cli::deploy::DeployHost`. Public native consumers use that
same host and the same `fabric_deploy::publish` engine. The definition-only
`TransferHost` is not a second deploy implementation. The root host reuses its
existing files/UI/configuration, credential, HTTP executor and runtime. A thin
header adapter binds the CLI-owned user agent and source JSON content type for
discovery and mutations. It neither owns a client/retry/poller nor discovers
identity. User-supplied `host_app` cannot override those actual request headers.

### Configuration and behavior ledger

| Field or behavior | Current ported boundary | Proof and remaining boundary |
| --- | --- | --- |
| Real YAML config | Event parsing with config last-key-wins, merges and source-style representable scalar resolution | Two unchanged config YAML fixtures; YAML syntax is not JSON-only input |
| Parameter YAML loader | Separate lower-based duplicate checks and merge rejection before map collapse | Existing duplicate/Unicode/merge fixtures exercise classifications; exact duplicate diagnostics and complete parameter schemas are not yet equivalent |
| Scalar types | Boolean/null, bounded integers including octal/hex/sexagesimal, finite decimal floats, quoted strings, `1e3` string | Explicit typed failure for timestamps, non-finite/out-of-domain numbers, complex keys and unsupported tags; no coercion waiver |
| YAML expansion | Bounded bytes, nodes, nesting and alias expansion | New limit/cyclic-alias/multiple-document tests; no emission parity claim |
| `core.workspace_id` | Exact environment selection, GUID validation and ID precedence | Twelve new actual validator/helper cases; display-name `core.workspace` resolution remains unsupported |
| `core.repository_directory` | Explicit config-directory-relative path through injected stable-root policy | Actual root-handler/file tests; parent traversal, unresolved aliases, ambient tilde/environment expansion remain explicit path-policy limits |
| `core.item_types_in_scope` | Source-supported type validation; explicit Notebook publication scope | All other item processors fail preflight; omitted all-type scope does not become Notebook-only success |
| `core.parameter` | No auto-discovery when absent; optional missing config environment remains absent; explicit literal files use the same confined reads and publication transforms | Ten actual schema/transform plus six source payload cases; templates, filters, dynamic variables, JSONPath/regex, missing parameter-environment skip notices and type-specific parameters remain unported |
| `publish.skip` | Source booleans and exact environment selection | No mutation when explicitly skipped |
| `publish.exclude_regex`, `folder_exclude_regex`, `folder_path_to_include`, `items_to_include`, `shortcut_exclude_regex` | Recognized and rejected before writes | Source regex/feature-gating/filter semantics remain unported, not ignored |
| `unpublish.skip` | Must explicitly resolve true in this bounded publisher | Source omission means unpublish, so omission fails preflight rather than silently pruning or reporting publish-only success |
| `unpublish.exclude_regex`, `items_to_include` | Recognized and rejected before writes | No deletion, hard-delete/admin feature, pruning or paid-resource provisioning is implemented |
| `features`, `constants` | Explicit preflight failure | No mutable SDK globals; full source command-local option semantics remain required |
| CLI `--params` | Max-depth-one dotted keys, JSON typing, backslash normalization and trusted `host_app` override | Malformed/Unicode substring-recovery quirks remain explicit lexical limits; additional kwargs fail typed |
| `config_override` | Source section/field restrictions and environment-merge behavior | Twelve immutable source cases; exact full aggregate error wording remains incomplete |
| `--force` / confirmation | Exact source confirmation, silent decline, no automatic approval | Caller-injected UI; native interactive signin is not added |
| `--bulk_publish` | Explicit unsupported before reads/mutation | No global bulk flags or source-shaped fallback-success |
| Repository discovery | Complete bounded injected snapshot; original listing order, `.platform` names/IDs, duplicate checks | Root-level Notebooks only; workspace folders, nested resources and other processors fail before service work |
| Target discovery | Existing SDK completes all bounded pages before selection | Duplicate exact Notebook names and invalid IDs fail; existing folder move requirement fails before writes |
| Planning/transforms | Preflight every Notebook using the existing logical/default-workspace-ID codec | Unknown/new referenced IDs fail, never dummy IDs; full cross-type ordering, cycles and sequential dependency resolution remain required |
| Notebook create/update | Existing definition codec; actual source field order, empty folder ID, metadata-updating definition endpoint | Two fixed actual publisher wire receipts; tests assert exact body bytes and actual returned IDs |
| Operations/reconciliation | Existing shared operation engine; sequential mutations, actual LRO result and original uncertainty | New root tests retain accepted failures/writer failures; no POST replay, cleanup deletion or rollback claim |
| Source wrapper output | Final `Deployment completed successfully`, silent decline, typed deployment failure | Existing shared stream renderer; original completed/uncertain evidence survives output delivery failure |

### Item processor ledger

The bounded `Notebook` processor uses the original native loader/transforms and
publisher. It does not reinterpret arbitrary definitions as Notebook content.
The following exact source item types are recognized but **not ported** in this
config checkpoint: `VariableLibrary`, `Warehouse`, `MirroredDatabase`,
`Lakehouse`, `SQLDatabase`, `CosmosDBDatabase`, `Environment`, `UserDataFunction`,
`Eventhouse`, `SparkJobDefinition`, `SemanticModel`, `Report`, `PaginatedReport`,
`CopyJob`, `DataBuildToolJob`, `KQLDatabase`, `KQLQueryset`, `Dataflow`,
`DataPipeline`, `Reflex`, `Eventstream`, `KQLDashboard`, `GraphQLApi`,
`ApacheAirflowJob`, `MountedDataFactory`, `Ontology`, `DataAgent`, `MLExperiment`,
`Map`, `GraphModel`, `OrgApp`, and `OrgAppAudience`.

Each needs its own pinned preprocessing/dependency/provisioning/publication and
unpublication consumers. Raw generic definition import is not a replacement for
those processors. Parameter adapters and source item-function inventories are
dependencies, not a basis for estimating those ports as complete. Full config
validation/diagnostics and folder support are further independent microbatches.
Pruning/paid/admin behavior requires an explicit execution policy and offline
proof; a source flag is never authorization for live work.

### Evidence and definition of done

`fabric-deploy/tests/config_reference.py` captures twelve actual unchanged
`ConfigValidator` override/structure/section/environment and config utility
functions, without package/auth initialization or filesystem path resolution.
`publish_reference.py` executes actual `NotebookPublisher.publish_one`,
`FabricWorkspace._publish_item` and its filters/transforms, plus the original
`File.base64_payload` property against a recording synthetic endpoint. The two
fixed wire receipts were captured before native expected outputs. No existing
fixture was regenerated. Exact duplicate error diagnostics, complete YAML
scalar domains and cross-platform fixture-path metadata remain separate limits.

Run from `rust/` with an authorized build lease:

```text
python -B crates/fabric-deploy/tests/config_reference.py --reference-root <exact-cicd-checkout>
python -B crates/fabric-deploy/tests/publish_reference.py --reference-root <exact-cicd-checkout>
cargo test --offline --locked -p fabric-deploy
cargo test --offline --locked -p fabric-cli --features native-read --test deploy_mount
cargo clippy --offline --locked -p fabric-deploy -p fabric-cli --all-targets --features native-read -- -D warnings
```

These are required commands, not passed results. The previous 50 bounded
production happy paths, default/native suites, strict Clippy, workspace/docs and
independent pinned comparisons remain gates. Full deployment completion requires
all field/type/behavior rows above, public native and root-argv proofs, failed/
cancelled/partial/uncertain outcomes, precise platform evidence and independent
source comparison. Neither one new happy vertical nor a command count closes
that definition of done.

### Locked candidate and additive wrapper/scalar proof

The locked follow-up is
`770197da9f3d3f7e93a6bb5c8589846ca8744367`, over source checkpoint
`f360c47a7f074016ee91d92e1a852d12f433e8ce`. The integration owner explicitly
approved `yaml-rust2 =0.10.4`, default features disabled, and the five exact new
registry records (`arraydeque 0.5.1`, `foldhash 0.1.5`, `hashbrown 0.15.5`,
`hashlink 0.10.0`, `yaml-rust2 0.10.4`). All original 218 registry versions,
checksums, order and resolved dependency edges remain unchanged. Cargo requires
exactly two approved spelling qualifications: existing `indexmap 2.14.2` names
`hashbrown 0.17.1`, and existing `serde_json 1.0.154` names `foldhash 0.2.0`.
The other 216 original registry records remain byte-identical.

After restoring only the pinned missing oracle dependencies into the deployment
session's private virtual environment, all 44 unchanged protected source cases
passed on Windows x64, Python 3.13.15, PyYAML 6.0.3 and jsonpath-ng 1.9.1.
The prior installed-jsonpath-ng-1.8.0 refusal remains evidence; no oracle guard,
expectation or source pin changed. Twelve new config and two actual Notebook
wire source captures also passed. These are Python/source results, not native
Rust test results.

`fabric-cli/tests/deploy_wrapper_reference.py` additionally captures eight
max-depth-one/JSON-typing parameter cases and five actual wrapper lifecycle
cases from exact Python source, with synthetic capability callbacks. They
preserve the unexpected regex comma boundary inside quotes, backslash
normalization, exact confirmation, forced/no-prompt behavior, trusted `host_app`
override, and bulk-flag finally cleanup on success/failure. Bulk native
publication itself remains unsupported; the source receipt is not a port claim.
`fabric-deploy/tests/yaml_scalar_reference.py` captures 57 exact PyYAML scalar
observations, retaining non-JSON type evidence and explicit unsupported domains.
Both fixed receipts were captured before their native test expectations.

The root-host tests additionally exercise confirmation decline before file
reads/credentials, explicit force bypass, real config overrides, later discovery
pages before update selection, partial sequential failure preserving the earlier
actual item ID, and owned-file-root escape and duplicate-service-target failures.
These are registered tests pending actual native compilation/execution, not an
assertion that they passed. Direct per-file formatting can be used when the
shared Windows host's `cargo fmt --all` command exceeds its command-line length;
that OS error is not a formatting waiver.

Five further immutable actual `deploy_with_config` observations cover both
operations skipped, publish-only/no parameter auto-discovery, default publish
then unpublish, explicit parameter forwarding, and failure/feature-scope cleanup.
The source constructs its workspace on the both-skipped branch but does not
enumerate or load item definitions. The native host now preserves that boundary:
it validates/resolves its explicit repository directory, then returns the source
final message without discovery or definition reads when publication is skipped.
It still rejects unsupported configuration before that branch.

Four actual environment-validator observations capture optional-setting warnings
in source field order. The library exposes typed `EnvironmentWarning` outcomes;
the CLI uses its existing Python-style formatter and injected warning writer.
A warning delivery failure fails explicitly before credential acquisition or
service mutation. The source warnings do not authorize ignoring unsupported
parameter fields or silently treating an incomplete scope as a full deployment.

### Explicit literal parameter file follow-up

The `parameters` module adds the source-valid literal-only `find_replace`
schema. It validates every rule and replacement environment before publication,
requires source string flags rather than YAML booleans, rejects mixed `_ALL_`
mappings, preserves literal backreferences and ordered replacements, and uses the
existing logical/literal/workspace transform sequence. A configured file must be
explicitly decoded and resolved; a failed subsequent resolution clears the prior
resolved rules. It cannot silently reuse stale rules or treat configured input
as absent. Parameter diagnostics and reserved-alias/false-regex notices are
caller-owned, not global logging or mutable SDK flags.

`literal_parameter_reference.py` executes the real pinned `Parameter` schema
methods and `FabricWorkspace._replace_parameters` for ten additive cases.
`literal_parameter_payload_reference.py` subsequently executes the original
`File.base64_payload` property for six valid source observations. Both receipts
were captured before native expected outputs and require the protected oracle's
exact PyYAML/jsonpath-ng versions. The root handler test reads an actual YAML
parameter file and asserts that its changed POST definition part equals the
source receipt; another rejects parameter duplicate keys and unsupported regex
before credential/service work. These native tests remain pending execution.

Full regex/ignore-case/JSONPath mutation, dynamic metadata, templates, file/item
filters, missing parameter-environment skip notices and all item-specific
parameter schemas remain explicit unported branches. No new dependency is added
for this literal follow-up. Future syntax primitives require the integration
owner's exact dependency/feature decision and semantic proof, not an assumption
that a similarly named Rust library matches Python.

An additional three-case actual operation-validator receipt protects source-
forbidden `unpublish.folder_exclude_regex`, `folder_path_to_include` and
`shortcut_exclude_regex`. They must fail even with `unpublish.skip: true`;
unpublication being disabled is not permission to silently ignore invalid
source configuration. The native validator retains the exact source errors.

### First hosted compiler receipt and narrow helper correction

Hosted Linux run `40542574209`, job `122324567961`, used source
`770197da9f3d3f7e93a6bb5c8589846ca8744367` and separate CI head
`a168d31bda05465e773a28645c08db095757397e`. Default and explicit-native
all-target compilation and default strict Clippy passed. Native strict Clippy
failed because the new `deploy_mount` consumer imported the shared harness's
unused read-metadata convenience methods. Tests, docs and source-comparison CI
steps were not executed after that failure. This receipt does not validate later
source heads or establish a native deploy happy path.

The correction moves those three unchanged convenience methods into a metadata
extension included only by the family/table consumers that use them. The
deploy-only UI convenience method likewise lives only in its deploy consumer.
The common harness still supplies the same injected capabilities and request
assertions. No lint allowance, test expectation, production behavior, dependency
or protected oracle changes to address this failure.

### Deferred transform adapters: independent source observations

The parser dependencies for regex/JSONPath remain deferred until the existing
vertical passes native tests and strict Clippy. No manifest or registry records
are added for this investigation. The additive
`transform_adapter_reference.py` captures 25 further immutable observations from
the exact pinned original leaves using the protected oracle's pinned
dependencies. This is source-only inventory, not native adapter acceptance.

Important observations to preserve or explicitly fail before enabling a branch:

- Python regex replacement changes only capture 1 within each complete match.
  Pattern backreferences are evaluated, but replacement `\1`, `$1` and `\g<1>`
  remain literal. An optional first capture that does not participate fails;
  a later nonparticipating capture follows Python's `-1` slicing behavior and can
  duplicate match text. Unicode ignore-case includes `I/i/İ/ı`, `S/s/ſ` and
  `K/k/K`, not just ASCII case folding.
- `jsonpath_ng.ext` numeric filter comparisons can coerce `"1"` and treat
  `true` as equal to `1`. A missing path does not create data or reformat the
  original bytes. A root-path match can set `replacement_made` and trigger JSON
  re-emission without replacing the root value. Object traversal/emission order
  and typed replacement values remain significant.
- The transform leaf accepts null/empty replacement values or boolean regex
  flags that full parameter schema validation rejects. Leaf proof must never be
  mislabeled as acceptance of those full configured parameter files.

These are observed source semantics, not an instruction to repair upstream or
normalize native outputs. The existing typed unsupported boundary remains in
effect for native regex/JSONPath. Any improvement to the later-capture slicing
or root-path behavior needs a separate owner decision and unchanged independent
source evidence.

### Second hosted registration failure and prevention

Hosted Linux run `40557934344`, job `122378892695`, used source
`cf9ce0c75b33dc7f7deb81a86e8b667f231f9e9c` and separate CI head
`da58ee153b60cf045b43a11f6552c3bbe293b5a3`. Default/native all-target checks
passed, then default strict Clippy rejected a nested `#[test]` in
`deploy_parameters.rs`: the optional-environment warning test was inside the
invalid-parameter test and therefore never registered. Tests/docs did not run.
This is a failed registration gate, not evidence that the warning behavior was
tested successfully.

The repair places that unchanged warning test at module scope. Each of the six
new deployment integration-test crates now denies `unnameable_test_items`, as the
existing definition consumer already does, so accidental nested tests fail during
ordinary compilation rather than waiting for strict Clippy. A source inventory
after formatting must show every declared test at module scope; the authorized
native runner must also compare that inventory with actual `-- --list` output
before reporting native test coverage. No assertion, oracle, lint waiver,
production behavior or dependency is changed to repair this failure.

### Terminal-result runtime failure: new synthetic input repaired

Hosted Linux run `40561857558`, job `122392692898`, used source
`cceae13dbd5e8be621e1f921abb5fcdd36703aa1` and separate CI head
`284d1220f1eb8506b1cd09024f86c040cb8bd801`. Default/native compilation and
strict Clippy passed. Actual `-- --list` output contained all 42 declared tests
across the six new test crates. The deploy library suite passed 87 integration
tests plus one doctest (88 invocations, not 88 new unique behaviors). The
front-door mount suite passed 14 tests and failed
`root_yaml_deploy_waits_for_actual_operation_result_id`; the three registered
CLI parameter tests and later pipeline stages were not executed.

The failing new synthetic terminal response supplied `{"status":"Succeeded"}`
without a `Location` header, but queued an item result that it expected the
shared session to fetch. The existing `PythonSynchronous` policy intentionally
returns the original response with status 200 when terminal result `Location`
is absent. The Notebook create consumer then correctly rejects its empty body:
it cannot derive a created item ID from a successful operation state.

Evidence predating this repair includes the shared operation README/engine,
`source_read_id_opt_in_polls_safe_opaque_ids_without_mutation_evidence`
in the shared standalone consumer, and
`local_import_create_acceptance_retrieves_actual_result_and_never_replays_post` in the
definition consumer. Their successful result responses advertise `Location`.
The original pinned Python CLI `_poll_operation`/`_fetch_operation_result` and
CICD `_handle_response` have the same terminal-location distinction.
`operation_result_reference.py` additionally executes those actual unchanged
source functions with synthetic HTTP/time callbacks and captures both branches
before the new fixture input is corrected.

Only the new successful synthetic response is corrected to advertise its actual
result URL (and its initial operation URL). The previous missing-header shape
remains an additive negative root-handler test: one POST, one poll, no result
fetch/replay/delete, no invented item ID, and original operation ID plus mutation
uncertainty retained. Existing request-body receipts and protected expectations
are unchanged. No production transport, result fallback, UUID guard, polling or
uncertainty policy is weakened. The mount registration inventory is now 16,
making 43 declared tests across the six new crates; actual corrected-head native
listing/execution still gates acceptance.

### Verified bounded Linux checkpoint

Source `7970f9fc25a8fc4be4f3f66837b836fac459da3e` passed hosted run
`40567053818`, job `122411015632`, using separate CI head
`06f93740b560ca09c5c17a943e30d6769447814b` on
`x86_64-unknown-linux-gnu`, Rust 1.99.1
(`58cf8f9c35b355199727627d07ae34a4e2846d67`). Provider comparison confirms
the CI head's parent is the exact source checkpoint and its additional files are
automation/documentation/performance harness only. Candidate production code,
tests, oracle files and lock are unchanged. The source lock SHA-256 is
`60ac2a3cb96513cd98dcc49d341ffced07159c16a58f50a7cd3e95f9610d1aa5`.

The bounded receipts and actual log establish:

| Selected gate | Actual result |
| --- | --- |
| Formatting and locked dependency restoration | Passed |
| Selected default/native all-target compilation | Passed |
| Selected default/native strict Clippy | Passed |
| Six new test crates' native registration | All 43 unique names match the source inventory |
| Deployment library integration tests | 87 passed, including existing definition and Notebook regressions |
| Deployment library doctest | One passed |
| Root deploy mount tests | All 16 passed, including actual create/update/result IDs and the missing-result negative case |
| CLI parameter tests | All three passed, including the formerly nested warning test |
| Selected pinned source comparisons | Passed: config 12, warnings 4, orchestration 5, operation sections 3, literal schema 10, literal parts 6, deferred transforms 25, scalars 57, operation result 2, wrapper parameters 8 and lifecycle 5 |
| Native-feature CLI doctests | Command succeeded with zero registered tests; not additional coverage |

Two intermediate runs on the same product source are preserved:
`40565709558` failed a raw source-hash comparison because the separate CICD
checkout had different text newline acquisition, and `40566377386` passed an
unsupported argument to the scalar comparator. The CI owner corrected only
source acquisition (`core.autocrlf=true`, matching the original raw-byte source
receipt) and comparator command construction. No fixed source expectation,
protected fixture or candidate file was regenerated or normalized. All bounded
command exits in the final receipts are zero.

The protected 44-case oracle and two original publisher-wire source cases were
also rerun successfully in the private pinned Windows Python environment; they
were not selected by this hosted family stage and are separate evidence.
These results establish a bounded **offline config/literal-parameter-to-Notebook
production-host vertical**, not full deployment parity. The old 50-command
canonical gate, complete default/native CLI suites, whole workspace checks,
other native platforms, live identity/service behavior and all remaining
configuration/item-type/planning/unpublish rows are still separate requirements.
The integration owner remains responsible for shared-branch publication and
the next exact dependency/feature decision; this successful checkpoint does not
authorize new regex/JSONPath packages or live deployment.

## Bounded parameter adapter follow-up (pending native validation)

After independently verifying the preceding native checkpoint, the integration
owner approved `fancy-regex =0.16.2` with default features disabled and explicit
`std,unicode`, and `jsonpath-rust =0.7.5` with default features disabled.
They are centralized workspace dependencies inherited only by `fabric-deploy`.
The approved decision is parsing primitives, **not Python regex or JSONPath
equivalence**. The baseline 223 registry versions/checksums/order/resolved edges
are preserved; 222 records remain literal matches and `yasna 0.6.0` only changes
its dependency label to `bit-vec 0.9.1` to disambiguate the added `bit-vec 0.8.0`.

The 13 exact additive records are `aho-corasick 1.1.5`, `bit-set 0.8.0`,
`bit-vec 0.8.0`, `fancy-regex 0.16.2`, `jsonpath-rust 0.7.5`,
`pest/pest_derive/pest_generator/pest_meta 2.9.2`, `regex 1.13.1`,
`regex-automata 0.4.18`, `regex-syntax 0.8.11`, and `ucd-trie 0.1.7`.
Their checksums are fixed in the additive lock delta. Although fancy-regex's
direct `perf` feature is disabled, jsonpath-rust enables its transitive regex
dependency's default/performance/Unicode features. The actual unified feature
graph is recorded in the handoff; no blanket "performance features off" claim
is made. Fetching followed actual manifest changes and an offline missing-cache
failure; no old registry package was upgraded.

### Engine and observable boundaries

`fabric_deploy::parameter_transforms` supplies a shared `ParameterProgram`,
`FindReplacement`, `JsonPathReplacement`, typed `TransformError`, and finite
`TransformLimits`/`TransformBudget` with caller cancellation.
`DeploymentConfig::resolve_parameters_controlled` and
`NotebookPublishPlan::prepare_controlled` accept that explicit bounded preparation
policy; convenience methods use documented finite defaults. Failed resolution
still clears stale rules. The original literal-only API remains for existing
bounded direct consumers; configured deployment now uses the full program.

The same original Notebook codec supports `prepare_parameters`, retaining
logical-ID processing, then all key/value rules, all find rules and default
workspace-ID substitution. `.platform` bypasses parameterization.
Public native consumers and root-argv commands use this same program and shared
publisher. No HTTP, paging, LRO, retry, credential, runtime or serializer is
duplicated. `ConfigurationError::Transform` and `DeploymentError::Transform`
retain the original typed failure instead of turning resource/cancellation
exhaustion into a missing match or successful partial deployment.

| Branch | Written native behavior | Proof status and explicit limits |
| --- | --- | --- |
| Regex syntax/captures | Exactly one capture validated at compile time, including before excluded-filter execution; first empty/missing capture fails; later missing capture follows source negative slicing | Protected regex cases and additive source observations consumed; new native run pending |
| Regex replacements | Capture 1 replaced inside each whole match; replacements remain literal, including backreference-looking text; named captures/lookbehind/pattern backreferences and zero-width cases supported | Source expectations unchanged; inline flags, advanced control/subroutines and unproven escape/class domains fail explicitly |
| Unicode matching | Source-style word/digit/whitespace classes, case-insensitive literals with source lowercase no-match gate, extra dotted/dotless I handling | Source Unicode cases consumed; ignore-case ranges and complete Python Unicode-version equivalence remain unproven |
| JSONPath selection | Parsed external AST, own bounded interpreter for concrete field/index/union/positive-step slice, wildcard, named recursive descent, simple current-path filters | Never uses CLI JMESPath or the crate's unbounded whole-result evaluator; preserves source path order and duplicates |
| JSONPath comparisons | Simple comparisons preserve source string-to-int coercion and bool/number equality | Large/extended integer domains, compound/dynamic-right filters, expression functions, recursive wildcard and escaped path names remain explicit unsupported |
| Mutation/emission | Only matched existing paths mutate; no matches or missing environment preserve exact bytes; typed replacements; source root-path re-emission quirk retained; shared Python JSON encoder | Negative single indices, ambiguous zero slice endpoints, overlapping ancestor/descendant updates and nested YAML emission remain unsupported |
| YAML mutation | Existing bounded YAML reader, unchanged empty input, sorted flat scalar mapping emission with source quoting | Protected flat mapping/empty cases consumed; not complete PyYAML emission parity |
| Full configured schema | Source string flags, same-type nonnull key/value replacements, nonempty string find replacements, key/value-before-find order | Filters, templates, dynamic metadata and item-specific schemas still fail preflight; leaf-only boolean flags/null replacements are not configured-file acceptance |
| Finite preparation | Default 1 MiB content/output, 4 KiB pattern, depth 64, 100,000 visits/backtracks, 10,000 matches, 1 MiB delegated regex allocations, five-second boundary-checked preparation deadline | Caller cancellation and typed failures; regex search is bounded but not preemptively interrupted or hard real-time guaranteed; all preparation finishes before writes |

### Independent new configured-program proof

Before native expected outputs,
`transform_program_reference.py` executed the actual pinned `Parameter`
name/structure/per-section schema methods, `FabricWorkspace._replace_parameters`
and original `File.base64_payload`. Its four immutable cases use source-valid
string flags and capture exact transformed content and definition parts:
capture-only literal backreferences, key/value-before-regex/literal order,
case-insensitive literal matching, and typed numeric-filter mutation.
The comparator passes against the unchanged source and exact pinned Python
dependencies. These are additional independent expectations, not Rust-derived
goldens or changes to the protected 44 fixtures.

New public primitive/program tests consume the 18 protected transform cases,
25 additive source observations, four full-schema cases, source no-op bytes,
duplicate paths, and finite cancellation/deadline/visit/depth/match/output/
backtracking failures. New production-root consumers assert actual POST parts
against the four source-derived payloads and validate that invalid captures never
dispatch a mutation. The older authored "all regex unsupported" negative input
now uses explicitly unsupported inline flags; this is an intentional capability
expansion with positive source-wire tests, not a protected-oracle rewrite.

All of this follow-up is **written source pending its own native compile/test/
strict-Clippy run**. The preceding 7970 results do not validate changed adapter
code or added dependencies. Required next checks include:

```text
cargo test --offline --locked -p fabric-deploy --test parameter_transforms
cargo test --offline --locked -p fabric-deploy
cargo test --offline --locked -p fabric-cli --features native-read --test deploy_mount --test deploy_parameters
cargo clippy --offline --locked -p fabric-deploy -p fabric-cli --all-targets --features native-read -- -D warnings
```

Default/native full suites, old 50-command regressions, workspace/docs/platform
proof and the remaining item/config/planning/unpublish ledger remain open.

Self-review additionally keeps Rust-style named groups, braced hex/Unicode
escapes, property escapes and class set operations outside the accepted regex
domain. A parsing primitive accepting them is not proof that Python accepts or
interprets them identically. Python word-boundary and named-character escapes
also remain explicit unsupported until their Unicode semantics are proven.
The syntax guard tests enforce refusal; they do not label every refused pattern
invalid Python or establish equivalence for untested regex grammar.

### Pinned JSONPath public API compile repair

Hosted run `40575687429` rejected the new adapter's private
`jsonpath_rust::parser::model` imports with E0603. In the actual pinned 0.7.5
source, `parser` publicly reexports `FilterExpression`, `JsonPathIndex` and
`Operand`; `JsonPath` is also reexported at the crate root. `FilterSign` has no
public import path. The adapter now imports only those public AST types.
It recognizes the six supported comparisons by comparing inferred operator
values with operators in six bounded, parsed reference ASTs through their public
`PartialEq` implementation. It does not stringify/debug the AST, evaluate sample
data, access private modules, fork the crate, or delegate traversal to the
unbounded query evaluator. Reference parse failures remain explicit typed errors.

The public consumer regression compiles and destructures these actual exported
types, including inferred operator equality. Additional executable regressions
exercise all six comparisons and preserve typed refusal of advanced filters.
The existing bounded visitor, source ordering, no-create mutation, cancellation
and resource limits remain in force. Dependencies, features, lock and independent
Python fixtures are unchanged. This repair needs its own exact-head compilation,
strict Clippy and regression run; the earlier 7970 checkpoint does not validate it.
