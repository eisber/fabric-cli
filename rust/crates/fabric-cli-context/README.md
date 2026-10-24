<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI context

Pure CLI-layer Fabric tenant/workspace/folder/item resolution over caller-owned
metadata. It does not perform authentication, HTTP, filesystem access or context
persistence, and does not add a command to `fab-rs`.

```rust
use fabric_cli_context::{
    Catalog, Element, HomeContext, NodeKind, PathInput, ResolveOptions,
    Resolver, WorkspaceType,
};
use std::sync::Arc;

let tenant = Element::tenant("Synthetic", "opaque-tenant");
let workspace = Element::child(
    "Example",
    Some("opaque-workspace".into()),
    NodeKind::Workspace(WorkspaceType::Workspace),
    Arc::clone(&tenant),
)?;
let catalog = Catalog::new(vec![workspace]);
let resolver = Resolver::new(
    &catalog,
    tenant,
    HomeContext::Workspace("Example.Workspace".into()),
);
let resolved = resolver.resolve(PathInput::Scalar("~"), ResolveOptions::default())?;
assert_eq!(resolved.path(), "/Example.Workspace");
assert_eq!(resolved.path_id(), "/opaque-workspace");
assert_eq!(resolver.current().path(), "/");
# Ok::<(), fabric_cli_context::ContextError>(())
```

Implement `Lookup` (or provide a closure) to inject already-resolved metadata
without using `Catalog`. It distinguishes workspace, workspace-wide folder/item
and parent-specific nested-folder queries. Preserve lookup order and the
first-match duplicate behavior; absence and failures are distinct.

`resolve` leaves current context unchanged. Only the explicit `change_context`
operation replaces it, after complete success. IDs remain opaque; absent IDs
represent unresolved creation targets, not successful service operations.

OneLake traversal, virtual/admin collections, external data shares, explicitly
local paths and mixed filesystem fallback return typed `Unsupported` errors.
This is a bounded translation, not full Python `get_command_context` parity.
See [context compatibility](../../docs/context-compatibility.md) for the exact
source pin, Python quirks, unsupported boundaries and independent oracle.
The `resolve` example is a standalone offline consumer.
