// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone virtual, OneLake and local consumers with immutable caller snapshots.

use fabric_cli_context::*;
use std::{error::Error, sync::Arc};

struct SelectedLocalFile;

impl LocalLookup for SelectedLocalFile {
    fn exists(&self, probe: LocalProbe<'_>) -> Result<bool, ContextError> {
        Ok(probe == LocalProbe::Path("selected.ipynb"))
    }
    fn access(&self, path: &str, mode: LocalAccess) -> Result<bool, ContextError> {
        Ok(path == "selected.ipynb" && mode == LocalAccess::Read)
    }
    fn dirname(&self, path: &str) -> Result<String, ContextError> {
        if path != "selected.ipynb" {
            return Err(ContextError::lookup(
                "InvalidInput",
                "Unselected local path",
            ));
        }
        Ok(String::new())
    }
    fn describe(&self, path: &str) -> Result<LocalMetadata, ContextError> {
        if path != "selected.ipynb" {
            return Err(ContextError::lookup(
                "InvalidInput",
                "Unselected local path",
            ));
        }
        Ok(LocalMetadata {
            path: path.into(),
            name: path.into(),
            id: "caller-selected-file".into(),
            kind: LocalKind::File,
        })
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let tenant = Element::tenant("Synthetic", "tenant");
    let workspace = Element::child(
        "Demo",
        Some("workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Arc::clone(&tenant),
    )?;
    let lake = Element::child(
        "Lake",
        Some("lake".into()),
        NodeKind::Item(ItemType::parse("Lakehouse")?),
        Arc::clone(&workspace),
    )?;
    let current = PathElement::from_fabric(tenant);
    let domains = PathElement::child(
        ".domains",
        None,
        PathKind::VirtualWorkspace(VirtualWorkspaceKind::Domain),
        Arc::clone(&current),
    )?;
    let domain = PathElement::child(
        "Example",
        Some("domain".into()),
        PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Domain),
        domains,
    )?;
    let catalog = HierarchyCatalog::new(
        Catalog::new(vec![workspace, lake]),
        vec![domain],
        vec![(
            "workspace/lake/Tables/Orders".into(),
            OneLakeProperties {
                status_code: 200,
                resource_type: Some("directory".into()),
                is_shortcut: false,
            },
        )],
    );
    let resolver = PathResolver::new(
        &catalog,
        current,
        HomeContext::Workspace("Demo.Workspace".into()),
        "ls",
    )?
    .with_local(&SelectedLocalFile);
    let resolved = resolver
        .resolve(
            PathInput::Scalar("/.domains/Example.Domain"),
            ResolveOptions::default(),
        )
        .map_err(|failure| failure.error)?;
    assert_eq!(resolved.element.path_id(), "/None/domain");
    let resolved = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Tables/Orders"),
            ResolveOptions::default(),
        )
        .map_err(|failure| failure.error)?;
    assert_eq!(
        resolved.element.kind(),
        PathKind::OneLake(OneLakeKind::Table)
    );
    assert_eq!(resolved.element.path_id(), "/workspace/lake/Tables/Orders");
    let local = resolver.resolve(
        PathInput::Scalar("selected.ipynb"),
        ResolveOptions {
            supports_local_path: true,
            ..ResolveOptions::default()
        },
    )?;
    assert_eq!(local.element.kind(), PathKind::Local(LocalKind::File));
    assert_eq!(
        local.element.local_metadata().unwrap().id,
        "caller-selected-file"
    );
    assert_eq!(resolver.current().path(), "/");
    Ok(())
}
