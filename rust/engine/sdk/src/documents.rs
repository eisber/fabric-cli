// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Provider-neutral, read-only documentation materialization (grouped get rev16).
//!
//! Providers advertise exact scopes and return already-served text or native
//! content, not synthesized schema. There is deliberately no built-in FabricAIHub
//! transport, identity discovery, authored/served fallback, HTTP client, model
//! invocation, query execution, or runtime here. The caller injects a provider,
//! cancellation (including its total deadline), and every numeric budget.
//!
//! JSON preserves page content/media without interpreting Markdown as a native
//! schema. Markdown is a bounded, escaped presentation with internal references;
//! it never follows source links or renders provider strings as raw HTML.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    future::{Future, poll_fn},
    io::{self, Write},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
};

/// Canonical owner identity. Display names and aliases must be resolved upstream.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocsOwner {
    /// An item in one explicit tenant and workspace.
    Item {
        /// Canonical tenant identifier.
        tenant: String,
        /// Canonical workspace identifier.
        workspace: String,
        /// Canonical item identifier, not a connection/alias identifier.
        item: String,
    },
    /// An explicitly identified non-item owner; never an implicit current scope.
    Other {
        /// Provider's canonical owner kind.
        owner_kind: String,
        /// Provider's canonical owner identifier.
        id: String,
    },
}

/// Invocation-wide identity, independent of the revision observed while reading.
///
/// Identifiers are case-sensitive opaque canonical strings: callers/providers
/// normalize them before constructing keys. Empty, padded, or control-containing
/// identity/selector strings are rejected. `view` is `docs` or a slash-separated
/// descendant, without empty, `.` or `..` segments. No URI is a view.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct DocsKey {
    /// Canonical item or explicit non-item owner.
    pub owner: DocsOwner,
    /// Addressed read-only view, for example `docs/context` or `docs/related`.
    pub view: String,
    /// Provider-advertised facet, if applicable.
    pub facet: Option<String>,
    /// Exact provider-qualified subresource, if applicable.
    pub subresource: Option<String>,
    /// Explicit selected stage/root, distinct from an observed revision.
    pub stage: Option<String>,
    /// Caller-selected revision, not the revision subsequently observed.
    pub revision_selector: Option<String>,
}

impl DocsKey {
    /// Validates identity and read-only docs scope without invoking a provider.
    ///
    /// # Errors
    /// Returns invalid configuration or an unsupported non-docs scope.
    pub fn validate(&self) -> Result<(), DocsErrorKind> {
        let owner_valid = match &self.owner {
            DocsOwner::Item {
                tenant,
                workspace,
                item,
            } => [tenant, workspace, item].into_iter().all(|s| valid_atom(s)),
            DocsOwner::Other { owner_kind, id } => valid_atom(owner_kind) && valid_atom(id),
        };
        if !owner_valid
            || !valid_atom(&self.view)
            || [
                &self.facet,
                &self.subresource,
                &self.stage,
                &self.revision_selector,
            ]
            .into_iter()
            .flatten()
            .any(|value| !valid_atom(value))
            || self
                .view
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || self.view.contains(['\\', '?', '#', ':'])
        {
            return Err(DocsErrorKind::InvalidConfiguration);
        }
        if self.view != "docs" && !self.view.starts_with("docs/") {
            return Err(DocsErrorKind::Provider(ProviderErrorKind::Unsupported));
        }
        Ok(())
    }

    fn includes_related(&self) -> bool {
        self.view == "docs/related" || self.view.starts_with("docs/related/")
    }
}

fn valid_atom(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

/// Provider or traversal coverage; complete means only the explicitly selected scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Coverage {
    /// No known omissions within the selected scope.
    Complete,
    /// Known omissions, upstream limits, or interrupted work.
    Incomplete {
        /// Nonempty, deterministically ordered reasons in a materialized graph.
        reasons: Vec<CoverageReason>,
    },
}

