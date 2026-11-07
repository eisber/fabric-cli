// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone consumer: caller-owned metadata, no SDK, runtime or ambient state.

use fabric_cli_context::{
    Catalog, Element, HomeContext, ItemType, NodeKind, PathInput, ResolveOptions, Resolver,
    WorkspaceType,
};
use std::{error::Error, sync::Arc};

fn main() -> Result<(), Box<dyn Error>> {
    let tenant = Element::tenant("Synthetic", "opaque-tenant");
    let workspace = Element::child(
        "Example",
        Some("opaque-workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Arc::clone(&tenant),
    )?;
    let notebook = Element::child(
        "Analysis",
        Some("opaque-item".into()),
        NodeKind::Item(ItemType::parse("Notebook")?),
        Arc::clone(&workspace),
    )?;
    let catalog = Catalog::new(vec![workspace, notebook]);
    let resolver = Resolver::new(
        &catalog,
        tenant,
        HomeContext::Workspace("Example.Workspace".into()),
    );
    let result = resolver.resolve(
        PathInput::Scalar("~/Analysis.Notebook"),
        ResolveOptions::default(),
    )?;
    assert_eq!(result.path(), "/Example.Workspace/Analysis.Notebook");
    assert_eq!(result.path_id(), "/opaque-workspace/opaque-item");
    assert_eq!(resolver.current().path(), "/");
    Ok(())
}
