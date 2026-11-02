<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Folder reads

`FolderClient` provides Fabric folder listing and lookup. The SDK facade exposes
`Client::list_folders` and `get_folder` using the same implementation.

## Configure execution

Inject a credential, explicit `CredentialSelection` and `FolderExecutor`.
The caller binds credentials to the selected account and tenant; selection
metadata does not verify token claims.

`new` uses public Fabric. `with_context(RequestContext)` preserves the supplied
endpoint for initial and continuation requests. Every request uses the Fabric
audience; configure the shared HTTP executor with the same binding.

```rust,no_run
use fabric_sdk_folders::{FolderClient, FolderExecutor, FolderListOptions,
    FolderGetOptions, FolderQueryValue};
use std::future::pending;

async fn read(client: &FolderClient, executor: &dyn FolderExecutor)
    -> Result<(), Box<dyn std::error::Error>>
{
    let folders = client.list_folders(executor, "workspace",
        FolderListOptions {
            recursive: Some(FolderQueryValue::Bool(false)),
            root_folder_id: Some("opaque-parent".into()),
            ..Default::default()
        }, pending()).await?;
    for row in folders.value {
        let detail = client.get_folder(executor, "workspace", &row.id,
            FolderGetOptions::default(), pending()).await?;
        assert_eq!(detail.id, row.id);
    }
    Ok(())
}
```

## Queries and data

Ordered query pairs retain insertion positions when repeated keys are replaced.
Values are form-encoded once. A continuation token replaces its query value
without dropping other parameters.

LIST defaults `recursive` to the string `True` unless the query already supplies
that key. Explicit false, null and absence remain distinct.
`root_folder_id` replaces `rootFolderId` only when supplied. GET adds no defaults.
Query scalar domains and unsafe route input are validated before authentication.

Results retain page order and opaque IDs. Errors distinguish missing resources,
malformed responses, incomplete paging, execution failure and cancellation.
Dropping a call cancels local futures, not remote work.

Executors own bounded streaming, deadlines, read retries and cleanup. The shared
HTTP adapter supplies these policies without redirects or a second paging engine.
The folder client installs no transport, runtime or ambient configuration.
