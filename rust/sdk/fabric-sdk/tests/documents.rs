// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public contracts for grouped get rev16's injected docs/graph slice.

#![deny(unnameable_test_items)]

use fabric_sdk::documents::{
    Coverage, CoverageReason, CoverageReasonKind, DocEdge, DocPage, DocRequest, DocsErrorKind,
    DocsKey, DocsOptions, DocsOwner, DocumentsFuture, DocumentsProvider, EdgeDisposition,
    EdgeStatus, GraphResult, NodeStatus, ProviderError, ProviderErrorKind,
    UnavailableDocumentsProvider, materialize,
};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn, ready},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn key(item: &str, view: &str) -> DocsKey {
    DocsKey {
        owner: DocsOwner::Item {
            tenant: "tenant".into(),
            workspace: "workspace".into(),
            item: item.into(),
        },
        view: view.into(),
        facet: None,
        subresource: None,
        stage: Some("served".into()),
        revision_selector: None,
    }
}

fn page(content: &str, edges: Vec<DocEdge>) -> DocPage {
    DocPage {
        content: content.into(),
        media_type: "text/markdown".into(),
        revision: Some("r1".into()),
        snapshot: Some("s1".into()),
        edges,
        continuation: None,
        coverage: Coverage::Complete,
    }
}

fn edge(target: DocsKey, alias: &str, disposition: Option<EdgeDisposition>) -> DocEdge {
    DocEdge {
        target,
        kind: Some("advertised".into()),
        provenance: Some(alias.into()),
        disposition,
    }
}

fn options(depth: Option<u32>) -> DocsOptions {
    DocsOptions {
        recursive: depth.is_some(),
        depth,
        max_nodes: nz(100),
        max_pages: nz(100),
        max_bytes: nz(1_000_000),
    }
}

#[derive(Clone, Debug)]
struct Request {
    key: DocsKey,
    continuation: Option<String>,
    expected_revision: Option<String>,
    expected_snapshot: Option<String>,
    max_bytes: usize,
}

#[derive(Default)]
struct Provider {
    pages: Mutex<BTreeMap<DocsKey, VecDeque<Result<DocPage, ProviderError>>>>,
    rejected: BTreeMap<DocsKey, ProviderErrorKind>,
    requests: Mutex<Vec<Request>>,
    checks: AtomicUsize,
}

impl Provider {
    fn with(pages: Vec<(DocsKey, Vec<DocPage>)>) -> Self {
        Self {
            pages: Mutex::new(
                pages
                    .into_iter()
                    .map(|(key, pages)| (key, pages.into_iter().map(Ok).collect()))
                    .collect(),
            ),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl DocumentsProvider for Provider {
    fn check_scope(&self, key: &DocsKey) -> Result<(), ProviderError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.rejected.get(key).map_or(Ok(()), |kind| {
            Err(ProviderError {
                kind: *kind,
                detail: None,
            })
        })
    }

    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a> {
        self.requests.lock().unwrap().push(Request {
            key: request.key.clone(),
            continuation: request.continuation.map(str::to_owned),
            expected_revision: request.expected_revision.map(str::to_owned),
            expected_snapshot: request.expected_snapshot.map(str::to_owned),
            max_bytes: request.max_bytes.get(),
        });
        let result = self
            .pages
            .lock()
            .unwrap()
            .get_mut(request.key)
            .expect("unexpected canonical scope")
            .pop_front()
            .expect("unexpected repeated scope/page");
        Box::pin(ready(result))
    }
}

fn has_reason(graph: &GraphResult, kind: CoverageReasonKind) -> bool {
    matches!(&graph.coverage, Coverage::Incomplete { reasons } if reasons.iter().any(|reason| reason.kind == kind))
}

#[test]
fn rendering_errors_remain_typed_when_boxed_by_an_execution_head() {
    let result: Result<(), DocsErrorKind> = Err(DocsErrorKind::Budget);
    let error = result
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<DocsErrorKind>(),
        Some(&DocsErrorKind::Budget)
    );
    assert_eq!(error.to_string(), "documents invocation budget exceeded");
    for (kind, message) in [
        (
            DocsErrorKind::Provider(ProviderErrorKind::Denied),
            "documents access denied",
        ),
        (
            DocsErrorKind::Provider(ProviderErrorKind::NotFound),
            "documents scope not found",
        ),
        (
            DocsErrorKind::Provider(ProviderErrorKind::Unsupported),
            "documents scope unsupported",
        ),
        (
            DocsErrorKind::Provider(ProviderErrorKind::Unavailable),
            "documents provider unavailable",
        ),
    ] {
        assert_eq!(kind.to_string(), message);
    }
}

#[tokio::test]
async fn root_only_keeps_text_media_pins_and_does_not_follow_markdown_urls() {
    let root = key("a", "docs/context");
    let text = "# Native text\n[not an edge](https://example.invalid/a)\n{\"not\":\"schema\"}";
    let provider = Provider::with(vec![(root.clone(), vec![page(text, vec![])])]);
    let graph = materialize(&provider, root.clone(), options(None), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 1);
    assert_eq!(graph.root, root);
    assert_eq!(graph.coverage, Coverage::Complete);
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].pages[0].content, text);
    assert_eq!(graph.nodes[0].pages[0].media_type, "text/markdown");
    assert_eq!(graph.nodes[0].pages[0].revision.as_deref(), Some("r1"));
    let json: serde_json::Value =
        serde_json::from_slice(&graph.to_json(nz(100_000)).unwrap()).unwrap();
    assert_eq!(json["nodes"][0]["pages"][0]["content"], text);
    assert!(json["nodes"][0].get("schema").is_none());
    assert!(graph.edges.is_empty());
}

