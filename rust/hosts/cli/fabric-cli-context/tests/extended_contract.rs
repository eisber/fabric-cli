// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public extended consumers, including conditional local boundary order.

use fabric_cli_context::*;
use std::{cell::RefCell, sync::Arc};

fn graph() -> (Arc<PathElement>, HierarchyCatalog) {
    let tenant = Element::tenant("Synthetic", "t");
    let workspace = Element::child(
        "W",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Arc::clone(&tenant),
    )
    .unwrap();
    let lake = Element::child(
        "Lake",
        Some("l".into()),
        NodeKind::Item(ItemType::parse("Lakehouse").unwrap()),
        Arc::clone(&workspace),
    )
    .unwrap();
    let note = Element::child(
        "Note",
        Some("n".into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        Arc::clone(&workspace),
    )
    .unwrap();
    let root = PathElement::from_fabric(tenant);
    let mut virtuals = Vec::new();
    for kind in VirtualWorkspaceKind::ALL {
        let collection = PathElement::child(
            kind.collection(),
            None,
            PathKind::VirtualWorkspace(kind),
            Arc::clone(&root),
        )
        .unwrap();
        virtuals.push(
            PathElement::child(
                "Demo",
                Some(format!("id-{}", kind.suffix())),
                PathKind::VirtualWorkspaceItem(kind),
                collection,
            )
            .unwrap(),
        );
    }
    for kind in VirtualItemKind::ALL {
        let collection = PathElement::child(
            kind.collection(),
            None,
            PathKind::VirtualItemContainer(kind),
            PathElement::from_fabric(Arc::clone(&workspace)),
        )
        .unwrap();
        virtuals.push(
            PathElement::child(
                "Demo",
                Some(format!("id-{}", kind.suffix())),
                PathKind::VirtualItem(kind),
                collection,
            )
            .unwrap(),
        );
    }
    let properties = [
        ("w/l/Files/data.csv", "file", false),
        ("w/l/Tables/T", "directory", false),
        ("w/l/Tables/T/part", "directory", false),
        ("w/l/Files/link", "directory", true),
        ("w/l/Files/link/child", "file", false),
        ("w/l/Files/literal.Shortcut", "file", false),
        ("w/l/Files/unknown", "unexpected", false),
    ]
    .into_iter()
    .map(|(path, resource_type, is_shortcut)| {
        (
            path.into(),
            OneLakeProperties {
                status_code: 200,
                resource_type: Some(resource_type.into()),
                is_shortcut,
            },
        )
    })
    .collect();
    (
        root,
        HierarchyCatalog::new(
            Catalog::new(vec![workspace, lake, note]),
            virtuals,
            properties,
        ),
    )
}

fn resolver<'a>(
    lookup: &'a HierarchyCatalog,
    root: Arc<PathElement>,
) -> PathResolver<'a, HierarchyCatalog> {
    PathResolver::new(
        lookup,
        root,
        HomeContext::Workspace("W.Workspace".into()),
        "ls",
    )
    .unwrap()
}

#[test]
fn all_virtual_collections_and_leaf_types_resolve_with_source_idpaths() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    for kind in VirtualWorkspaceKind::ALL {
        let path = format!(
            "/{}/Demo.{}",
            kind.collection().to_uppercase(),
            kind.suffix().to_lowercase()
        );
        let value = resolver
            .resolve(PathInput::Scalar(&path), ResolveOptions::default())
            .unwrap();
        assert_eq!(
            value.element.path(),
            format!("/{}/Demo.{}", kind.collection(), kind.suffix())
        );
        assert_eq!(
            value.element.path_id(),
            format!("/None/id-{}", kind.suffix())
        );
        assert_eq!(value.element.kind(), PathKind::VirtualWorkspaceItem(kind));
    }
    for kind in VirtualItemKind::ALL {
        let path = format!("~/{}/Demo.{}", kind.collection(), kind.suffix());
        let value = resolver
            .resolve(PathInput::Scalar(&path), ResolveOptions::default())
            .unwrap();
        assert_eq!(value.element.path_id(), format!("/w/id-{}", kind.suffix()));
        assert_eq!(value.element.kind(), PathKind::VirtualItem(kind));
        assert_eq!(
            value.notices.len(),
            usize::from(kind == VirtualItemKind::ExternalDataShare)
        );
    }
}

