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

## Additive extended hierarchy

`PathResolver` opts into virtual collections/resources, OneLake traversal and
conditional local fallback without changing the original `Resolver`, `Element`
or `NodeKind` contracts. Adapt a Fabric current context with
`PathElement::from_fabric`. `PathKind` distinguishes Fabric, virtual, OneLake and
local objects without guessing from OS-looking strings.

Implement `HierarchyLookup: Lookup` for caller-resolved virtual IDs and OneLake
properties, or compose an immutable `HierarchyCatalog`. Supply the source
`ls`/`dir` alias explicitly for item traversal diagnostics. `CommandPolicy`
models source supported/unsupported-item precedence; `source_list` is the pinned
internal traversal policy, not an authorization grant.

`LocalLookup` is optional and entirely caller-owned: exists/access/dirname and
`LocalMetadata` classification are injected, including the selected OS basename
and opaque local identity. `supports_local_path=True` attempts Fabric first and
only falls back for the source's three eligible error codes. No ambient
filesystem access is introduced. `LocalProbe::Boolean` deliberately represents
the source's writable-parent `exists(dirname and access)` quirk rather than
performing a real file-descriptor stat.

`PathResolution` and `PathFailure` retain source diagnostics separately from
rendering, including failed local-fallback attempts and external-share warnings.
Resolution remains non-mutating; explicit `change_context` is atomic and
remote-only. Consumers own command support, service execution and persistence.

See [the source branch ledger](../../docs/context-branch-ledger.md) for exact
function/line mappings, caller boundaries, evidence and remaining exclusions.
The `extended_resolve` example is a public hidden-resource/OneLake consumer.
