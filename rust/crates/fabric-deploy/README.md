<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook deployment and definition transfer

## Multi-item orchestration checkpoint

The additive `model`, `planner`, `processors`, and `workflow` modules translate
the pinned CICD publisher hierarchy rather than routing every type through the
definition-import codec. `workflow::deploy_repository` accepts a complete
`DeploymentRepository`, `EngineContext`, injected SDK client/executor, bounds and
cancellation. It resolves workspace display names, selects source type/dependency
order, publishes folders and items sequentially, performs processor lifecycle
calls, and runs default orphan unpublication with source feature gates and empty
folder pruning. All flags, constants, parameter source and optional environment
values belong to the explicit invocation context; there are no global flags.
Bulk mode uses real `bulkImportDefinitions` parts, source fallback conditions,
logical-ID publish units and dependency stages.

This is an **unvalidated implementation checkpoint**, not full deployment parity
or a replacement for the existing Notebook API. The root CLI still uses the
bounded Notebook preparation capability; mounting the broader workflow requires
the transfer owner's additive asynchronous full-repository snapshot method.
No native Cargo test/build slot has been granted to this lane. Source capture,
Rust formatting and locked no-dependency metadata checks are distinct from native
compilation and passing tests.

| Source item type | Implemented processor path |
|---|---|
| VariableLibrary | Parameter-only definition; active value-set PATCH |
| Warehouse | Source creationPayload; metadata-only update |
| MirroredDatabase | Source generic definition publisher |
| Lakehouse | Schema creation payload; SQL readiness; shortcut reconciliation |
| SQLDatabase | Source shell create/metadata PATCH |
| CosmosDBDatabase | Source generic definition publisher |
| Environment | Sparkcompute pool mapping; staging publish; state checks |
| UserDataFunction | Source generic definition publisher |
| Eventhouse | Parent definition excludes `.children` |
| SparkJobDefinition | `SparkJobDefinitionV2` format |
| Notebook | Content-before-settings ordering; optional ipynb format |
| SemanticModel | `.pbi` exclusion; purge options; connection bindings |
| Report | `.pbi` exclusion; repository byPath model conversion |
| PaginatedReport | Source generic definition publisher, serialized I/O |
| CopyJob | Source generic definition publisher |
| DataBuildToolJob | Source generic publisher; bulk fallback |
| KQLDatabase | Source generic definition publisher |
| KQLQueryset | Repository database reference and cluster URI lookup |
| Dataflow | Parameter-derived dependency order and source-ID conversion |
| DataPipeline | Actual nested activity references; ordered publish/unpublish |
| Reflex | Generic definition plus escaped default-workspace replacement |
| Eventstream | Source generic definition publisher |
| KQLDashboard | Deployed database-name and cluster URI lookup |
| GraphQLApi | Source generic definition publisher |
| ApacheAirflowJob | Source generic definition publisher |
| MountedDataFactory | Source generic definition publisher |
| Ontology | Source generic definition publisher |
| DataAgent | `.pbi` exclusion |
| MLExperiment | Source shell create/metadata PATCH |
| Map | Source generic definition publisher |
| GraphModel | Source generic definition publisher |
| OrgApp | Parent definition excludes `.children` |
| OrgAppAudience | Generic definition; parent-managed location does not move |

Source proof is under `rust/tests/compatibility/deploy_workflow`: actual source
AST definitions produce all 66 create/update publisher cases, actual nested
pipeline/sort cases, family preprocessing outputs/metadata reads, and soft/hard
delete/pruning calls before native test expectations are added. The capture uses
public CICD commit `5ca096c00a9a878c646c59fe537b883eba951329`, synthetic endpoint
responses, and no credentials, package initialization or network. Its first
66 cases cover common bodies, **not all lifecycle/configuration branches**.
The new native selectors are `--test publisher_source_contract --test workflow`.
They have not yet been executed.

