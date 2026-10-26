<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Notebook deployment and definition transfer

This crate implements a bounded **single-Notebook** path, not the full Python
`fab deploy` command. It loads real local files, performs supported native
transforms, builds inline-base64 definition parts, discovers an existing
Notebook by exact name/type, and creates or updates it through `fabric-sdk`.
Asynchronous operations are polled to terminal state; accepted is not completed.

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

This slice does not support YAML deployment files, JSONPath or Python-regex
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
