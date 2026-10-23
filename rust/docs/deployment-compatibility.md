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
