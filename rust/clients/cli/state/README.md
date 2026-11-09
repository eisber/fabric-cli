<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI context

`fabric-cli-context` resolves Fabric tenant, workspace, folder and item paths
against caller-owned metadata. It performs no authentication, HTTP, filesystem
access or persistence.

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

## Metadata and state

Use `Catalog`, implement `Lookup`, or provide a lookup closure. Workspace-wide
and parent-specific queries remain distinct. Missing metadata is not
interchangeable with a lookup failure.

Resolution leaves the current context unchanged. Only `change_context` replaces
it, after complete success. IDs are opaque; an absent ID represents an unresolved
creation target, not a completed service operation.

## Extended paths

`PathResolver` handles virtual collections, OneLake traversal and optional local
fallback. `PathElement::from_fabric` adapts a Fabric context; `PathKind`
distinguishes Fabric, virtual, OneLake and local resources explicitly.

Implement `HierarchyLookup: Lookup` or use `HierarchyCatalog` for resolved
virtual IDs and OneLake properties. `CommandPolicy` applies resource support
rules; it does not grant authorization.

`LocalLookup` supplies existence, access, parent and metadata classification.
The resolver never probes the filesystem itself. Local fallback must be enabled
explicitly and applies only to eligible resolution failures.

`PathResolution` and `PathFailure` retain diagnostics and fallback evidence
separately from rendering. Extended context changes are atomic and remote-only.
The caller owns service execution, persistence and warning delivery.

Run the offline `resolve` and `extended_resolve` examples from the workspace:

```text
cargo run --offline --locked -p fabric-cli-context --example resolve
cargo run --offline --locked -p fabric-cli-context --example extended_resolve
```
