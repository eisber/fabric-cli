// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public consumer contracts complementing the independently authored oracle.

use fabric_cli_context::{
    Catalog, ContextError, Element, ErrorKind, HomeContext, ItemType, Lookup, LookupKind,
    LookupRequest, NodeKind, PathInput, ResolveOptions, Resolver, UnsupportedBranch, WorkspaceType,
};
use std::{cell::RefCell, sync::Arc};

fn child(parent: &Arc<Element>, name: &str, id: &str, kind: NodeKind) -> Arc<Element> {
    Element::child(name, Some(id.into()), kind, Arc::clone(parent)).unwrap()
}

fn graph() -> (Arc<Element>, Catalog) {
    let root = Element::tenant("Synthetic", "tenant");
    let workspace = child(
        &root,
        "Team",
        "w:first",
        NodeKind::Workspace(WorkspaceType::Workspace),
    );
    let folder = child(&workspace, "First", "f:1", NodeKind::Folder);
    let nested = child(&folder, "Second", "f:2", NodeKind::Folder);
    let notebook = NodeKind::Item(ItemType::parse("Notebook").unwrap());
    let item = child(&nested, "A.B", "i:first", notebook);
    let duplicate_item = child(&workspace, "A.B", "i:second", notebook);
    let duplicate_ws = child(
        &root,
        "Team",
        "w:second",
        NodeKind::Workspace(WorkspaceType::Workspace),
    );
    let other = child(
        &root,
        "Other",
        "other",
        NodeKind::Workspace(WorkspaceType::Workspace),
    );
    (
        root,
        Catalog::new(vec![
            workspace,
            folder,
            nested,
            item,
            duplicate_item,
            duplicate_ws,
            other,
        ]),
    )
}

fn home() -> HomeContext {
    HomeContext::Workspace("Team.Workspace".into())
}

#[test]
fn root_dot_parents_and_empty_tokens_do_not_lookup() {
    let root = Element::tenant("Explicit", "opaque");
    let lookup = |_: LookupRequest<'_>| -> Result<Option<String>, ContextError> {
        panic!("root/dot/parent paths must not lookup");
    };
    let resolver = Resolver::new(&lookup, Arc::clone(&root), home());
    for path in [
        "/", "/..", ".", "..", "../../..", "", "////", "/./../", "././",
    ] {
        let value = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap();
        assert!(Arc::ptr_eq(&value, &root), "{path}");
        assert_eq!(value.path_id(), "/");
    }
}

#[test]
fn source_folder_paths_and_workspace_wide_first_matches_are_preserved() {
    let (root, catalog) = graph();
    let resolver = Resolver::new(&catalog, root, home());
    for (path, expected, idpath) in [
        ("/Team.workspace", "/Team.Workspace", "/w:first"),
        (
            "~/First.Folder/Second.Folder",
            "/Team.Workspace/First.Folder/Second.Folder",
            "/w:first",
        ),
        (
            "~/A.B.notebook",
            "/Team.Workspace/A.B.Notebook",
            "/w:first/i:first",
        ),
        (
            "~/First.Folder/A.B.Notebook",
            "/Team.Workspace/First.Folder/A.B.Notebook",
            "/w:first/i:first",
        ),
        (
            "~/Second.Folder",
            "/Team.Workspace/Second.Folder",
            "/w:first",
        ),
    ] {
        let value = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap();
        assert_eq!(value.path(), expected, "{path}");
        assert_eq!(value.path_id(), idpath, "{path}");
    }
}

