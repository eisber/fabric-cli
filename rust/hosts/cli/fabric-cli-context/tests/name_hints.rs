// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public opt-in contracts; existing compatibility oracles remain unchanged.

#![deny(unnameable_test_items)]

use fabric_cli_context::*;
use std::{cell::Cell, sync::Arc};

fn child(parent: &Arc<Element>, name: &str, id: &str, kind: NodeKind) -> Arc<Element> {
    Element::child(name, Some(id.into()), kind, Arc::clone(parent)).unwrap()
}

fn item(parent: &Arc<Element>, name: &str, id: &str, suffix: &str) -> Arc<Element> {
    child(
        parent,
        name,
        id,
        NodeKind::Item(ItemType::parse(suffix).unwrap()),
    )
}

fn graph() -> (Arc<Element>, Catalog) {
    let root = Element::tenant("Selected", "tenant");
    let foreign = Element::tenant("Foreign", "foreign-tenant");
    let workspace_kind = NodeKind::Workspace(WorkspaceType::Workspace);
    let team = child(&root, "Team", "team", workspace_kind);
    let other = child(&root, "Other", "other", workspace_kind);
    let foreign_team = child(&foreign, "Teams", "foreign-team", workspace_kind);
    let personal = child(
        &root,
        "Teammate",
        "personal",
        NodeKind::Workspace(WorkspaceType::Personal),
    );
    let plan = child(&team, "Plan", "plan", NodeKind::Folder);
    let planning = child(&plan, "Planning", "planning", NodeKind::Folder);
    let plain = child(&team, "Plain", "plain", NodeKind::Folder);
    let plank = child(&plain, "Plank", "plank", NodeKind::Folder);
    let rows = vec![
        Arc::clone(&team),
        Arc::clone(&other),
        Arc::clone(&foreign_team),
        personal,
        Arc::clone(&plan),
        Arc::clone(&planning),
        plain,
        plank,
        child(&other, "Planner", "other-folder", NodeKind::Folder),
        child(&foreign_team, "Planned", "foreign-folder", NodeKind::Folder),
        item(&planning, "Analysis", "first-notebook", "Notebook"),
        item(&team, "Analysis", "second-notebook", "Notebook"),
        item(&team, "Archive", "archive", "Notebook"),
        item(&team, "Analysis", "same-name-report", "Report"),
        item(&team, "Analyses", "report", "Report"),
        item(&team, "Lake", "lake", "Lakehouse"),
        item(&other, "Analyst", "other-notebook", "Notebook"),
        item(&foreign_team, "Analytic", "foreign-notebook", "Notebook"),
        item(&team, "Absent", "", "Notebook"),
        Element::child(
            "Unresolved",
            None,
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            team,
        )
        .unwrap(),
    ];
    (root, Catalog::new(rows))
}

fn resolver<'a, L: Lookup + ?Sized>(lookup: &'a L, current: Arc<Element>) -> Resolver<'a, L> {
    Resolver::new(
        lookup,
        current,
        HomeContext::Workspace("Team.Workspace".into()),
    )
}

fn all_names() -> SearchOptions {
    // A low score threshold does not bypass the engine's eligibility rules.
    SearchOptions {
        max_results: 20,
        min_score: 0.0,
    }
}

fn names(hints: &NameHints) -> Vec<&str> {
    match hints {
        NameHints::Suggestions(values) => values.iter().map(|value| value.name.as_str()).collect(),
        other => panic!("expected structured suggestions, got {other:?}"),
    }
}

fn sorted_names(hints: &NameHints) -> Vec<&str> {
    let mut result = names(hints);
    result.sort_unstable();
    result
}

#[test]
fn workspace_miss_keeps_error_context_and_selected_tenant_and_type() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, Arc::clone(&root));
    let path = PathInput::Scalar("/Tem.Workspace");
    let legacy = resolver
        .resolve(path, ResolveOptions::default())
        .unwrap_err();
    let hinted = resolver
        .resolve_with_hints(path, ResolveOptions::default(), all_names())
        .unwrap_err();
    assert_eq!(hinted.failure, legacy);
    assert_eq!(hinted.to_string().as_bytes(), legacy.to_string().as_bytes());
    assert_eq!(legacy.code, "NotFound");
    assert_eq!(sorted_names(&hinted.hints), ["Team.Workspace"]);
    let candidates: Vec<_> = catalog
        .candidate_names(LookupRequest {
            kind: LookupKind::Workspace,
            parent: &root,
            name: "Tem.Workspace",
        })
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(candidates, ["Team", "Other"]);
    assert!(Arc::ptr_eq(resolver.current(), &root));
}

