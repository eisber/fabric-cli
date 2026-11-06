<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Capability dependency catalog

`impact-graph.json` is declarative schema-1 input to the impact selector. It
contains no commands, execution permission, oracle updates, or required-check
policy. Cargo metadata remains authoritative for workspace membership, target
registration, feature existence, and the union of normal, development, build,
optional, and platform-specific path dependencies.

The catalog was traced against the owner's current working sources based on
`558d0dda718328b760059fa7a5bf55e7491c2006`, including uncommitted integration
work. This is source analysis, not a validation receipt for that revision or the
dirty candidate. No expected output, source receipt, or source pin was changed.

## Data contract

The root object has `schema`, `components`, `profiles` and `checks`, plus the
optional `copyright` string. Schema-1 parsers accept this copyright field as data,
not a JSON comment; other undeclared fields remain errors. For planning,
source-binding, execution and validation-mode usage, see
[dependency validation](../docs/dependency-validation.md). This document records
the catalog API and its source-derived contract mapping.

All paths are repository-relative POSIX globs, irrespective of the execution
platform. A component has an owning Cargo package, input globs, dependency
component IDs, exact test descriptors, and additional check IDs. A dependency
means **consumer depends on dependency**. Changing an input selects that
component and its reverse component closure, not its dependencies' tests.

A component can declare `coverage: "runtime"` or `coverage: "structural"`;
omitting it means `runtime`. Only `workspace-architecture` is structural.
Structural tests execute normally and their packages participate in compilation
and check closures, but they do not prove behavioral consumer coverage. The
engine's coverage set uses the **test packages of selected runtime components**,
not component owning-package fields or test packages supplied solely by
structural components. An uncovered reverse Cargo consumer still requires its
full runtime tests even when its architecture contract is selected.

A test descriptor names its Cargo package and one actual `lib`, `bin:NAME`,
`test:NAME`, or `example:NAME` target. This catalog deliberately uses no test-name
filters: the integration targets often contain several item families or include
additional test modules. Target names are not claims of isolated behavior.
Compilation and documentation validation are separate from these test selections.

Profiles declare the actual feature-provider packages, qualified Cargo features,
platforms, and check IDs. They do not authorize credentials, UI, certificates,
network service calls, or installations. Windows features are listed on all
three platforms because their unsupported-platform behavior is part of the
offline contract. Only the transport and updater fixture features select their
corresponding standalone example/helper targets.

Checks are execution-registry IDs, never shell fragments. `packages` preserves
package-wide fallback eligibility, `components` preserves capability eligibility,
and `inputs` records non-Cargo oracle dependencies. Check dependencies select
required obligations; the executor owns lifecycle ordering. In particular,
`oracle-tools` is setup and `frozen-verify` must execute as an always-run finalizer
after comparisons, not merely as a successful pre-comparison check. Every Python
comparison check selects both obligations.

The ten profiles include the distinct `native-read-repl` combination in
addition to standalone native reads and the combined host. Both certificate
profiles explicitly name the identity provider and native certificate feature;
the combined host does not rely solely on a CLI feature forwarding that edge.
Profile checks can be markers without auxiliary commands: their evidence is the
core planner's actual feature-specific check/lint and selected-target execution,
not the presence of the marker alone. Profile-provider packages outside the
selected capability compile without acquiring unrelated test suites.

The `v2-discovery` profile covers the optional `fab-cli` binary and discovery
consumer tests on all three platforms. The combined profile enables discovery
alongside the existing host features, including self-update, to preserve
feature-composition coverage. This profile grants no live data or mutation
authority.

## Source-derived contract map

The following inventory accounts for every component. The JSON is authoritative
for the complete test-target lists.

