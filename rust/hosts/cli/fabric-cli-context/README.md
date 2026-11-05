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

## Opt-in similar-name hints

`resolve` and `change_context` retain their existing exact-name selection,
return types, errors and messages. No command grammar or default CLI behavior
changes. Call `Resolver::resolve_with_hints` explicitly to request advisory names:

```rust
use fabric_cli_context::{
    Catalog, Element, HomeContext, NameHints, NodeKind, PathInput, ResolveOptions,
    Resolver, SearchOptions, WorkspaceType,
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
let resolver = Resolver::new(&catalog, tenant, HomeContext::Unspecified);
let failure = resolver
    .resolve_with_hints(
        PathInput::Scalar("/Exampel.Workspace"),
        ResolveOptions::default(),
        SearchOptions::default(),
    )
    .unwrap_err();
assert_eq!(failure.failure.code, "NotFound");
let NameHints::Suggestions(names) = &failure.hints else {
    panic!("expected a names-only snapshot");
};
assert_eq!(names[0].name, "Example.Workspace");
// Error display is unchanged. Advisory display separately escapes snapshot names.
assert_eq!(failure.to_string(), failure.failure.to_string());
assert!(failure.hints.to_string().contains("not selected"));
assert_eq!(resolver.current().path(), "/");
# Ok::<(), fabric_cli_context::ContextError>(())
```

`NameHintFailure` keeps the original failure separate from `NameHints`.
`Suggestions` contains full canonical names and shared-engine scores, not IDs or
successful elements. It can be empty. `Unavailable` explicitly records an absent
snapshot, failed snapshot read, or typed `SearchError`; the original `NotFound`
is not replaced. `NotApplicable` means the failure is ineligible. Search defaults
are five results at minimum score 0.6. Invalid options, names longer than 512
Unicode scalars, more than 4,096 candidates, or aggregate UTF-8 input beyond
1 MiB yield an unavailable diagnostic, never truncation or a fabricated match.
The engine ranks short names (without artifact suffixes); the resolver attaches
the unchanged requested canonical suffix to its suggestions.

`Lookup::candidate_names` defaults to exposing nothing, so existing closures and
implementations remain compatible. Opt-in implementations must provide only
short names from an **already supplied immutable, caller-owned snapshot**, filtered
to the failed lookup's exact scope and artifact suffix/type. They must not call a
service, enumerate remotely, use credentials, expose notes/values, widen scope or
fall back to another owner/type. The iterator is consumed lazily by the bounded
shared `fabric-name-search` engine; no local fuzzy matcher is used.

`Catalog` supplies this names-only view using the same scope predicate as exact
lookup: selected tenant for workspaces, selected workspace for items/root-folder
lookups (including the existing workspace-wide folder/item quirks), and selected
folder for nested-folder lookups. Workspace subtypes and item types stay distinct;
unresolved/empty-ID metadata is not suggested. `HierarchyCatalog` delegates its
Fabric names to that same view. Neither catalog acquires metadata.

Exact success never accesses candidates or ranks names, even with invalid search
options. Only an eligible exact lookup `NotFound` can have hints. Permission,
access, validation, unsupported-capability and transient failures do not collect
names. `raise_error = false` retains the exact legacy placeholder behavior and
does not collect hints, including subsequent missing-parent failures.

`PathResolver::resolve_with_hints` uses the same adapter for Fabric workspace,
folder and item paths. Original `PathFailure` errors/notices remain intact.
Virtual and OneLake failures have no hint source. A local fallback attempt never
collects hints, even when it ultimately fails with `NotFound`.

Treat `NameHint::name` as untrusted display metadata. `escaped_name` and `Display`
escape controls, quotes, backslashes and non-ASCII characters, including terminal
and bidirectional controls. `NameHints` display never echoes the query. Legacy
errors are intentionally unchanged and must still follow the consumer's existing
output-sanitization policy. Do not retry a mutation using a suggested name: require
a new explicit, exact-name request and apply normal authorization/confirmation.

The executable offline example `variable_delete_hints` demonstrates a
purely synthetic variable-name deletion guard. A typo produces ranked names and
zero simulated effects; an exact name produces one simulated effect. Notes and
values are not search inputs. It does not implement a Fabric variable operation,
remote deletion or native definition authoring:

```text
cargo run --offline --locked -p fabric-cli-context --example variable_delete_hints
cargo test --offline --locked -p fabric-cli-context --test name_hints --example variable_delete_hints
```

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