#[test]
fn item_candidates_use_workspace_wide_exact_boundary_and_requested_artifact_type() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    for path in ["~/Analyss.Notebook", "~/Plain.Folder/Analyss.Notebook"] {
        let hinted = resolver
            .resolve_with_hints(
                PathInput::Scalar(path),
                ResolveOptions::default(),
                all_names(),
            )
            .unwrap_err();
        assert_eq!(hinted.failure.code, "NotFound");
        assert_eq!(sorted_names(&hinted.hints), ["Analysis.Notebook"]);
    }
    let team = resolver
        .resolve(PathInput::Scalar("~"), ResolveOptions::default())
        .unwrap();
    let candidates: Vec<_> = catalog
        .candidate_names(LookupRequest {
            kind: LookupKind::Item,
            parent: &team,
            name: "Analysis.Notebook",
        })
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(candidates, ["Analysis", "Analysis", "Archive"]);
}

#[test]
fn folder_candidates_preserve_workspace_wide_quirk_but_nested_parent_boundary() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, root);
    for (path, expected) in [
        (
            "~/Plann.Folder",
            vec!["Plain.Folder", "Plan.Folder", "Plank.Folder"],
        ),
        ("~/Plan.Folder/Plannin.Folder", vec!["Planning.Folder"]),
    ] {
        let failure = resolver
            .resolve_with_hints(
                PathInput::Scalar(path),
                ResolveOptions::default(),
                all_names(),
            )
            .unwrap_err();
        assert_eq!(sorted_names(&failure.hints), expected);
    }
    let team = resolver
        .resolve(PathInput::Scalar("~"), ResolveOptions::default())
        .unwrap();
    let workspace_candidates: Vec<_> = catalog
        .candidate_names(LookupRequest {
            kind: LookupKind::Folder,
            parent: &team,
            name: "Plann.Folder",
        })
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(workspace_candidates, ["Plan", "Planning", "Plain", "Plank"]);
    let plan = resolver
        .resolve(
            PathInput::Scalar("~/Plan.Folder"),
            ResolveOptions::default(),
        )
        .unwrap();
    let nested_candidates: Vec<_> = catalog
        .candidate_names(LookupRequest {
            kind: LookupKind::NestedFolder,
            parent: &plan,
            name: "Plannin.Folder",
        })
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(nested_candidates, ["Planning"]);
}

struct Counted {
    catalog: Catalog,
    exact_calls: Cell<usize>,
    candidate_calls: Cell<usize>,
}

impl Lookup for Counted {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self.exact_calls.set(self.exact_calls.get() + 1);
        self.catalog.lookup(request)
    }

    fn candidate_names(
        &self,
        request: LookupRequest<'_>,
    ) -> Result<Option<CandidateNames<'_>>, ContextError> {
        self.candidate_calls.set(self.candidate_calls.get() + 1);
        self.catalog.candidate_names(request)
    }
}

#[test]
fn exact_hit_preserves_first_match_and_never_accesses_candidates_or_validates_search() {
    let (root, catalog) = graph();
    let lookup = Counted {
        catalog,
        exact_calls: Cell::new(0),
        candidate_calls: Cell::new(0),
    };
    let resolver = resolver(&lookup, root);
    let value = resolver
        .resolve_with_hints(
            PathInput::Scalar("~/Analysis.Notebook"),
            ResolveOptions::default(),
            SearchOptions {
                max_results: 0,
                min_score: f64::NAN,
            },
        )
        .unwrap();
    assert_eq!(value.id(), Some("first-notebook"));
    assert_eq!(lookup.exact_calls.get(), 2);
    assert_eq!(lookup.candidate_calls.get(), 0);
}

