// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::TypedQueryResult;
use std::fmt;
use uuid::Uuid;

/// Explicit DataAgent configuration stage, independent of typed-result opt-in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryStage {
    /// Published configuration; no Sandbox variant is sent.
    Production,
    /// Draft configuration; the exact Sandbox variant is sent consistently.
    Sandbox,
}

/// Explicit opt-in; callers cannot accidentally request legacy CSV as typed data.
#[derive(Clone, Copy, Debug)]
pub enum TypedJson {
    /// Require the versioned typed resource and fail if it is unavailable.
    Required,
}

/// Public `language` is a template selector, never an engine dialect override.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryMode {
    /// Omit `language`; the selected datasource owns the engine language.
    Native,
    /// Send `jmespath` only when discovery positively advertises it.
    Template,
}

/// Exact DataAgent/datasource target; workspace/item paths are not accepted.
#[derive(Clone)]
pub struct DataAgentTarget {
    pub(crate) artifact_id: String,
    pub(crate) stage: QueryStage,
}

impl DataAgentTarget {
    /// Validate two nonempty canonical GUIDs and preserve the explicit stage.
    pub fn new(
        data_agent_id: &str,
        datasource_id: &str,
        stage: QueryStage,
    ) -> Result<Self, QueryError> {
        for id in [data_agent_id, datasource_id] {
            if id.len() != 36 || Uuid::parse_str(id).map_or(true, |id| id.is_nil()) {
                return Err(QueryError::before(QueryErrorKind::InvalidInput));
            }
        }
        Ok(Self {
            artifact_id: format!("{data_agent_id}/{datasource_id}"),
            stage,
        })
    }

    /// Exact provider-owned public identifier, never a workspace/item pair.
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    /// Caller-selected stage, never inferred from typed-result opt-in.
    pub const fn stage(&self) -> QueryStage {
        self.stage
    }
}

impl fmt::Debug for DataAgentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataAgentTarget(<redacted>)")
    }
}

/// Query arguments; no arbitrary unsupported parameters can be added.
#[derive(Clone)]
pub struct QueryRequest {
    pub(crate) target: DataAgentTarget,
    pub(crate) query: String,
    pub(crate) mode: QueryMode,
    pub(crate) filename: Option<String>,
    pub(crate) max_rows: u32,
}

impl QueryRequest {
    /// Require typed JSON explicitly; the service's default row cap is 1,000.
    pub fn new(
        target: DataAgentTarget,
        query: String,
        _typed_json: TypedJson,
    ) -> Result<Self, QueryError> {
        if query.trim().is_empty() || query.len() > 1024 * 1024 {
            return Err(QueryError::before(QueryErrorKind::InvalidInput));
        }
        Ok(Self {
            target,
            query,
            mode: QueryMode::Native,
            filename: None,
            max_rows: 1000,
        })
    }

    /// Select native execution or advertised curated templates, not SQL/DAX/KQL.
    pub fn with_mode(mut self, mode: QueryMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set a safe filename base, at most 50 ASCII letters, digits, `_` or `-`.
    ///
    /// This does not write a file or select a result resource.
    pub fn with_filename(mut self, filename: &str) -> Result<Self, QueryError> {
        if filename.is_empty()
            || filename.len() > 50
            || !filename
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(QueryError::before(QueryErrorKind::InvalidInput));
        }
        self.filename = Some(filename.into());
        Ok(self)
    }

    /// Request 1..=10,000 rows. This output cap is not a compute/cost bound.
    pub fn with_max_rows(mut self, max_rows: u32) -> Result<Self, QueryError> {
        if !(1..=10_000).contains(&max_rows) {
            return Err(QueryError::before(QueryErrorKind::InvalidInput));
        }
        self.max_rows = max_rows;
        Ok(self)
    }
}

impl fmt::Debug for QueryRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("QueryRequest(<redacted>)")
    }
}

/// Finite application bounds, in addition to the one caller-supplied deadline.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Decoded HTTP response bound, including MCP content (default 32 MiB).
    pub response_bytes: usize,
    /// Combined resource text and metadata bound (default 16 MiB).
    pub resource_bytes: usize,
    /// Maximum content blocks (default 128).
    pub content_blocks: usize,
    /// Maximum typed resultsets; every resultset is retained (default 32).
    pub resultsets: usize,
    /// Maximum columns per resultset (default 1,024).
    pub columns: usize,
    /// Maximum rows across all resultsets (default 10,000).
    pub rows: usize,
    /// Maximum cells across all resultsets (default 1,000,000).
    pub cells: usize,
    /// Maximum discovery pages (default 8).
    pub discovery_pages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            response_bytes: 32 * 1024 * 1024,
            resource_bytes: 16 * 1024 * 1024,
            content_blocks: 128,
            resultsets: 32,
            columns: 1024,
            rows: 10_000,
            cells: 1_000_000,
            discovery_pages: 8,
        }
    }
}