#[test]
fn virtual_mismatch_invalid_name_leaf_and_missing_errors_are_exact() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    for (path, code, message) in [
        (
            "/.capacities/x.Domain",
            "InvalidPath",
            "The item type 'Domain' is not supported in the context 'VirtualWorkspace'",
        ),
        (
            "~/.sparkpools/x.ManagedIdentity",
            "InvalidPath",
            "The item type 'ManagedIdentity' is not supported in the context 'VirtualItemContainer'",
        ),
        (
            "/.domains/noSuffix",
            "Invalid item name",
            "Invalid item name 'noSuffix'",
        ),
        (
            "/.connections/x.Notebook",
            "InvalidItemType",
            "'Notebook' is not a valid Fabric item type",
        ),
        (
            "/.gateways/Absent.Gateway",
            "NotFound",
            "The Gateway 'Absent.Gateway' could not be found",
        ),
        (
            "~/.sparkpools/Absent.SparkPool",
            "NotFound",
            "The Spark Pool 'Absent.SparkPool' could not be found",
        ),
        (
            "/.domains/Demo.Domain/child",
            "InvalidPath",
            "Traversing into '/.domains/Demo.Domain' is not supported",
        ),
    ] {
        let error = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap_err();
        assert_eq!(
            (error.error.code.as_str(), error.error.message.as_str()),
            (code, message)
        );
    }
    let value = resolver
        .resolve(
            PathInput::Scalar("/.capacities/New.Capacity"),
            ResolveOptions {
                raise_error: false,
                ..ResolveOptions::default()
            },
        )
        .unwrap();
    assert_eq!(value.element.id(), None);
    assert_eq!(value.element.path_id(), "/None/None");
}

#[test]
fn onelake_folders_tables_shortcuts_retry_and_undefined_follow_source() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    for (path, kind, expected_path) in [
        (
            "~/Lake.Lakehouse/files/data.csv",
            OneLakeKind::File,
            "/W.Workspace/Lake.Lakehouse/Files/data.csv",
        ),
        (
            "~/Lake.Lakehouse/Tables/T",
            OneLakeKind::Table,
            "/W.Workspace/Lake.Lakehouse/Tables/T",
        ),
        (
            "~/Lake.Lakehouse/Tables/T/part",
            OneLakeKind::Folder,
            "/W.Workspace/Lake.Lakehouse/Tables/T/part",
        ),
        (
            "~/Lake.Lakehouse/Files/link.Shortcut",
            OneLakeKind::Shortcut,
            "/W.Workspace/Lake.Lakehouse/Files/link.Shortcut",
        ),
        (
            "~/Lake.Lakehouse/Files/literal.Shortcut",
            OneLakeKind::File,
            "/W.Workspace/Lake.Lakehouse/Files/literal.Shortcut",
        ),
        (
            "~/Lake.Lakehouse/Files/unknown",
            OneLakeKind::Undefined,
            "/W.Workspace/Lake.Lakehouse/Files/unknown",
        ),
    ] {
        let value = resolver
            .resolve(PathInput::Scalar(path), ResolveOptions::default())
            .unwrap();
        assert_eq!(value.element.kind(), PathKind::OneLake(kind));
        assert_eq!(value.element.path(), expected_path);
        assert_eq!(value.element.id(), Some("0000"));
    }
    let child = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/link.Shortcut/child"),
            ResolveOptions::default(),
        )
        .unwrap()
        .element;
    assert_eq!(child.path_id(), "/w/l/Files/link/child");
    assert_eq!(child.local_path().as_deref(), Some("Files/link/child"));
    assert_eq!(child.root_folder(), Some("Files"));
    assert!(child.is_shortcut_path());
}

