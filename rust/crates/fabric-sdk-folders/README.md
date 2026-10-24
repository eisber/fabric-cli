<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read-only folder SDK

`fabric-sdk-folders` implements handwritten Fabric folder LIST and GET in the
SDK layer. Direct consumers use `FolderClient`; the additive
`fabric_sdk::Client::{list_folders,get_folder}` facade delegates with the same
credential and explicit selection. `fabric-sdk-http::HttpExecutor` implements
`FolderExecutor` through its existing private shared HTTP execution engine.
No transport, TLS stack, identity backend, runtime, environment lookup or CLI
command is installed.

## Caller interface

Consumers inject `Arc<dyn azure_core::credentials::TokenCredential>` and
`CredentialSelection`. Construction performs no I/O. The caller binds the
provider to the selected account/tenant; selection does not verify token claims.
Every page requests `https://api.fabric.microsoft.com/.default`.

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

Pass a caller-owned cancellation future instead of `pending()` to cancel.
Dropping either call also drops in-flight credential/executor futures.
Executors must enforce streaming body limits, finite deadlines/retry budgets,
no redirects and local resource cleanup on future drop. The shared HTTP adapter
already provides bounded reads/deadlines/read retries; this crate adds no HTTP
retry policy.

## Python query contract

Source is `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`client/fab_api_folders.py::{list_folders,get_folder}` and actual
`fab_api_client.py::do_request`. This is the raw SDK boundary, not a
reimplementation of Python's memstore or folder tree.

`query` is an ordered dictionary expressed as pairs: a repeated key replaces
its value in the first key's position. Keys/values use Python `quote_plus`
encoding once, including `~` unescaped, `*` escaped, spaces as `+`, and literal
percent input escaped again. A continuation token replaces an existing key in
place or appends it; it never replaces the caller's other parameters.

For LIST, any existing `recursive` query key overrides `recursive`, including
empty, false or null values. Otherwise absent `recursive` means the **string**
`True`; present boolean false means `False`; present `FolderQueryValue::None`
means `None`. `root_folder_id: Some(...)` corresponds to the **presence** of
Python `args.folder_id`: it replaces `rootFolderId` before URI-derived
`recursive` is appended. `Some(FolderQueryValue::None)` becomes
`rootFolderId=None`; `None` means absent and leaves an existing query value alone.
GET adds no default query fields.

Supported Python scalar values are strings, booleans, null/None and signed
64-bit integers. Arbitrary integers, floating-point representations, lists,
objects and custom Python string conversions are deliberately unrepresentable.
A recursive **attribute** string containing `?`, `&`, `=`, `#` or a control
character is rejected before authentication unless the query already overrides
it. Python's naive URI splitting can inject parameters, truncate input or raise
an unstructured exception; those are not supported URI-building features.
Query values themselves can contain punctuation and are safely encoded.

## Rows, progress and failures

`Folder` requires string `id` and `displayName`, without a UUID restriction.
`parentFolderId` is raw metadata: absent, null, opaque strings and future JSON
shapes remain distinguishable via `parent_folder_id()`. Unknown fields,
Unicode, case, row order and duplicates survive. No tree building, orphan
discarding, name normalization, caching or local recursive filtering occurs.

Defaults are **100 pages, 100,000 rows and 4 MiB per response**; all limits
are nonzero and customizable. A complete exact-limit response succeeds.
An additional page/row/byte is an explicit failure, never truncated success.
LIST errors contain only full committed pages and the last unconsumed token
when known. A malformed or over-limit page is never partly appended.
Cancellation takes priority over simultaneous success.

Header presence wins over a body token case-insensitively: an empty
`x-ms-continuation` suppresses even a truthy/ill-typed body token. String/empty/
null body tokens are supported; other token types fail explicitly. Active URI
hints must be HTTPS on the Fabric host, port 443, the original exact collection
path, with no userinfo/fragment/raw-path normalization. They are validated but
never followed: only the token updates the original request query. An unused
terminal URI is ignored. Repeated tokens, including the initial token, fail.

200/initial 201 are typed completions. 202/204, a follow-up 201, malformed JSON,
missing/wrong-type fields and paginated single-object GET are explicit failures,
not empty success. 401/403 prefer nonempty public error header, then legacy
header, then Unauthorized/Forbidden. 404 always maps to NotFound; other
statuses use body `errorCode` or UnknownError. Structured service diagnostics and
headers remain untrusted data; error Display excludes them. Request Debug never
formats credentials, identifiers or query tokens.

## Evidence and non-goals

See [folder compatibility](../../docs/folder-compatibility.md) and the new
[source oracle](../../tests/compatibility/folders/README.md). The standalone
test consumes hand-authored expectations shared with an independent actual
Python AST oracle; facade and external shared-HTTP tests exercise both methods.
Real default limit boundaries, cancellation/drop, URI rejection, token cycles,
service precedence and partial failures are additional Rust safety evidence.

No create/delete/move/patch, recursive tree reconstruction, memstore cache,
folder path resolution, CLI `ls`/`get` projection/export/query behavior,
specialized item collections, identity implementation or live service parity is
claimed. In particular Python memstore recognizes roots by **absence** of
`parentFolderId`; a null parent is not silently reinterpreted as a root here.