| Components | Dependency evidence and exercised boundary |
| --- | --- |
| `core` | Endpoint/request context and credential interoperability feed SDK and identity consumers. |
| `json`, `output`, `query` | Ordered/native JSON encoding feeds output, definitions, API and table bodies. Query projection consumes the output parser; CLI read and GET consumers exercise the shared boundary. |
| `context`, `local` | Context model/catalog/resolution feed navigation, configuration, local commands and every resource-family resolver. `support.json` and local fixture files are inputs, not documentation. |
| `sdk-workspaces`, `sdk-items`, `sdk-folders`, `sdk-capacities`, `sdk` | Extracted typed clients and facade reexports have their own standalone/facade tests. Workspace metadata and mutations contain Spark settings GET/PATCH routes; these files are shared resource sources, not Environment deployment leaves. |
| `operations`, `http`, `transport`, `paging` | Shared reconciliation, typed HTTP adapters, optional native transport and paging are cross-family capabilities. Their consumers include deployment, resources, jobs, storage and tables, rather than a single command namespace. |
| `sdk-api`, `sdk-access`, `sdk-catalog`, `resources` | Explicit API operations feed access/catalog clients and CLI frontdoor/rendering/selection; JSON fixtures exercise wire, mutation, interaction and error branches. |
| `mutations`, `resource-set` | `MutationHost` composes workspace/item/folder mutations and OneLake behavior. `set.rs` independently routes workspace `sparkSettings` to its special PATCH endpoint and validates metadata/definition queries. Shared create/templates/removal/transport changes intentionally remain wide. |
| `onelake`, `onelake-http`, `storage-files` | OneLake models, HTTP adaptation and physical-file transfer are distinct from definition transfer. Copy/download/upload and selected-adapter tests cover local/remote authority and partial effects. |
| `sdk-jobs`, `jobs`, `cli-jobs` | Job type conversion includes Notebook and Spark job definitions; numeric/polling/header/schedule behavior is shared. SDK/adapter/host targets cover command, endpoint, execution, cancellation, mutation, watchdog and settings consumers. |
| `sdk-tables`, `tables`, `cli-tables` | Table pipelines use OneLake, operations and maintenance jobs. Table changes therefore select real maintenance-job consumers, rather than being treated as an isolated storage pathname. |
| `deploy-shared` | Configuration/YAML, repository discovery, planner, parameters/transform programs, processor dispatch and common text/parameter processing affect all supported item families. `processors.rs` implements Notebook part ordering and SparkJobDefinitionV2 emission in the same source file, so neither is a falsely isolated leaf. |
| `deploy-workflow` | Shared service orchestration, bulk dispatch, family preprocessing, post-publish work and unpublish feed Notebook and Environment workflows. Pipeline-selection and publisher/refresh/wait/shortcut fixtures are shared workflow inputs. |
| `deploy-notebook` | `notebook.rs` prepares/publishes typed Notebook definitions; `publish.rs` composes repository snapshots, transforms, metadata discovery and operation reconciliation. Repository, planner, transformation and endpoint tests are included, not only the target named `notebook`. |
| `deploy-spark-environment` | `workflow/families/environment.rs` reads Spark pools, replaces instance-pool settings, emits YAML and drives Environment publish readiness. `workflow_support/environment_scalars.rs` is included by the real `workflow` integration target. |
| `definitions`, `transfer` | Definition codecs, local/bulk acquisition, projected GET output and import/export are separate from Notebook publication. `TransferFiles` also exposes Notebook/repository discovery and Environment import lifecycle; its consumers therefore join the relevant deployment closures. |
| `cli-deployment`, `cli-get` | Central deployment uses typed preparation and the worker bridge; native transfer files and export storage feed mounted transfer/deploy behavior. GET borrows `TransferFiles` and definition decoding, and separately enriches Notebook schedules and Environment Spark settings. |
| `cli-reads`, `cli-resources`, `cli-local` | Read projection, resource-family dispatch and mounted local behavior have distinct source inputs and consumer targets. `families.rs` is shared dispatch and is intentionally not a Notebook-only component. |
| `identity-policy`, `identity-broker`, `identity-headless`, `identity-certificate` | Selection, silent/explicit broker behavior, unattended providers/assertion files/pipelines and certificate composition have distinct provider inputs. Certificate composition depends on headless, not the reverse. |
| `windows-broker`, `windows-certificate`, `cli-auth`, `cli-headless` | Native request/Entra interop, native certificate vectors and CLI argument/provider composition are independently visible boundaries. Feature checks retain offline policy and off-Windows negative contracts. |
| `repl`, `cli-repl`, `updater`, `cli-updater` | Standalone shell/editor and signed-update engine/helper targets are explicit, alongside their removable CLI adapters. Updating the provider selects its own tests as well as mounted-host tests. |
| `cli-native-shared` | Native composition, settings, console and terminal UI are shared across GET, jobs, resources, tables and deployment. They require a broad consumer set, not just a similarly named target. |
| `cli-shared-test-helpers` | `family_support`, `folder_support` and `support` are included by multiple real integration targets. The full inclusion union is listed. Job helper changes additionally cover `native_settings`, which directly imports that helper. |
| `workspace-architecture` | The CLI `workspace_contract` target inspects actual workspace metadata, package layers, feature isolation and dependency directions across all mapped components. It is a shared contract consumer, not a production dependency. Its closure also selects feature/duplicate graph inspection for the dynamically selected package set. |

### Notebook and Spark examples

A change to `rust/capabilities/deployment/fabric-deploy/src/notebook.rs` selects
`deploy-notebook`, `transfer`, `cli-deployment`, `cli-get` and the shared
`workspace-architecture` contract. Its declared test-target union is:

| Package | Targets |
| --- | --- |
| `fabric-deploy` | `lib`, `test:notebook`, `test:repository`, `test:parameter_transforms`, `test:planner_source`, `test:deployment_endpoint`, `test:workflow` |
| `fabric-cli-transfer` | `lib`, `test:commands` |
| `fabric-cli` | `lib`, `test:async_transfer_files`, `test:deploy_full`, `test:deploy_inputs_debug`, `test:deploy_mount`, `test:deploy_parameters`, `test:deployment_worker_bridge`, `test:export_parity_mount`, `test:family_mounts`, `test:native_deploy_inputs`, `test:get_host`, `test:get_options`, `test:workspace_get`, `test:native_endpoint_reads`, `test:output_contract_audit`, `test:workspace_contract` |

