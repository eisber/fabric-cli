// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline documentation retrieval from a version-pinned immutable Tantivy snapshot.
//! No credentials, network, extraction, global runtime, or writable cache are used.

#![cfg_attr(test, deny(unnameable_test_items))]

pub mod areas;
pub mod browse;
#[cfg(feature = "index-builder")]
pub mod builder;
mod directory;
pub mod rich;
mod rich_search;
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};
use tantivy::{
    Index, IndexReader, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    directory::Directory,
    query::{BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser, TermQuery},
    schema::{IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value},
    tokenizer::{
        Language, LowerCaser, RegexTokenizer, RemoveLongFilter, SimpleTokenizer, Stemmer,
        TextAnalyzer, TokenStream, WhitespaceTokenizer,
    },
};

/// Versioned analyzer/schema contract, independent of the corpus revision.
pub const SCHEMA_ID: &str = "fabric-docs-sections-v2";

/// One checksummed component in the finalized index snapshot.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Component {
    /// Original Tantivy component basename.
    pub name: String,
    /// Original component length, including its footer.
    pub bytes: u64,
    /// Lowercase SHA-256 of the original bytes.
    pub sha256: String,
}

/// An offline-accessible license or attribution notice.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LicenseNotice {
    /// Original source notice filename.
    pub name: String,
    /// Original notice text, not a relicensing assertion.
    pub text: String,
}

/// Immutable corpus provenance and component inventory.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BundleManifest {
    /// Measured optional index additions; older manifests deserialize as baseline.
    #[serde(default)]
    pub features: rich::IndexFeatures,
    /// Exact source metadata vocabulary, unique-article counts.
    #[serde(default)]
    pub filter_catalog: rich::FilterCatalog,
    /// Bundle serialization version.
    pub schema_version: u32,
    /// Exact builder/reader Tantivy version.
    pub tantivy_version: String,
    /// Analyzer and record-layout contract.
    pub schema_id: String,
    /// Public documentation repository URL.
    pub source_repository: String,
    /// Full immutable source commit.
    pub source_commit: String,
    /// Raw Markdown files under the source docs directory.
    pub source_markdown_files: usize,
    /// Raw source Markdown bytes, not index or executable bytes.
    pub source_markdown_bytes: u64,
    /// Indexed section records.
    pub documents: u64,
    /// Selected stored-document codec.
    pub compression: String,
    /// Final searchable segment count.
    pub segments: usize,
    /// Source locations of includes unavailable in this repository snapshot.
    pub unresolved_includes: Vec<String>,
    /// Explicit non-article inputs excluded from standalone results.
    pub skipped_inputs: Vec<String>,
    /// Source license and notice texts, retained offline.
    pub licenses: Vec<LicenseNotice>,
    /// Finalized components verified during executable compilation.
    pub files: Vec<Component>,
}

/// Bounded local query options; validation is repeated at the library boundary.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Boolean/phrase query text; wildcard and regex syntax are not enabled.
    pub text: String,
    /// Result cap, from 1 through 50.
    pub limit: usize,
    /// Exact first source-directory component, such as `onelake`.
    pub workload: Option<String>,
    /// Optional term edit distance, from 0 through 2; incompatible with phrases.
    pub fuzzy: u8,
}

impl SearchOptions {
    /// Validate finite query, expansion, and delivery budgets without opening an index.
    ///
    /// # Errors
    /// Rejects empty, excessive, or unsupported query criteria.
    pub fn validate(&self) -> Result<(), SearchError> {
        validate_options(self)
    }
}

/// One ranked documentation section with source provenance and plain-text context.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    /// Stable source-path/section identifier, not a Tantivy document address.
    pub id: String,
    /// Source article title.
    pub title: String,
    /// Source section heading.
    pub heading: String,
    /// Canonical published article URL.
    pub url: String,
    /// Immutable source URL.
    pub source_url: String,
    /// Repository-relative source path.
    pub source_path: String,
    /// Exact source workload.
    pub workload: String,
    /// Source-local BM25 score, not a probability.
    pub score: f32,
    /// Bounded plain-text source fragment.
    pub snippet: String,
}

/// Actual elapsed time for one query on an already-open reader, in microseconds.
#[derive(Clone, Debug, Serialize)]
pub struct Timings {
    /// Query validation and preparation.
    pub parse_us: u128,
    /// Top-K collection, excluding stored-text retrieval.
    pub search_us: u128,
    /// Stored-field retrieval and snippet generation.
    pub fetch_us: u128,
    /// Complete library call, excluding reader open and CLI rendering.
    pub total_us: u128,
}