#[test]
fn opt_out_keeps_byte_identical_not_found_and_never_collects_names() {
    let (root, catalog) = graph();
    let lookup = Counted {
        catalog,
        exact_calls: Cell::new(0),
        candidate_calls: Cell::new(0),
    };
    let resolver = resolver(&lookup, root);
    let path = PathInput::Scalar("~/Analyss.Notebook");
    let legacy = resolver
        .resolve(path, ResolveOptions::default())
        .unwrap_err();
    assert_eq!(
        legacy.to_string(),
        "NotFound: The Item 'Analyss.Notebook' could not be found"
    );
    assert_eq!(lookup.candidate_calls.get(), 0);
    let hinted = resolver
        .resolve_with_hints(path, ResolveOptions::default(), SearchOptions::default())
        .unwrap_err();
    assert_eq!(hinted.failure, legacy);
    assert_eq!(lookup.candidate_calls.get(), 1);
}

#[test]
fn suppressed_lookup_placeholders_and_missing_parents_remain_unchanged() {
    let (root, catalog) = graph();
    let lookup = Counted {
        catalog,
        exact_calls: Cell::new(0),
        candidate_calls: Cell::new(0),
    };
    let resolver = resolver(&lookup, Arc::clone(&root));
    let options = ResolveOptions {
        raise_error: false,
        ..ResolveOptions::default()
    };
    for path in ["/Tem.Workspace", "~/Analyss.Notebook", "~/Plann.Folder"] {
        let input = PathInput::Scalar(path);
        let legacy = resolver.resolve(input, options).unwrap();
        let hinted = resolver
            .resolve_with_hints(input, options, all_names())
            .unwrap();
        assert_eq!(hinted, legacy);
        assert_eq!(hinted.id(), None);
    }
    let path = PathInput::Scalar("/Tem.Workspace/Analyss.Notebook");
    let legacy = resolver.resolve(path, options).unwrap_err();
    let hinted = resolver
        .resolve_with_hints(path, options, all_names())
        .unwrap_err();
    assert_eq!(hinted.failure, legacy);
    assert!(matches!(hinted.hints, NameHints::NotApplicable));
    assert_eq!(lookup.candidate_calls.get(), 0);
    assert!(Arc::ptr_eq(resolver.current(), &root));
}

struct FailureLookup {
    error: ContextError,
}

impl Lookup for FailureLookup {
    fn lookup(&self, _: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        Err(self.error.clone())
    }

    fn candidate_names(
        &self,
        _: LookupRequest<'_>,
    ) -> Result<Option<CandidateNames<'_>>, ContextError> {
        panic!("non-NotFound failures must not access hint candidates")
    }
}

#[test]
fn denied_transient_invalid_and_unsupported_failures_never_collect_names() {
    for code in [
        "Forbidden",
        "Unauthorized",
        "PermissionDenied",
        "AccessDenied",
        "InvalidPath",
        "NotSupported",
        "UnsupportedCommand",
        "Timeout",
        "ServiceUnavailable",
    ] {
        let error = ContextError::lookup(code, "unchanged caller error");
        let lookup = FailureLookup {
            error: error.clone(),
        };
        let root = Element::tenant("Selected", "tenant");
        let resolver = resolver(&lookup, Arc::clone(&root));
        for raise_error in [true, false] {
            let failure = resolver
                .resolve_with_hints(
                    PathInput::Scalar("/Tem.Workspace"),
                    ResolveOptions {
                        raise_error,
                        supports_local_path: false,
                    },
                    all_names(),
                )
                .unwrap_err();
            assert_eq!(failure.failure, error);
            assert!(matches!(failure.hints, NameHints::NotApplicable));
            assert!(Arc::ptr_eq(resolver.current(), &root));
        }
    }
}

#[test]
fn validation_unsupported_and_home_failures_do_not_access_any_lookup() {
    let lookup = |_: LookupRequest<'_>| -> Result<Option<String>, ContextError> {
        panic!("path validation and home failures must not lookup")
    };
    let root = Element::tenant("Selected", "tenant");
    let resolver = Resolver::new(
        &lookup,
        root,
        HomeContext::Unavailable(ContextError::lookup("NotFound", "home unavailable")),
    );
    for path in [
        "/...bad",
        "/not-a-workspace",
        "/.capacities",
        "~",
        "~/Tem.Notebook",
    ] {
        let input = PathInput::Scalar(path);
        let legacy = resolver
            .resolve(input, ResolveOptions::default())
            .unwrap_err();
        let failure = resolver
            .resolve_with_hints(input, ResolveOptions::default(), all_names())
            .unwrap_err();
        assert_eq!(failure.failure, legacy);
        assert!(matches!(failure.hints, NameHints::NotApplicable));
    }
    let failure = resolver
        .resolve_with_hints(
            PathInput::Scalar("/Tem.Workspace"),
            ResolveOptions {
                supports_local_path: true,
                ..ResolveOptions::default()
            },
            all_names(),
        )
        .unwrap_err();
    assert!(matches!(failure.hints, NameHints::NotApplicable));
}

