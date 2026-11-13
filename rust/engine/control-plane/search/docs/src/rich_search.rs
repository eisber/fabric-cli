// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded lexical retrieval, not semantic search or calibrated relevance.
//!
//! The deliberately small, query-time-only acronym table is reviewed against:
//! - RLS: <https://learn.microsoft.com/en-us/fabric/security/service-admin-row-level-security>
//! - OLS: <https://learn.microsoft.com/en-us/fabric/security/service-admin-object-level-security>
//! - ADLS: <https://learn.microsoft.com/en-us/azure/storage/blobs/data-lake-storage-introduction>
//! - KQL: <https://learn.microsoft.com/en-us/kusto/query/?view=microsoft-fabric>
//!
//! Expansion replaces a plain input token with alternatives, never an entire
//! Boolean expression. Original-field boosts are title/heading/body/identifier
//! 4/2/1/2; the additional stemmed branch has weight 0.45.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    time::Instant,
};

use tantivy::{
    Index, Searcher, TantivyDocument, Term,
    collector::{Count, TopDocs},
    query::{
        BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, FuzzyTermQuery, Occur, Query,
        QueryParser, TermQuery,
    },
    schema::{Field, IndexRecordOption, Schema, Value},
    snippet::SnippetGenerator,
    tokenizer::{TextAnalyzer, TokenStream},
};

use crate::{
    DocsSearch, Hit, SearchError, Timings,
    rich::{
        FacetResult, FilterCatalog, HighlightSpan, Metadata, MetadataFilter, RichHit,
        RichSearchOptions, RichSearchResult, Suggestion, validate_filter,
    },
};

const MAX_CANDIDATES: usize = 500;
const MAX_SNIPPET_CHARS: usize = 320;
const ACRONYMS: &[(&str, &str)] = &[
    ("rls", "row level security"),
    ("adls", "azure data lake storage"),
    ("kql", "kusto query language"),
    ("ols", "object level security"),
];

type Clauses = Vec<(Occur, Box<dyn Query>)>;
type QueryTerms = BTreeMap<Field, BTreeSet<String>>;

struct PreparedQuery {
    query: Box<dyn Query>,
    terms: QueryTerms,
    expansions: BTreeSet<String>,
}

impl DocsSearch {
    /// Return the exact source-derived metadata vocabulary and article counts.
    ///
    /// An empty catalog is not a guessed vocabulary or a complete metadata claim.
    pub fn filter_catalog(&self) -> &FilterCatalog {
        &self.manifest.filter_catalog
    }

    /// Search with explicitly bounded lexical expansion, reranking and grouping.
    ///
    /// BM25 remains in `hit.score`; the additive rerank bonus is separate.
    /// Facets count unique articles over their own bounded matching-section
    /// collection, independently of the displayed hits and candidate pool.
    ///
    /// # Errors
    /// Rejects invalid criteria, unavailable requested index features, expansion
    /// overflow, corrupt stored records and underlying index failures.
    pub fn search_rich(
        &self,
        options: &RichSearchOptions,
    ) -> Result<RichSearchResult, SearchError> {
        let started = Instant::now();
        self.validate_rich(options)?;
        let schema = self.index.schema();
        let searcher = self.reader.searcher();
        let prepared = self.prepare_query(options, &searcher)?;
        let query = self.apply_filters(
            prepared.query,
            &options.filters,
            options.query.workload.as_deref(),
        )?;
        let parse_us = started.elapsed().as_micros();
        let search_started = Instant::now();
        let (matches, total_matches) = searcher.search(
            &query,
            &(
                TopDocs::with_limit(options.candidates).order_by_score(),
                Count,
            ),
        )?;
        let candidates_examined = matches.len();
        let search_us = search_started.elapsed().as_micros();
        let fetch_started = Instant::now();
        let generators = snippet_generators(&self.index, &schema, &prepared.terms)?;
        let mut data = Vec::with_capacity(candidates_examined);
        for (score, address) in matches {
            let document: TantivyDocument = searcher.doc(address)?;
            let title = stored_text(&document, &schema, "title")?;
            let heading = stored_text(&document, &schema, "heading")?;
            let body = stored_text(&document, &schema, "body")?;
            let (snippet, highlights) = make_snippet(&document, &title, &generators)?;
            let article_id = stored_text(&document, &schema, "source_path")?;
            let rerank_bonus = if options.rerank {
                lexical_bonus(&options.query.text, &title, &heading, &body)
            } else {
                0.0
            };
            data.push(RichHit {
                hit: Hit {
                    id: stored_text(&document, &schema, "id")?,
                    title,
                    heading,
                    url: stored_text(&document, &schema, "url")?,
                    source_url: stored_text(&document, &schema, "source_url")?,
                    source_path: article_id.clone(),
                    workload: stored_text(&document, &schema, "workload")?,
                    score,
                    snippet,
                },
                article_id,
                metadata: self.stored_metadata(&document, &schema)?,
                heading_path: if self.manifest.features.hierarchy {
                    serde_json::from_str(&stored_text(&document, &schema, "heading_path")?)
                        .map_err(|_| SearchError::IncompatibleIndex)?
                } else {
                    Vec::new()
                },
                highlights,
                rerank_bonus,
            });
        }
        data.sort_by(|left, right| {
            (right.hit.score + right.rerank_bonus)
                .total_cmp(&(left.hit.score + left.rerank_bonus))
                .then_with(|| left.hit.id.cmp(&right.hit.id))
        });
        if options.group_articles {
            let mut seen = BTreeSet::new();
            let mut seen_urls = BTreeSet::new();
            data.retain(|hit| {
                let article_url = hit.hit.url.split('#').next().unwrap_or(&hit.hit.url);
                if seen.contains(&hit.article_id) || seen_urls.contains(article_url) {
                    return false;
                }
                seen.insert(hit.article_id.clone());
                seen_urls.insert(article_url.to_owned());
                true
            });
        }
        data.truncate(options.query.limit);
        let facets = self.query_facets(&searcher, query.as_ref(), options, total_matches)?;
        Ok(RichSearchResult {
            data,
            source_commit: self.manifest.source_commit.clone(),
            indexed_sections: self.manifest.documents,
            result_limit: options.query.limit,
            candidates_examined,
            candidates_limited: total_matches > candidates_examined,
            expansions: prepared.expansions.into_iter().collect(),
            facets,
            timings: Timings {
                parse_us,
                search_us,
                fetch_us: fetch_started.elapsed().as_micros(),
                total_us: started.elapsed().as_micros(),
            },
        })
    }