#[test]
fn relative_resolution_and_explicit_change_are_atomic() {
    let (root, catalog) = graph();
    let mut resolver = Resolver::new(&catalog, root, home());
    let folder = resolver
        .change_context(
            PathInput::Scalar("~/First.Folder"),
            ResolveOptions::default(),
        )
        .unwrap();
    let parent = resolver
        .resolve(PathInput::Scalar(".."), ResolveOptions::default())
        .unwrap();
    assert_eq!(parent.path(), "/Team.Workspace");
    assert!(Arc::ptr_eq(resolver.current(), &folder));
    let error = resolver
        .change_context(
            PathInput::Scalar("Second.Folder/Missing.Notebook"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, "NotFound");
    assert_eq!(
        error.message,
        "The Item 'Missing.Notebook' could not be found"
    );
    assert!(Arc::ptr_eq(resolver.current(), &folder));
    let value = resolver
        .change_context(
            PathInput::Scalar("../../Other.Workspace"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(value.path(), "/Other.Workspace");
    assert!(Arc::ptr_eq(resolver.current(), &value));
}

#[test]
fn raise_error_false_retains_unresolved_nodes_and_enforces_missing_parents() {
    let (root, catalog) = graph();
    let resolver = Resolver::new(&catalog, root, home());
    let options = ResolveOptions {
        raise_error: false,
        ..ResolveOptions::default()
    };
    for (path, expected) in [
        ("/Missing.Workspace", "/None"),
        ("~/Missing.Notebook", "/w:first/None"),
        ("~/Missing.Folder", "/w:first"),
    ] {
        let value = resolver.resolve(PathInput::Scalar(path), options).unwrap();
        assert_eq!(value.id(), None);
        assert_eq!(value.path_id(), expected);
    }
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("/Missing.Workspace/X.Notebook"), options)
            .unwrap_err()
            .message,
        "The Workspace 'Missing.Workspace' could not be found"
    );
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("~/Missing.Folder/X.Notebook"), options)
            .unwrap_err()
            .message,
        "Folder 'Missing.Folder' not found"
    );
    let recovered = resolver
        .resolve(
            PathInput::Scalar("/Missing.Workspace/../Team.Workspace"),
            options,
        )
        .unwrap();
    assert_eq!(recovered.id(), Some("w:first"));
}

#[test]
fn lookup_sequence_and_failure_are_not_rewritten_or_suppressed() {
    let (root, catalog) = graph();
    let trace = RefCell::new(Vec::new());
    let lookup = |request: LookupRequest<'_>| {
        trace
            .borrow_mut()
            .push((request.kind, request.parent.path(), request.name.to_owned()));
        if request.name == "Failure.Notebook" {
            Err(ContextError::lookup(
                "Forbidden",
                "caller-defined service failure",
            ))
        } else {
            catalog.lookup(request)
        }
    };
    let resolver = Resolver::new(&lookup, Arc::clone(&root), home());
    let error = resolver
        .resolve(
            PathInput::Scalar("~/First.Folder/Failure.Notebook"),
            ResolveOptions {
                raise_error: false,
                ..ResolveOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        error,
        ContextError::lookup("Forbidden", "caller-defined service failure")
    );
    assert_eq!(
        *trace.borrow(),
        [
            (LookupKind::Workspace, "/".into(), "Team.Workspace".into()),
            (
                LookupKind::Folder,
                "/Team.Workspace".into(),
                "First.Folder".into()
            ),
            (
                LookupKind::Item,
                "/Team.Workspace".into(),
                "Failure.Notebook".into()
            ),
        ]
    );
    assert!(Arc::ptr_eq(resolver.current(), &root));
}