struct Snapshot {
    names: Vec<String>,
    result: Result<Option<String>, ContextError>,
    fail_names: bool,
}

impl Snapshot {
    fn new(names: Vec<String>) -> Self {
        Self {
            names,
            result: Ok(None),
            fail_names: false,
        }
    }
}

impl Lookup for Snapshot {
    fn lookup(&self, _: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        self.result.clone()
    }

    fn candidate_names(
        &self,
        _: LookupRequest<'_>,
    ) -> Result<Option<CandidateNames<'_>>, ContextError> {
        if self.fail_names {
            return Err(ContextError::lookup(
                "Forbidden",
                "sensitive snapshot failure",
            ));
        }
        Ok(Some(Box::new(self.names.iter().map(String::as_str))))
    }
}

fn snapshot_failure(
    snapshot: &Snapshot,
    query: &str,
    search: SearchOptions,
) -> NameHintFailure<ContextError> {
    let root = Element::tenant("Selected", "tenant");
    resolver(snapshot, root)
        .resolve_with_hints(PathInput::Scalar(query), ResolveOptions::default(), search)
        .unwrap_err()
}

#[test]
fn caller_not_found_and_empty_id_misses_keep_the_original_failure() {
    for result in [
        Err(ContextError::lookup(
            "NotFound",
            "original exact catalog miss",
        )),
        Ok(Some(String::new())),
    ] {
        let snapshot = Snapshot {
            result,
            ..Snapshot::new(vec!["Team".into()])
        };
        let failure = snapshot_failure(&snapshot, "/Tem.Workspace", all_names());
        assert_eq!(names(&failure.hints), ["Team.Workspace"]);
        match &snapshot.result {
            Err(error) => assert_eq!(&failure.failure, error),
            Ok(_) => assert_eq!(
                failure.failure.message,
                "The Workspace 'Tem.Workspace' could not be found"
            ),
        }
    }
}

#[test]
fn absent_or_failed_hint_snapshots_report_unavailable_without_rewriting_not_found() {
    let lookup = |_: LookupRequest<'_>| -> Result<Option<String>, ContextError> { Ok(None) };
    let root = Element::tenant("Selected", "tenant");
    let failure = resolver(&lookup, root)
        .resolve_with_hints(
            PathInput::Scalar("/Tem.Workspace"),
            ResolveOptions::default(),
            all_names(),
        )
        .unwrap_err();
    assert_eq!(failure.failure.code, "NotFound");
    assert!(matches!(
        failure.hints,
        NameHints::Unavailable(NameHintsUnavailable::SnapshotNotProvided)
    ));
    let snapshot = Snapshot {
        fail_names: true,
        ..Snapshot::new(vec!["Team".into()])
    };
    let failure = snapshot_failure(&snapshot, "/Tem.Workspace", all_names());
    assert_eq!(failure.failure.code, "NotFound");
    assert!(matches!(
        failure.hints,
        NameHints::Unavailable(NameHintsUnavailable::SnapshotFailure)
    ));
    assert_eq!(
        failure.hints.to_string(),
        "Name hints unavailable: names-only snapshot lookup failed."
    );
    assert!(!failure.hints.to_string().contains("sensitive"));
}

#[test]
fn invalid_search_options_are_explicit_diagnostics_not_matches_or_replacement_errors() {
    let snapshot = Snapshot::new(vec!["Team".into()]);
    for (max_results, min_score) in [
        (0, 0.6),
        (21, 0.6),
        (5, -0.1),
        (5, 1.1),
        (5, f64::NAN),
        (5, f64::INFINITY),
    ] {
        let failure = snapshot_failure(
            &snapshot,
            "/Tem.Workspace",
            SearchOptions {
                max_results,
                min_score,
            },
        );
        assert_eq!(failure.failure.code, "NotFound");
        assert!(matches!(
            failure.hints,
            NameHints::Unavailable(NameHintsUnavailable::Search(_))
        ));
        assert!(failure.hints.to_string().contains("unavailable"));
    }
}