The policy follow-up consumes selected source constants for shell-only updates,
definition formats/exclusions, placeholder IDs and workspace/Reflex references,
type/capacity/bulk gates, dependency files, provisioned attribute paths, parent
folder patterns and unpublication features. Opt-in response collection retains
source publish/unpublish response trees separately from the effect ledger.
Lifecycle waits use the source's bounded exponential sequence, captured from
actual `handle_retry` with an injected clock/environment in `waits_source.json`;
no mutation is retried and no blocking sleep is installed.
`deploy_repository_with_clients` additionally accepts immutable caller-bound
workspace and Fabric-specific clients. Configured API-root overrides must match
those injected bindings; the library neither retargets a credential nor creates a
transport. Connection listing/binding uses the Fabric-specific client. Native
host selection of these clients remains the integration owner's boundary.
Accepted Environment staging effects retain their operation IDs and pending
state until a successful processor state check; missing/unknown final state is
not reported as completed.

The endpoint follow-up replaces the remaining legacy publication sessions with
`OperationSession::with_context` and makes the public Notebook SDK session retain
the caller's endpoint for discovery, continuation links and LROs. It does not add
a transport policy, accept arbitrary command hosts, discover identity, change
token audiences or remove the native host's nonpublic-family guard.
`deployment_endpoint` adds offline shared-HTTP cases for public/daily/MSIT profiles:
all 33 source dispatch contexts, legacy and public Notebook paging/create/LRO,
workspace-name lookup, bulk publication, orphan deletion and folder pruning.
Every emitted request is checked for its selected host and Fabric credential
scope; cross-origin continuation/LRO cases require rejection without fallback
or mutation replay. These new native tests remain **unexecuted** until the root's
serialized validation slot. The additional CLI unit selector is
`deployment_metadata_adapter_preserves_public_daily_and_msit_contexts`.

Shortcut deletion uses the existing approved `ResourceOperation::encoded` and
`FabricOperation::from_resource` APIs, not a new SDK API, unchecked URL or relaxed
raw-path constructor. Empty/dot/traversing,
separator-bearing and control components are rejected; Unicode, spaces and
literal percent characters are encoded as data. The same context, counted
executor, LRO reconciliation and single-attempt write policy execute the result.
`shortcuts_source.json` captures actual pinned `_unpublish_shortcuts` URL
construction plus offline Requests URL preparation before native expectations.
The added native scope/URL and constructor tests remain unexecuted.

Prepared definitions are bounded both per file and in aggregate through
`EngineContext::with_definition_limits`; repository preflight and standard/bulk
publication share the caller's aggregate byte budget. Processor post-hooks retain
the prepared file contents: VariableLibrary value-set activation and Lakehouse
shortcut reconciliation must not read a stale pre-parameterization snapshot.
`post_publish_source.json` captures actual VariableLibrary lifecycle methods with
an injected prepared-definition capability; it is not a replacement transform
oracle. New native regression cases cover both publication modes and aggregate
growth before a definition mutation. They remain unexecuted pending ROOT.

The pipeline planner follows the source ordered-publisher boundary: once any
pipeline is selected, dependency validation covers every repository pipeline
before filtering the execution order. Excluded cycles must not become a
successful selective deployment. `pipeline_selection_source.json` captures the
actual source selection, file-reading dependency planner and graph sorter.
The separate `planner_source` native selector is pending ROOT execution.
Source type pre-hooks still refresh metadata when selective publication yields
no items. Those refreshes do not retarget an initially absent item to a new
concurrently discovered ID; `refresh_source.json` captures the actual source
selection/refresh/create boundary. Native comparisons are pending ROOT.

Acceptance still needs native compilation/strict Clippy, complete processor
branch/lifecycle/cancellation/partial-failure source fixtures, full root mounting,
source constant-override coverage, binary classification equivalence, and
Sparkcompute YAML emission edge cases. The encoded shortcut consumer cases still
need native validation; the approved operation APIs and existing raw-path percent
guard remain unchanged.
Advanced regex/JSONPath fullness remains a
separate transform boundary. No live publish/delete operation is authorized.

This crate implements bounded native Notebook deployment, not full Python
`fab deploy` parity. The original **single-Notebook** path loads real local files, performs supported native
transforms, builds inline-base64 definition parts, discovers an existing
Notebook by exact name/type, and creates or updates it through `fabric-sdk`.
Asynchronous operations are polled to terminal state; accepted is not completed.

