// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Rich lexical retrieval options and evidence, without semantic/model execution.

use crate::{Hit, SearchError, SearchOptions, Timings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Independently measurable index additions; disabled features never get silent substitutes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexFeatures {
    /// Additional English-stemmed prose postings.
    pub stemmed: bool,
    /// Source metadata, description, aliases, and exact filter postings.
    pub metadata: bool,
    /// Ancestor-heading context and explicit source heading anchors.
    pub hierarchy: bool,
    /// Dedicated bounded edge-ngram suggestion postings.
    pub autocomplete: bool,
}

impl IndexFeatures {
    /// All implemented lexical index additions.
    pub const fn full() -> Self {
        Self {
            stemmed: true,
            metadata: true,
            hierarchy: true,
            autocomplete: true,
        }
    }

    /// Resolve an exact experiment profile, never a partial or guessed spelling.
    pub fn profile(name: &str) -> Option<Self> {
        Some(match name {
            "baseline" => Self::default(),
            "stemmed" => Self {
                stemmed: true,
                ..Self::default()
            },
            "metadata" => Self {
                metadata: true,
                ..Self::default()
            },
            "hierarchy" => Self {
                hierarchy: true,
                ..Self::default()
            },
            "autocomplete" => Self {
                autocomplete: true,
                ..Self::default()
            },
            "full" => Self::full(),
            _ => return None,
        })
    }
}

/// Source-authored/inherited metadata, retaining exact public field names and values.
pub type Metadata = BTreeMap<String, Vec<String>>;

/// Corpus inventory of exact values, counted once per source article.
pub type FilterCatalog = BTreeMap<String, BTreeMap<String, u64>>;

/// An exact metadata criterion. Repeated values for one field are OR; fields are AND.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadataFilter {
    /// Observed source field name; `workload` and `source_path` are derived structural facts.
    pub field: String,
    /// Exact case-sensitive source value.
    pub value: String,
}

/// Bounded lexical additions, separately selectable without changing legacy SearchOptions.
#[derive(Clone, Debug)]
pub struct RichSearchOptions {
    /// Original lexical criteria and result limits.
    pub query: SearchOptions,
    /// Use the additional English-stemmed prose field.
    pub stemming: bool,
    /// Expand a small reviewed Fabric acronym dictionary for plain-text queries only.
    pub synonyms: bool,
    /// Treat the final plain-text token as an explicitly bounded prefix.
    pub prefix: bool,
    /// Exact metadata filters.
    pub filters: Vec<MetadataFilter>,
    /// Requested facet fields, at most eight.
    pub facets: Vec<String>,
    /// Collapse returned sections by exact source article.
    pub group_articles: bool,
    /// Apply deterministic exact-title/heading/identifier reranking to fetched candidates.
    pub rerank: bool,
    /// Finite candidate pool, 1..=500 and no smaller than result limit.
    pub candidates: usize,
    /// Maximum completion/prefix term expansion count, 1..=128.
    pub max_expansions: usize,
    /// Limit matching sections whose metadata is examined for facets, 1..=10,000.
    pub facet_budget: usize,
}

impl RichSearchOptions {
    /// Enable richer lexical behavior with explicit finite budgets.
    pub fn new(query: SearchOptions) -> Self {
        Self {
            query,
            stemming: true,
            synonyms: true,
            prefix: false,
            filters: vec![],
            facets: vec![],
            group_articles: true,
            rerank: true,
            candidates: 200,
            max_expansions: 64,
            facet_budget: 5000,
        }
    }

    /// Validate request-only bounds without opening the embedded index.
    ///
    /// # Errors
    /// Rejects excessive candidates, filter/facet/expansion bounds, and invalid criteria.
    pub fn validate(&self) -> Result<(), SearchError> {
        self.query.validate()?;
        if !(1..=500).contains(&self.candidates)
            || self.candidates < self.query.limit
            || !(1..=128).contains(&self.max_expansions)
            || !(1..=10_000).contains(&self.facet_budget)
            || self.filters.len() > 32
            || self.facets.len() > 8
        {
            return Err(SearchError::InvalidQuery);
        }
        for filter in &self.filters {
            validate_filter(filter)?;
        }
        for field in &self.facets {
            validate_filter(&MetadataFilter {
                field: field.clone(),
                value: "field".into(),
            })?;
        }
        Ok(())
    }
}

/// UTF-8 byte offsets into the returned plain-text snippet, not terminal escape codes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HighlightSpan {
    /// Inclusive UTF-8 byte start.
    pub start: usize,
    /// Exclusive UTF-8 byte end.
    pub end: usize,
}

/// Ranked section plus source metadata and lexical-reranking evidence.
#[derive(Clone, Debug, Serialize)]
pub struct RichHit {
    /// Original source-backed hit.
    #[serde(flatten)]
    pub hit: Hit,
    /// Exact source article identity used for grouping.
    pub article_id: String,
    /// Ancestor headings in source order, including the immediate heading.
    pub heading_path: Vec<String>,
    /// Source metadata, absent values omitted rather than guessed.
    pub metadata: Metadata,
    /// Valid byte ranges in hit.snippet; no HTML/ANSI.
    pub highlights: Vec<HighlightSpan>,
    /// Deterministic additive lexical rerank bonus, not a model score/probability.
    pub rerank_bonus: f32,
}

/// Explicit facet coverage; counts refer to unique matching source articles.
#[derive(Clone, Debug, Serialize)]
pub struct FacetResult {
    /// Field -> exact source value -> matching article count.
    pub counts: FilterCatalog,
    /// Number of matching section records examined.
    pub examined_sections: usize,
    /// Whether all matches were examined, not merely the returned top-K.
    pub complete: bool,
}

/// Rich lexical retrieval with transparent limits and expansion evidence.
#[derive(Clone, Debug, Serialize)]
pub struct RichSearchResult {
    /// Ranked/grouped hits.
    pub data: Vec<RichHit>,
    /// Exact corpus revision.
    pub source_commit: String,
    /// Indexed section count.
    pub indexed_sections: u64,
    /// Result cap.
    pub result_limit: usize,
    /// Candidates collected before reranking/grouping.
    pub candidates_examined: usize,
    /// Whether matching candidates exceed the finite retrieval pool.
    pub candidates_limited: bool,
    /// Concrete applied reviewed synonym/prefix alternatives.
    pub expansions: Vec<String>,
    /// Explicit query-local facet coverage.
    pub facets: FacetResult,
    /// Stage durations, with fetch including reranking/grouping/facets.
    pub timings: Timings,
}

/// A bounded completion from actual indexed section titles/headings.
#[derive(Clone, Debug, Serialize)]
pub struct Suggestion {
    /// Source title or heading, not invented completion text.
    pub text: String,
    /// Corresponding immutable source path.
    pub source_path: String,
    /// Exact source workload.
    pub workload: String,
}

/// Fail consistently rather than silently accepting invalid metadata clauses.
pub(crate) fn validate_filter(filter: &MetadataFilter) -> Result<(), SearchError> {
    if filter.field.is_empty()
        || filter.field.len() > 80
        || !filter
            .field
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || filter.value.is_empty()
        || filter.value.len() > 512
        || filter.value.chars().any(char::is_control)
    {
        return Err(SearchError::InvalidQuery);
    }
    Ok(())
}