#[test]
fn name_candidate_count_and_aggregate_caps_remain_explicit_not_found_diagnostics() {
    let long_query = format!("/{}.Workspace", "q".repeat(513));
    let cases = [
        (Snapshot::new(vec!["Team".into()]), long_query),
        (
            Snapshot::new(vec!["x".repeat(513)]),
            "/Tem.Workspace".into(),
        ),
        (
            Snapshot::new((0..4097).map(|index| format!("name-{index}")).collect()),
            "/Tem.Workspace".into(),
        ),
        (
            Snapshot::new(
                (0..515)
                    .map(|index| format!("{}{:03}", "😀".repeat(509), index))
                    .collect(),
            ),
            "/Tem.Workspace".into(),
        ),
    ];
    for (snapshot, query) in cases {
        let failure = snapshot_failure(&snapshot, &query, all_names());
        assert_eq!(failure.failure.code, "NotFound");
        assert!(matches!(
            failure.hints,
            NameHints::Unavailable(NameHintsUnavailable::Search(_))
        ));
        assert!(failure.hints.to_string().contains("unavailable"));
    }
}

#[test]
fn suggestions_follow_the_shared_ranking_and_do_not_depend_on_catalog_order() {
    let (root, catalog) = graph();
    let mut reversed = catalog.elements().to_vec();
    reversed.reverse();
    let reversed = Catalog::new(reversed);
    let first = resolver(&catalog, Arc::clone(&root))
        .resolve_with_hints(
            PathInput::Scalar("~/Analyss.Notebook"),
            ResolveOptions::default(),
            all_names(),
        )
        .unwrap_err();
    let second = resolver(&reversed, root)
        .resolve_with_hints(
            PathInput::Scalar("~/Analyss.Notebook"),
            ResolveOptions::default(),
            all_names(),
        )
        .unwrap_err();
    let expected = fabric_name_search::suggest_names(
        "Analyss",
        ["Analysis", "Analysis", "Archive"],
        all_names(),
    )
    .unwrap();
    let NameHints::Suggestions(first) = first.hints else {
        panic!("missing suggestions")
    };
    let NameHints::Suggestions(second) = second.hints else {
        panic!("missing suggestions")
    };
    assert_eq!(first, second);
    assert_eq!(first.len(), expected.len());
    for (actual, expected) in first.iter().zip(expected) {
        assert_eq!(actual.name, format!("{}.Notebook", expected.name));
        assert_eq!(actual.score, expected.score);
    }
}

#[test]
fn textual_hints_escape_malicious_names_and_never_render_the_query() {
    let malicious = "Team\u{1b}[31m\u{202e}\t\"";
    let snapshot = Snapshot::new(vec![malicious.into()]);
    let failure = snapshot_failure(
        &snapshot,
        "/Teem\u{1b}[31m\u{202e}\t\".Workspace",
        all_names(),
    );
    assert_eq!(names(&failure.hints), [format!("{malicious}.Workspace")]);
    let text = failure.hints.to_string();
    assert!(text.is_ascii());
    assert!(!text.chars().any(char::is_control));
    assert!(text.contains("\\u{1b}"));
    assert!(text.contains("\\u{202e}"));
    assert!(text.contains("\\t\\\""));
    assert!(!text.contains("Teem"));
    let NameHints::Suggestions(values) = failure.hints else {
        panic!("missing suggestions")
    };
    assert_eq!(
        values[0].to_string(),
        format!("\"{}\"", values[0].escaped_name())
    );
    // Newlines and backslashes cannot survive name-token validation unchanged,
    // but names received from snapshots still require safe public rendering.
    let full_controls = NameHint {
        name: "Team\u{1b}[31m\u{202e}\n\t\"\\".into(),
        score: 1.0,
    };
    let rendered = full_controls.to_string();
    assert!(rendered.is_ascii());
    assert!(!rendered.chars().any(char::is_control));
    assert!(rendered.contains("\\u{1b}"));
    assert!(rendered.contains("\\u{202e}"));
    assert!(rendered.contains("\\n\\t\\\"\\\\"));
}

