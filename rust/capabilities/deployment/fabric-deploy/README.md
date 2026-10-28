<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric deployment

`fabric-deploy` provides configuration, parameter transforms, repository
planning, item publication, orphan removal and definition transfer APIs.
Applications supply credentials, SDK clients, execution, filesystem capabilities
and cancellation; the library does not invoke the CLI.

## Deployment workflow

`workflow::deploy_repository` accepts a complete `DeploymentRepository`,
`EngineContext`, SDK client, executor, limits and cancellation.
`deploy_repository_with_clients` additionally accepts separate caller-bound
workspace and Fabric clients.

The workflow resolves workspaces, prepares dependency/type order, publishes
folders and items, runs item-specific lifecycle hooks, and removes selected
orphans. Bulk publication uses `bulkImportDefinitions` with dependency units and
explicit fallback rules. API-root settings must match injected clients; the
library never retargets a credential or creates another transport.

`WorkflowResult` retains completed identities, service effects, nonfatal
warnings, notices and optional response collections. Failures preserve earlier
effects and the current operation. Accepted staging operations remain pending
until their completion checks succeed.

## Item processors

| Processor family | Behavior |
| --- | --- |
| Notebook, SparkJobDefinition | Content ordering, definition formats and settings |
| Environment | Spark pool mapping, scalar-safe settings emission, staging publication and state checks |
| Lakehouse | Schema creation, SQL endpoint readiness and shortcut reconciliation |
| SemanticModel, Report | Definition exclusions, model references and connection bindings |
| DataPipeline, Dataflow | Dependency ordering, nested references and ID substitution |
| KQLQueryset, KQLDashboard | Database references and cluster metadata |
| VariableLibrary | Definition parameters and active value-set updates |
| Warehouse, SQLDatabase, MLExperiment | Creation payloads and metadata-only update policy |
| Eventhouse, OrgApp | Parent definitions with child-content exclusion |
| Other supported item types | Validated generic definitions and applicable per-type lifecycle policy |

The dispatcher supports 33 item types. A generic definition request does not
replace a required type-specific lifecycle step.

## Configuration and parameters

`DeploymentConfig` validates settings. `command_configuration` creates an
immutable command snapshot containing workspace selection, repository paths,
publish/unpublish settings, features, constants and prepared parameters.
Unknown fields remain retained metadata; stricter embedded callers can
explicitly reject them.

Parameter preparation uses injected template and environment inputs.
`ParameterProgram::apply_scoped` awaits mutable metadata resolution at the
selected occurrence, preserving filters and conditional evaluation.
Context-free transforms remain available separately.

Regex and JSONPath use documented bounded subsets. Unsupported expressions,
invalid scalar domains, allocation limits and cancellation fail explicitly
rather than silently returning unchanged content.

## Files and definitions

Repository discovery returns an owned snapshot with validated metadata,
logical IDs and paths before remote writes. Callers own a stable tree and
offload OS-backed reads through an appropriate worker.

`definitions` implements generic import/export and bulk export with explicit
format, byte, path, decoding and effect policies. Environment import retains its
readiness and publication lifecycle. Remote import and unsupported Report
references are invalid inputs.

The existing Notebook preparation/publication APIs remain available for focused
consumers. They use the same authenticated execution and operation policies.

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

Provide a cancellation future or enforce equivalent executor deadlines in an
application. This example does not authenticate or perform a live deployment.

## Runtime boundaries

The caller supplies asynchronous time and cancellation. No blocking sleeps,
global flags, credential discovery, prompting or runtime initialization occur.
Mutations are not retried. Cancellation and output failure do not imply rollback;
retain returned evidence before deciding how to reconcile.

Run focused tests from the workspace:

```text
cargo test --offline --locked -p fabric-deploy
cargo test --offline --locked -p fabric-deploy --doc
```