#[test]
fn unsupported_branches_never_succeed_as_fabric_or_touch_local_lookup() {
    let (root, catalog) = graph();
    let resolver = Resolver::new(&catalog, root, home());
    for (path, branch) in [
        (
            "/.CAPACITIES/x.Capacity",
            UnsupportedBranch::VirtualWorkspace,
        ),
        (
            "~/.externaldatashares/x.ExternalDataShare",
            UnsupportedBranch::VirtualItemContainer,
        ),
        ("~/A.B.Notebook/Files", UnsupportedBranch::OneLake),
    ] {
        let error = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported(branch));
    }
    let no_lookup = |_: LookupRequest<'_>| -> Result<Option<String>, ContextError> {
        panic!("local lookup forbidden")
    };
    let resolver = Resolver::new(&no_lookup, Element::tenant("t", "t"), home());
    for (input, options) in [
        (
            PathInput::Local("C:\\Team.Workspace"),
            ResolveOptions::default(),
        ),
        (
            PathInput::Scalar("/Team.Workspace"),
            ResolveOptions {
                supports_local_path: true,
                ..ResolveOptions::default()
            },
        ),
    ] {
        assert_eq!(
            resolver.resolve(input, options).unwrap_err().kind,
            ErrorKind::Unsupported(UnsupportedBranch::LocalFilesystem)
        );
    }
}

#[test]
fn invalid_hidden_and_suffix_cases_keep_python_status_and_error_paths() {
    let (root, catalog) = graph();
    let resolver = Resolver::new(&catalog, root, home());
    for (path, code, message) in [
        ("/...", "InvalidPath", "The specified path is invalid"),
        (
            "~/...suffix",
            "InvalidPath",
            "The specified path is invalid",
        ),
        ("/bogus", "InvalidPath", "The path '/bogus' is invalid"),
        (
            "/Workspace",
            "Invalid workspace name",
            "Invalid workspace name 'Workspace'",
        ),
        (
            "~/.Notebook",
            "InvalidPath",
            "The path '/Team.Workspace/.Notebook' is invalid",
        ),
        (
            "~/x.Item",
            "InvalidItemType",
            "'Item' is not a valid Fabric item type",
        ),
        (
            "~/bad name.Lakehouse",
            "Special characters not supported for this item type",
            "Lakehouse name 'bad name' contains unsupported special characters",
        ),
        (
            "~/First.Folder/.sparkpools",
            "InvalidPath",
            "The path '/Team.Workspace/First.Folder/.sparkpools' is invalid",
        ),
    ] {
        let error = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            (code, message),
            "{path}"
        );
    }
}

#[test]
fn case_unicode_escaped_slashes_and_token_quotes_follow_python() {
    let root = Element::tenant("t", "t");
    let workspace = child(
        &root,
        "Team/A",
        "w",
        NodeKind::Workspace(WorkspaceType::Workspace),
    );
    let notebook = NodeKind::Item(ItemType::parse("Notebook").unwrap());
    let name = "\u{e9}.分析";
    let item = child(&workspace, name, "i", notebook);
    let quoted = child(&workspace, "A B\"", "q", notebook);
    let catalog = Catalog::new(vec![workspace, item, quoted]);
    let resolver = Resolver::new(&catalog, root, home());
    let value = resolver
        .resolve(
            PathInput::Scalar(
                "/Team\\/A.Wor\u{212a}space/\u{1c}\u{e9}.分析.NoteBoo\u{212a}\u{1f}//",
            ),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(value.path(), "/Team\\/A.Workspace/\u{e9}.分析.Notebook");
    let tokens = ["\"/Team\\/A.Workspace/A", "B\".Notebook"];
    let value = resolver
        .resolve(PathInput::Tokens(&tokens), ResolveOptions::default())
        .unwrap();
    assert_eq!(value.short_name(), "A B\"");
    let error = resolver
        .resolve(
            PathInput::Scalar("/team\\/A.Workspace"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, "NotFound");
}

#[test]
fn explicit_home_error_and_invalid_metadata_are_reported() {
    let (root, catalog) = graph();
    let error = ContextError::lookup(
        "InvalidOperation",
        "The personal workspace is only available with user authentication",
    );
    let resolver = Resolver::new(
        &catalog,
        Arc::clone(&root),
        HomeContext::Unavailable(error.clone()),
    );
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("~"), ResolveOptions::default())
            .unwrap_err(),
        error
    );
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("/"), ResolveOptions::default())
            .unwrap()
            .path(),
        "/"
    );
    let error = Element::child("invalid", None, NodeKind::Folder, root).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidContext);
}