#[test]
fn extended_resolver_reuses_scoped_fabric_hints_and_preserves_notices_and_context() {
    let (root, catalog) = graph();
    let lookup = HierarchyCatalog::new(catalog, Vec::new(), Vec::new());
    let root = PathElement::from_fabric(root);
    let resolver = PathResolver::new(
        &lookup,
        Arc::clone(&root),
        HomeContext::Workspace("Team.Workspace".into()),
        "ls",
    )
    .unwrap();
    for (path, expected) in [
        ("/Tem.Workspace", vec!["Team.Workspace"]),
        ("~/Analyss.Notebook", vec!["Analysis.Notebook"]),
        ("~/Plan.Folder/Plannin.Folder", vec!["Planning.Folder"]),
    ] {
        let input = PathInput::Scalar(path);
        let legacy = resolver
            .resolve(input, ResolveOptions::default())
            .unwrap_err();
        let failure = resolver
            .resolve_with_hints(input, ResolveOptions::default(), all_names())
            .unwrap_err();
        assert_eq!(failure.failure.error, legacy.error);
        assert_eq!(failure.failure.notices, legacy.notices);
        assert_eq!(sorted_names(&failure.hints), expected);
        assert!(Arc::ptr_eq(resolver.current(), &root));
    }
}

#[test]
fn extended_virtual_onelake_capability_and_local_fallback_failures_do_not_hint() {
    let (root, catalog) = graph();
    let counted = Counted {
        catalog,
        exact_calls: Cell::new(0),
        candidate_calls: Cell::new(0),
    };
    let root = PathElement::from_fabric(root);
    let resolver = PathResolver::new(
        &counted,
        root,
        HomeContext::Workspace("Team.Workspace".into()),
        "ls",
    )
    .unwrap();
    for path in [
        "/.capacities/Tem.Capacity",
        "~/Analysis.Notebook/Files",
        "~/Archive.Notebook/unavailable",
        "~/Lake.Lakehouse/Files/absent.csv",
        "/...invalid",
    ] {
        let failure = resolver
            .resolve_with_hints(
                PathInput::Scalar(path),
                ResolveOptions::default(),
                all_names(),
            )
            .unwrap_err();
        assert!(matches!(failure.hints, NameHints::NotApplicable));
    }
    let failure = resolver
        .resolve_with_hints(
            PathInput::Scalar("/Tem.Workspace"),
            ResolveOptions {
                supports_local_path: true,
                ..ResolveOptions::default()
            },
            all_names(),
        )
        .unwrap_err();
    assert!(matches!(failure.hints, NameHints::NotApplicable));
    assert_eq!(failure.failure.notices.len(), 1);
    assert_eq!(counted.candidate_calls.get(), 0);
}

impl HierarchyLookup for Counted {
    fn virtual_id(&self, _: VirtualLookupRequest<'_>) -> Result<Option<String>, ContextError> {
        Ok(None)
    }

    fn properties(&self, _: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        Ok(None)
    }
}

struct FailedLocal {
    code: &'static str,
}

impl LocalLookup for FailedLocal {
    fn exists(&self, _: LocalProbe<'_>) -> Result<bool, ContextError> {
        Ok(true)
    }

    fn access(&self, _: &str, _: LocalAccess) -> Result<bool, ContextError> {
        Err(ContextError::lookup(
            self.code,
            "original local access failure",
        ))
    }

    fn dirname(&self, _: &str) -> Result<String, ContextError> {
        panic!("raised fallback must not probe a creation parent")
    }

    fn describe(&self, _: &str) -> Result<LocalMetadata, ContextError> {
        panic!("failed local access must not produce an element")
    }
}