#[tokio::test]
async fn root_only_and_depth_zero_preserve_unexpanded_outgoing_scope() {
    for (opts, reason) in [
        (options(None), CoverageReasonKind::NonRecursive),
        (options(Some(0)), CoverageReasonKind::DepthLimit),
    ] {
        let root = key("a", "docs");
        let child = key("a", "docs/schema");
        let provider = Provider::with(vec![(
            root.clone(),
            vec![page(
                "root",
                vec![edge(child.clone(), "schema", Some(EdgeDisposition::Child))],
            )],
        )]);
        let graph = materialize(&provider, root, opts, pending()).await.unwrap();
        assert_eq!(provider.calls().len(), 1);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.edges[0].edge.target, child);
        assert_eq!(graph.edges[0].status, EdgeStatus::Unexpanded { reason });
        assert!(has_reason(&graph, reason));
    }
}

#[tokio::test]
async fn depth_is_edge_based_and_global() {
    for (depth, count) in [(0, 1), (1, 2), (2, 3)] {
        let root = key("a", "docs");
        let child = key("a", "docs/schema");
        let grandchild = key("a", "docs/schema/table");
        let provider = Provider::with(vec![
            (
                root.clone(),
                vec![page(
                    "root",
                    vec![edge(child.clone(), "child", Some(EdgeDisposition::Child))],
                )],
            ),
            (
                child.clone(),
                vec![page(
                    "child",
                    vec![edge(
                        grandchild.clone(),
                        "grandchild",
                        Some(EdgeDisposition::Child),
                    )],
                )],
            ),
            (grandchild, vec![page("grandchild", vec![])]),
        ]);
        let graph = materialize(&provider, root, options(Some(depth)), pending())
            .await
            .unwrap();
        assert_eq!(provider.calls().len(), count);
        assert_eq!(graph.nodes.len(), count);
        assert_eq!(graph.coverage == Coverage::Complete, depth == 2);
    }
}

#[tokio::test]
async fn invalid_options_and_unsupported_scopes_do_not_touch_provider() {
    let provider = Provider::default();
    let root = key("a", "docs");
    let mut invalid_depth = options(Some(1));
    invalid_depth.recursive = false;
    let mut missing_depth = options(None);
    missing_depth.recursive = true;
    for opts in [invalid_depth, missing_depth] {
        let error = materialize(&provider, root.clone(), opts, pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, DocsErrorKind::InvalidConfiguration);
    }
    for view in ["src/context", "relations", "settings"] {
        let error = materialize(&provider, key("a", view), options(None), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            DocsErrorKind::Provider(ProviderErrorKind::Unsupported)
        );
    }
    for view in ["", "docs/", "docs//schema", "docs/../src", "https://x/docs"] {
        let error = materialize(&provider, key("a", view), options(None), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, DocsErrorKind::InvalidConfiguration);
    }
    let mut empty_owner = root;
    empty_owner.owner = DocsOwner::Other {
        owner_kind: "workspace".into(),
        id: "".into(),
    };
    assert_eq!(
        materialize(&provider, empty_owner, options(None), pending())
            .await
            .unwrap_err()
            .kind,
        DocsErrorKind::InvalidConfiguration
    );
    assert_eq!(provider.checks.load(Ordering::SeqCst), 0);
    assert!(provider.calls().is_empty());
}

#[tokio::test]
async fn admission_failure_has_no_get_call_and_no_fabricaihub_fallback() {
    let root = key("a", "docs/topics");
    let mut provider = Provider::default();
    provider
        .rejected
        .insert(root.clone(), ProviderErrorKind::Unsupported);
    let error = materialize(&provider, root.clone(), options(None), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        DocsErrorKind::Provider(ProviderErrorKind::Unsupported)
    );
    assert!(provider.calls().is_empty());
    assert!(matches!(
        error.partial.nodes[0].status,
        NodeStatus::Failed {
            kind: ProviderErrorKind::Unsupported
        }
    ));
    let error = materialize(
        &UnavailableDocumentsProvider,
        root,
        options(None),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.kind,
        DocsErrorKind::Provider(ProviderErrorKind::Unavailable)
    );
}