#[test]
fn missing_onelake_is_only_an_unresolved_target_at_the_final_token() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    let options = ResolveOptions {
        raise_error: false,
        ..ResolveOptions::default()
    };
    let target = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/New.Shortcut"),
            options,
        )
        .unwrap();
    assert_eq!(target.element.id(), None);
    assert_eq!(
        target.element.kind(),
        PathKind::OneLake(OneLakeKind::Undefined)
    );
    assert_eq!(target.element.short_name(), "New.Shortcut");
    for path in [
        "~/Lake.Lakehouse/Files/New/child",
        "~/Lake.Lakehouse/Files/New/.",
    ] {
        assert_eq!(
            resolver
                .resolve(PathInput::Scalar(path), options)
                .unwrap_err()
                .error
                .code,
            "NotFound"
        );
    }
}

#[test]
fn source_list_support_precedes_missing_id_and_folder_validation() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    let error = resolver
        .resolve(
            PathInput::Scalar("~/Note.Notebook/Files"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.error.code, "UnsupportedCommand");
    assert_eq!(error.error.message, "not supported for command 'ls'");
    let error = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/not-valid"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.error.message,
        "The folder 'not-valid' could not be found in item 'Lake.Lakehouse'. Valid folders are: Files, Tables, TableMaintenance"
    );
    let policy = CommandPolicy {
        supported_elements: vec!["Item".into()],
        unsupported_items: vec!["Lakehouse".into()],
        ..CommandPolicy::default()
    };
    let resolver = resolver.with_list_policy(policy);
    assert_eq!(
        resolver
            .resolve(
                PathInput::Scalar("~/Lake.Lakehouse/Files"),
                ResolveOptions::default()
            )
            .unwrap_err()
            .error
            .code,
        "UnsupportedCommand"
    );
}