This closure does not select `sdk-jobs`, `jobs`, `cli-jobs`, `sdk-tables`,
`tables`, `cli-tables`, or the Environment component. It does select whole mixed
targets such as `workflow`, `family_mounts`, and `output_contract_audit`, including
their unrelated cases: no unsupported name-filter precision is implied.
Definition/import/parameter/export and native-read checks remain relevant;
GET consumers additionally select the read comparison. Scoped mounted consumers
select CLI help/version smoke, and all mapped leaves select dependency feature
and duplicate inspection without widening their runtime target set.

A change to
`rust/capabilities/deployment/fabric-deploy/src/workflow/families/environment.rs`
instead selects `deploy-spark-environment`, `transfer`, `cli-deployment`,
`cli-get` and `workspace-architecture`. Its `fabric-deploy` targets are `lib`, `test:workflow`,
`test:publisher_source_contract`, `test:deployment_endpoint`, and
`test:configured_flags`; transfer and CLI consumer targets are the same as above.
It does not select `test:notebook` merely because Environment contains Spark
configuration.

Spark is not one physical module. Workspace settings reads/mutations live in
`sdk-workspaces`; resource PATCH routing lives in `resource-set`; enriched item
GET lives in `cli-get`; Spark job execution lives in `sdk-jobs`/`jobs`/`cli-jobs`.
Each uses its own closure. A change to shared `processors.rs`, parameters, YAML
or repository processing selects both deployment families. Their shared source
cannot honestly provide file-level Notebook-versus-Spark precision.

### Embedded inputs and independent oracles

`notebook` and parameter-context tests read `deploy/ids.json`; Notebook tests
also read `deploy/config.json`. Transform and YAML tests include the corresponding
`deploy/transforms.json` and `deploy/yaml.json`. These exact shared fixture paths
belong to `deploy-shared`. Workflow JSON captures are included across
`workflow`, `publisher_source_contract`, `deployment_endpoint`, planner and
mounted CLI targets, so they select the shared workflow closure.

The new-Notebook creation definition is a literal Base64 payload in
`fabric-cli-mutations/src/create.rs`, not an external Notebook template.
`templates.rs` does use `include_bytes!` for the Report, SemanticModel and
MirroredDatabase trees; those existing template inputs belong to `mutations`.
There is no invented external Notebook template dependency.

Compatibility JSON consumed by extracted SDK, context, query, output, jobs and
table tests is mapped to those components. Known Python oracle directories and
top-level comparison drivers select their existing check IDs. These mappings do
not regenerate captures or certify that a check registry executes every
historical capture script. The registry remains responsible for preserving its
real comparison commands and immutable-source verification.

## Conservative boundaries

- The catalog is not a replacement for Cargo reverse-consumer coverage checks.
  An actual reverse Cargo consumer absent from a selected component's declared
  test coverage requires full tests for that package. New crates remain
  dynamically discovered; this catalog does not freeze workspace membership.
- Unmapped package inputs require full owner/reverse-consumer tests. Shared
  helper modules not explicitly traced, package READMEs used by `include_str!`,
  new source files, and additional fixtures are not presumed harmless.
- A top-level test/example source uses its actual Cargo target; unclassified
  nested helpers require a wide fallback. There is deliberately no `tests/**`
  component pretending all test helpers are isolated.
- Shared manifests, locks, toolchains, workflow/selector/catalog inputs and
  unknown non-crate Rust inputs require full validation and all registry checks.
  This fallback belongs to the engine, not a finite list of known paths here.
- Graph inspection uses the actual selected package set, including unmapped new
  crates. The engine retains that obligation and selects the shared workspace
  architecture contract for any Rust graph/package change, even if no existing
  catalog component matches or the new crate has no Cargo edge to the CLI.
  Architecture-only tests do not establish runtime behavior coverage for an
  otherwise uncovered consumer.
- Profiles describe compilation providers and feature combinations, not
  permission to run every provider's entire test suite. A CLI consumer in the
  Cargo compile closure is not evidence that broker, updater, REPL or certificate
  business behavior changed. The selector/executor must preserve scoped test
  selection when applying profiles. There is no catalog exclusion for
  certificate-enabled legacy golden targets: certificate selection adds a
  headless provider, while auth command registration and the corresponding
  `cli_contract`/`argv_contract` expectations both follow `windows-broker`.
  Selected targets execute under applicable profiles; a feature incompatibility
  requires a concrete source or execution counterexample, not a blanket skip.
- Source-file granularity cannot isolate Spark from other job types inside
  `fabric-sdk-jobs/src/lib.rs`, or Notebook from Spark definition formatting in
  `processors.rs`. Whole relevant targets and shared closures are intentional.
- Static source analysis, JSON/reference inspection and a reference audit against
  parent-supplied Cargo metadata are the evidence supplied with this catalog.
  That format-version-1 metadata contains 34 workspace packages; every declared
  package, test target and profile feature resolves. Notebook, Environment and
  shared-deployment closures each list tests for every actual reverse Cargo
  consumer package. Engine checks, negative-selection coverage, platform
  execution, independent review and required CI remain separate parent-owned
  evidence. This catalog makes no whole-GA or required-gate protection claim.