    /// Suggest actual indexed titles/headings containing a word with this prefix.
    ///
    /// The prefix is one alphanumeric token of 2..=20 characters. The dedicated
    /// completion field contains fixed 2..=20 character edge prefixes. At most
    /// 500 matching source sections are examined; the returned, limit-capped
    /// suggestions are not an exhaustive inventory of matching source labels.
    ///
    /// # Errors
    /// Returns typed input, feature, corruption or index errors.
    pub fn suggest(
        &self,
        prefix: &str,
        limit: usize,
        filters: &[MetadataFilter],
    ) -> Result<Vec<Suggestion>, SearchError> {
        if !(1..=50).contains(&limit)
            || !(2..=20).contains(&prefix.chars().count())
            || prefix.len() > 80
            || !prefix.chars().all(char::is_alphanumeric)
        {
            return Err(SearchError::InvalidQuery);
        }
        if !self.manifest.features.autocomplete {
            return Err(missing_feature("autocomplete"));
        }
        self.validate_filters(filters, None)?;
        let schema = self.index.schema();
        let normalized = prefix.to_lowercase();
        if normalized.chars().count() > 20 {
            return Err(SearchError::InvalidQuery);
        }
        let completion = field(&schema, "completion")?;
        let query = self.apply_filters(
            Box::new(TermQuery::new(
                Term::from_field_text(completion, &normalized),
                IndexRecordOption::Basic,
            )),
            filters,
            None,
        )?;
        let searcher = self.reader.searcher();
        let matches = searcher.search(
            &query,
            &TopDocs::with_limit(MAX_CANDIDATES).order_by_score(),
        )?;
        let mut suggestions = BTreeMap::new();
        for (_, address) in matches {
            let document: TantivyDocument = searcher.doc(address)?;
            self.stored_metadata(&document, &schema)?;
            let source_path = stored_text(&document, &schema, "source_path")?;
            let workload = stored_text(&document, &schema, "workload")?;
            for name in ["title", "heading"] {
                let source = stored_text(&document, &schema, name)?;
                if !source
                    .split(|character: char| !character.is_alphanumeric())
                    .any(|word| word.to_lowercase().starts_with(&normalized))
                {
                    continue;
                }
                let text: String = source
                    .chars()
                    .map(|character| {
                        if character.is_control() {
                            ' '
                        } else {
                            character
                        }
                    })
                    .collect();
                if text.trim().is_empty() {
                    continue;
                }
                suggestions
                    .entry((text.clone(), source_path.clone()))
                    .or_insert_with(|| Suggestion {
                        text,
                        source_path: source_path.clone(),
                        workload: workload.clone(),
                    });
            }
        }
        Ok(suggestions.into_values().take(limit).collect())
    }

    fn validate_rich(&self, options: &RichSearchOptions) -> Result<(), SearchError> {
        options.validate()?;
        if options.query.text.chars().any(char::is_control) {
            return Err(SearchError::InvalidQuery);
        }
        if options.stemming && !self.manifest.features.stemmed {
            return Err(missing_feature("stemmed"));
        }
        self.validate_explicit_fields(&options.query.text)?;
        if options.prefix {
            let tokens = plain_terms(&options.query.text).ok_or(SearchError::InvalidQuery)?;
            let last = tokens.last().ok_or(SearchError::InvalidQuery)?;
            if last.chars().count() < 3 || options.query.fuzzy != 0 {
                return Err(SearchError::InvalidQuery);
            }
        }
        self.validate_filters(&options.filters, options.query.workload.as_deref())?;
        if !options.facets.is_empty() && !self.manifest.features.metadata {
            return Err(missing_feature("metadata"));
        }
        for name in &options.facets {
            if !self.manifest.filter_catalog.contains_key(name) {
                return Err(SearchError::InvalidQuery);
            }
        }
        Ok(())
    }