#[test]
fn extended_relative_context_and_changes_remain_atomic_and_root_clamps() {
    let (root, catalog) = graph();
    let mut resolver = resolver(&catalog, root);
    let current = resolver
        .change_context(
            PathInput::Scalar("~/Lake.Lakehouse/Files"),
            ResolveOptions::default(),
        )
        .unwrap()
        .element;
    let error = resolver
        .change_context(PathInput::Scalar("missing/x"), ResolveOptions::default())
        .unwrap_err();
    assert_eq!(error.error.code, "NotFound");
    assert!(Arc::ptr_eq(resolver.current(), &current));
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("////"), ResolveOptions::default())
            .unwrap()
            .element
            .path(),
        "/"
    );
    let result = resolver
        .resolve(
            PathInput::Scalar("../../../.domains/Demo.Domain"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(result.element.path(), "/.domains/Demo.Domain");
}

struct Filesystem {
    calls: RefCell<Vec<String>>,
    present: bool,
    readable: bool,
    parent: String,
    writable: bool,
    boolean_exists: bool,
}

impl LocalLookup for Filesystem {
    fn exists(&self, probe: LocalProbe<'_>) -> Result<bool, ContextError> {
        self.calls.borrow_mut().push(format!("exists:{probe:?}"));
        Ok(match probe {
            LocalProbe::Path("") => false,
            LocalProbe::Path(_) => self.present,
            LocalProbe::Boolean(_) => self.boolean_exists,
        })
    }
    fn access(&self, path: &str, mode: LocalAccess) -> Result<bool, ContextError> {
        self.calls
            .borrow_mut()
            .push(format!("access:{path}:{mode:?}"));
        Ok(match mode {
            LocalAccess::Read => self.readable,
            LocalAccess::Write => self.writable,
        })
    }
    fn dirname(&self, path: &str) -> Result<String, ContextError> {
        self.calls.borrow_mut().push(format!("dirname:{path}"));
        Ok(self.parent.clone())
    }
    fn describe(&self, path: &str) -> Result<LocalMetadata, ContextError> {
        self.calls.borrow_mut().push(format!("describe:{path}"));
        Ok(LocalMetadata {
            path: path.into(),
            name: "new.txt".into(),
            id: "opaque-local".into(),
            kind: LocalKind::File,
        })
    }
}

fn filesystem() -> Filesystem {
    Filesystem {
        calls: RefCell::new(Vec::new()),
        present: true,
        readable: true,
        parent: "parent".into(),
        writable: true,
        boolean_exists: true,
    }
}

#[test]
fn local_fallback_is_conditional_and_never_replaces_remote_success() {
    let (root, catalog) = graph();
    let fs = filesystem();
    let resolver = resolver(&catalog, root).with_local(&fs);
    let options = ResolveOptions {
        supports_local_path: true,
        ..ResolveOptions::default()
    };
    let value = resolver
        .resolve(PathInput::Scalar("/W.Workspace"), options)
        .unwrap();
    assert!(value.element.fabric().is_some());
    assert!(fs.calls.borrow().is_empty());
    let value = resolver
        .resolve(PathInput::Scalar("parent/new.txt"), options)
        .unwrap();
    assert_eq!(value.element.kind(), PathKind::Local(LocalKind::File));
    assert_eq!(value.element.path(), "parent/new.txt");
    assert_eq!(value.element.path_id(), "/");
    assert_eq!(
        value.notices[0].message(),
        "parent/new.txt not found in Fabric, checking local file system"
    );
    assert_eq!(
        *fs.calls.borrow(),
        [
            "exists:Path(\"parent/new.txt\")",
            "access:parent/new.txt:Read",
            "describe:parent/new.txt"
        ]
    );
}

#[test]
fn writable_parent_preserves_boolean_exists_bug_and_two_dirname_calls() {
    let (root, catalog) = graph();
    let mut fs = filesystem();
    fs.present = false;
    let resolver = resolver(&catalog, root).with_local(&fs);
    let value = resolver
        .resolve(
            PathInput::Scalar("parent/new.txt"),
            ResolveOptions {
                supports_local_path: true,
                raise_error: false,
            },
        )
        .unwrap();
    assert!(matches!(value.element.kind(), PathKind::Local(_)));
    assert_eq!(
        *fs.calls.borrow(),
        [
            "exists:Path(\"parent/new.txt\")",
            "dirname:parent/new.txt",
            "dirname:parent/new.txt",
            "access:parent:Write",
            "exists:Boolean(true)",
            "describe:parent/new.txt"
        ]
    );
}

#[test]
fn missing_local_and_unsupported_command_leave_precise_failure_and_notices() {
    let (root, catalog) = graph();
    let mut fs = filesystem();
    fs.present = false;
    fs.boolean_exists = false;
    let resolver = resolver(&catalog, root).with_local(&fs);
    let options = ResolveOptions {
        supports_local_path: true,
        ..ResolveOptions::default()
    };
    let error = resolver
        .resolve(PathInput::Scalar("missing.txt"), options)
        .unwrap_err();
    assert_eq!(error.error.code, "InvalidPath");
    assert_eq!(error.notices.len(), 1);
    fs.calls.borrow_mut().clear();
    let error = resolver
        .resolve(PathInput::Scalar("~/Note.Notebook/Files"), options)
        .unwrap_err();
    assert_eq!(error.error.code, "UnsupportedCommand");
    assert!(error.notices.is_empty());
    assert!(fs.calls.borrow().is_empty());
}

#[test]
fn home_selection_is_pure_ordered_and_identity_explicit() {
    let tenant = Element::tenant("t", "t");
    let first = Element::child(
        "Home",
        Some("a".into()),
        NodeKind::Workspace(WorkspaceType::Personal),
        Arc::clone(&tenant),
    )
    .unwrap();
    let second = Element::child(
        "Later",
        Some("b".into()),
        NodeKind::Workspace(WorkspaceType::Personal),
        tenant,
    )
    .unwrap();
    assert!(
        matches!(select_home(true, &[first, second]), HomeContext::Workspace(name) if name == "Home.Personal")
    );
    assert!(
        matches!(select_home(true, &[]), HomeContext::Unavailable(error) if error.code == "NotFound")
    );
    assert!(
        matches!(select_home(false, &[]), HomeContext::Unavailable(error) if error.code == "InvalidOperation")
    );
}

struct TracedLookup {
    catalog: HierarchyCatalog,
    calls: RefCell<Vec<String>>,
}

impl Lookup for TracedLookup {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self.catalog.lookup(request)
    }
}