#[tokio::test]
async fn report_model_cycle_fetches_each_canonical_owner_view_once() {
    let report = key("report", "docs/related");
    let model = key("model", "docs/related");
    let provider = Provider::with(vec![
        (
            report.clone(),
            vec![page(
                "report",
                vec![edge(
                    model.clone(),
                    "datasetId",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
        (
            model,
            vec![page(
                "model",
                vec![edge(
                    report.clone(),
                    "report link",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
    ]);
    let graph = materialize(&provider, report.clone(), options(Some(1)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 2);
    assert_eq!(graph.references.len(), 1);
    assert_eq!(graph.references[0].target, report);
    assert_eq!(graph.coverage, Coverage::Complete);
}

#[tokio::test]
async fn diamond_preserves_aliases_and_different_views_of_same_owner() {
    let root = key("report", "docs/related");
    let left = key("left", "docs/related");
    let right = key("right", "docs/related");
    let model = key("model", "docs/schema");
    let model_context = key("model", "docs/context");
    let provider = Provider::with(vec![
        (
            root.clone(),
            vec![page(
                "root",
                vec![
                    edge(right.clone(), "right", Some(EdgeDisposition::Related)),
                    edge(left.clone(), "left", Some(EdgeDisposition::Related)),
                    edge(
                        model_context.clone(),
                        "context",
                        Some(EdgeDisposition::Related),
                    ),
                ],
            )],
        ),
        (
            left,
            vec![page(
                "left",
                vec![edge(
                    model.clone(),
                    "datasource alias one",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
        (
            right,
            vec![page(
                "right",
                vec![edge(
                    model.clone(),
                    "datasource alias two",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
        (model.clone(), vec![page("schema", vec![])]),
        (model_context.clone(), vec![page("context", vec![])]),
    ]);
    let graph = materialize(&provider, root, options(Some(3)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 5);
    assert_eq!(graph.nodes.len(), 5);
    assert_eq!(graph.edges.len(), 5);
    assert_eq!(graph.references.len(), 1);
    assert_eq!(
        provider
            .calls()
            .iter()
            .filter(|request| request.key == model)
            .count(),
        1
    );
    assert!(graph.nodes.iter().any(|node| node.key == model_context));
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.edge.target == model)
            .map(|edge| edge.edge.provenance.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["datasource alias one", "datasource alias two"]
    );
}

#[tokio::test]
async fn exact_duplicate_edges_do_not_disappear_or_fetch_twice() {
    let root = key("a", "docs");
    let child = key("a", "docs/schema");
    let advertised = edge(child.clone(), "same alias", Some(EdgeDisposition::Child));
    let provider = Provider::with(vec![
        (
            root.clone(),
            vec![page("root", vec![advertised.clone(), advertised])],
        ),
        (child, vec![page("child", vec![])]),
    ]);
    let graph = materialize(&provider, root, options(Some(1)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(graph.edges.len(), 2);
    assert_eq!(graph.references.len(), 1);
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.status == EdgeStatus::Expanded)
            .count(),
        1
    );
}

#[tokio::test]
async fn only_advertised_related_subtree_can_cross_owners() {
    for root_view in ["docs/related", "docs/related/models"] {
        let root = key("a", root_view);
        let target = key("b", "docs/schema");
        let provider = Provider::with(vec![
            (
                root.clone(),
                vec![page(
                    "root",
                    vec![edge(
                        target.clone(),
                        "native",
                        Some(EdgeDisposition::Related),
                    )],
                )],
            ),
            (target, vec![page("child", vec![])]),
        ]);
        let graph = materialize(&provider, root, options(Some(1)), pending())
            .await
            .unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(provider.calls().len(), 2);
    }
    for (disposition, reason) in [
        (
            Some(EdgeDisposition::Related),
            CoverageReasonKind::CrossOwnerBoundary,
        ),
        (
            Some(EdgeDisposition::Child),
            CoverageReasonKind::CrossOwnerBoundary,
        ),
        (None, CoverageReasonKind::Unadvertised),
        (
            Some(EdgeDisposition::Reference),
            CoverageReasonKind::ReferenceOnly,
        ),
    ] {
        let root = key("a", "docs/context");
        let provider = Provider::with(vec![(
            root.clone(),
            vec![page(
                "root",
                vec![edge(key("b", "docs/schema"), "link", disposition)],
            )],
        )]);
        let graph = materialize(&provider, root, options(Some(2)), pending())
            .await
            .unwrap();
        assert_eq!(provider.calls().len(), 1);
        assert!(has_reason(&graph, reason));
    }
}

#[tokio::test]
async fn docs_root_does_not_activate_related_cross_owner_reads_indirectly() {
    let root = key("a", "docs");
    let related = key("a", "docs/related");
    let provider = Provider::with(vec![
        (
            root.clone(),
            vec![page(
                "root",
                vec![
                    edge(
                        related.clone(),
                        "structural related index",
                        Some(EdgeDisposition::Child),
                    ),
                    edge(
                        key("b", "docs"),
                        "direct related link",
                        Some(EdgeDisposition::Related),
                    ),
                ],
            )],
        ),
        (
            related,
            vec![page(
                "related index",
                vec![edge(
                    key("c", "docs"),
                    "indirect related link",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
    ]);
    let graph = materialize(&provider, root, options(Some(2)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(graph.nodes.len(), 2);
    assert!(has_reason(&graph, CoverageReasonKind::CrossOwnerBoundary));
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| matches!(
                edge.status,
                EdgeStatus::Unexpanded {
                    reason: CoverageReasonKind::CrossOwnerBoundary
                }
            ))
            .count(),
        2
    );
}

#[tokio::test]
async fn structural_children_cannot_escape_addressed_view() {
    let root = key("a", "docs/context");
    let provider = Provider::with(vec![(
        root.clone(),
        vec![page(
            "root",
            vec![edge(
                key("a", "docs/schema"),
                "not a child",
                Some(EdgeDisposition::Child),
            )],
        )],
    )]);
    let graph = materialize(&provider, root, options(Some(2)), pending())
        .await
        .unwrap();
    assert!(has_reason(&graph, CoverageReasonKind::OutsideSubtree));
    assert_eq!(provider.calls().len(), 1);
}

#[tokio::test]
async fn node_budget_is_global_and_n_plus_one_is_rejected_before_dispatch() {
    for max in [1, 2, 3] {
        let root = key("a", "docs");
        let child = key("a", "docs/schema");
        let other = key("a", "docs/context");
        let provider = Provider::with(vec![
            (
                root.clone(),
                vec![page(
                    "root",
                    vec![
                        edge(child.clone(), "schema", Some(EdgeDisposition::Child)),
                        edge(other.clone(), "context", Some(EdgeDisposition::Child)),
                    ],
                )],
            ),
            (child, vec![page("schema", vec![])]),
            (other, vec![page("context", vec![])]),
        ]);
        let mut opts = options(Some(1));
        opts.max_nodes = nz(max);
        let result = materialize(&provider, root, opts, pending()).await;
        assert_eq!(provider.calls().len(), max);
        if max < 3 {
            let error = result.unwrap_err();
            assert_eq!(error.kind, DocsErrorKind::Budget);
            assert_eq!(error.partial.nodes.len(), max);
            assert!(has_reason(&error.partial, CoverageReasonKind::Budget));
        } else {
            assert_eq!(result.unwrap().nodes.len(), 3);
        }
    }
}

#[tokio::test]
async fn page_budget_spans_root_paging_and_children_with_pins_in_request() {
    for max in [1, 2, 3] {
        let root = key("a", "docs");
        let child = key("a", "docs/schema");
        let mut first = page(
            "page1",
            vec![edge(child.clone(), "child", Some(EdgeDisposition::Child))],
        );
        first.continuation = Some("next".into());
        let provider = Provider::with(vec![
            (root.clone(), vec![first, page("page2", vec![])]),
            (child, vec![page("child", vec![])]),
        ]);
        let mut opts = options(Some(1));
        opts.max_pages = nz(max);
        let result = materialize(&provider, root, opts, pending()).await;
        assert_eq!(provider.calls().len(), max);
        if max < 3 {
            let error = result.unwrap_err();
            assert_eq!(error.kind, DocsErrorKind::Budget);
            assert_eq!(error.partial.pages_read, max);
        } else {
            assert_eq!(result.unwrap().pages_read, 3);
        }
        let calls = provider.calls();
        assert!(calls[0].expected_revision.is_none());
        assert!(calls[0].expected_snapshot.is_none());
        assert!(calls[0].continuation.is_none());
        if max > 1 {
            assert_eq!(calls[1].continuation.as_deref(), Some("next"));
            assert_eq!(calls[1].expected_revision.as_deref(), Some("r1"));
            assert_eq!(calls[1].expected_snapshot.as_deref(), Some("s1"));
            assert!(calls[1].max_bytes < calls[0].max_bytes);
        }
        if max > 2 {
            assert!(calls[2].expected_revision.is_none());
            assert!(calls[2].continuation.is_none());
            assert!(calls[2].max_bytes < calls[1].max_bytes);
        }
    }
}

#[tokio::test]
async fn byte_budget_covers_serialization_escaping_and_does_not_clip_root() {
    let root = key("a", "docs/context");
    let text = "é\n\"\\\u{0001}<b>";
    let provider = Provider::with(vec![(root.clone(), vec![page(text, vec![])])]);
    let graph = materialize(&provider, root.clone(), options(None), pending())
        .await
        .unwrap();
    let exact = graph.to_json(nz(100_000)).unwrap().len();
    for (cap, success) in [(exact, true), (exact - 1, false)] {
        let provider = Provider::with(vec![(root.clone(), vec![page(text, vec![])])]);
        let mut opts = options(None);
        opts.max_bytes = nz(cap);
        let result = materialize(&provider, root.clone(), opts, pending()).await;
        if success {
            assert_eq!(result.unwrap().to_json(nz(cap)).unwrap().len(), exact);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, DocsErrorKind::Budget);
            assert!(error.partial.nodes.iter().all(|node| node.pages.is_empty()));
        }
        assert_eq!(provider.calls().len(), 1);
    }
    assert_eq!(
        graph.to_json(nz(exact - 1)).unwrap_err(),
        DocsErrorKind::Budget
    );
    let provider = Provider::default();
    let mut opts = options(None);
    opts.max_bytes = nz(1);
    assert_eq!(
        materialize(&provider, root, opts, pending())
            .await
            .unwrap_err()
            .kind,
        DocsErrorKind::Budget
    );
    assert!(provider.calls().is_empty());
}

#[tokio::test]
async fn oversized_later_page_keeps_previous_whole_page_only() {
    let root = key("a", "docs");
    let mut first = page("kept", vec![]);
    first.continuation = Some("next".into());
    let provider = Provider::with(vec![(
        root.clone(),
        vec![first, page(&"too large".repeat(10_000), vec![])],
    )]);
    let mut opts = options(None);
    opts.max_bytes = nz(4_000);
    let error = materialize(&provider, root, opts, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Budget);
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(error.partial.nodes[0].pages.len(), 1);
    assert_eq!(error.partial.nodes[0].pages[0].content, "kept");
    assert_eq!(error.partial.nodes[0].continuation.as_deref(), Some("next"));
}

#[tokio::test]
async fn serialized_byte_boundary_is_shared_across_nodes_edges_and_references() {
    let root = key("a", "docs/related");
    let child = key("b", "docs/schema");
    let fixtures = vec![
        (
            root.clone(),
            vec![page(
                "root \"escaped\"",
                vec![
                    edge(child.clone(), "alias 1", Some(EdgeDisposition::Related)),
                    edge(child.clone(), "alias 2", Some(EdgeDisposition::Related)),
                ],
            )],
        ),
        (
            child,
            vec![page(
                "child",
                vec![edge(
                    root.clone(),
                    "back edge",
                    Some(EdgeDisposition::Related),
                )],
            )],
        ),
    ];
    let provider = Provider::with(fixtures.clone());
    let graph = materialize(&provider, root.clone(), options(Some(2)), pending())
        .await
        .unwrap();
    let exact = graph.to_json(nz(100_000)).unwrap().len();
    for (cap, success) in [(exact, true), (exact - 1, false)] {
        let provider = Provider::with(fixtures.clone());
        let mut opts = options(Some(2));
        opts.max_bytes = nz(cap);
        let result = materialize(&provider, root.clone(), opts, pending()).await;
        if success {
            assert_eq!(result.unwrap().to_json(nz(cap)).unwrap().len(), exact);
        } else {
            assert_eq!(result.unwrap_err().kind, DocsErrorKind::Budget);
        }
        assert!(provider.calls().len() <= 2);
    }
}

#[tokio::test]
async fn byte_reservation_stops_pagination_before_an_unfunded_call() {
    let root = key("a", "docs");
    let mut first = page("first page", vec![]);
    first.continuation = Some("next".into());
    let fixtures = vec![(root.clone(), vec![first, page("last page", vec![])])];
    let provider = Provider::with(fixtures.clone());
    let mut prefix = materialize(&provider, root.clone(), options(None), pending())
        .await
        .unwrap();
    prefix.pages_read = 1;
    prefix.nodes[0].pages.pop();
    prefix.nodes[0].continuation = Some("next".into());
    prefix.nodes[0].status = NodeStatus::Pending;
    let exact_prefix = prefix.to_json(nz(100_000)).unwrap().len();
    let provider = Provider::with(fixtures);
    let mut opts = options(None);
    opts.max_bytes = nz(exact_prefix);
    let error = materialize(&provider, root, opts, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Budget);
    assert_eq!(provider.calls().len(), 1);
    assert_eq!(error.partial.pages_read, 1);
    assert_eq!(error.partial.nodes[0].pages[0].content, "first page");
}

#[tokio::test]
async fn a_missing_served_root_never_falls_back_to_available_authored_content() {
    let served = key("a", "docs/topics");
    let mut authored = served.clone();
    authored.stage = Some("authored".into());
    let mut provider = Provider::with(vec![(authored, vec![page("authored source", vec![])])]);
    provider
        .rejected
        .insert(served.clone(), ProviderErrorKind::NotFound);
    let error = materialize(&provider, served.clone(), options(None), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        DocsErrorKind::Provider(ProviderErrorKind::NotFound)
    );
    assert!(provider.calls().is_empty());
    assert_eq!(error.partial.root, served);
    assert!(error.partial.nodes[0].pages.is_empty());
}

#[tokio::test]
async fn native_json_text_is_wrapped_with_its_actual_media_type_without_reinterpretation() {
    let root = key("a", "docs/schema");
    let mut native = page("{\"native\":\"dto\"}", vec![]);
    native.media_type = "application/json".into();
    let provider = Provider::with(vec![(root.clone(), vec![native])]);
    let graph = materialize(&provider, root, options(None), pending())
        .await
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&graph.to_json(nz(100_000)).unwrap()).unwrap();
    let content = &json["nodes"][0]["pages"][0];
    assert_eq!(content["media_type"], "application/json");
    assert_eq!(content["content"], "{\"native\":\"dto\"}");
    assert!(content.get("native").is_none());
}

#[tokio::test]
async fn malformed_provider_data_cannot_schedule_unadvertised_scopes() {
    let root = key("a", "docs");
    let provider = Provider::with(vec![(
        root.clone(),
        vec![page(
            "must not commit",
            vec![edge(
                key("a", "src/definition"),
                "not docs",
                Some(EdgeDisposition::Child),
            )],
        )],
    )]);
    let error = materialize(&provider, root, options(Some(1)), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::InvalidProviderData);
    assert_eq!(provider.calls().len(), 1);
    assert!(error.partial.nodes[0].pages.is_empty());
}

#[tokio::test]
async fn repeated_continuation_is_not_a_graph_cycle_and_never_loops() {
    let root = key("a", "docs");
    let mut first = page("first", vec![]);
    first.continuation = Some("same".into());
    let mut repeated = page("must not mix", vec![]);
    repeated.continuation = Some("same".into());
    let provider = Provider::with(vec![(root.clone(), vec![first, repeated])]);
    let error = materialize(&provider, root, options(None), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::CursorCycle);
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(error.partial.nodes[0].pages.len(), 1);
    assert!(has_reason(&error.partial, CoverageReasonKind::CursorCycle));
}

#[tokio::test]
async fn changed_revision_snapshot_or_absence_never_splices_pages() {
    for (revision, snapshot) in [
        (Some("r2"), Some("s1")),
        (Some("r1"), Some("s2")),
        (None, Some("s1")),
        (Some("r1"), None),
    ] {
        let root = key("a", "docs");
        let mut first = page("first", vec![]);
        first.continuation = Some("next".into());
        let mut changed = page("must not mix", vec![]);
        changed.revision = revision.map(str::to_owned);
        changed.snapshot = snapshot.map(str::to_owned);
        let provider = Provider::with(vec![(root.clone(), vec![first, changed])]);
        let error = materialize(&provider, root, options(None), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, DocsErrorKind::Drift);
        assert_eq!(provider.calls().len(), 2);
        assert_eq!(error.partial.nodes.len(), 1);
        assert_eq!(error.partial.nodes[0].pages.len(), 1);
        assert_eq!(error.partial.nodes[0].pages[0].content, "first");
        assert!(has_reason(&error.partial, CoverageReasonKind::Drift));
    }
}

#[tokio::test]
async fn page_cursor_guards_are_per_key_and_no_pin_is_not_exhaustive_success() {
    let root = key("a", "docs");
    let child = key("a", "docs/schema");
    let mut first = page(
        "root first",
        vec![edge(child.clone(), "child", Some(EdgeDisposition::Child))],
    );
    first.continuation = Some("shared-token".into());
    let mut child_first = page("child first", vec![]);
    child_first.continuation = Some("shared-token".into());
    let mut child_last = page("child last", vec![]);
    for page in [&mut child_first, &mut child_last] {
        page.revision = None;
        page.snapshot = None;
    }
    let provider = Provider::with(vec![
        (root.clone(), vec![first, page("root last", vec![])]),
        (child, vec![child_first, child_last]),
    ]);
    let graph = materialize(&provider, root, options(Some(1)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 4);
    assert!(has_reason(&graph, CoverageReasonKind::UnpinnedPagination));
}

#[tokio::test]
async fn denied_not_found_unsupported_and_unavailable_remain_distinct_per_node() {
    let root = key("a", "docs/related");
    let mut child_keys = Vec::new();
    let mut replies = Vec::new();
    for (item, kind) in [
        ("b", ProviderErrorKind::Denied),
        ("c", ProviderErrorKind::NotFound),
        ("d", ProviderErrorKind::Unsupported),
        ("e", ProviderErrorKind::Unavailable),
    ] {
        let child = key(item, "docs");
        child_keys.push(edge(child.clone(), item, Some(EdgeDisposition::Related)));
        replies.push((child, kind));
    }
    let provider = Provider::with(vec![(root.clone(), vec![page("root", child_keys)])]);
    for (child, kind) in &replies {
        provider.pages.lock().unwrap().insert(
            child.clone(),
            VecDeque::from([Err(ProviderError {
                kind: *kind,
                detail: None,
            })]),
        );
    }
    let error = materialize(&provider, root, options(Some(1)), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        DocsErrorKind::Provider(ProviderErrorKind::Denied)
    );
    assert_eq!(provider.calls().len(), 5);
    assert_eq!(error.partial.nodes.len(), 5);
    for (child, kind) in replies {
        let node = error
            .partial
            .nodes
            .iter()
            .find(|node| node.key == child)
            .unwrap();
        assert_eq!(node.status, NodeStatus::Failed { kind });
        assert!(node.pages.is_empty());
    }
    assert_eq!(error.partial.nodes[0].pages[0].content, "root");
}

#[tokio::test]
async fn upstream_and_advertised_omissions_are_preserved_without_child_reads() {
    let root = key("a", "docs");
    let mut root_page = page(
        "viewport",
        vec![
            edge(
                key("a", "docs/denied"),
                "one",
                Some(EdgeDisposition::Denied),
            ),
            edge(
                key("a", "docs/missing"),
                "two",
                Some(EdgeDisposition::NotFound),
            ),
            edge(
                key("a", "docs/unsupported"),
                "three",
                Some(EdgeDisposition::Unsupported),
            ),
            edge(
                key("a", "docs/unavailable"),
                "four",
                Some(EdgeDisposition::Unavailable),
            ),
            edge(
                key("a", "docs/omitted"),
                "five",
                Some(EdgeDisposition::Omitted),
            ),
        ],
    );
    root_page.coverage = Coverage::Incomplete {
        reasons: vec![CoverageReason {
            kind: CoverageReasonKind::ProviderPartial,
            key: Some(root.clone()),
            detail: Some("first 100 lines; schema unavailable".into()),
        }],
    };
    let provider = Provider::with(vec![(root.clone(), vec![root_page])]);
    let graph = materialize(&provider, root, options(Some(2)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 1);
    for kind in [
        CoverageReasonKind::ProviderPartial,
        CoverageReasonKind::Denied,
        CoverageReasonKind::NotFound,
        CoverageReasonKind::Unsupported,
        CoverageReasonKind::Unavailable,
        CoverageReasonKind::Omitted,
    ] {
        assert!(has_reason(&graph, kind));
    }
}

#[tokio::test]
async fn selectors_and_non_item_owner_are_real_distinct_canonical_identities() {
    let root = key("a", "docs/related");
    let first = DocsKey {
        owner: DocsOwner::Other {
            owner_kind: "workspace".into(),
            id: "canonical-workspace".into(),
        },
        view: "docs".into(),
        facet: Some("schema".into()),
        subresource: Some("table".into()),
        stage: Some("served".into()),
        revision_selector: Some("selected-1".into()),
    };
    let mut second = first.clone();
    second.revision_selector = Some("selected-2".into());
    let mut third = first.clone();
    third.stage = Some("effective".into());
    let provider = Provider::with(vec![
        (
            root.clone(),
            vec![page(
                "root",
                vec![
                    edge(first.clone(), "first", Some(EdgeDisposition::Related)),
                    edge(second.clone(), "second", Some(EdgeDisposition::Related)),
                    edge(third.clone(), "third", Some(EdgeDisposition::Related)),
                ],
            )],
        ),
        (first, vec![page("one", vec![])]),
        (second, vec![page("two", vec![])]),
        (third, vec![page("three", vec![])]),
    ]);
    let graph = materialize(&provider, root, options(Some(1)), pending())
        .await
        .unwrap();
    assert_eq!(provider.calls().len(), 4);
    assert_eq!(graph.nodes.len(), 4);
}

struct DropGuard(Arc<AtomicBool>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct WaitingProvider {
    root: DocsKey,
    child: DocsKey,
    started: tokio::sync::Notify,
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
}

struct SignallingProvider {
    inner: Provider,
    cancelled: AtomicBool,
}

impl DocumentsProvider for SignallingProvider {
    fn check_scope(&self, key: &DocsKey) -> Result<(), ProviderError> {
        self.inner.check_scope(key)
    }

    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a> {
        let response = self.inner.get(request);
        self.cancelled.store(true, Ordering::SeqCst);
        response
    }
}

#[tokio::test]
async fn cancellation_wins_over_a_simultaneously_ready_page_before_commit() {
    let root = key("a", "docs");
    let provider = SignallingProvider {
        inner: Provider::with(vec![(root.clone(), vec![page("must not commit", vec![])])]),
        cancelled: AtomicBool::new(false),
    };
    let cancellation = poll_fn(|_| {
        if provider.cancelled.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let error = materialize(&provider, root, options(None), cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Cancelled);
    assert_eq!(provider.inner.calls().len(), 1);
    assert!(error.partial.nodes[0].pages.is_empty());
}

#[tokio::test]
async fn cancellation_at_dispatch_does_not_charge_an_undispatched_page() {
    let provider = Provider::default();
    let polls = AtomicUsize::new(0);
    let cancellation = poll_fn(|_| {
        if polls.fetch_add(1, Ordering::SeqCst) == 2 {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let error = materialize(&provider, key("a", "docs"), options(None), cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Cancelled);
    assert!(provider.calls().is_empty());
    assert_eq!(error.partial.pages_read, 0);
}

impl DocumentsProvider for WaitingProvider {
    fn check_scope(&self, _: &DocsKey) -> Result<(), ProviderError> {
        Ok(())
    }

    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if request.key == &self.root {
                return Ok(page(
                    "previous accepted root",
                    vec![edge(
                        self.child.clone(),
                        "child",
                        Some(EdgeDisposition::Child),
                    )],
                ));
            }
            let _guard = DropGuard(self.dropped.clone());
            self.started.notify_one();
            pending().await
        })
    }
}

#[tokio::test]
async fn cancellation_before_dispatch_and_during_pending_work_preserves_evidence() {
    let root = key("a", "docs");
    let provider = Provider::default();
    let error = materialize(&provider, root.clone(), options(None), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Cancelled);
    assert!(provider.calls().is_empty());
    assert_eq!(provider.checks.load(Ordering::SeqCst), 0);
    let provider = WaitingProvider {
        root: root.clone(),
        child: key("a", "docs/schema"),
        started: tokio::sync::Notify::new(),
        dropped: Arc::new(AtomicBool::new(false)),
        calls: AtomicUsize::new(0),
    };
    let error = materialize(
        &provider,
        root,
        options(Some(1)),
        provider.started.notified(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, DocsErrorKind::Cancelled);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(provider.dropped.load(Ordering::SeqCst));
    assert!(error.partial.nodes.iter().any(|node| {
        node.pages
            .iter()
            .any(|page| page.content == "previous accepted root")
    }));
    assert!(has_reason(&error.partial, CoverageReasonKind::Cancelled));
    assert!(
        error
            .partial
            .nodes
            .iter()
            .any(|node| node.status == NodeStatus::Pending)
    );
}

#[tokio::test]
async fn deterministic_json_and_bounded_markdown_escape_provider_markup() {
    let root = key("a", "docs/related");
    let child = key("b", "docs");
    let mut jsons = Vec::new();
    let mut markdowns = Vec::new();
    for reverse in [false, true] {
        let mut edges = vec![
            edge(
                child.clone(),
                "<script>alert(1)</script>",
                Some(EdgeDisposition::Related),
            ),
            edge(
                root.clone(),
                "[unsafe](javascript:bad)",
                Some(EdgeDisposition::Related),
            ),
        ];
        if reverse {
            edges.reverse();
        }
        let provider = Provider::with(vec![
            (
                root.clone(),
                vec![page(
                    "<img src=x onerror=bad>\n[link](javascript:bad)",
                    edges,
                )],
            ),
            (child.clone(), vec![page("native child", vec![])]),
        ]);
        let graph = materialize(&provider, root.clone(), options(Some(2)), pending())
            .await
            .unwrap();
        jsons.push(graph.to_json(nz(100_000)).unwrap());
        let markdown = graph.to_markdown(nz(100_000)).unwrap();
        assert!(markdown.contains("&lt;img"));
        assert!(markdown.contains("&lt;script&gt;"));
        assert!(!markdown.contains("<img"));
        assert!(!markdown.contains("<script>"));
        assert!(!markdown.contains("](javascript:"));
        assert!(markdown.contains("(#document-"));
        assert!(markdown.contains("tenant=tenant; workspace=workspace; item="));
        assert!(markdown.contains("revision=r1; snapshot=s1"));
        let minimum = markdown.len().max(jsons.last().unwrap().len());
        assert_eq!(graph.to_markdown(nz(minimum)).unwrap(), markdown);
        assert_eq!(
            graph.to_markdown(nz(markdown.len() - 1)).unwrap_err(),
            DocsErrorKind::Budget
        );
        markdowns.push(markdown);
    }
    assert_eq!(jsons[0], jsons[1]);
    assert_eq!(markdowns[0], markdowns[1]);
}