    fn validate_explicit_fields(&self, text: &str) -> Result<(), SearchError> {
        let mut quoted = false;
        let mut escaped = false;
        for (offset, character) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '"' => quoted = !quoted,
                ':' if !quoted => {
                    let preceding = text[..offset].trim_end();
                    let start = preceding
                        .char_indices()
                        .rev()
                        .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_')
                        .map_or(0, |(position, c)| position + c.len_utf8());
                    let name = &preceding[start..];
                    let needed = match name {
                        "body_stem" if !self.manifest.features.stemmed => Some("stemmed"),
                        "description" | "aliases" | "metadata_terms"
                            if !self.manifest.features.metadata =>
                        {
                            Some("metadata")
                        }
                        "heading_context" if !self.manifest.features.hierarchy => Some("hierarchy"),
                        "completion" if !self.manifest.features.autocomplete => {
                            Some("autocomplete")
                        }
                        _ => None,
                    };
                    if let Some(feature) = needed {
                        return Err(missing_feature(feature));
                    }
                    if matches!(name, "metadata_terms" | "completion") {
                        return Err(SearchError::InvalidQuery);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_filters(
        &self,
        filters: &[MetadataFilter],
        workload: Option<&str>,
    ) -> Result<(), SearchError> {
        if filters.len() > 32 {
            return Err(SearchError::InvalidQuery);
        }
        if !filters.is_empty() && !self.manifest.features.metadata {
            return Err(missing_feature("metadata"));
        }
        for filter in filters {
            validate_filter(filter)?;
            if !self
                .manifest
                .filter_catalog
                .get(&filter.field)
                .is_some_and(|values| values.contains_key(&filter.value))
            {
                return Err(SearchError::InvalidQuery);
            }
        }
        if let Some(workload) = workload {
            if self.manifest.features.metadata
                && !self
                    .manifest
                    .filter_catalog
                    .get("workload")
                    .is_some_and(|values| values.contains_key(workload))
            {
                return Err(SearchError::InvalidQuery);
            }
            let workload_filters: Vec<_> = filters
                .iter()
                .filter(|filter| filter.field == "workload")
                .collect();
            if !workload_filters.is_empty()
                && !workload_filters
                    .iter()
                    .any(|filter| filter.value == workload)
            {
                return Err(SearchError::InvalidQuery);
            }
        }
        Ok(())
    }

    fn parser(&self, stemming: bool, fuzzy: u8) -> Result<QueryParser, SearchError> {
        let schema = self.index.schema();
        let mut fields = self.lexical_fields()?;
        if stemming {
            fields.push((field(&schema, "body_stem")?, 0.45));
        }
        let mut parser = QueryParser::for_index(
            &self.index,
            fields.iter().map(|(field, _)| *field).collect(),
        );
        parser.set_conjunction_by_default();
        for (field, boost) in fields {
            parser.set_field_boost(field, boost);
            if fuzzy > 0 {
                parser.set_field_fuzzy(field, false, fuzzy, true);
            }
        }
        Ok(parser)
    }

    fn lexical_fields(&self) -> Result<Vec<(Field, f32)>, SearchError> {
        let schema = self.index.schema();
        let mut fields = vec![
            (field(&schema, "title")?, 4.0),
            (field(&schema, "heading")?, 2.0),
            (field(&schema, "body")?, 1.0),
            (field(&schema, "identifiers")?, 2.0),
        ];
        if self.manifest.features.metadata {
            fields.extend([
                (field(&schema, "description")?, 1.0),
                (field(&schema, "aliases")?, 1.0),
            ]);
        }
        if self.manifest.features.hierarchy {
            fields.push((field(&schema, "heading_context")?, 1.0));
        }
        Ok(fields)
    }

    fn prepare_query(
        &self,
        options: &RichSearchOptions,
        searcher: &Searcher,
    ) -> Result<PreparedQuery, SearchError> {
        let parser = self.parser(options.stemming, 0)?;
        let mut expansions = BTreeSet::new();
        let mut highlight_terms = QueryTerms::new();
        let mut fuzzy_text = options.query.text.clone();
        let text = if let Some(tokens) = plain_terms(&options.query.text) {
            let mut clauses = Vec::new();
            let mut prefix_query = None;
            for (position, token) in tokens.iter().enumerate() {
                if options.prefix && position + 1 == tokens.len() {
                    prefix_query = Some(self.prefix_query(
                        searcher,
                        token,
                        options.max_expansions,
                        &mut expansions,
                    )?);
                    continue;
                }
                if options.synonyms
                    && let Some((_, phrase)) = ACRONYMS
                        .iter()
                        .find(|(acronym, _)| token.eq_ignore_ascii_case(acronym))
                {
                    record_expansion(&mut expansions, phrase, options.max_expansions)?;
                    clauses.push(format!("({token} OR \"{phrase}\")"));
                } else {
                    clauses.push((*token).to_owned());
                }
            }
            fuzzy_text = clauses.join(" ");
            let parsed = if clauses.is_empty() {
                None
            } else {
                Some(
                    parser
                        .parse_query(&fuzzy_text)
                        .map_err(|_| SearchError::InvalidQuery)?,
                )
            };
            match (parsed, prefix_query) {
                (Some(query), Some(prefix)) => Box::new(BooleanQuery::new(vec![
                    (Occur::Must, query),
                    (Occur::Must, prefix),
                ])) as Box<dyn Query>,
                (Some(query), None) | (None, Some(query)) => query,
                (None, None) => return Err(SearchError::InvalidQuery),
            }
        } else {
            // The parser preserves explicit Boolean, phrase and field syntax.
            parser
                .parse_query(&options.query.text)
                .map_err(|_| SearchError::InvalidQuery)?
        };
        self.validate_query_fields(text.as_ref())?;
        collect_terms(text.as_ref(), &mut highlight_terms);
        let query = if options.query.fuzzy > 0 {
            // Preflight every concrete fuzzy alternative; the fuzzy parser is
            // used only after its complete vocabulary fits the finite budget.
            let mut fuzzy_terms = QueryTerms::new();
            text.query_terms(&mut |term, positions_required| {
                if !positions_required && let Some(value) = term.value().as_str() {
                    fuzzy_terms
                        .entry(term.field())
                        .or_default()
                        .insert(value.to_owned());
                }
            });
            self.fuzzy_terms(
                searcher,
                &fuzzy_terms,
                options,
                &mut expansions,
                &mut highlight_terms,
            )?;
            self.parser(options.stemming, options.query.fuzzy)?
                .parse_query(&fuzzy_text)
                .map_err(|_| SearchError::InvalidQuery)?
        } else {
            text
        };
        Ok(PreparedQuery {
            query,
            terms: highlight_terms,
            expansions,
        })
    }

    fn validate_query_fields(&self, query: &dyn Query) -> Result<(), SearchError> {
        let schema = self.index.schema();
        let mut failure = None;
        query.query_terms(&mut |term, _| {
            let name = schema.get_field_name(term.field());
            let needed = match name {
                "body_stem" if !self.manifest.features.stemmed => Some("stemmed"),
                "description" | "aliases" if !self.manifest.features.metadata => Some("metadata"),
                "heading_context" if !self.manifest.features.hierarchy => Some("hierarchy"),
                _ => None,
            };
            if let Some(feature) = needed {
                failure = Some(missing_feature(feature));
            } else if matches!(name, "completion" | "metadata_terms") {
                // Exact metadata must go through catalog validation.
                failure = Some(SearchError::InvalidQuery);
            }
        });
        failure.map_or(Ok(()), Err)
    }

    fn prefix_query(
        &self,
        searcher: &Searcher,
        prefix: &str,
        budget: usize,
        expansions: &mut BTreeSet<String>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let prefix = prefix.to_lowercase();
        let mut upper = prefix.as_bytes().to_vec();
        // A valid UTF-8 token cannot end in 0xff; this is a bytewise FST bound.
        if let Some(last) = upper.last_mut() {
            *last += 1;
        }
        let mut clauses = Vec::new();
        for (field, boost) in self.lexical_fields()? {
            let mut terms = BTreeSet::new();
            for segment in searcher.segment_readers() {
                let inverted = segment.inverted_index(field)?;
                let mut stream = inverted
                    .terms()
                    .range()
                    .ge(prefix.as_bytes())
                    .lt(&upper)
                    .into_stream()?;
                while stream.advance() {
                    let term = std::str::from_utf8(stream.key())
                        .map_err(|_| SearchError::IncompatibleIndex)?;
                    record_expansion(expansions, term, budget)?;
                    terms.insert(term.to_owned());
                }
            }
            for term in terms {
                clauses.push((Occur::Should, boosted_term(field, &term, boost)));
            }
        }
        Ok(disjunction(clauses))
    }

    fn fuzzy_terms(
        &self,
        searcher: &Searcher,
        original: &QueryTerms,
        options: &RichSearchOptions,
        expansions: &mut BTreeSet<String>,
        highlights: &mut QueryTerms,
    ) -> Result<(), SearchError> {
        let fuzzy_fields: BTreeSet<_> =
            self.lexical_fields()?.into_iter().map(|(f, _)| f).collect();
        let stem = field(&self.index.schema(), "body_stem")?;
        for (&field, terms) in original {
            if !fuzzy_fields.contains(&field) && !(options.stemming && field == stem) {
                continue;
            }
            for term in terms {
                let fuzzy = FuzzyTermQuery::new(
                    Term::from_field_text(field, term),
                    options.query.fuzzy,
                    true,
                );
                for segment in searcher.segment_readers() {
                    let inverted = segment.inverted_index(field)?;
                    let mut stream = inverted.terms().search(fuzzy.automaton()?).into_stream()?;
                    while stream.advance() {
                        let alternative = std::str::from_utf8(stream.key())
                            .map_err(|_| SearchError::IncompatibleIndex)?;
                        if alternative != term {
                            record_expansion(expansions, alternative, options.max_expansions)?;
                        }
                        highlights
                            .entry(field)
                            .or_default()
                            .insert(alternative.to_owned());
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_filters(
        &self,
        query: Box<dyn Query>,
        filters: &[MetadataFilter],
        workload: Option<&str>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let schema = self.index.schema();
        let mut grouped: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for filter in filters {
            grouped
                .entry(&filter.field)
                .or_default()
                .insert(&filter.value);
        }
        let mut clauses = vec![(Occur::Must, query)];
        for (name, values) in grouped {
            let target = field(
                &schema,
                if name == "workload" {
                    "workload"
                } else {
                    "metadata_terms"
                },
            )?;
            let alternatives = values
                .into_iter()
                .map(|value| {
                    let term = if name == "workload" {
                        value.to_owned()
                    } else {
                        format!("{name}={value}")
                    };
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(target, &term),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    )
                })
                .collect();
            clauses.push((
                Occur::Must,
                Box::new(ConstScoreQuery::new(disjunction(alternatives), 0.0)),
            ));
        }
        if let Some(workload) = workload {
            clauses.push((
                Occur::Must,
                Box::new(ConstScoreQuery::new(
                    Box::new(TermQuery::new(
                        Term::from_field_text(field(&schema, "workload")?, workload),
                        IndexRecordOption::Basic,
                    )),
                    0.0,
                )),
            ));
        }
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    fn stored_metadata(
        &self,
        document: &TantivyDocument,
        schema: &Schema,
    ) -> Result<Metadata, SearchError> {
        if !self.manifest.features.metadata {
            return Ok(Metadata::new());
        }
        let metadata: Metadata =
            serde_json::from_str(&stored_text(document, schema, "metadata_json")?)
                .map_err(|_| SearchError::IncompatibleIndex)?;
        for (name, values) in &metadata {
            if values.is_empty() || !self.manifest.filter_catalog.contains_key(name) {
                return Err(SearchError::IncompatibleIndex);
            }
            for value in values {
                validate_filter(&MetadataFilter {
                    field: name.clone(),
                    value: value.clone(),
                })
                .map_err(|_| SearchError::IncompatibleIndex)?;
                if !self
                    .manifest
                    .filter_catalog
                    .get(name)
                    .is_some_and(|catalog| catalog.contains_key(value))
                {
                    return Err(SearchError::IncompatibleIndex);
                }
            }
        }
        for name in ["workload", "source_path"] {
            let expected = stored_text(document, schema, name)?;
            if metadata.get(name).map(Vec::as_slice) != Some(std::slice::from_ref(&expected)) {
                return Err(SearchError::IncompatibleIndex);
            }
        }
        Ok(metadata)
    }

    fn query_facets(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        options: &RichSearchOptions,
        total_matches: usize,
    ) -> Result<FacetResult, SearchError> {
        let mut counts: FilterCatalog = options
            .facets
            .iter()
            .map(|name| (name.clone(), BTreeMap::new()))
            .collect();
        if options.facets.is_empty() {
            return Ok(FacetResult {
                counts,
                examined_sections: 0,
                complete: true,
            });
        }
        let schema = self.index.schema();
        let matches = searcher.search(
            query,
            &TopDocs::with_limit(options.facet_budget + 1).order_by_score(),
        )?;
        let examined_sections = matches.len().min(options.facet_budget);
        let mut seen = BTreeSet::new();
        for (_, address) in matches.into_iter().take(options.facet_budget) {
            let document: TantivyDocument = searcher.doc(address)?;
            let article = stored_text(&document, &schema, "source_path")?;
            let metadata = self.stored_metadata(&document, &schema)?;
            for name in &options.facets {
                if let Some(values) = metadata.get(name) {
                    for value in values {
                        if seen.insert((name.clone(), value.clone(), article.clone())) {
                            *counts
                                .entry(name.clone())
                                .or_default()
                                .entry(value.clone())
                                .or_default() += 1;
                        }
                    }
                }
            }
        }
        Ok(FacetResult {
            counts,
            examined_sections,
            complete: total_matches <= options.facet_budget,
        })
    }
}

fn missing_feature(feature: &'static str) -> SearchError {
    SearchError::UnsupportedFeature { feature }
}

fn field(schema: &Schema, name: &str) -> Result<Field, SearchError> {
    schema
        .get_field(name)
        .map_err(|_| SearchError::IncompatibleIndex)
}

fn stored_text(
    document: &TantivyDocument,
    schema: &Schema,
    name: &str,
) -> Result<String, SearchError> {
    let field = field(schema, name)?;
    let mut values = document.get_all(field);
    let result = values
        .next()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(SearchError::IncompatibleIndex)?;
    if values.next().is_some() {
        return Err(SearchError::IncompatibleIndex);
    }
    Ok(result)
}

fn plain_terms(text: &str) -> Option<Vec<&str>> {
    let tokens: Vec<_> = text.split_whitespace().collect();
    if tokens.is_empty()
        || tokens.iter().any(|token| {
            matches!(token.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT")
                || !token.chars().next().is_some_and(char::is_alphanumeric)
                || !token
                    .chars()
                    .all(|c| c.is_alphanumeric() || "._/-".contains(c))
        })
    {
        None
    } else {
        Some(tokens)
    }
}

fn record_expansion(
    terms: &mut BTreeSet<String>,
    term: &str,
    budget: usize,
) -> Result<(), SearchError> {
    if !terms.contains(term) && terms.len() >= budget {
        return Err(SearchError::ExpansionLimit);
    }
    terms.insert(term.to_owned());
    Ok(())
}

fn boosted_term(field: Field, text: &str, boost: f32) -> Box<dyn Query> {
    Box::new(BoostQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(field, text),
            IndexRecordOption::WithFreqs,
        )),
        boost,
    ))
}

fn disjunction(clauses: Clauses) -> Box<dyn Query> {
    if clauses.is_empty() {
        Box::new(EmptyQuery)
    } else {
        Box::new(BooleanQuery::new(clauses))
    }
}

fn collect_terms(query: &dyn Query, result: &mut QueryTerms) {
    query.query_terms(&mut |term, _| {
        if let Some(text) = term.value().as_str() {
            result
                .entry(term.field())
                .or_default()
                .insert(text.to_owned());
        }
    });
}

fn snippet_generators(
    index: &Index,
    schema: &Schema,
    terms: &QueryTerms,
) -> Result<Vec<SnippetGenerator>, SearchError> {
    let mut generators = Vec::new();
    for (query_field, source_field) in [
        ("identifiers", "body"),
        ("body", "body"),
        ("body_stem", "body"),
        ("description", "description"),
        ("aliases", "aliases"),
        ("heading", "heading"),
        ("title", "title"),
    ] {
        let query_field = field(schema, query_field)?;
        if let Some(terms) = terms.get(&query_field) {
            generators.push(SnippetGenerator::new(
                terms.iter().map(|term| (term.clone(), 1.0)).collect(),
                index.tokenizer_for_field(query_field)?,
                field(schema, source_field)?,
                MAX_SNIPPET_CHARS,
            ));
        }
    }
    Ok(generators)
}

fn make_snippet(
    document: &TantivyDocument,
    title: &str,
    generators: &[SnippetGenerator],
) -> Result<(String, Vec<HighlightSpan>), SearchError> {
    for generator in generators {
        let snippet = generator.snippet_from_doc(document);
        if !snippet.highlighted().is_empty() {
            return bounded_fragment(snippet.fragment(), snippet.highlighted());
        }
    }
    bounded_fragment(title, &[])
}

fn bounded_fragment(
    fragment: &str,
    ranges: &[Range<usize>],
) -> Result<(String, Vec<HighlightSpan>), SearchError> {
    let mut text = String::new();
    let mut offsets = BTreeMap::from([(0, 0)]);
    let mut end = 0;
    for (offset, character) in fragment.char_indices().take(MAX_SNIPPET_CHARS) {
        text.push(if character.is_control() {
            ' '
        } else {
            character
        });
        end = offset + character.len_utf8();
        offsets.insert(end, text.len());
    }
    let mut highlights = Vec::new();
    for range in ranges {
        if range.start > range.end
            || range.end > fragment.len()
            || !fragment.is_char_boundary(range.start)
            || !fragment.is_char_boundary(range.end)
        {
            return Err(SearchError::IncompatibleIndex);
        }
        if range.start >= end {
            continue;
        }
        let start = *offsets
            .get(&range.start)
            .ok_or(SearchError::IncompatibleIndex)?;
        let stop = *offsets
            .get(&range.end.min(end))
            .ok_or(SearchError::IncompatibleIndex)?;
        if start < stop {
            highlights.push(HighlightSpan { start, end: stop });
        }
    }
    highlights.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<HighlightSpan> = Vec::new();
    for span in highlights {
        if let Some(last) = merged.last_mut()
            && span.start < last.end
        {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    Ok((text, merged))
}

fn tokens_with(mut analyzer: TextAnalyzer, text: &str) -> Vec<String> {
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    tokens
}

fn prose_tokens(text: &str) -> Vec<String> {
    tokens_with(crate::prose_analyzer(), text)
}

fn lexical_bonus(query: &str, title: &str, heading: &str, body: &str) -> f32 {
    // Advanced syntax is retrieval evidence, not an exact source phrase.
    if plain_terms(query).is_none() {
        return 0.0;
    }
    let query_tokens = prose_tokens(query);
    if query_tokens.is_empty() {
        return 0.0;
    }
    let normalized_query = query_tokens.join(" ");
    let title_tokens = prose_tokens(title);
    let heading_tokens = prose_tokens(heading);
    let mut bonus = 0.0;
    for (tokens, exact, phrase) in [(&title_tokens, 12.0, 5.0), (&heading_tokens, 8.0, 3.0)] {
        if tokens.join(" ") == normalized_query {
            bonus += exact;
        } else if tokens
            .windows(query_tokens.len())
            .any(|window| window == query_tokens.as_slice())
        {
            bonus += phrase;
        }
    }
    if query.contains(['.', '_', '/', '-']) {
        let needle = query.trim().to_lowercase();
        if [title, heading, body].iter().any(|text| {
            text.split(|c: char| !c.is_alphanumeric() && !"._/:-".contains(c))
                .any(|token| token.to_lowercase() == needle)
        }) {
            bonus += 6.0;
        }
    }
    let unique: BTreeSet<_> = query_tokens.iter().collect();
    for (tokens, weight) in [(title_tokens, 0.25), (heading_tokens, 0.15)] {
        let present: BTreeSet<_> = tokens.iter().collect();
        bonus += unique.intersection(&present).count() as f32 * weight;
    }
    let mut analyzer = crate::prose_analyzer();
    let mut stream = analyzer.token_stream(body);
    let mut present = BTreeSet::new();
    while stream.advance() {
        if query_tokens.contains(&stream.token().text) {
            present.insert(stream.token().text.clone());
            if present.len() == unique.len() {
                break;
            }
        }
    }
    bonus += present.len() as f32 * 0.05;
    bonus
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use crate::{BundleManifest, SCHEMA_ID, SearchOptions, rich::IndexFeatures};
    use tantivy::{IndexSettings, directory::RamDirectory};

    struct Record {
        article: String,
        section: String,
        title: String,
        heading: String,
        body: String,
        workload: String,
        metadata: Metadata,
        description: String,
        aliases: String,
        ancestors: Vec<String>,
        metadata_override: Option<String>,
        omit_metadata: bool,
        omit_hierarchy: bool,
        completion_override: Option<String>,
        url_override: Option<String>,
    }

    impl Record {
        fn new(article: &str, section: &str, title: &str, body: &str) -> Self {
            Self {
                article: article.into(),
                section: section.into(),
                title: title.into(),
                heading: title.into(),
                body: body.into(),
                workload: "security".into(),
                metadata: Metadata::new(),
                description: String::new(),
                aliases: String::new(),
                ancestors: vec![title.into()],
                metadata_override: None,
                omit_metadata: false,
                omit_hierarchy: false,
                completion_override: None,
                url_override: None,
            }
        }

        fn metadata(mut self, field: &str, values: &[&str]) -> Self {
            self.metadata.insert(
                field.into(),
                values.iter().map(|value| (*value).to_owned()).collect(),
            );
            self
        }
    }

    fn fixture(records: Vec<Record>, features: IndexFeatures) -> DocsSearch {
        let directory = RamDirectory::create();
        let index = Index::create(
            directory.clone(),
            crate::index_schema(),
            IndexSettings::default(),
        )
        .unwrap();
        crate::register_tokenizers(&index).unwrap();
        let schema = index.schema();
        let field = |name| schema.get_field(name).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 50_000_000)
            .unwrap();
        let mut catalog = FilterCatalog::new();
        let mut catalog_seen = BTreeSet::new();
        let documents = records.len() as u64;
        for mut record in records {
            record
                .metadata
                .insert("source_path".into(), vec![record.article.clone()]);
            record
                .metadata
                .insert("workload".into(), vec![record.workload.clone()]);
            let mut document = TantivyDocument::default();
            for (name, value) in [
                ("id", format!("{}#{}", record.article, record.section)),
                ("title", record.title.clone()),
                ("heading", record.heading.clone()),
                ("body", record.body.clone()),
                (
                    "identifiers",
                    format!("{}\n{}\n{}", record.title, record.heading, record.body),
                ),
                ("source_path", record.article.clone()),
                ("workload", record.workload.clone()),
                (
                    "url",
                    record.url_override.clone().unwrap_or_else(|| {
                        format!(
                            "https://example.invalid/fabric/{}#{}",
                            record.article, record.section
                        )
                    }),
                ),
                (
                    "source_url",
                    format!("https://example.invalid/pinned/{}", record.article),
                ),
            ] {
                document.add_text(field(name), value);
            }
            if features.stemmed {
                document.add_text(field("body_stem"), &record.body);
            }
            if features.metadata {
                for (name, values) in &record.metadata {
                    for value in values {
                        if catalog_seen.insert((
                            name.clone(),
                            value.clone(),
                            record.article.clone(),
                        )) {
                            *catalog
                                .entry(name.clone())
                                .or_default()
                                .entry(value.clone())
                                .or_default() += 1;
                        }
                        document.add_text(field("metadata_terms"), format!("{name}={value}"));
                    }
                }
                if !record.omit_metadata {
                    document.add_text(
                        field("metadata_json"),
                        record
                            .metadata_override
                            .unwrap_or_else(|| serde_json::to_string(&record.metadata).unwrap()),
                    );
                }
                document.add_text(field("description"), record.description);
                document.add_text(field("aliases"), record.aliases);
            }
            if features.hierarchy && !record.omit_hierarchy {
                document.add_text(field("heading_context"), record.ancestors.join("\n"));
                document.add_text(
                    field("heading_path"),
                    serde_json::to_string(&record.ancestors).unwrap(),
                );
            }
            if features.autocomplete {
                // The builder supplies per-word edges; the schema only lowercases
                // whitespace-separated terms, independently of prose token limits.
                let completion = record.completion_override.unwrap_or_else(|| {
                    [&record.title, &record.heading]
                        .into_iter()
                        .flat_map(|text| text.split(|character: char| !character.is_alphanumeric()))
                        .filter(|word| !word.is_empty())
                        .flat_map(|word| {
                            let word = word.to_lowercase();
                            (2..=word.chars().count().min(20))
                                .map(move |end| word.chars().take(end).collect::<String>())
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                document.add_text(field("completion"), completion);
            }
            writer.add_document(document).unwrap();
        }
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        DocsSearch::open(
            directory,
            BundleManifest {
                features,
                filter_catalog: catalog,
                schema_version: 1,
                tantivy_version: "0.27.0".into(),
                schema_id: SCHEMA_ID.into(),
                source_repository: "https://example.invalid/fabric-docs".into(),
                source_commit: "a".repeat(40),
                source_markdown_files: documents as usize,
                source_markdown_bytes: 0,
                documents,
                compression: "lz4".into(),
                segments: 1,
                unresolved_includes: vec![],
                skipped_inputs: vec![],
                licenses: vec![],
                files: vec![],
            },
        )
        .unwrap()
    }

    fn options(text: &str) -> RichSearchOptions {
        RichSearchOptions::new(SearchOptions {
            text: text.into(),
            limit: 10,
            workload: None,
            fuzzy: 0,
        })
    }

    fn filter(field: &str, value: &str) -> MetadataFilter {
        MetadataFilter {
            field: field.into(),
            value: value.into(),
        }
    }

    fn metadata_fixture() -> DocsSearch {
        fixture(
            vec![
                Record::new("docs/a.md", "one", "Storage", "Storage settings.")
                    .metadata("audience", &["admin", "operator"])
                    .metadata("kind", &["reference"]),
                Record::new("docs/a.md", "two", "Storage", "Storage configuration.")
                    .metadata("audience", &["admin", "operator"])
                    .metadata("kind", &["reference"]),
                Record::new("docs/b.md", "one", "Storage", "Storage settings.")
                    .metadata("audience", &["developer"])
                    .metadata("kind", &["reference"]),
                Record::new("docs/c.md", "one", "Storage", "Storage settings.")
                    .metadata("audience", &["admin"])
                    .metadata("kind", &["tutorial"]),
            ],
            IndexFeatures::full(),
        )
    }

    #[test]
    fn stemming_is_additive_exact_original_scores_and_rerank_are_visible() {
        let reader = fixture(
            vec![
                Record::new("docs/exact.md", "one", "Configure", "Configure storage."),
                Record::new("docs/stem.md", "one", "Settings", "Configuring storage."),
            ],
            IndexFeatures::full(),
        );
        let result = reader.search_rich(&options("configure")).unwrap();
        assert_eq!(result.data.len(), 2);
        assert_eq!(result.data[0].article_id, "docs/exact.md");
        assert!(result.data[0].rerank_bonus > result.data[1].rerank_bonus);
        assert!(result.data[0].hit.score > 0.0);
        let stem_hit = result
            .data
            .iter()
            .find(|hit| hit.article_id == "docs/stem.md")
            .unwrap();
        assert!(
            stem_hit
                .highlights
                .iter()
                .any(|span| { &stem_hit.hit.snippet[span.start..span.end] == "Configuring" })
        );
        let mut exact = options("configure");
        exact.stemming = false;
        assert_eq!(reader.search_rich(&exact).unwrap().data.len(), 1);
        let mut no_rerank = options("configure");
        no_rerank.rerank = false;
        assert!(
            reader
                .search_rich(&no_rerank)
                .unwrap()
                .data
                .iter()
                .all(|hit| hit.rerank_bonus == 0.0)
        );
        let legacy = reader.search(&exact.query).unwrap();
        assert_eq!(legacy.data.len(), 1);
        let rich_exact = reader.search_rich(&exact).unwrap();
        // A visible bonus must not be added back into the BM25 field.
        assert!(rich_exact.data[0].rerank_bonus >= 20.0);
    }

    #[test]
    fn reviewed_aliases_preserve_phrase_and_conjunction_without_rewriting_syntax() {
        let reader = fixture(
            vec![
                Record::new(
                    "docs/phrase.md",
                    "one",
                    "Models",
                    "Row-level security protects lakehouse models.",
                ),
                Record::new(
                    "docs/literal.md",
                    "one",
                    "Roles",
                    "RLS protects lakehouse models.",
                ),
                Record::new(
                    "docs/reversed.md",
                    "one",
                    "Models",
                    "Security level row protects lakehouse models.",
                ),
                Record::new(
                    "docs/no-lake.md",
                    "one",
                    "Models",
                    "Row level security protects reports.",
                ),
                Record::new(
                    "docs/adls.md",
                    "one",
                    "Storage",
                    "Azure Data Lake Storage contains files.",
                ),
                Record::new(
                    "docs/kql.md",
                    "one",
                    "Queries",
                    "Kusto Query Language evaluates tables.",
                ),
                Record::new(
                    "docs/ols.md",
                    "one",
                    "Permissions",
                    "Object level security hides columns.",
                ),
            ],
            IndexFeatures::full(),
        );
        let expanded = reader.search_rich(&options("RLS lakehouse")).unwrap();
        assert_eq!(expanded.data.len(), 2);
        assert_eq!(expanded.expansions, ["row level security"]);
        assert!(expanded.data.iter().all(|hit| {
            matches!(
                hit.article_id.as_str(),
                "docs/phrase.md" | "docs/literal.md"
            )
        }));
        for text in ["RLS AND lakehouse", "\"RLS\" lakehouse", "body:RLS"] {
            let result = reader.search_rich(&options(text)).unwrap();
            assert!(result.expansions.is_empty(), "{text}");
            assert_eq!(result.data.len(), 1, "{text}");
            assert_eq!(result.data[0].article_id, "docs/literal.md");
        }
        for (text, article) in [
            ("ADLS", "docs/adls.md"),
            ("KQL", "docs/kql.md"),
            ("OLS", "docs/ols.md"),
        ] {
            assert_eq!(
                reader.search_rich(&options(text)).unwrap().data[0].article_id,
                article
            );
        }
        let mut disabled = options("RLS lakehouse");
        disabled.synonyms = false;
        assert_eq!(reader.search_rich(&disabled).unwrap().data.len(), 1);
        let mut fuzzy = options("RLS lakehouse");
        fuzzy.query.fuzzy = 1;
        let fuzzy = reader.search_rich(&fuzzy).unwrap();
        assert!(fuzzy.expansions.contains(&"row level security".into()));
        assert!(
            fuzzy
                .data
                .iter()
                .any(|hit| hit.article_id == "docs/phrase.md")
        );
        let mut bounded = options("RLS ADLS");
        bounded.max_expansions = 1;
        assert!(matches!(
            reader.search_rich(&bounded),
            Err(SearchError::ExpansionLimit)
        ));
    }

    #[test]
    fn grouping_selects_best_section_and_discloses_candidate_coverage() {
        let reader = fixture(
            vec![
                Record::new("docs/a.md", "z", "Storage", "Storage storage."),
                Record::new("docs/a.md", "a", "Storage", "Storage storage."),
                Record::new("docs/b.md", "a", "Overview", "Storage storage."),
            ],
            IndexFeatures::full(),
        );
        let result = reader.search_rich(&options("storage")).unwrap();
        assert_eq!(result.candidates_examined, 3);
        assert!(!result.candidates_limited);
        assert_eq!(result.data.len(), 2);
        assert_eq!(result.data[0].hit.id, "docs/a.md#a");
        let mut ungrouped = options("storage");
        ungrouped.group_articles = false;
        assert_eq!(reader.search_rich(&ungrouped).unwrap().data.len(), 3);
        let mut bounded = options("storage");
        bounded.query.limit = 1;
        bounded.candidates = 1;
        let bounded = reader.search_rich(&bounded).unwrap();
        assert_eq!(bounded.candidates_examined, 1);
        assert!(bounded.candidates_limited);
    }

    #[test]
    fn grouping_deduplicates_an_identical_article_url_but_not_shared_titles() {
        let mut original = Record::new(
            "docs/a.md",
            "one",
            "Semantic link",
            "Semantic link overview.",
        );
        original.url_override = Some("https://example.invalid/fabric/semantic-link#first".into());
        let mut alias = Record::new(
            "docs/alias.md",
            "two",
            "Semantic link",
            "Semantic link overview.",
        );
        alias.url_override = Some("https://example.invalid/fabric/semantic-link#second".into());
        let distinct = Record::new(
            "docs/b.md",
            "one",
            "Semantic link",
            "Semantic link connectivity.",
        );
        let reader = fixture(vec![original, alias, distinct], IndexFeatures::full());
        let grouped = reader.search_rich(&options("semantic link")).unwrap();
        assert_eq!(grouped.data.len(), 2);
        assert_eq!(
            grouped
                .data
                .iter()
                .filter(|hit| hit
                    .hit
                    .url
                    .starts_with("https://example.invalid/fabric/semantic-link#"))
                .count(),
            1
        );
        let mut sections = options("semantic link");
        sections.group_articles = false;
        assert_eq!(reader.search_rich(&sections).unwrap().data.len(), 3);
    }

    #[test]
    fn filters_are_exact_values_or_fields_and_with_article_facets() {
        let reader = metadata_fixture();
        assert_eq!(reader.filter_catalog()["audience"]["admin"], 2);
        let mut request = options("storage");
        request.filters = vec![
            filter("audience", "admin"),
            filter("audience", "developer"),
            filter("audience", "admin"),
            filter("kind", "reference"),
        ];
        request.facets = vec!["audience".into(), "kind".into(), "source_path".into()];
        request.query.limit = 1;
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.candidates_examined, 3);
        assert_eq!(result.facets.examined_sections, 3);
        assert!(result.facets.complete);
        assert_eq!(result.facets.counts["audience"]["admin"], 1);
        assert_eq!(result.facets.counts["audience"]["operator"], 1);
        assert_eq!(result.facets.counts["audience"]["developer"], 1);
        assert_eq!(result.facets.counts["kind"]["reference"], 2);
        assert_eq!(result.facets.counts["source_path"].len(), 2);
        request.filters = vec![filter("source_path", "docs/c.md")];
        assert_eq!(
            reader.search_rich(&request).unwrap().data[0].article_id,
            "docs/c.md"
        );
        for bad in [
            filter("invented", "admin"),
            filter("audience", "Admin"),
            filter("audience", "unknown"),
            filter("bad field", "admin"),
            filter("audience", ""),
            filter("audience", "admin\n"),
        ] {
            request.filters = vec![bad];
            assert!(matches!(
                reader.search_rich(&request),
                Err(SearchError::InvalidQuery)
            ));
        }
        request.filters.clear();
        request.facets = vec!["unknown".into()];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
    }

    #[test]
    fn workload_criteria_intersect_without_silent_contradictions() {
        let mut other = Record::new("docs/other.md", "one", "Storage", "Storage settings.");
        other.workload = "onelake".into();
        let reader = fixture(
            vec![
                Record::new("docs/a.md", "one", "Storage", "Storage settings."),
                other,
            ],
            IndexFeatures::full(),
        );
        let mut request = options("storage");
        request.query.workload = Some("security".into());
        request.filters = vec![filter("workload", "onelake")];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
        request.filters.push(filter("workload", "security"));
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].hit.workload, "security");
        request.filters.clear();
        request.query.workload = Some("unknown".into());
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
    }

    #[test]
    fn facets_scan_beyond_display_and_candidates_and_expose_partial_coverage() {
        let reader = metadata_fixture();
        let mut request = options("storage");
        request.query.limit = 1;
        request.candidates = 1;
        request.facets = vec!["kind".into()];
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.candidates_examined, 1);
        assert!(result.candidates_limited);
        assert_eq!(result.facets.examined_sections, 4);
        assert!(result.facets.complete);
        assert_eq!(result.facets.counts["kind"]["reference"], 2);
        assert_eq!(result.facets.counts["kind"]["tutorial"], 1);
        request.facet_budget = 1;
        let partial = reader.search_rich(&request).unwrap();
        assert_eq!(partial.facets.examined_sections, 1);
        assert!(!partial.facets.complete);
        assert_eq!(partial.facets.counts["kind"].values().sum::<u64>(), 1);
        request.query.text = "zzzzzzunavailable".into();
        let empty = reader.search_rich(&request).unwrap();
        assert!(empty.data.is_empty());
        assert_eq!(empty.facets.examined_sections, 0);
        assert!(empty.facets.complete);
        assert!(empty.facets.counts["kind"].is_empty());
        let reader = fixture(
            (0..15)
                .map(|ordinal| {
                    Record::new(
                        &format!("docs/{ordinal}.md"),
                        "one",
                        "Storage",
                        "Storage settings.",
                    )
                    .metadata("kind", &["reference"])
                })
                .collect(),
            IndexFeatures::full(),
        );
        request.query.text = "storage".into();
        request.facet_budget = 5000;
        let beyond_ten = reader.search_rich(&request).unwrap();
        assert_eq!(beyond_ten.data.len(), 1);
        assert_eq!(beyond_ten.facets.examined_sections, 15);
        assert_eq!(beyond_ten.facets.counts["kind"]["reference"], 15);
    }

    #[test]
    fn prefix_expansion_is_explicit_bounded_and_only_for_last_plain_token() {
        let reader = fixture(
            vec![
                Record::new("docs/a.md", "one", "Settings", "Storage configure."),
                Record::new("docs/b.md", "one", "Settings", "Storage configuring."),
                Record::new("docs/c.md", "one", "Settings", "Other configuration."),
            ],
            IndexFeatures::full(),
        );
        let mut request = options("storage conf");
        request.prefix = true;
        request.stemming = false;
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 2);
        assert_eq!(
            result.expansions,
            [
                "configuration.",
                "configure.",
                "configuring.",
                "configuration",
                "configure",
                "configuring"
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert!(
            result
                .data
                .iter()
                .all(|hit| hit.highlights.iter().any(|span| {
                    hit.hit.snippet[span.start..span.end]
                        .to_lowercase()
                        .starts_with("config")
                }))
        );
        request.max_expansions = 1;
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::ExpansionLimit)
        ));
        request.max_expansions = 128;
        for invalid in [
            "co",
            "storage AND conf",
            "\"conf\"",
            "body:conf",
            "(storage conf)",
        ] {
            request.query.text = invalid.into();
            assert!(
                matches!(reader.search_rich(&request), Err(SearchError::InvalidQuery)),
                "{invalid}"
            );
        }
        request.query.text = "conf".into();
        request.query.fuzzy = 1;
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
        request.query.fuzzy = 0;
        request.prefix = false;
        assert!(reader.search_rich(&request).unwrap().data.is_empty());
    }

    #[test]
    fn fuzzy_alternatives_and_dotted_identifiers_have_source_backed_highlights() {
        let reader = fixture(
            vec![Record::new(
                "docs/a.md",
                "one",
                "Settings",
                "Lakehouse uses spark.sql.parquet.vorder.default settings.",
            )],
            IndexFeatures::full(),
        );
        let mut request = options("lakehous");
        request.stemming = false;
        request.query.fuzzy = 1;
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 1);
        assert!(result.expansions.contains(&"lakehouse".into()));
        assert!(
            result.data[0]
                .highlights
                .iter()
                .any(|span| { &result.data[0].hit.snippet[span.start..span.end] == "Lakehouse" })
        );
        let result = reader
            .search_rich(&options("spark.sql.parquet.vorder.default"))
            .unwrap();
        assert_eq!(result.data.len(), 1);
        assert!(result.data[0].rerank_bonus >= 6.0);
        assert!(result.data[0].highlights.iter().any(|span| {
            &result.data[0].hit.snippet[span.start..span.end] == "spark.sql.parquet.vorder.default"
        }));
        let bounded_reader = fixture(
            vec![Record::new("docs/a.md", "one", "Words", "storage storags")],
            IndexFeatures::full(),
        );
        let mut bounded = options("storag");
        bounded.stemming = false;
        bounded.query.fuzzy = 1;
        bounded.max_expansions = 1;
        assert!(matches!(
            bounded_reader.search_rich(&bounded),
            Err(SearchError::ExpansionLimit)
        ));
        let mut fuzzy_identifier = options("spark.sql.parquet.vorde.default");
        fuzzy_identifier.query.fuzzy = 1;
        let result = reader.search_rich(&fuzzy_identifier).unwrap();
        assert!(
            result
                .expansions
                .contains(&"spark.sql.parquet.vorder.default".into())
        );
        assert!(result.data[0].highlights.iter().any(|span| {
            &result.data[0].hit.snippet[span.start..span.end] == "spark.sql.parquet.vorder.default"
        }));
    }

    #[test]
    fn hierarchy_metadata_description_and_aliases_remain_actual_source_evidence() {
        let mut record = Record::new("docs/a.md", "one", "Overview", "Unrelated prose.");
        record.heading = "Details".into();
        record.ancestors = vec!["Security".into(), "Roles".into(), "Details".into()];
        record.description = "Configure permissions".into();
        record.aliases = "Access policies".into();
        record = record.metadata("audience", &["admin"]);
        let reader = fixture(vec![record], IndexFeatures::full());
        for text in ["permissions", "policies", "roles"] {
            let result = reader.search_rich(&options(text)).unwrap();
            assert_eq!(result.data.len(), 1);
            assert_eq!(
                result.data[0].heading_path,
                ["Security", "Roles", "Details"]
            );
            assert_eq!(result.data[0].metadata["audience"], ["admin"]);
            assert_eq!(
                result.data[0].hit.url,
                "https://example.invalid/fabric/docs/a.md#one"
            );
            if text == "roles" {
                assert_eq!(result.data[0].hit.snippet, "Overview");
                assert!(result.data[0].highlights.is_empty());
            }
        }
    }

    #[test]
    fn utf8_offsets_are_relative_to_exact_sanitized_fragment_and_overlaps_merge() {
        let body = format!("{}Café 🔐 storage lakehouse.", "préambule ".repeat(50));
        let reader = fixture(
            vec![Record::new("docs/a.md", "one", "Overview", &body)],
            IndexFeatures::full(),
        );
        let result = reader.search_rich(&options("storage")).unwrap();
        let hit = &result.data[0];
        assert!(body.contains(&hit.hit.snippet));
        assert!(body.find(&hit.hit.snippet).unwrap() > 0);
        assert!(hit.hit.snippet.chars().count() <= MAX_SNIPPET_CHARS);
        for span in &hit.highlights {
            assert!(hit.hit.snippet.is_char_boundary(span.start));
            assert!(hit.hit.snippet.is_char_boundary(span.end));
            assert_eq!(&hit.hit.snippet[span.start..span.end], "storage");
        }
        let source = "é\u{0085} lakehouse";
        let start = source.find("lakehouse").unwrap();
        let (text, spans) =
            bounded_fragment(source, &[start..source.len(), start..source.len()]).unwrap();
        assert_eq!(text, "é  lakehouse");
        assert_eq!(spans.len(), 1);
        assert_eq!(&text[spans[0].start..spans[0].end], "lakehouse");
        let (text, spans) =
            bounded_fragment(&"é".repeat(400), std::slice::from_ref(&(630..650))).unwrap();
        assert_eq!(text.chars().count(), 320);
        assert_eq!(
            spans,
            [HighlightSpan {
                start: 630,
                end: 640
            }]
        );
        let (_, merged) = bounded_fragment("abcdef", &[0..3, 2..5, 0..3]).unwrap();
        assert_eq!(merged, [HighlightSpan { start: 0, end: 5 }]);
        assert!(bounded_fragment("é", std::slice::from_ref(&(1..2))).is_err());
    }

    #[test]
    fn suggestions_use_only_dedicated_prefixes_and_actual_titles_with_exact_filters() {
        let mut a = Record::new(
            "docs/a.md",
            "one",
            "Configure storage",
            "Storage body only.",
        );
        a.heading = "Configuring\u{1b} models".into();
        a = a.metadata("audience", &["admin"]);
        let b = Record::new(
            "docs/b.md",
            "one",
            "Overview",
            "Configure body-only content.",
        )
        .metadata("audience", &["developer"]);
        let mut c = Record::new("docs/c.md", "one", "Unrelated", "No completion text.");
        // A corrupt/spurious completion posting cannot invent a source string.
        c.completion_override = Some("conf".into());
        c = c.metadata("audience", &["developer"]);
        let long_title = format!("{} lakehouse", "a".repeat(150));
        let d = Record::new("docs/d.md", "one", &long_title, "Long title token.");
        let reader = fixture(vec![a, b, c, d], IndexFeatures::full());
        let result = reader.suggest("Conf", 50, &[]).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|value| value.source_path == "docs/a.md"));
        assert_eq!(result[0].text, "Configure storage");
        assert_eq!(result[1].text, "Configuring  models");
        assert!(
            result
                .iter()
                .all(|value| !value.text.chars().any(char::is_control))
        );
        assert_eq!(
            reader.suggest("stor", 50, &[]).unwrap()[0].text,
            "Configure storage"
        );
        assert_eq!(
            reader.suggest("mod", 50, &[]).unwrap()[0].text,
            "Configuring  models"
        );
        assert_eq!(reader.suggest("aaa", 50, &[]).unwrap()[0].text, long_title);
        assert!(
            reader
                .suggest("conf", 50, &[filter("audience", "developer")])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reader
                .suggest("conf", 1, &[filter("audience", "admin")])
                .unwrap()
                .len(),
            1
        );
        for invalid in [
            "",
            "c",
            "conf*",
            "two words",
            "a:b",
            "é".repeat(21).as_str(),
        ] {
            assert!(matches!(
                reader.suggest(invalid, 10, &[]),
                Err(SearchError::InvalidQuery)
            ));
        }
        for limit in [0, 51] {
            assert!(matches!(
                reader.suggest("conf", limit, &[]),
                Err(SearchError::InvalidQuery)
            ));
        }
        assert!(matches!(
            reader.suggest("conf", 1, &[filter("unknown", "value")]),
            Err(SearchError::InvalidQuery)
        ));
    }

    #[test]
    fn suggestions_use_a_bounded_source_pool_without_claiming_exhaustiveness() {
        let records = (0..=MAX_CANDIDATES)
            .map(|ordinal| {
                Record::new(
                    &format!("docs/{ordinal}.md"),
                    "one",
                    "Configure storage",
                    "Storage.",
                )
            })
            .collect();
        let reader = fixture(records, IndexFeatures::full());
        let suggestions = reader.suggest("conf", 10, &[]).unwrap();
        assert_eq!(suggestions.len(), 10);
        assert!(suggestions.iter().all(|value| {
            value.text == "Configure storage" && value.source_path.starts_with("docs/")
        }));
        let mut request = options("configure");
        request.candidates = MAX_CANDIDATES;
        request.query.limit = 50;
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 50);
        assert_eq!(result.candidates_examined, MAX_CANDIDATES);
        assert!(result.candidates_limited);
    }

    #[test]
    fn missing_features_are_explicit_and_legacy_remains_available() {
        let reader = fixture(
            vec![Record::new(
                "docs/a.md",
                "one",
                "Storage",
                "Storage settings.",
            )],
            IndexFeatures::default(),
        );
        assert!(matches!(
            reader.search_rich(&options("storage")),
            Err(SearchError::UnsupportedFeature { feature: "stemmed" })
        ));
        assert_eq!(
            reader.search(&options("storage").query).unwrap().data.len(),
            1
        );
        let mut request = options("storage");
        request.stemming = false;
        let result = reader.search_rich(&request).unwrap();
        assert_eq!(result.data.len(), 1);
        assert!(result.data[0].metadata.is_empty());
        assert!(result.data[0].heading_path.is_empty());
        request.filters = vec![filter("workload", "security")];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::UnsupportedFeature {
                feature: "metadata"
            })
        ));
        request.filters.clear();
        request.facets = vec!["workload".into()];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::UnsupportedFeature {
                feature: "metadata"
            })
        ));
        request.facets.clear();
        for text in ["body_stem:storage", "body_stem:[a TO z]"] {
            request.query.text = text.into();
            assert!(matches!(
                reader.search_rich(&request),
                Err(SearchError::UnsupportedFeature { feature: "stemmed" })
            ));
        }
        request.query.text = "heading_context:storage".into();
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::UnsupportedFeature {
                feature: "hierarchy"
            })
        ));
        assert!(matches!(
            reader.suggest("stor", 10, &[]),
            Err(SearchError::UnsupportedFeature {
                feature: "autocomplete"
            })
        ));
    }

    #[test]
    fn malformed_or_missing_enabled_stored_features_never_default_to_success() {
        for malformed in [
            "not JSON",
            "{}",
            r#"{"audience":42}"#,
            r#"{"unknown":["value"]}"#,
        ] {
            let mut record = Record::new("docs/a.md", "one", "Storage", "Storage settings.");
            record.metadata_override = Some(malformed.into());
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.search_rich(&options("storage")),
                Err(SearchError::IncompatibleIndex)
            ));
        }
        for metadata_missing in [true, false] {
            let mut record = Record::new("docs/a.md", "one", "Storage", "Storage settings.");
            record.omit_metadata = metadata_missing;
            record.omit_hierarchy = !metadata_missing;
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.search_rich(&options("storage")),
                Err(SearchError::IncompatibleIndex)
            ));
        }
    }

    #[test]
    fn invalid_inputs_and_resource_limits_are_not_empty_successes() {
        let reader = metadata_fixture();
        for text in [
            "",
            "storage*",
            "unknownfield:storage",
            "metadata_terms:admin",
            "metadata_terms:[a TO z]",
            "ébody_stem:storage",
            "storage\nsettings",
        ] {
            assert!(
                matches!(
                    reader.search_rich(&options(text)),
                    Err(SearchError::InvalidQuery)
                ),
                "{text}"
            );
        }
        for candidates in [0, 501, 5] {
            let mut request = options("storage");
            request.candidates = candidates;
            assert!(matches!(
                reader.search_rich(&request),
                Err(SearchError::InvalidQuery)
            ));
        }
        for expansions in [0, 129] {
            let mut request = options("storage");
            request.max_expansions = expansions;
            assert!(matches!(
                reader.search_rich(&request),
                Err(SearchError::InvalidQuery)
            ));
        }
        for budget in [0, 10_001] {
            let mut request = options("storage");
            request.facet_budget = budget;
            assert!(matches!(
                reader.search_rich(&request),
                Err(SearchError::InvalidQuery)
            ));
        }
        let mut request = options("storage");
        request.filters = vec![filter("audience", "admin"); 33];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
        request.filters.truncate(32);
        assert!(reader.search_rich(&request).is_ok());
        request.query.workload = Some("security".into());
        assert!(reader.search_rich(&request).is_ok());
        request.filters.clear();
        request.query.workload = None;
        request.facets = vec!["workload".into(); 9];
        assert!(matches!(
            reader.search_rich(&request),
            Err(SearchError::InvalidQuery)
        ));
    }
}