impl Coverage {
    fn add(&mut self, reason: CoverageReason) {
        match self {
            Self::Complete => {
                *self = Self::Incomplete {
                    reasons: vec![reason],
                };
            }
            Self::Incomplete { reasons } => {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
    }

    fn extend(&mut self, other: &Self) {
        if let Self::Incomplete { reasons } = other {
            if reasons.is_empty() {
                self.add(CoverageReason {
                    kind: CoverageReasonKind::ProviderPartial,
                    key: None,
                    detail: None,
                });
            }
            for reason in reasons {
                self.add(reason.clone());
            }
        }
    }

    fn sort(&mut self) {
        if let Self::Incomplete { reasons } = self {
            reasons.sort();
            reasons.dedup();
        }
    }
}

/// One explicit reason a graph or node is not exhaustive.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct CoverageReason {
    /// Machine-readable omission/failure.
    pub kind: CoverageReasonKind,
    /// Canonical affected view, when known.
    pub key: Option<DocsKey>,
    /// Inert provider explanation/provenance, not instructions or markup.
    pub detail: Option<String>,
}

/// Distinct boundaries, failure classifications, and upstream limitations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReasonKind {
    /// Upstream viewport or component coverage was incomplete.
    ProviderPartial,
    /// Recursion was not requested.
    NonRecursive,
    /// The selected finite edge depth was reached.
    DepthLimit,
    /// Cross-owner/related scope was not selected.
    CrossOwnerBoundary,
    /// A purported structural child is outside its source view subtree.
    OutsideSubtree,
    /// An edge has no advertised expansion disposition.
    Unadvertised,
    /// Provider intentionally omitted this edge.
    Omitted,
    /// Provider advertised a reference only, without an expandable target.
    ReferenceOnly,
    /// Provider denied the scope.
    Denied,
    /// Provider reported a missing scope.
    NotFound,
    /// Scope or representation is unsupported.
    Unsupported,
    /// Scope or provider is unavailable.
    Unavailable,
    /// Paginated data had neither a revision nor snapshot pin.
    UnpinnedPagination,
    /// A global budget ended traversal.
    Budget,
    /// Source revision/snapshot changed across pages.
    Drift,
    /// A continuation token repeated for one key.
    CursorCycle,
    /// Caller cancellation/deadline interrupted traversal.
    Cancelled,
    /// A provider returned malformed identity/media/token data.
    InvalidProviderData,
}

/// Schema-advertised edge behavior; absent disposition never authorizes a read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDisposition {
    /// Structural child within the same owner's docs subtree.
    Child,
    /// Explicit docs/related link, enabled only by the selected root subtree.
    Related,
    /// Reference only; not permission to fetch.
    Reference,
    /// Intentionally omitted scope.
    Omitted,
    /// Known authorization denial.
    Denied,
    /// Known missing target.
    NotFound,
    /// Known unsupported target.
    Unsupported,
    /// Known unavailable target.
    Unavailable,
}

/// An advertised navigation edge, never a URL extracted from source Markdown.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DocEdge {
    /// Complete canonical target identity.
    pub target: DocsKey,
    /// Provider-advertised relationship/child kind, when known.
    pub kind: Option<String>,
    /// Alias/path/provider provenance, retained even for duplicate targets.
    pub provenance: Option<String>,
    /// Explicit expansion/omission capability. `None` is not expandable.
    pub disposition: Option<EdgeDisposition>,
}

/// One provider page. Text/media remain intact; no native DTO is inferred.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocPage {
    /// Exact served text, not clipped, merged, or interpreted.
    pub content: String,
    /// Advertised media type, including native structured text when provided.
    pub media_type: String,
    /// Observed source revision; never part of the visited identity.
    pub revision: Option<String>,
    /// Observed snapshot pin, distinct from a selected revision.
    pub snapshot: Option<String>,
    /// Only these explicit edges may be considered for expansion.
    pub edges: Vec<DocEdge>,
    /// Opaque next-page token for this exact key.
    pub continuation: Option<String>,
    /// Upstream component/viewport completeness.
    pub coverage: Coverage,
}

/// Distinct provider failures, never converted to an empty successful page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// The caller is not allowed to read this scope.
    Denied,
    /// The addressed scope was not found.
    NotFound,
    /// The exact scope/representation is unsupported.
    Unsupported,
    /// No admitted provider is configured, or it is temporarily unavailable.
    Unavailable,
}

impl ProviderErrorKind {
    fn coverage(self) -> CoverageReasonKind {
        match self {
            Self::Denied => CoverageReasonKind::Denied,
            Self::NotFound => CoverageReasonKind::NotFound,
            Self::Unsupported => CoverageReasonKind::Unsupported,
            Self::Unavailable => CoverageReasonKind::Unavailable,
        }
    }
}

/// Provider failure with an optional inert, nonsecret diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderError {
    /// Stable failure classification.
    pub kind: ProviderErrorKind,
    /// Sanitized explanation; must not contain credentials or continuation URLs.
    pub detail: Option<String>,
}

/// Borrowed page request with invocation-wide remaining byte allowance.
#[derive(Clone, Copy, Debug)]
pub struct DocRequest<'a> {
    /// Exact requested canonical identity.
    pub key: &'a DocsKey,
    /// Opaque token returned by the preceding accepted page for this key.
    pub continuation: Option<&'a str>,
    /// First accepted page's revision, to enforce at the source when supported.
    pub expected_revision: Option<&'a str>,
    /// First accepted page's snapshot, to enforce at the source when supported.
    pub expected_snapshot: Option<&'a str>,
    /// Remaining serialized-data allowance, also an upper bound on page content.
    ///
    /// Providers must bound acquisition/decoding before allocating unbounded
    /// responses. Final JSON accounting includes all graph/envelope overhead.
    pub max_bytes: NonZeroUsize,
}