/// Successful local retrieval, including an explicit empty result set.
#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    /// Ranked section results.
    pub data: Vec<Hit>,
    /// Full corpus source revision.
    pub source_commit: String,
    /// Searchable section count; not a total Learn coverage claim.
    pub indexed_sections: u64,
    /// Whether this result set is capped by the requested limit.
    pub result_limit: usize,
    /// Actual library-stage timings.
    pub timings: Timings,
}

/// Explicit local query, embedding, or index failures.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Valid local address is absent from the pinned embedded snapshot.
    #[error("Documentation address was not found in the embedded snapshot.")]
    NotFound,
    /// Stored content exceeds a caller-supplied finite retrieval limit.
    #[error("Documentation exceeds the configured local retrieval bound.")]
    ResultLimit,
    /// An explicitly requested feature is not present in this measured index profile.
    #[error("The embedded index does not contain the requested {feature} feature.")]
    UnsupportedFeature {
        /// Missing index capability, not supplied user content.
        feature: &'static str,
    },
    /// A finite dictionary/collection budget was exceeded, not silently truncated.
    #[error(
        "The requested term expansion exceeds the configured budget; use a longer prefix or narrower criteria."
    )]
    ExpansionLimit,
    /// Engine-only builds do not silently substitute a fixture or download data.
    #[error(
        "No documentation index is embedded. Rebuild with FAB_DOCS_INDEX set to a prepared bundle directory."
    )]
    NotEmbedded,
    /// Rejected input is not echoed into diagnostics.
    #[error(
        "Invalid docs query or limit. Use 1-50 results, at most 2048 query bytes and 64 tokens, nesting at most 8, and no wildcard/regex syntax."
    )]
    InvalidQuery,
    /// The bundle does not match the shipped schema or record count.
    #[error("The documentation index has incompatible or inconsistent provenance.")]
    IncompatibleIndex,
    /// The embedded metadata cannot be decoded.
    #[error("The documentation bundle manifest is invalid.")]
    Manifest(#[source] serde_json::Error),
    /// Underlying index errors remain available to library callers.
    #[error("The documentation index could not complete the requested operation.")]
    Index(#[from] tantivy::TantivyError),
    /// An immutable component could not be read.
    #[error("The documentation index component could not be read.")]
    Io(#[from] std::io::Error),
}

/// A caller-owned reader reusable across queries, with no background reload watcher.
pub struct DocsSearch {
    index: Index,
    reader: IndexReader,
    manifest: BundleManifest,
}

impl DocsSearch {
    /// Open the built-in immutable snapshot lazily.
    ///
    /// # Errors
    /// Returns an explicit error for engine-only, malformed, or incompatible builds.
    pub fn embedded() -> Result<Self, SearchError> {
        let manifest = embedded_manifest()?;
        Self::open(directory::EmbeddedDirectory::new(embedded::FILES), manifest)
    }

    /// Open the same embedded bytes after copying them into RAM, for controlled measurements.
    ///
    /// # Errors
    /// Returns embedding, copy, or index failures without falling back to another source.
    pub fn copied_embedded() -> Result<Self, SearchError> {
        let manifest = embedded_manifest()?;
        let directory = tantivy::directory::RamDirectory::create();
        for (name, bytes) in embedded::FILES {
            directory.atomic_write(Path::new(name), bytes)?;
        }
        Self::open(directory, manifest)
    }

    /// Open an explicitly supplied directory and its matching manifest.
    ///
    /// # Errors
    /// Rejects incompatible schemas, malformed provenance, and mismatched record counts.
    pub fn open<D: Directory + 'static>(
        directory: D,
        manifest: BundleManifest,
    ) -> Result<Self, SearchError> {
        if manifest.schema_version != 1
            || manifest.tantivy_version != "0.27.0"
            || manifest.schema_id != SCHEMA_ID
            || manifest.source_commit.len() != 40
            || !manifest
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || manifest.documents == 0
        {
            return Err(SearchError::IncompatibleIndex);
        }
        let index = Index::open(directory)?;
        if index.schema() != index_schema() {
            return Err(SearchError::IncompatibleIndex);
        }
        register_tokenizers(&index)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let result = Self {
            index,
            reader,
            manifest,
        };
        if result.reader.searcher().num_docs() != result.manifest.documents {
            return Err(SearchError::IncompatibleIndex);
        }
        Ok(result)
    }

    /// Return the exact source inventory and offline attribution.
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    /// Execute a bounded Boolean/phrase query with optional exact workload filtering.
    ///
    /// # Errors
    /// Rejects invalid or excessive input and surfaces index/document failures.
    /// This synchronous API does not claim preemptive CPU cancellation.
    pub fn search(&self, options: &SearchOptions) -> Result<SearchResult, SearchError> {
        let started = Instant::now();
        validate_options(options)?;
        let schema = self.index.schema();
        let field = |name| {
            schema
                .get_field(name)
                .map_err(|_| SearchError::IncompatibleIndex)
        };
        let fields = [
            field("title")?,
            field("heading")?,
            field("body")?,
            field("identifiers")?,
        ];
        let mut parser = QueryParser::for_index(&self.index, fields.to_vec());
        parser.set_conjunction_by_default();
        parser.set_field_boost(fields[0], 4.0);
        parser.set_field_boost(fields[1], 2.0);
        parser.set_field_boost(fields[3], 2.0);
        if options.fuzzy > 0 {
            for value in fields {
                parser.set_field_fuzzy(value, false, options.fuzzy, true);
            }
        }
        let mut query = parser
            .parse_query(&options.text)
            .map_err(|_| SearchError::InvalidQuery)?;
        if let Some(workload) = &options.workload {
            let filter = TermQuery::new(
                Term::from_field_text(field("workload")?, workload),
                IndexRecordOption::Basic,
            );
            query = Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (
                    Occur::Must,
                    Box::new(ConstScoreQuery::new(Box::new(filter), 0.0)) as Box<dyn Query>,
                ),
            ]));
        }
        let parse_us = started.elapsed().as_micros();
        let searcher = self.reader.searcher();
        let search_started = Instant::now();
        let matches =
            searcher.search(&query, &TopDocs::with_limit(options.limit).order_by_score())?;
        let search_us = search_started.elapsed().as_micros();
        let fetch_started = Instant::now();
        let mut snippets =
            tantivy::snippet::SnippetGenerator::create(&searcher, query.as_ref(), fields[2])?;
        snippets.set_max_num_chars(320);
        let mut data = Vec::with_capacity(matches.len());
        for (score, address) in matches {
            let document: TantivyDocument = searcher.doc(address)?;
            let text = |name| -> Result<String, SearchError> {
                document
                    .get_first(field(name)?)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .ok_or(SearchError::IncompatibleIndex)
            };
            let snippet = snippets.snippet_from_doc(&document);
            let snippet = if snippet.fragment().is_empty() {
                text("body")?.chars().take(320).collect()
            } else {
                snippet.fragment().chars().take(320).collect()
            };
            data.push(Hit {
                id: text("id")?,
                title: text("title")?,
                heading: text("heading")?,
                url: text("url")?,
                source_url: text("source_url")?,
                source_path: text("source_path")?,
                workload: text("workload")?,
                score,
                snippet,
            });
        }
        Ok(SearchResult {
            data,
            source_commit: self.manifest.source_commit.clone(),
            indexed_sections: self.manifest.documents,
            result_limit: options.limit,
            timings: Timings {
                parse_us,
                search_us,
                fetch_us: fetch_started.elapsed().as_micros(),
                total_us: started.elapsed().as_micros(),
            },
        })
    }
}