impl HierarchyLookup for TracedLookup {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        self.calls.borrow_mut().push(format!(
            "virtual:{}:{}",
            request.parent.path(),
            request.name
        ));
        if request.name == "Denied.Capacity" {
            return Err(ContextError::lookup("Forbidden", "denied by caller"));
        }
        self.catalog.virtual_id(request)
    }
    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        self.calls.borrow_mut().push(path.into());
        if path.ends_with("/denied") {
            return Err(ContextError::lookup("Forbidden", "properties denied"));
        }
        if path.ends_with("/missing") {
            return Err(ContextError::lookup("NotFound", "source missing"));
        }
        if path.ends_with("/wrong-status") {
            return Ok(Some(OneLakeProperties {
                status_code: 204,
                resource_type: Some("file".into()),
                is_shortcut: false,
            }));
        }
        self.catalog.properties(path)
    }
}

#[test]
fn source_properties_retry_and_error_paths_have_exact_lookup_order() {
    let (root, catalog) = graph();
    let lookup = TracedLookup {
        catalog,
        calls: RefCell::new(Vec::new()),
    };
    let resolver = PathResolver::new(
        &lookup,
        root,
        HomeContext::Workspace("W.Workspace".into()),
        "dir",
    )
    .unwrap();
    let error = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/missing"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.error.message,
        "The path 'w/l/Files/missing' could not be found"
    );
    assert_eq!(
        *lookup.calls.borrow(),
        ["w/l/Files/missing", "w/l/Files/missing"]
    );
    lookup.calls.borrow_mut().clear();
    let error = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/denied.Shortcut"),
            ResolveOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.error.code, "Forbidden");
    assert_eq!(*lookup.calls.borrow(), ["w/l/Files/denied"]);
    lookup.calls.borrow_mut().clear();
    let value = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/literal.Shortcut"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(value.element.kind(), PathKind::OneLake(OneLakeKind::File));
    assert_eq!(
        *lookup.calls.borrow(),
        ["w/l/Files/literal", "w/l/Files/literal.Shortcut"]
    );
}

#[test]
fn virtual_nonnotfound_and_home_errors_do_not_fall_back_to_local() {
    let (root, catalog) = graph();
    let lookup = TracedLookup {
        catalog,
        calls: RefCell::new(Vec::new()),
    };
    let fs = filesystem();
    let resolver = PathResolver::new(
        &lookup,
        Arc::clone(&root),
        HomeContext::Workspace("W.Workspace".into()),
        "ls",
    )
    .unwrap()
    .with_local(&fs);
    let error = resolver
        .resolve(
            PathInput::Scalar("/.capacities/Denied.Capacity"),
            ResolveOptions {
                raise_error: false,
                supports_local_path: true,
            },
        )
        .unwrap_err();
    assert_eq!(error.error.code, "Forbidden");
    assert!(fs.calls.borrow().is_empty());
    let resolver = PathResolver::new(
        &lookup,
        root,
        HomeContext::Unavailable(ContextError::lookup("NotFound", "no home")),
        "ls",
    )
    .unwrap()
    .with_local(&fs);
    assert_eq!(
        resolver
            .resolve(
                PathInput::Scalar("~"),
                ResolveOptions {
                    raise_error: false,
                    supports_local_path: true
                }
            )
            .unwrap_err()
            .error
            .message,
        "no home"
    );
    assert!(fs.calls.borrow().is_empty());
}