/// Borrowed, caller-runtime future; dropping it must stop local provider work.
pub type DocumentsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DocPage, ProviderError>> + Send + 'a>>;

/// Injected, read-only provider for already-advertised documentation capabilities.
pub trait DocumentsProvider: Send + Sync {
    /// Checks exact scope support without I/O, authentication, or side effects.
    ///
    /// # Errors
    /// Returns unsupported/unavailable/denied scope before page acquisition.
    fn check_scope(&self, key: &DocsKey) -> Result<(), ProviderError>;

    /// Reads one bounded page of the exact scope. No fallback or implicit effects.
    fn get<'a>(&'a self, request: DocRequest<'a>) -> DocumentsFuture<'a>;
}

/// Explicit disabled capability until the host injects an admitted real provider.
#[derive(Debug)]
pub struct UnavailableDocumentsProvider;

impl DocumentsProvider for UnavailableDocumentsProvider {
    fn check_scope(&self, _: &DocsKey) -> Result<(), ProviderError> {
        Err(ProviderError {
            kind: ProviderErrorKind::Unavailable,
            detail: None,
        })
    }

    fn get<'a>(&'a self, _: DocRequest<'a>) -> DocumentsFuture<'a> {
        Box::pin(async {
            Err(ProviderError {
                kind: ProviderErrorKind::Unavailable,
                detail: None,
            })
        })
    }
}

/// Required finite invocation limits; deliberately no numeric defaults.
#[derive(Clone, Copy, Debug)]
pub struct DocsOptions {
    /// Explicit expansion opt-in.
    pub recursive: bool,
    /// Root is zero. Required for recursive reads and invalid otherwise.
    pub depth: Option<u32>,
    /// Maximum unique node attempts, including failed nodes.
    pub max_nodes: NonZeroUsize,
    /// Maximum total provider page attempts across all nodes.
    pub max_pages: NonZeroUsize,
    /// Maximum complete compact JSON graph bytes, before projection.
    pub max_bytes: NonZeroUsize,
}

impl DocsOptions {
    fn validate(self) -> Result<u32, DocsErrorKind> {
        match (self.recursive, self.depth) {
            (false, None) => Ok(0),
            (true, Some(depth)) => Ok(depth),
            _ => Err(DocsErrorKind::InvalidConfiguration),
        }
    }
}

/// Preserved native page content and source pins, without duplicated graph edges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocContent {
    /// Exact unmodified provider text.
    pub content: String,
    /// Native advertised media type.
    pub media_type: String,
    /// Observed revision for this page.
    pub revision: Option<String>,
    /// Observed snapshot for this page.
    pub snapshot: Option<String>,
}

/// Acquisition state, including incomplete nodes retained as error evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NodeStatus {
    /// Acquisition was interrupted or has not yet finished paging.
    Pending,
    /// All returned pages were acquired, subject to declared coverage.
    Available,
    /// Provider failed; any earlier accepted pages remain explicit partial data.
    Failed {
        /// Exact failure classification.
        kind: ProviderErrorKind,
    },
}

/// One canonical owner/view, fetched at most once per invocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocNode {
    /// Visited identity, independent of observed revisions.
    pub key: DocsKey,
    /// Pages remain separate; incompatible revisions are never spliced.
    pub pages: Vec<DocContent>,
    /// Node acquisition status.
    pub status: NodeStatus,
    /// Native page and interruption coverage.
    pub coverage: Coverage,
    /// Last accepted next-page token, if acquisition did not finish.
    pub continuation: Option<String>,
}

/// Traversal outcome for an explicitly advertised edge.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EdgeStatus {
    /// Target has been scheduled but traversal was interrupted before completion.
    Pending,
    /// This edge introduced the unique target attempt.
    Expanded,
    /// Target was already visited/scheduled; no second acquisition.
    Reference,
    /// Target was not fetched, with the explicit boundary retained.
    Unexpanded {
        /// Machine-readable omission reason.
        reason: CoverageReasonKind,
    },
}

/// An edge with canonical origin and retained alias/path provenance.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Canonical originating node.
    pub source: DocsKey,
    /// Provider edge, including complete target identity.
    pub edge: DocEdge,
    /// Traversal disposition, distinct from the provider's advertisement.
    pub status: EdgeStatus,
}

/// One alias/back-edge to an already scheduled canonical node.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DocReference {
    /// Canonical origin of the reference.
    pub source: DocsKey,
    /// Canonical target, which need not have completed when traversal failed.
    pub target: DocsKey,
    /// Provider's edge kind, when known.
    pub kind: Option<String>,
    /// Original alias/path provenance.
    pub provenance: Option<String>,
}