fn embedded_manifest() -> Result<BundleManifest, SearchError> {
    serde_json::from_str(embedded::MANIFEST.ok_or(SearchError::NotEmbedded)?)
        .map_err(SearchError::Manifest)
}

/// Read embedded corpus provenance without opening an index, filesystem, or runtime.
///
/// # Errors
/// Reports malformed embedded metadata; engine-only builds return `Ok(None)`.
pub fn embedded_corpus_info() -> Result<Option<BundleManifest>, SearchError> {
    embedded::MANIFEST
        .map(|text| serde_json::from_str(text).map_err(SearchError::Manifest))
        .transpose()
}

fn validate_options(options: &SearchOptions) -> Result<(), SearchError> {
    let text = &options.text;
    let mut depth = 0usize;
    if text.trim().is_empty()
        || text.len() > 2048
        || !(1..=50).contains(&options.limit)
        || options.fuzzy > 2
        || options.fuzzy > 0 && text.contains('"')
        || text.contains(['*', '?'])
        || text.contains(":/")
        || text.split_whitespace().count() > 64
        || options.workload.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 80
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(SearchError::InvalidQuery);
    }
    let mut analyzer = prose_analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut tokens = 0;
    while stream.advance() {
        tokens += 1;
        if tokens > 64 {
            return Err(SearchError::InvalidQuery);
        }
        if options.fuzzy > 0 && (tokens > 8 || stream.token().text.chars().count() < 3) {
            return Err(SearchError::InvalidQuery);
        }
    }
    let mut quote = None;
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' => {
                depth += 1;
                if depth > 8 {
                    return Err(SearchError::InvalidQuery);
                }
            }
            ')' => {
                depth = depth.checked_sub(1).ok_or(SearchError::InvalidQuery)?;
            }
            _ => {}
        }
    }
    if depth != 0 || quote.is_some() || escaped {
        return Err(SearchError::InvalidQuery);
    }
    Ok(())
}