#[test]
fn local_fallback_permission_and_not_found_errors_never_use_remote_miss_candidates() {
    let (root, catalog) = graph();
    let lookup = Counted {
        catalog,
        exact_calls: Cell::new(0),
        candidate_calls: Cell::new(0),
    };
    let root = PathElement::from_fabric(root);
    for code in ["Forbidden", "NotFound", "Timeout"] {
        let local = FailedLocal { code };
        let resolver =
            PathResolver::new(&lookup, Arc::clone(&root), HomeContext::Unspecified, "ls")
                .unwrap()
                .with_local(&local);
        let input = PathInput::Scalar("/Tem.Workspace");
        let options = ResolveOptions {
            supports_local_path: true,
            ..ResolveOptions::default()
        };
        let legacy = resolver.resolve(input, options).unwrap_err();
        let failure = resolver
            .resolve_with_hints(input, options, all_names())
            .unwrap_err();
        assert_eq!(failure.failure.error, legacy.error);
        assert_eq!(failure.failure.notices, legacy.notices);
        assert_eq!(failure.failure.error.code, code);
        assert!(matches!(failure.hints, NameHints::NotApplicable));
        assert!(Arc::ptr_eq(resolver.current(), &root));
        assert_eq!(lookup.candidate_calls.get(), 0);
    }
}

#[test]
fn invalid_context_and_unsupported_origins_cannot_opt_in_by_spelling_not_found() {
    for kind in [
        ErrorKind::InvalidContext,
        ErrorKind::Unsupported(UnsupportedBranch::OneLake),
    ] {
        let error = ContextError {
            kind,
            code: "NotFound".into(),
            message: "ineligible failure origin".into(),
        };
        let lookup = FailureLookup {
            error: error.clone(),
        };
        let root = Element::tenant("Selected", "tenant");
        let failure = resolver(&lookup, root)
            .resolve_with_hints(
                PathInput::Scalar("/Tem.Workspace"),
                ResolveOptions::default(),
                all_names(),
            )
            .unwrap_err();
        assert_eq!(failure.failure, error);
        assert!(matches!(failure.hints, NameHints::NotApplicable));
    }
}

#[test]
fn resolver_failure_guards_a_mutator_without_selecting_a_suggestion() {
    let (root, catalog) = graph();
    let resolver = resolver(&catalog, Arc::clone(&root));
    let mut effects = Vec::new();
    let attempted = resolver.resolve_with_hints(
        PathInput::Scalar("~/Analyss.Notebook"),
        ResolveOptions::default(),
        SearchOptions::default(),
    );
    if let Ok(exact) = &attempted {
        effects.push(exact.id().unwrap().to_owned());
    }
    let failure = attempted.unwrap_err();
    assert_eq!(failure.failure.code, "NotFound");
    assert!(!names(&failure.hints).is_empty());
    assert!(effects.is_empty());
    assert!(Arc::ptr_eq(resolver.current(), &root));
    let exact = resolver
        .resolve_with_hints(
            PathInput::Scalar("~/Analysis.Notebook"),
            ResolveOptions::default(),
            SearchOptions::default(),
        )
        .unwrap();
    effects.push(exact.id().unwrap().to_owned());
    assert_eq!(effects, ["first-notebook"]);
}

#[test]
fn offline_variable_delete_requires_exact_names_and_never_searches_values_or_notes() {
    struct Variable<'a> {
        name: &'a str,
        value: &'a str,
        note: &'a str,
    }

    let large_value = "x".repeat(1_048_577);
    let snapshot = [
        Variable {
            name: "environment",
            value: &large_value,
            note: "environmnt",
        },
        Variable {
            name: "environment_name",
            value: "synthetic",
            note: "synthetic",
        },
    ];
    let mut effects = Vec::new();
    for (query, expected_effects) in [("environmnt", 0), ("Environment", 0), ("environment", 1)] {
        if let Some(exact) = snapshot.iter().find(|variable| variable.name == query) {
            effects.push(exact.name);
        } else {
            let suggestions = fabric_name_search::suggest_names(
                query,
                snapshot.iter().map(|variable| variable.name),
                SearchOptions::default(),
            )
            .unwrap();
            assert_eq!(
                suggestions.first().map(|name| name.name),
                Some("environment")
            );
            assert!(suggestions.iter().all(|suggestion| {
                snapshot
                    .iter()
                    .any(|variable| variable.name == suggestion.name)
            }));
        }
        assert_eq!(effects.len(), expected_effects);
    }
    assert_eq!(effects, ["environment"]);
    assert_eq!(snapshot[0].value.len(), 1_048_577);
    assert_eq!(snapshot[0].note, "environmnt");
}