/// Deterministic JSON graph, not a manufactured structured native schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphResult {
    /// Exact requested root and selectors.
    pub root: DocsKey,
    /// Completeness only within the selected scope and explicit limits.
    pub coverage: Coverage,
    /// Canonically ordered unique owner/views.
    pub nodes: Vec<DocNode>,
    /// All advertised edges, including distinct aliases and omitted targets.
    pub edges: Vec<GraphEdge>,
    /// Canonically ordered duplicate-target/back-edge references.
    pub references: Vec<DocReference>,
    /// Total dispatched page attempts, including provider failures.
    pub pages_read: usize,
}

impl GraphResult {
    fn new(root: DocsKey) -> Self {
        Self {
            root,
            coverage: Coverage::Complete,
            nodes: Vec::new(),
            edges: Vec::new(),
            references: Vec::new(),
            pages_read: 0,
        }
    }

    fn normalize(&mut self) {
        self.nodes.sort_by(|a, b| a.key.cmp(&b.key));
        self.edges.sort();
        self.references.sort();
        self.coverage.sort();
        for node in &mut self.nodes {
            node.coverage.sort();
        }
    }

    /// Encodes the whole graph as compact JSON, never clipping content.
    ///
    /// # Errors
    /// Returns a byte-budget or encoding failure with no partial output bytes.
    pub fn to_json(&self, max_bytes: NonZeroUsize) -> Result<Vec<u8>, DocsErrorKind> {
        let mut writer = BoundedWriter::new(max_bytes.get(), true);
        serde_json::to_writer(&mut writer, self).map_err(|_| writer.error())?;
        Ok(writer.bytes)
    }

    /// Renders escaped Markdown with canonical provenance and internal-only links.
    ///
    /// Native source Markdown is displayed as inert quoted text, not parsed,
    /// followed, or promoted to a structured metadata schema. Both the output and
    /// its source graph must fit the independently supplied serialization bound.
    ///
    /// # Errors
    /// Returns a byte-budget or encoding failure; no clipped document is returned.
    pub fn to_markdown(&self, max_bytes: NonZeroUsize) -> Result<String, DocsErrorKind> {
        encoded_len(self, max_bytes.get())?;
        let mut out = BoundedWriter::new(max_bytes.get(), true);
        render_markdown(self, &mut out).map_err(|_| out.error())?;
        String::from_utf8(out.bytes).map_err(|_| DocsErrorKind::Encoding)
    }
}

/// Typed materialization failure; no clipped source becomes successful output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "provider", rename_all = "snake_case")]
pub enum DocsErrorKind {
    /// Invalid identity/options, rejected before provider calls.
    InvalidConfiguration,
    /// A global node/page/serialized-data ceiling was reached.
    Budget,
    /// Revision or snapshot changed while paging one canonical key.
    Drift,
    /// One key's continuation token repeated.
    CursorCycle,
    /// Caller cancellation/deadline won; pending local work was dropped.
    Cancelled,
    /// Distinct provider failure, never an empty page.
    Provider(ProviderErrorKind),
    /// Provider returned malformed keys, media, or continuation information.
    InvalidProviderData,
    /// A serialization or presentation encoding failed.
    Encoding,
}

impl fmt::Display for DocsErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DocsErrorKind::InvalidConfiguration => "invalid documents configuration",
            DocsErrorKind::Budget => "documents invocation budget exceeded",
            DocsErrorKind::Drift => "documents source revision or snapshot changed",
            DocsErrorKind::CursorCycle => "documents continuation token repeated",
            DocsErrorKind::Cancelled => "documents retrieval cancelled",
            DocsErrorKind::Provider(ProviderErrorKind::Denied) => "documents access denied",
            DocsErrorKind::Provider(ProviderErrorKind::NotFound) => "documents scope not found",
            DocsErrorKind::Provider(ProviderErrorKind::Unsupported) => {
                "documents scope unsupported"
            }
            DocsErrorKind::Provider(ProviderErrorKind::Unavailable) => {
                "documents provider unavailable"
            }
            DocsErrorKind::InvalidProviderData => "invalid documents provider data",
            DocsErrorKind::Encoding => "documents encoding failed",
        })
    }
}

impl std::error::Error for DocsErrorKind {}

/// Failure with canonical location and previously committed graph evidence.
///
/// The byte cap applies to successful data serialization. Error diagnostics and
/// coverage annotations may add overhead to the previously bounded partial
/// graph; no failed/over-budget page content is added or clipped.
#[derive(Clone, Debug)]
pub struct DocsError {
    /// Stable error classification.
    pub kind: DocsErrorKind,
    /// Exact affected scope when known.
    pub key: Option<Box<DocsKey>>,
    /// Bounded accepted data plus explicit interruption/omission coverage.
    pub partial: Box<GraphResult>,
}