impl Limits {
    pub(crate) fn validate(self) -> Result<Self, QueryError> {
        let defaults = Self::default();
        let supplied = [
            self.response_bytes,
            self.resource_bytes,
            self.content_blocks,
            self.resultsets,
            self.columns,
            self.rows,
            self.cells,
            self.discovery_pages,
        ];
        let maxima = [
            defaults.response_bytes,
            defaults.resource_bytes,
            defaults.content_blocks,
            defaults.resultsets,
            defaults.columns,
            defaults.rows,
            defaults.cells,
            defaults.discovery_pages,
        ];
        if supplied
            .into_iter()
            .zip(maxima)
            .any(|(n, cap)| n == 0 || n > cap)
        {
            return Err(QueryError::before(QueryErrorKind::InvalidInput));
        }
        Ok(self)
    }
}

/// Exact bounded-rowset completeness, not a promise about total engine compute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completeness {
    /// Every returned resource explicitly reports `truncated: false`.
    Complete,
    /// At least one resource explicitly reports truncation; not completed data.
    Truncated,
}

/// Typed result observation; incomplete data cannot masquerade as completion.
pub enum QueryOutcome {
    /// Valid typed resources, all explicitly untruncated.
    Complete(TypedQueryResult),
    /// Valid but truncated resources; the caller must explicitly handle this.
    Incomplete(TypedQueryResult),
}

impl fmt::Debug for QueryOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete(result) => f.debug_tuple("Complete").field(result).finish(),
            Self::Incomplete(result) => f.debug_tuple("Incomplete").field(result).finish(),
        }
    }
}

/// Sanitized actionable failure category; raw service/query/identity text is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryErrorKind {
    /// Invalid target, argument, binding or bound, rejected before dispatch.
    InvalidInput,
    /// Credential acquisition failed without sign-in or provider fallback.
    Authentication,
    /// HTTP failed with its actual status, including 401/403/429; never retried.
    Http(u16),
    /// The transport or decoded body stream failed.
    Transport,
    /// One absolute deadline expired; remote execution may still be running.
    Deadline,
    /// Local observation was canceled; remote cancellation is not claimed.
    Canceled,
    /// A finite byte, discovery or shape budget was exceeded.
    Limit,
    /// Unsupported negotiation, lifecycle, content or malformed protocol.
    Protocol,
    /// The server returned a JSON-RPC error, without exposing its message/data.
    Rpc(i64),
    /// Tool discovery did not advertise the required supported call shape.
    UnsupportedTool,
    /// MCP `isError` is true; no content is accepted as completed data.
    ToolError,
    /// Require DataAgent routing and typed JSON; CSV/Markdown is not a fallback.
    MissingTypedJson,
    /// Version, schema, value, ID, count or metadata validation failed.
    InvalidTypedResource,
    /// A resource/URI/result ID appeared more than once.
    DuplicateResource,
    /// The representation's logical type or encoding is unsupported.
    UnsupportedRepresentation,
}

/// Failure evidence without provider text or identity; never authorizes replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryError {
    /// Sanitized cause; do not infer completion from an HTTP 2xx alone.
    pub kind: QueryErrorKind,
    /// The query or catalog tool call reached HTTP; its remote outcome may be unknown.
    pub query_dispatched: bool,
}

impl QueryError {
    pub(crate) const fn before(kind: QueryErrorKind) -> Self {
        Self {
            kind,
            query_dispatched: false,
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self.kind {
            QueryErrorKind::InvalidInput => {
                "invalid AIHub binding, DataAgent target, arguments or limits"
            }
            QueryErrorKind::Authentication => {
                "injected AIHub credential failed; verify the supported caller auth binding"
            }
            QueryErrorKind::Http(_) => {
                "AIHub HTTP request failed; inspect the status, do not replay automatically"
            }
            QueryErrorKind::Transport => {
                "AIHub HTTP transport or body stream failed; remote outcome may be unknown"
            }
            QueryErrorKind::Deadline => {
                "AIHub absolute deadline expired; local timeout is not remote cancellation"
            }
            QueryErrorKind::Canceled => {
                "AIHub local observation canceled; remote execution may continue"
            }
            QueryErrorKind::Limit => "AIHub byte, resource or shape budget exceeded",
            QueryErrorKind::Protocol => {
                "unsupported or malformed AIHub MCP protocol/lifecycle response"
            }
            QueryErrorKind::Rpc(_) => "AIHub returned a JSON-RPC error; no completed query data",
            QueryErrorKind::UnsupportedTool => {
                "AIHub discovery does not advertise the required execute_query arguments or template selector"
            }
            QueryErrorKind::ToolError => {
                "AIHub execute_query returned isError; no completed query data"
            }
            QueryErrorKind::MissingTypedJson => {
                "typed application/json result missing; verify DataAgent routing, eligibility and TypedQueryResults opt-in; CSV is not supported"
            }
            QueryErrorKind::InvalidTypedResource => {
                "AIHub typed resource schema, metadata, value or counts are inconsistent"
            }
            QueryErrorKind::DuplicateResource => {
                "AIHub returned duplicate resource URI or query result ID"
            }
            QueryErrorKind::UnsupportedRepresentation => {
                "AIHub typed resource logical type or encoding is unsupported"
            }
        };
        f.write_str(text)
    }
}

impl std::error::Error for QueryError {}