The additive `yaml`, `deployment_config`, `repository`, and `publish` modules
compose real YAML configuration, exact environment selection/overrides, injected
bounded repository discovery, complete SDK item paging, preflight and sequential
Notebook publication. `PreparedNotebook::publish_with_session` preserves the
pinned CICD create/update body, including field order and the empty root folder
ID, using the existing shared operation engine. The original single-item API and
definition-transfer consumers remain available unchanged.

The CLI's dedicated `Invocation::Deploy` selects `fabric_cli::deploy::DeployHost`,
which uses this engine with caller-owned files, confirmation and the same HTTP
adapter/credential/runtime. Config paths resolve relative to the explicit local
root; repository paths resolve relative to the config directory. The first
bounded config requires `core.workspace_id`, `core.repository_directory`,
`core.item_types_in_scope: [Notebook]`, and explicit `unpublish.skip: true`.
No parameter file is auto-discovered. Explicit `core.parameter` supports ordered
case-sensitive literal `find_replace` rules with string-valued environment
replacements, a sole case-insensitive `_ALL_` alias, and source string flags
(`"false"`, not YAML booleans). Parameters are read relative to the config
directory through the same confined file capability, validated before service
work, and applied after logical IDs and before workspace IDs. Direct library
consumers must call `DeploymentConfig::resolve_parameters` for a configured file;
unresolved parameters fail preflight rather than disappearing.