fn prose_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(128))
        .filter(LowerCaser)
        .build()
}

fn register_tokenizers(index: &Index) -> Result<(), SearchError> {
    index
        .tokenizers()
        .register("fabric_prose", prose_analyzer());
    index.tokenizers().register(
        "fabric_stem",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(128))
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    index.tokenizers().register(
        "fabric_completion",
        TextAnalyzer::builder(WhitespaceTokenizer::default())
            .filter(LowerCaser)
            .build(),
    );
    let identifiers = RegexTokenizer::new(r"[A-Za-z0-9_][A-Za-z0-9_./:-]*")
        .map_err(|_| SearchError::IncompatibleIndex)?;
    index.tokenizers().register(
        "fabric_identifiers",
        TextAnalyzer::builder(identifiers)
            .filter(RemoveLongFilter::limit(256))
            .filter(LowerCaser)
            .build(),
    );
    Ok(())
}

fn index_schema() -> Schema {
    let mut schema = Schema::builder();
    for name in ["id", "url", "source_url", "source_path"] {
        schema.add_text_field(name, STORED);
    }
    schema.add_text_field("workload", STRING | STORED);
    for name in ["heading_path", "metadata_json"] {
        schema.add_text_field(name, STORED);
    }
    schema.add_text_field("metadata_terms", STRING);
    for (name, tokenizer, stored, positions) in [
        ("body_stem", "fabric_stem", false, true),
        ("description", "fabric_prose", true, true),
        ("aliases", "fabric_prose", true, true),
        ("heading_context", "fabric_prose", false, true),
        ("completion", "fabric_completion", false, false),
    ] {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(tokenizer)
            .set_index_option(if positions {
                IndexRecordOption::WithFreqsAndPositions
            } else {
                IndexRecordOption::Basic
            });
        let options = TextOptions::default().set_indexing_options(indexing);
        schema.add_text_field(
            name,
            if stored {
                options.set_stored()
            } else {
                options
            },
        );
    }
    for name in ["title", "heading", "body", "identifiers"] {
        let tokenizer = if name == "identifiers" {
            "fabric_identifiers"
        } else {
            "fabric_prose"
        };
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(tokenizer)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let options = TextOptions::default().set_indexing_options(indexing);
        schema.add_text_field(
            name,
            if name == "identifiers" {
                options
            } else {
                options.set_stored()
            },
        );
    }
    schema.build()
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use tantivy::{IndexSettings, directory::RamDirectory, doc};

    fn fixture() -> DocsSearch {
        let directory = RamDirectory::create();
        let index =
            Index::create(directory.clone(), index_schema(), IndexSettings::default()).unwrap();
        register_tokenizers(&index).unwrap();
        let schema = index.schema();
        let field = |name| schema.get_field(name).unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 50_000_000)
            .unwrap();
        for (ordinal, workload, title, body) in [
            (
                0,
                "onelake",
                "OneLake shortcuts",
                "OneLake shortcuts connect existing storage without copying data.",
            ),
            (
                1,
                "security",
                "Workspace identity",
                "Workspace identity supports trusted workspace access to firewall-enabled storage.",
            ),
            (
                2,
                "data-engineering",
                "V-Order configuration",
                "Set spark.sql.parquet.vorder.default to configure V-Order writes.",
            ),
        ] {
            writer.add_document(doc!(
                field("id") => format!("docs/{workload}/sample.md#section-{ordinal}"),
                field("title") => title, field("heading") => title,
                field("body") => body, field("identifiers") => body,
                field("workload") => workload, field("source_path") => format!("docs/{workload}/sample.md"),
                field("url") => "https://learn.microsoft.com/en-us/fabric/",
                field("source_url") => "https://github.com/MicrosoftDocs/fabric-docs/",
            )).unwrap();
        }
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        DocsSearch::open(
            directory,
            BundleManifest {
                features: rich::IndexFeatures::default(),
                filter_catalog: rich::FilterCatalog::new(),
                schema_version: 1,
                tantivy_version: "0.27.0".into(),
                schema_id: SCHEMA_ID.into(),
                source_repository: "https://github.com/MicrosoftDocs/fabric-docs".into(),
                source_commit: "a".repeat(40),
                source_markdown_files: 3,
                source_markdown_bytes: 300,
                documents: 3,
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

    fn options(text: &str) -> SearchOptions {
        SearchOptions {
            text: text.into(),
            limit: 10,
            workload: None,
            fuzzy: 0,
        }
    }

    #[test]
    fn phrases_boolean_filters_fuzzy_and_identifiers_use_real_reader() {
        let reader = fixture();
        assert_eq!(
            reader
                .search(&options("\"OneLake shortcuts\""))
                .unwrap()
                .data[0]
                .workload,
            "onelake"
        );
        let mut scoped = options("(identity OR shortcuts) AND storage");
        scoped.workload = Some("security".into());
        let result = reader.search(&scoped).unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].title, "Workspace identity");
        assert!(!result.data[0].snippet.contains("<b>"));
        let mut fuzzy = options("onlake");
        fuzzy.fuzzy = 1;
        assert_eq!(reader.search(&fuzzy).unwrap().data[0].workload, "onelake");
        assert_eq!(
            reader
                .search(&options("spark.sql.parquet.vorder.default"))
                .unwrap()
                .data[0]
                .workload,
            "data-engineering"
        );
        assert!(
            reader
                .search(&options("zzzzunavailabletopic"))
                .unwrap()
                .data
                .is_empty()
        );
    }

    #[test]
    fn invalid_and_expansive_queries_never_become_empty_success() {
        for text in [
            "",
            "  ",
            "*",
            "body:/.*",
            "(unclosed",
            "a ".repeat(65).as_str(),
            "(((((((((deep)))))))))",
        ] {
            assert!(options(text).validate().is_err(), "{text}");
        }
        let mut invalid = options("onlake");
        invalid.limit = 0;
        assert!(invalid.validate().is_err());
        invalid.limit = 51;
        assert!(invalid.validate().is_err());
        invalid.limit = 10;
        invalid.fuzzy = 3;
        assert!(invalid.validate().is_err());
        invalid.fuzzy = 1;
        invalid.text = "a".into();
        assert!(invalid.validate().is_err());
        invalid.text = "\"OneLake shortcuts\"".into();
        assert!(invalid.validate().is_err());
        assert!(fixture().search(&options("unknownfield:secret")).is_err());
    }

    #[test]
    fn query_nesting_counts_only_unquoted_unescaped_groups() {
        for text in [
            r#""COUNT(""#,
            "'COUNT('",
            r"COUNT\(",
            r"COUNT\)",
            r"COUNT\\\(",
            r#""say \"COUNT(""#,
            r"'say \'COUNT('",
            r#"("COUNT(" OR 'SUM)')"#,
            "((((((((\"COUNT(((((((((\"))))))))",
        ] {
            assert!(options(text).validate().is_ok(), "{text}");
        }
        for text in [
            r"COUNT\\(",
            r#""COUNT("#,
            "'COUNT(",
            "COUNT\\",
            r#"("COUNT(""#,
            r#""COUNT(")"#,
            r#"((((((((("COUNT(")))))))))"#,
        ] {
            assert!(options(text).validate().is_err(), "{text}");
        }
        let reader = fixture();
        for text in [r#""OneLake(""#, "'OneLake)'", r"OneLake\(", r"OneLake\)"] {
            let result = reader.search(&options(text)).unwrap();
            assert_eq!(result.data.len(), 1, "{text}");
            assert_eq!(result.data[0].workload, "onelake", "{text}");
        }
    }

    #[test]
    fn stored_text_preview_handles_title_only_and_unicode_hits() {
        let result = fixture()
            .search(&options("title:\"Workspace identity\""))
            .unwrap();
        assert_eq!(result.data.len(), 1);
        assert!(result.data[0].snippet.contains("firewall"));
        assert!(result.data[0].snippet.chars().count() <= 320);
    }
}
