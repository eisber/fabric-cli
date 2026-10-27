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
| `core.parameter` | No auto-discovery when absent; optional missing environment remains absent | Configured files fail explicitly; templates, dynamic variables, JSONPath/regex and type-specific parameters remain unported |
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