The additive `parameter_transforms` follow-up writes bounded capture-only regex,
case-insensitive literal and typed JSONPath rules into the same configured-file
program and Notebook preparation path. Its new code/dependency graph requires a
separate native run; the earlier 7970 checkpoint does not validate it.
See the [pending adapter ledger](../../docs/deployment-compatibility.md#bounded-parameter-adapter-follow-up-pending-native-validation)
for exact supported syntax, source proof and finite limits.

Other processors, unported parameter branches,
folder moves, features/constants, filters, bulk publication and unpublication
fail explicitly before writes; they are not silently ignored or counted as a
completed full deployment port. See the [field/type ledger](../../docs/deployment-compatibility.md#config-driven-checkpoint-october-27).

The bounded source checkpoint `7970f9fc25a8fc4be4f3f66837b836fac459da3e`
has actual Linux x64 compile/strict-Clippy evidence, 87 deployment integration
tests plus one doctest, 16 production-root deploy tests and three CLI parameter
tests passing. All 43 new test declarations were also verified in native
registration. Source captures, test registration and passing execution are
recorded separately in the [compatibility ledger](../../docs/deployment-compatibility.md#verified-bounded-linux-checkpoint).
These bounded results are not full config/item-type/transform/unpublish parity,
the complete command/workspace regression gates, other-platform proof or live
deployment authorization.
Further additive pinned source receipts cover wrapper/JSON typing, scalar
resolution, skip orchestration, optional-environment warnings and literal
parameter validation/transformed parts. Unproven regex/JSONPath branches, filters, templates,
dynamic variables and missing parameter-environment skip notices remain explicit
unsupported branches; bounded adapters do not establish full parameter parity.

The additive `definitions` module supplies generic ordered definition reads,
caller-injected local export and raw copy/SET codec APIs. It is not the full
export/import/deploy command family. See the
[definition-transfer contract and remaining branches](../../docs/definition-transfer-compatibility.md).

It has no Python/CLI subprocess bridge, credential discovery, global logger,
mutable CLI context, prompts, or owned async runtime. It does not install a
production HTTP transport. Applications inject their SDK credential, executor
and cancellation/deadline future. The executor is responsible for real HTTP,
bearer authentication, deadlines, streaming response bounds, and asynchronous
poll waits; it must not follow redirects or replay POSTs.

## Use from a native consumer

Load and prepare synchronously before starting async service work. The library
does not hide filesystem blocking inside an async worker. This example compiles
as a native consumer; the application supplies an executor and an existing SDK
client, not a shell command:

```rust,no_run
use fabric_deploy::{
    notebook::{DefinitionLimits, LocalNotebook},
    transforms::NotebookTransforms,
};
use fabric_sdk::{Client, notebooks::{NotebookExecutor, NotebookLimits}};
use std::path::Path;

async fn publish(
    client: &Client,
    executor: &dyn NotebookExecutor,
    notebook: &fabric_deploy::notebook::PreparedNotebook,
) -> Result<(), fabric_deploy::notebook::DeploymentError> {
    let completed = notebook
        .deploy(client, executor, NotebookLimits::default(), std::future::pending())
        .await?;
    assert!(!completed.item_id.is_empty());
    Ok(())
}

fn prepare() -> Result<fabric_deploy::notebook::PreparedNotebook,
    fabric_deploy::notebook::DeploymentError> {
    LocalNotebook::load(Path::new("Daily.Load.Notebook"), DefinitionLimits::default())?
        .prepare("11111111-1111-1111-1111-111111111111", &NotebookTransforms::default())
}
```

Production consumers should supply a deadline/cancellation future instead of
`pending()` unless executor deadlines and future disposal already enforce their
policy. No authentication or live deployment is performed by this example.

## Supported definition and transform boundary

- A single directory with a valid Notebook `.platform` containing display name,
  type, and nonempty logical ID, plus **exactly one** `notebook-content.py` or
  `notebook-content.ipynb`. Git format optionally includes `notebook-settings.json`.
  Jupyter content must be a v4 notebook with a cells array.
- `.platform` precedes content, which precedes settings. Its bytes are never
  parameterized; Python universal-newline normalization is retained. IPYNB
  definitions set format `ipynb`; Git definitions omit format.
- Caller-supplied ordered logical-ID mappings, then case-sensitive literal
  replacements, then the pinned default-workspace-ID text substitutions.
  All-zero logical IDs are skipped, referenced undeployed IDs fail, and empty
  literal replacement strings are no-ops. These are resolved inputs, not a
  `parameter.yml` interpretation or an implicit dynamic-variable lookup.
- Three parsed-JSON configuration/environment helpers consume the existing
  independent five-case reference fixture. They do not parse or validate YAML.

Unsupported files, folders, binary assets, mixed formats, invalid metadata,
invalid transformed JSON, symlinks/special files, and excessive byte counts fail
explicitly before service work. Inputs are a stable local directory snapshot;
hostile concurrent filesystem replacement is not a supported isolation boundary.
The default aggregate input/output content limit is 10 MiB. Base64/JSON wire
encoding adds bounded overhead to that content.

The original single-Notebook API does not interpret YAML deployment files, JSONPath or Python-regex
parameterization, environment expansion, dependency planning, dynamic metadata
resolution, folder placement/moves, arbitrary notebook resources, bulk publishing,
or unpublishing. A repository-level driver must not silently drop unsupported
configuration before calling this typed API.

## Service lifecycle and safety

Discovery completes all bounded pages before selecting create/update. Exact
duplicate Notebook names fail instead of choosing one arbitrarily; other item
types and case variants do not collide. Updates include `.platform` with
`updateMetadata=True`. Concurrent creates/conflicts surface as service failures,
not an automatic second mutation.

The SDK follows a validated operation handle, not arbitrary service URLs.
Only `NotStarted`/`Running` continue, `Succeeded` completes, and `Failed`,
`Cancelled`, unknown states, malformed responses or exhausted bounds fail.
Creation retrieves and checks the actual operation result item. Mutation
uncertainty and accepted operation IDs survive errors for caller reconciliation.
Cancellation drops local in-flight work; **it is not rollback**.

Default session limits are 100 requests (including discovery, polling and result),
100,000 discovery items, 4 MiB per response, and at most 60 seconds per service
poll interval. These finite limits do not replace executor request deadlines or
an overall caller deadline. No automatic retry/throttling/backoff is claimed.

## Reference and execution evidence

Source: `microsoft/fabric-cicd` commit
`5ca096c00a9a878c646c59fe537b883eba951329`, specifically NotebookPublisher,
Item/File collection and `_publish_item`/ID/parameter helper behavior. REST
create/update and operation-state/result contracts are linked in the
[deployment inventory](../../docs/deployment-compatibility.md).

The offline integration suite exercises actual temporary local definition files
through native SDK request serialization and a scripted service executor, including
create/update, pagination, LROs, errors, identity failure, limits and cancellation.
It also consumes all five immutable config cases and the three applicable
logical/non-Reflex workspace-ID cases without changing the oracle.
This is native offline behavior evidence, not live Fabric or complete deployment
parity. Windows-only local validation does not prove macOS/Linux behavior.