#[test]
fn trait_object_boundary_and_not_found_failure_keep_source_raise_error_semantics() {
    let root = Element::tenant("t", "t");
    let lookup =
        |_: LookupRequest<'_>| Err(ContextError::lookup("NotFound", "original caller miss"));
    let boundary: &dyn Lookup = &lookup;
    let resolver = Resolver::new(boundary, root, home());
    let raised = resolver
        .resolve(
            PathInput::Scalar("/Missing.Workspace"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(raised.message, "original caller miss");
    let unresolved = resolver
        .resolve(
            PathInput::Scalar("/Missing.Workspace"),
            ResolveOptions {
                raise_error: false,
                ..ResolveOptions::default()
            },
        )
        .unwrap();
    assert_eq!(unresolved.id(), None);
    assert_eq!(unresolved.path_id(), "/None");
}

#[test]
fn every_pinned_item_suffix_is_typed_and_keeps_its_exact_canonical_spelling() {
    for suffix in [
        "AISkill",
        "ApacheAirflowJob",
        "Exploration",
        "RetailDataManager",
        "Healthcaredatasolution",
        "MetricSet",
        "OrgApp",
        "SustainabilityDataSolution",
        "CosmosDBDatabase",
        "Dashboard",
        "Datamart",
        "DataPipeline",
        "DigitalTwinBuilder",
        "DigitalTwinBuilderFlow",
        "Environment",
        "Eventhouse",
        "Eventstream",
        "GraphModel",
        "KQLDashboard",
        "KQLDatabase",
        "KQLQueryset",
        "Lakehouse",
        "Map",
        "MirroredWarehouse",
        "MirroredDatabase",
        "MLExperiment",
        "MLModel",
        "Notebook",
        "Ontology",
        "PaginatedReport",
        "Reflex",
        "Report",
        "SemanticModel",
        "SparkJobDefinition",
        "SQLEndpoint",
        "Warehouse",
        "CopyJob",
        "GraphQLApi",
        "GraphQuerySet",
        "UserDataFunction",
        "MountedDataFactory",
        "SQLDatabase",
        "Dataflow",
        "VariableLibrary",
    ] {
        assert_eq!(
            ItemType::parse(&suffix.to_uppercase()).unwrap().as_str(),
            suffix
        );
    }
    assert_eq!(
        ItemType::parse("NoteBoo\u{212a}").unwrap().as_str(),
        "Notebook"
    );
    for suffix in ["", "FutureItem", "Item", "Personal", "Folder"] {
        assert_eq!(ItemType::parse(suffix).unwrap_err().code, "InvalidItemType");
    }
}

#[test]
fn catalog_is_ordered_and_unrelated_tenants_or_workspaces_never_leak() {
    let (root, first) = graph();
    let other_tenant = Element::tenant("other", "other-tenant");
    let foreign_workspace = child(
        &other_tenant,
        "Team",
        "foreign",
        NodeKind::Workspace(WorkspaceType::Workspace),
    );
    let foreign_item = child(
        &foreign_workspace,
        "A.B",
        "foreign-item",
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
    );
    let mut nodes = vec![foreign_workspace, foreign_item];
    nodes.extend_from_slice(first.elements());
    let catalog = Catalog::new(nodes);
    let resolver = Resolver::new(&catalog, root, home());
    let result = resolver
        .resolve(
            PathInput::Scalar("~/A.B.Notebook"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(result.path_id(), "/w:first/i:first");
}

#[test]
fn direct_catalog_consumers_cannot_treat_invalid_scopes_as_absent_nodes() {
    let (root, catalog) = graph();
    let error = catalog
        .lookup(LookupRequest {
            kind: LookupKind::Item,
            parent: &root,
            name: "A.B.Notebook",
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidContext);
}