#[test]
fn false_permission_boolean_and_empty_parent_keep_the_source_expression_shape() {
    let (root, catalog) = graph();
    let mut fs = filesystem();
    fs.present = false;
    fs.writable = false;
    let resolver = resolver(&catalog, Arc::clone(&root)).with_local(&fs);
    let options = ResolveOptions {
        raise_error: false,
        supports_local_path: true,
    };
    assert!(
        resolver
            .resolve(PathInput::Scalar("parent/new.txt"), options)
            .is_ok()
    );
    assert!(
        fs.calls
            .borrow()
            .iter()
            .any(|call| call == "exists:Boolean(false)")
    );
    fs.calls.borrow_mut().clear();
    fs.parent.clear();
    let resolver = PathResolver::new(&catalog, root, HomeContext::Unspecified, "ls")
        .unwrap()
        .with_local(&fs);
    let error = resolver
        .resolve(PathInput::Scalar("new.txt"), options)
        .unwrap_err();
    assert_eq!(error.error.code, "InvalidPath");
    assert_eq!(
        *fs.calls.borrow(),
        [
            "exists:Path(\"new.txt\")",
            "dirname:new.txt",
            "exists:Path(\"\")"
        ]
    );
}

#[test]
fn explicit_local_metadata_and_failed_context_change_cannot_corrupt_remote_current() {
    let (root, catalog) = graph();
    let fs = filesystem();
    let mut resolver = resolver(&catalog, Arc::clone(&root)).with_local(&fs);
    let value = resolver
        .resolve(
            PathInput::Local("C:\\explicit\\new.txt"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert!(matches!(value.element.kind(), PathKind::Local(_)));
    assert_eq!(value.element.local_metadata().unwrap().id, "opaque-local");
    assert_eq!(*fs.calls.borrow(), ["describe:C:\\explicit\\new.txt"]);
    assert!(
        resolver
            .change_context(PathInput::Local("local"), ResolveOptions::default())
            .is_err()
    );
    assert!(Arc::ptr_eq(resolver.current(), &root));
    assert!(PathResolver::new(&catalog, value.element, HomeContext::Unspecified, "ls").is_err());
}

#[test]
fn final_external_warning_does_not_leak_to_sibling_or_parent_resolution() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    let value = resolver
        .resolve(
            PathInput::Scalar("~/.externaldatashares"),
            ResolveOptions::default(),
        )
        .unwrap();
    assert_eq!(value.notices, [PathNotice::ExternalDataSharing]);
    for path in [
        "~/.externaldatashares/..",
        "~/.externaldatashares/../Lake.Lakehouse",
    ] {
        assert!(
            resolver
                .resolve(PathInput::Scalar(path), ResolveOptions::default())
                .unwrap()
                .notices
                .is_empty()
        );
    }
}

#[test]
fn raw_non200_properties_is_a_typed_source_assertion_not_a_local_fallback() {
    let (root, catalog) = graph();
    let lookup = TracedLookup {
        catalog,
        calls: RefCell::new(Vec::new()),
    };
    let fs = filesystem();
    let resolver = PathResolver::new(
        &lookup,
        root,
        HomeContext::Workspace("W.Workspace".into()),
        "ls",
    )
    .unwrap()
    .with_local(&fs);
    let error = resolver
        .resolve(
            PathInput::Scalar("~/Lake.Lakehouse/Files/wrong-status"),
            ResolveOptions {
                raise_error: false,
                supports_local_path: true,
            },
        )
        .unwrap_err();
    assert_eq!(error.error.code, "AssertionError");
    assert_eq!(error.error.message, "");
    assert!(error.notices.is_empty());
    assert!(fs.calls.borrow().is_empty());
}

#[test]
fn source_item_folder_map_is_not_a_posix_folder_guess() {
    for (suffix, expected, supported) in [
        (
            "Lakehouse",
            &["Files", "Tables", "TableMaintenance"][..],
            true,
        ),
        ("Warehouse", &["Files", "Tables"][..], true),
        ("SemanticModel", &["Tables"][..], true),
        (
            "SparkJobDefinition",
            &["Libs", "Main", "Snapshots"][..],
            true,
        ),
        ("KQLDatabase", &["Tables", "Shortcut"][..], true),
        ("MirroredDatabase", &["Files", "Tables"][..], true),
        ("MirroredWarehouse", &["Files", "Tables"][..], false),
        ("SQLDatabase", &["Tables", "Files", "Code"][..], true),
        (
            "CosmosDBDatabase",
            &["Tables", "Files", "Code", "Audit"][..],
            true,
        ),
        ("GraphQLApi", &[][..], true),
        ("Notebook", &[][..], false),
    ] {
        let kind = ItemType::parse(suffix).unwrap();
        assert_eq!(item_folders(kind), expected, "{suffix}");
        let tenant = Element::tenant("t", "t");
        let workspace = Element::child(
            "w",
            Some("w".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            tenant,
        )
        .unwrap();
        let item = PathElement::from_fabric(
            Element::child("item", Some("i".into()), NodeKind::Item(kind), workspace).unwrap(),
        );
        assert_eq!(
            CommandPolicy::source_list().check(&item, "dir").is_ok(),
            supported,
            "{suffix}"
        );
    }
}

#[test]
fn invalid_local_adapter_metadata_fails_explicitly_and_cannot_change_context() {
    struct Broken;
    impl LocalLookup for Broken {
        fn exists(&self, _: LocalProbe<'_>) -> Result<bool, ContextError> {
            Err(ContextError::lookup("LocalIoFailure", "snapshot failed"))
        }
        fn access(&self, _: &str, _: LocalAccess) -> Result<bool, ContextError> {
            panic!("no access after exists failure")
        }
        fn dirname(&self, _: &str) -> Result<String, ContextError> {
            panic!("no dirname after exists failure")
        }
        fn describe(&self, _: &str) -> Result<LocalMetadata, ContextError> {
            Ok(LocalMetadata {
                path: "wrong".into(),
                name: "wrong".into(),
                id: "x".into(),
                kind: LocalKind::Other,
            })
        }
    }
    let (root, catalog) = graph();
    let mut resolver = resolver(&catalog, Arc::clone(&root)).with_local(&Broken);
    let error = resolver
        .change_context(
            PathInput::Scalar("new.txt"),
            ResolveOptions {
                raise_error: false,
                supports_local_path: true,
            },
        )
        .unwrap_err();
    assert_eq!(error.error.code, "LocalIoFailure");
    assert_eq!(error.notices.len(), 1);
    assert!(Arc::ptr_eq(resolver.current(), &root));
    assert_eq!(
        resolver
            .resolve(PathInput::Local("expected"), ResolveOptions::default())
            .unwrap_err()
            .error
            .code,
        "InvalidContext"
    );
}

#[test]
fn missing_local_capability_is_not_a_global_remote_resolution_failure() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    let options = ResolveOptions {
        supports_local_path: true,
        ..ResolveOptions::default()
    };
    assert_eq!(
        resolver
            .resolve(PathInput::Scalar("/W.Workspace"), options)
            .unwrap()
            .element
            .id(),
        Some("w")
    );
    let failure = resolver
        .resolve(PathInput::Scalar("selected.ipynb"), options)
        .unwrap_err();
    assert_eq!(
        failure.error.kind,
        ErrorKind::Unsupported(UnsupportedBranch::LocalFilesystem)
    );
    assert_eq!(failure.notices.len(), 1);
}