impl fmt::Display for DocsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.kind, f)
    }
}

impl std::error::Error for DocsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

fn fail(mut graph: GraphResult, kind: DocsErrorKind, key: Option<DocsKey>) -> DocsError {
    let reason = CoverageReason {
        kind: match kind {
            DocsErrorKind::Budget => CoverageReasonKind::Budget,
            DocsErrorKind::Drift => CoverageReasonKind::Drift,
            DocsErrorKind::CursorCycle => CoverageReasonKind::CursorCycle,
            DocsErrorKind::Cancelled => CoverageReasonKind::Cancelled,
            DocsErrorKind::Provider(provider) => provider.coverage(),
            _ => CoverageReasonKind::InvalidProviderData,
        },
        key: key.clone(),
        detail: None,
    };
    graph.coverage.add(reason.clone());
    if let Some(node) = graph
        .nodes
        .iter_mut()
        .find(|node| Some(&node.key) == key.as_ref())
    {
        node.coverage.add(reason);
    }
    graph.normalize();
    DocsError {
        kind,
        key: key.map(Box::new),
        partial: Box::new(graph),
    }
}

/// Materializes an exact docs root and explicitly advertised bounded subtree.
///
/// Breadth-first expansion sorts each node's edges before scheduling, so aliases
/// and cycles are deterministic. Only an explicitly selected `docs/related` root
/// or descendant permits `Related` edges; structural children never cross owners
/// or leave the source view. Source strings, URLs, and arbitrary relationship
/// facts are inert.
///
/// Cancellation is polled before constructing/dispatching a provider request,
/// during every pending future, and before returning. Dropping pending work does
/// not send a remote cancel. Provider failures are retained per-node; independent
/// scheduled siblings may still be read, and the invocation returns an error.
///
/// # Errors
/// Returns typed invalid/budget/drift/cursor/cancel/provider failures with partial
/// evidence. New pages are committed atomically only after pin and size checks.
pub async fn materialize(
    provider: &dyn DocumentsProvider,
    root: DocsKey,
    options: DocsOptions,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<GraphResult, DocsError> {
    let mut graph = GraphResult::new(root.clone());
    let depth_limit = options
        .validate()
        .and_then(|depth| root.validate().map(|()| depth))
        .map_err(|kind| fail(graph.clone(), kind, Some(root.clone())))?;
    let mut cancelled = pin!(cancelled);
    let mut queue = VecDeque::from([(root.clone(), 0_u32)]);
    let mut visited = BTreeSet::from([root.clone()]);
    let mut first_provider_failure = None;
    while let Some((key, depth)) = queue.pop_front() {
        if is_cancelled(cancelled.as_mut()).await {
            return Err(fail(graph, DocsErrorKind::Cancelled, Some(key)));
        }
        if graph.nodes.len() >= options.max_nodes.get()
            || graph.pages_read >= options.max_pages.get()
        {
            return Err(fail(graph, DocsErrorKind::Budget, Some(key)));
        }
        let node_index = graph.nodes.len();
        graph.nodes.push(DocNode {
            key: key.clone(),
            pages: Vec::new(),
            status: NodeStatus::Pending,
            coverage: Coverage::Complete,
            continuation: None,
        });
        if let Err(kind) = encoded_len(&graph, options.max_bytes.get()) {
            graph.nodes.pop();
            return Err(fail(graph, kind, Some(key)));
        }
        for edge in &mut graph.edges {
            if edge.edge.target == key && edge.status == EdgeStatus::Pending {
                edge.status = EdgeStatus::Expanded;
            }
        }
        if let Err(error) = provider.check_scope(&key) {
            record_provider_failure(&mut graph, node_index, &error);
            first_provider_failure.get_or_insert((error.kind, key));
            continue;
        }
        let mut seen_tokens = BTreeSet::new();
        let mut pins: Option<(Option<String>, Option<String>)> = None;
        let mut pending_edges = Vec::new();
        loop {
            if is_cancelled(cancelled.as_mut()).await {
                return Err(fail(graph, DocsErrorKind::Cancelled, Some(key)));
            }
            if graph.pages_read >= options.max_pages.get() {
                return Err(fail(graph, DocsErrorKind::Budget, Some(key)));
            }
            // Charge the dispatched attempt and its serialization before I/O.
            graph.pages_read += 1;
            let used = match encoded_len(&graph, options.max_bytes.get()) {
                Ok(used) => used,
                Err(kind) => {
                    graph.pages_read -= 1;
                    return Err(fail(graph, kind, Some(key)));
                }
            };
            let remaining = options.max_bytes.get().saturating_sub(used);
            let Some(max_bytes) = NonZeroUsize::new(remaining) else {
                graph.pages_read -= 1;
                return Err(fail(graph, DocsErrorKind::Budget, Some(key)));
            };
            let request = DocRequest {
                key: &key,
                continuation: graph.nodes[node_index].continuation.as_deref(),
                expected_revision: pins.as_ref().and_then(|pin| pin.0.as_deref()),
                expected_snapshot: pins.as_ref().and_then(|pin| pin.1.as_deref()),
                max_bytes,
            };
            let mut dispatched = false;
            let response = read_page(provider, request, cancelled.as_mut(), &mut dispatched).await;
            if !dispatched {
                graph.pages_read -= 1;
            }
            let page = match response {
                None => return Err(fail(graph, DocsErrorKind::Cancelled, Some(key))),
                Some(Err(error)) => {
                    record_provider_failure(&mut graph, node_index, &error);
                    first_provider_failure.get_or_insert((error.kind, key.clone()));
                    break;
                }
                Some(Ok(page)) => page,
            };
            if !valid_page(&page) {
                return Err(fail(graph, DocsErrorKind::InvalidProviderData, Some(key)));
            }
            if pins.as_ref().is_some_and(|(revision, snapshot)| {
                revision != &page.revision || snapshot != &page.snapshot
            }) {
                return Err(fail(graph, DocsErrorKind::Drift, Some(key)));
            }
            if page
                .continuation
                .as_ref()
                .is_some_and(|token| seen_tokens.contains(token))
            {
                return Err(fail(graph, DocsErrorKind::CursorCycle, Some(key)));
            }
            // Keep the previous graph intact if the *whole* next page cannot fit.
            let mut candidate = graph.clone();
            let node = &mut candidate.nodes[node_index];
            node.coverage.extend(&page.coverage);
            candidate.coverage.extend(&page.coverage);
            if page.continuation.is_some() && page.revision.is_none() && page.snapshot.is_none() {
                let reason = CoverageReason {
                    kind: CoverageReasonKind::UnpinnedPagination,
                    key: Some(key.clone()),
                    detail: None,
                };
                node.coverage.add(reason.clone());
                candidate.coverage.add(reason);
            }
            node.continuation.clone_from(&page.continuation);
            node.status = if page.continuation.is_none() {
                NodeStatus::Available
            } else {
                NodeStatus::Pending
            };
            node.pages.push(DocContent {
                content: page.content,
                media_type: page.media_type,
                revision: page.revision.clone(),
                snapshot: page.snapshot.clone(),
            });
            // Edges count against the same budget before another page is read.
            candidate
                .edges
                .extend(page.edges.iter().cloned().map(|edge| GraphEdge {
                    source: key.clone(),
                    edge,
                    status: EdgeStatus::Pending,
                }));
            encoded_len(&candidate, options.max_bytes.get())
                .map_err(|kind| fail(graph.clone(), kind, Some(key.clone())))?;
            graph = candidate;
            pending_edges.extend(page.edges);
            if pins.is_none() {
                pins = Some((page.revision, page.snapshot));
            }
            if let Some(token) = page.continuation {
                seen_tokens.insert(token);
            } else {
                break;
            }
        }
        // Failed pagination still retains and classifies previously accepted edges.
        pending_edges.sort();
        let before_expansion = graph.clone();
        for edge in pending_edges {
            let boundary = edge_boundary(&root, &key, &edge, options.recursive, depth, depth_limit);
            let status = if options.recursive
                && matches!(boundary, None | Some(CoverageReasonKind::DepthLimit))
                && visited.contains(&edge.target)
            {
                graph.references.push(DocReference {
                    source: key.clone(),
                    target: edge.target.clone(),
                    kind: edge.kind.clone(),
                    provenance: edge.provenance.clone(),
                });
                EdgeStatus::Reference
            } else if let Some(reason) = boundary {
                graph.coverage.add(CoverageReason {
                    kind: reason,
                    key: Some(edge.target.clone()),
                    detail: edge.provenance.clone(),
                });
                EdgeStatus::Unexpanded { reason }
            } else {
                visited.insert(edge.target.clone());
                queue.push_back((edge.target.clone(), depth + 1));
                EdgeStatus::Pending
            };
            // Equal duplicate edges are retained, and assigned once each.
            if let Some(stored) = graph.edges.iter_mut().find(|stored| {
                stored.source == key && stored.edge == edge && stored.status == EdgeStatus::Pending
            }) {
                stored.status = status;
            }
        }
        encoded_len(&graph, options.max_bytes.get())
            .map_err(|kind| fail(before_expansion, kind, Some(key.clone())))?;
    }
    if is_cancelled(cancelled.as_mut()).await {
        return Err(fail(graph, DocsErrorKind::Cancelled, None));
    }
    graph.normalize();
    if let Some((kind, key)) = first_provider_failure {
        return Err(fail(graph, DocsErrorKind::Provider(kind), Some(key)));
    }
    encoded_len(&graph, options.max_bytes.get()).map_err(|kind| fail(graph.clone(), kind, None))?;
    Ok(graph)
}

fn record_provider_failure(graph: &mut GraphResult, index: usize, error: &ProviderError) {
    let reason = CoverageReason {
        kind: error.kind.coverage(),
        key: Some(graph.nodes[index].key.clone()),
        detail: error.detail.clone(),
    };
    graph.nodes[index].status = NodeStatus::Failed { kind: error.kind };
    graph.nodes[index].coverage.add(reason.clone());
    graph.coverage.add(reason);
}

fn valid_page(page: &DocPage) -> bool {
    valid_atom(&page.media_type)
        && [&page.revision, &page.snapshot, &page.continuation]
            .into_iter()
            .flatten()
            .all(|value| valid_atom(value))
        && page.edges.iter().all(|edge| {
            edge.target.validate().is_ok() && edge.kind.as_ref().is_none_or(|kind| valid_atom(kind))
        })
}

fn edge_boundary(
    root: &DocsKey,
    source: &DocsKey,
    edge: &DocEdge,
    recursive: bool,
    depth: u32,
    depth_limit: u32,
) -> Option<CoverageReasonKind> {
    let disposition = match edge.disposition {
        Some(EdgeDisposition::Child) => None,
        Some(EdgeDisposition::Related) if root.includes_related() => None,
        Some(EdgeDisposition::Related) => Some(CoverageReasonKind::CrossOwnerBoundary),
        Some(EdgeDisposition::Reference) => Some(CoverageReasonKind::ReferenceOnly),
        Some(EdgeDisposition::Omitted) => Some(CoverageReasonKind::Omitted),
        Some(EdgeDisposition::Denied) => Some(CoverageReasonKind::Denied),
        Some(EdgeDisposition::NotFound) => Some(CoverageReasonKind::NotFound),
        Some(EdgeDisposition::Unsupported) => Some(CoverageReasonKind::Unsupported),
        Some(EdgeDisposition::Unavailable) => Some(CoverageReasonKind::Unavailable),
        None => Some(CoverageReasonKind::Unadvertised),
    };
    if disposition.is_some() {
        return disposition;
    }
    if matches!(edge.disposition, Some(EdgeDisposition::Child)) {
        if edge.target.owner != source.owner {
            return Some(CoverageReasonKind::CrossOwnerBoundary);
        }
        if edge.target.view != source.view
            && !edge.target.view.starts_with(&format!("{}/", source.view))
        {
            return Some(CoverageReasonKind::OutsideSubtree);
        }
    }
    if !recursive {
        Some(CoverageReasonKind::NonRecursive)
    } else if depth >= depth_limit {
        Some(CoverageReasonKind::DepthLimit)
    } else {
        None
    }
}

async fn is_cancelled(mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>) -> bool {
    poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await
}

async fn read_page(
    provider: &dyn DocumentsProvider,
    request: DocRequest<'_>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    dispatched: &mut bool,
) -> Option<Result<DocPage, ProviderError>> {
    // Lazy construction prevents dispatch when cancellation is already ready.
    let mut pending = None;
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        let response = pending
            .get_or_insert_with(|| {
                *dispatched = true;
                provider.get(request)
            })
            .as_mut()
            .poll(cx);
        match response {
            Poll::Ready(_) if cancelled.as_mut().poll(cx).is_ready() => Poll::Ready(None),
            Poll::Ready(response) => Poll::Ready(Some(response)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

struct BoundedWriter {
    bytes: Vec<u8>,
    count: usize,
    limit: usize,
    exceeded: bool,
    retain: bool,
}

impl BoundedWriter {
    fn new(limit: usize, retain: bool) -> Self {
        Self {
            bytes: Vec::new(),
            count: 0,
            limit,
            exceeded: false,
            retain,
        }
    }

    fn error(&self) -> DocsErrorKind {
        if self.exceeded {
            DocsErrorKind::Budget
        } else {
            DocsErrorKind::Encoding
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.count) {
            self.exceeded = true;
            return Err(io::Error::other("documents byte budget exceeded"));
        }
        self.count += bytes.len();
        if self.retain {
            self.bytes.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encoded_len(value: &impl Serialize, limit: usize) -> Result<usize, DocsErrorKind> {
    let mut writer = BoundedWriter::new(limit, false);
    serde_json::to_writer(&mut writer, value).map_err(|_| writer.error())?;
    Ok(writer.count)
}

fn escape(out: &mut impl Write, value: &str) -> io::Result<()> {
    for character in value.chars() {
        match character {
            '<' => out.write_all(b"&lt;")?,
            '>' => out.write_all(b"&gt;")?,
            '&' => out.write_all(b"&amp;")?,
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '~' => write!(out, "\\{character}")?,
            '\n' => out.write_all(b"\n> ")?,
            c if c.is_control() => write!(out, "\\u{{{:x}}}", u32::from(c))?,
            c => write!(out, "{c}")?,
        }
    }
    Ok(())
}

fn render_key(out: &mut impl Write, key: &DocsKey) -> io::Result<()> {
    match &key.owner {
        DocsOwner::Item {
            tenant,
            workspace,
            item,
        } => {
            out.write_all(b"tenant=")?;
            escape(out, tenant)?;
            out.write_all(b"; workspace=")?;
            escape(out, workspace)?;
            out.write_all(b"; item=")?;
            escape(out, item)?;
        }
        DocsOwner::Other { owner_kind, id } => {
            out.write_all(b"owner kind=")?;
            escape(out, owner_kind)?;
            out.write_all(b"; id=")?;
            escape(out, id)?;
        }
    }
    for (name, value) in [
        ("view", Some(&key.view)),
        ("facet", key.facet.as_ref()),
        ("subresource", key.subresource.as_ref()),
        ("stage", key.stage.as_ref()),
        ("revision selector", key.revision_selector.as_ref()),
    ] {
        if let Some(value) = value {
            write!(out, "; {name}=")?;
            escape(out, value)?;
        }
    }
    Ok(())
}

fn render_coverage(out: &mut impl Write, coverage: &Coverage) -> io::Result<()> {
    match coverage {
        Coverage::Complete => out.write_all(b"Complete within the requested scope.\n"),
        Coverage::Incomplete { reasons } => {
            out.write_all(b"Incomplete:\n")?;
            for reason in reasons {
                write!(out, "- {:?}: ", reason.kind)?;
                if let Some(key) = &reason.key {
                    render_key(out, key)?;
                }
                if let Some(detail) = &reason.detail {
                    out.write_all(b"; ")?;
                    escape(out, detail)?;
                }
                out.write_all(b"\n")?;
            }
            Ok(())
        }
    }
}

fn render_markdown(graph: &GraphResult, out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"# Documentation\n\nRequested root: ")?;
    render_key(out, &graph.root)?;
    out.write_all(b"\n\n")?;
    render_coverage(out, &graph.coverage)?;
    writeln!(out, "\nPage attempts: {}\n", graph.pages_read)?;
    let indices: BTreeMap<_, _> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (&node.key, index + 1))
        .collect();
    for (index, node) in graph.nodes.iter().enumerate() {
        writeln!(out, "## Document {}\n", index + 1)?;
        render_key(out, &node.key)?;
        writeln!(out, "\n\nAcquisition: {:?}\n", node.status)?;
        render_coverage(out, &node.coverage)?;
        for (page_index, page) in node.pages.iter().enumerate() {
            write!(out, "\n### Page {}\n\nMedia: ", page_index + 1)?;
            escape(out, &page.media_type)?;
            for (name, value) in [("revision", &page.revision), ("snapshot", &page.snapshot)] {
                if let Some(value) = value {
                    write!(out, "; {name}=")?;
                    escape(out, value)?;
                }
            }
            out.write_all(b"\n\n> ")?;
            escape(out, &page.content)?;
            out.write_all(b"\n\n")?;
        }
    }
    out.write_all(b"## Edges\n\n")?;
    for edge in &graph.edges {
        if let Some(index) = indices.get(&edge.source) {
            write!(out, "- [Document {index}](#document-{index}) -> ")?;
        } else {
            out.write_all(b"- ")?;
            render_key(out, &edge.source)?;
            out.write_all(b" -> ")?;
        }
        if let Some(index) = indices.get(&edge.edge.target) {
            write!(out, "[Document {index}](#document-{index})")?;
        } else {
            render_key(out, &edge.edge.target)?;
        }
        if let Some(kind) = &edge.edge.kind {
            out.write_all(b"; kind=")?;
            escape(out, kind)?;
        }
        write!(
            out,
            "; {:?}; advertised={:?}",
            edge.status, edge.edge.disposition
        )?;
        if let Some(provenance) = &edge.edge.provenance {
            out.write_all(b"; provenance=")?;
            escape(out, provenance)?;
        }
        out.write_all(b"\n")?;
    }
    out.write_all(b"\n## References\n\n")?;
    for reference in &graph.references {
        out.write_all(b"- ")?;
        render_key(out, &reference.source)?;
        out.write_all(b" -> ")?;
        if let Some(index) = indices.get(&reference.target) {
            write!(out, "[Document {index}](#document-{index})")?;
        } else {
            render_key(out, &reference.target)?;
        }
        if let Some(kind) = &reference.kind {
            out.write_all(b"; kind=")?;
            escape(out, kind)?;
        }
        if let Some(provenance) = &reference.provenance {
            out.write_all(b"; provenance=")?;
            escape(out, provenance)?;
        }
        out.write_all(b"\n")?;
    }
    Ok(())
}
