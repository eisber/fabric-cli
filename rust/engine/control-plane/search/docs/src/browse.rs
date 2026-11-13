// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Local documentation addresses and bounded stored-content retrieval.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Write},
};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use tantivy::{
    TantivyDocument, Term,
    collector::{Count, TopDocs},
    query::TermQuery,
    schema::{IndexRecordOption, Schema, Value},
};

use crate::{
    DocsSearch, SearchError,
    rich::{Metadata, MetadataFilter, validate_filter},
};

const MAX_SECTIONS: usize = 4096;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;

/// One stored source section, ordered by its original heading ordinal.
#[derive(Clone, Debug, Serialize)]
pub struct DocSection {
    /// Stable snapshot-local section ID.
    pub id: String,
    /// Display heading from source.
    pub heading: String,
    /// Source heading ancestry.
    pub heading_path: Vec<String>,
    /// Normalized complete stored section content, not a search snippet.
    pub content: String,
}

/// Bounded local article/section retrieval, without network or credentials.
#[derive(Clone, Debug, Serialize)]
pub struct DocArticle {
    /// Exact source article identity.
    pub source_path: String,
    /// Local `docs/...` address accepted by the executable.
    pub address: String,
    /// Local command reference, not a web link.
    pub reference: String,
    /// Source article title.
    pub title: String,
    /// Ordered requested sections.
    pub sections: Vec<DocSection>,
    /// Exact corpus revision, kept in machine-readable provenance.
    pub source_commit: String,
}

/// Validate a canonical local article address without opening an index.
///
/// Addresses retain the exact, case-sensitive repository path under `docs/`,
/// including `.md`, `.yml`, or `.yaml`. An optional `#section-N` selects the
/// snapshot's stored ordinal, not a published heading slug. Source paths are
/// limited to 512 UTF-8 bytes, matching the exact metadata posting contract.
/// The `docs/` catalog root is not an article address.
///
/// # Errors
/// Returns [`SearchError::InvalidQuery`] for noncanonical or unsafe addresses.
pub fn validate_address(address: &str) -> Result<(), SearchError> {
    parse_address(address).map(|_| ())
}

/// Return an exact local source address, preserving an optional section ordinal.
///
/// This performs syntax validation only; [`DocsSearch::get_docs`] verifies that
/// the source exists in the caller's snapshot. No URL/slug conversion is used.
///
/// # Errors
/// Returns [`SearchError::InvalidQuery`] for invalid source addresses.
pub fn address_for_source(source_path: &str) -> Result<String, SearchError> {
    validate_address(source_path)?;
    Ok(source_path.to_owned())
}

/// Return a local `fab-rs get` reference with PowerShell-safe single quoting.
///
/// Embedded apostrophes, including PowerShell's typographic single-quote forms,
/// are doubled inside the single-quoted address. The reference never contains a
/// generated remote documentation URL.
///
/// # Errors
/// Returns [`SearchError::InvalidQuery`] for invalid source addresses.
pub fn reference_for_source(source_path: &str) -> Result<String, SearchError> {
    let address = address_for_source(source_path)?;
    let mut reference = String::from("fab-rs get '");
    for character in address.chars() {
        reference.push(character);
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
        ) {
            reference.push(character);
        }
    }
    reference.push('\'');
    Ok(reference)
}

impl DocsSearch {
    /// Retrieve complete normalized stored sections by exact local source path.
    ///
    /// Content is the snapshot builder's normalized text, not the original
    /// Markdown/YAML bytes. No raw corpus file, network, credential, or search
    /// snippet is used. Section IDs are validated and numerically ordered before
    /// an optional exact `#section-N` selection.
    ///
    /// `max_bytes` must be 1..=8 MiB and bounds the complete article's compact
    /// JSON representation, including provenance and JSON escaping. No partial
    /// article is returned. Independently, at most 4096 matching sections and
    /// 8 MiB of relevant stored fields are retrieved, with each metadata or
    /// hierarchy JSON field bounded to 256 KiB before parsing. Heading ancestry
    /// is empty when the snapshot does not contain the hierarchy feature.
    ///
    /// # Errors
    /// Returns typed invalid-input, unsupported-metadata, not-found, retrieval
    /// limit, corrupt-index, and underlying Tantivy errors. A baseline snapshot
    /// is not scanned to substitute for absent exact metadata postings.
    pub fn get_docs(&self, address: &str, max_bytes: usize) -> Result<DocArticle, SearchError> {
        let (source_path, requested_section) = parse_address(address)?;
        if !(1..=MAX_BYTES).contains(&max_bytes) {
            return Err(SearchError::InvalidQuery);
        }
        if !self.manifest.features.metadata {
            return Err(SearchError::UnsupportedFeature {
                feature: "metadata",
            });
        }
        let schema = self.index.schema();
        let metadata_terms = schema
            .get_field("metadata_terms")
            .map_err(|_| SearchError::IncompatibleIndex)?;
        let query = TermQuery::new(
            Term::from_field_text(metadata_terms, &format!("source_path={source_path}")),
            IndexRecordOption::Basic,
        );
        let searcher = self.reader.searcher();
        let (matches, count) = searcher.search(
            &query,
            &(TopDocs::with_limit(MAX_SECTIONS).order_by_score(), Count),
        )?;
        if count > MAX_SECTIONS {
            return Err(SearchError::ResultLimit);
        }
        if count == 0 {
            return Err(SearchError::NotFound);
        }
        if count != matches.len() {
            return Err(SearchError::IncompatibleIndex);
        }

        let mut budget = ByteBudget::new(MAX_BYTES);
        let mut title: Option<String> = None;
        let mut sections = BTreeMap::new();
        for (_, document_address) in matches {
            let document: TantivyDocument = searcher.doc(document_address)?;
            let stored_path = stored_text(&document, &schema, "source_path", &mut budget)?;
            if stored_path != source_path {
                return Err(SearchError::IncompatibleIndex);
            }
            let id = stored_text(&document, &schema, "id", &mut budget)?;
            let (id_path, ordinal) =
                parse_address(id).map_err(|_| SearchError::IncompatibleIndex)?;
            let ordinal = ordinal.ok_or(SearchError::IncompatibleIndex)?;
            if id_path != source_path || sections.contains_key(&ordinal) {
                return Err(SearchError::IncompatibleIndex);
            }
            let stored_title = stored_text(&document, &schema, "title", &mut budget)?;
            if title
                .as_deref()
                .is_some_and(|expected| expected != stored_title)
            {
                return Err(SearchError::IncompatibleIndex);
            }
            if title.is_none() {
                title = Some(stored_title.to_owned());
            }
            let heading = stored_text(&document, &schema, "heading", &mut budget)?;
            let content = stored_text(&document, &schema, "body", &mut budget)?;
            let workload = stored_text(&document, &schema, "workload", &mut budget)?;
            let metadata_json = stored_text(&document, &schema, "metadata_json", &mut budget)?;
            self.validate_article_metadata(metadata_json, source_path, workload)?;
            let heading_path = if self.manifest.features.hierarchy {
                let json = stored_text(&document, &schema, "heading_path", &mut budget)?;
                check_json_bound(json)?;
                serde_json::from_str::<Vec<String>>(json)
                    .map_err(|_| SearchError::IncompatibleIndex)?
            } else {
                Vec::new()
            };
            sections.insert(
                ordinal,
                DocSection {
                    id: id.to_owned(),
                    heading: heading.to_owned(),
                    heading_path,
                    content: content.to_owned(),
                },
            );
        }
        let sections = if let Some(ordinal) = requested_section {
            let section = sections.remove(&ordinal).ok_or(SearchError::NotFound)?;
            if section.id != address {
                return Err(SearchError::IncompatibleIndex);
            }
            vec![section]
        } else {
            sections.into_values().collect()
        };
        let article = DocArticle {
            source_path: source_path.to_owned(),
            address: address_for_source(address)?,
            reference: reference_for_source(address)?,
            title: title.ok_or(SearchError::IncompatibleIndex)?,
            sections,
            source_commit: self.manifest.source_commit.clone(),
        };
        let mut output = ByteBudget::new(max_bytes);
        serde_json::to_writer(&mut output, &article).map_err(|_| SearchError::ResultLimit)?;
        Ok(article)
    }

    fn validate_article_metadata(
        &self,
        json: &str,
        source_path: &str,
        workload: &str,
    ) -> Result<(), SearchError> {
        check_json_bound(json)?;
        let StrictMetadata(metadata) =
            serde_json::from_str(json).map_err(|_| SearchError::IncompatibleIndex)?;
        for (name, values) in &metadata {
            if values.is_empty() || values.iter().collect::<BTreeSet<_>>().len() != values.len() {
                return Err(SearchError::IncompatibleIndex);
            }
            let catalog = self
                .manifest
                .filter_catalog
                .get(name)
                .ok_or(SearchError::IncompatibleIndex)?;
            for value in values {
                validate_filter(&MetadataFilter {
                    field: name.clone(),
                    value: value.clone(),
                })
                .map_err(|_| SearchError::IncompatibleIndex)?;
                if !catalog.get(value).is_some_and(|count| *count > 0) {
                    return Err(SearchError::IncompatibleIndex);
                }
            }
        }
        for (name, expected) in [("source_path", source_path), ("workload", workload)] {
            if !metadata
                .get(name)
                .is_some_and(|values| values.len() == 1 && values[0] == expected)
            {
                return Err(SearchError::IncompatibleIndex);
            }
        }
        Ok(())
    }
}

fn parse_address(address: &str) -> Result<(&str, Option<u64>), SearchError> {
    if address.len() > 512 + "#section-".len() + 20 {
        return Err(SearchError::InvalidQuery);
    }
    let (source, ordinal) = if let Some((source, fragment)) = address.split_once('#') {
        let ordinal = fragment
            .strip_prefix("section-")
            .filter(|value| {
                !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && (value.len() == 1 || !value.starts_with('0'))
            })
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(SearchError::InvalidQuery)?;
        (source, Some(ordinal))
    } else {
        (address, None)
    };
    if source.len() > 512
        || !source.starts_with("docs/")
        || ![".md", ".yml", ".yaml"]
            .iter()
            .any(|extension| source.ends_with(extension))
        || source
            .chars()
            .any(|character| character.is_control() || "\\:?#%".contains(character))
        || source.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.trim() != part
                || part.ends_with('.')
        })
    {
        return Err(SearchError::InvalidQuery);
    }
    Ok((source, ordinal))
}

fn stored_text<'a>(
    document: &'a TantivyDocument,
    schema: &Schema,
    name: &str,
    budget: &mut ByteBudget,
) -> Result<&'a str, SearchError> {
    let field = schema
        .get_field(name)
        .map_err(|_| SearchError::IncompatibleIndex)?;
    let mut values = document.get_all(field);
    let text = values
        .next()
        .and_then(|value| value.as_str())
        .ok_or(SearchError::IncompatibleIndex)?;
    if values.next().is_some() {
        return Err(SearchError::IncompatibleIndex);
    }
    budget.consume(text.len())?;
    Ok(text)
}

fn check_json_bound(json: &str) -> Result<(), SearchError> {
    if json.len() > MAX_METADATA_BYTES {
        return Err(SearchError::ResultLimit);
    }
    Ok(())
}

struct ByteBudget {
    remaining: usize,
}

impl ByteBudget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn consume(&mut self, bytes: usize) -> Result<(), SearchError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(SearchError::ResultLimit)?;
        Ok(())
    }
}

impl Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.consume(bytes.len())
            .map_err(|_| io::Error::other("local documentation byte bound exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct StrictMetadata(Metadata);

impl<'de> Deserialize<'de> for StrictMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(MetadataVisitor)
    }
}

struct MetadataVisitor;

impl<'de> Visitor<'de> for MetadataVisitor {
    type Value = StrictMetadata;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a source metadata object with unique field names")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        let mut metadata = Metadata::new();
        while let Some((name, values)) = entries.next_entry::<String, Vec<String>>()? {
            if metadata.insert(name, values).is_some() {
                return Err(serde::de::Error::custom("duplicate source metadata field"));
            }
        }
        Ok(StrictMetadata(metadata))
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use crate::{BundleManifest, SCHEMA_ID, rich::IndexFeatures};
    use tantivy::{Index, IndexSettings, directory::RamDirectory};

    const SOURCE: &str = "docs/data-engineering/lakehouse-overview.md";

    struct Record {
        source: String,
        id: String,
        title: String,
        heading: String,
        content: String,
        metadata: Option<String>,
        hierarchy: Option<String>,
        omitted: Option<&'static str>,
        duplicate: Option<&'static str>,
        posting_source: Option<String>,
    }

    impl Record {
        fn new(ordinal: u64, content: &str) -> Self {
            Self::at_source(SOURCE, ordinal, content)
        }

        fn at_source(source: &str, ordinal: u64, content: &str) -> Self {
            Self {
                source: source.into(),
                id: format!("{source}#section-{ordinal}"),
                title: "Lakehouse overview".into(),
                heading: format!("Heading {ordinal}"),
                content: content.into(),
                metadata: Some(
                    serde_json::json!({
                        "source_path": [source],
                        "workload": ["data-engineering"],
                        "ms.topic": ["overview"],
                    })
                    .to_string(),
                ),
                hierarchy: Some(
                    serde_json::json!(["Lakehouse overview", format!("Heading {ordinal}")])
                        .to_string(),
                ),
                omitted: None,
                duplicate: None,
                posting_source: None,
            }
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
        let documents = records.len() as u64;
        let mut catalog = crate::rich::FilterCatalog::new();
        for record in records {
            for (name, value) in [
                ("source_path", record.source.as_str()),
                ("workload", "data-engineering"),
                ("ms.topic", "overview"),
            ] {
                catalog
                    .entry(name.into())
                    .or_default()
                    .insert(value.into(), 1);
            }
            let mut fields = vec![
                ("source_path", Some(record.source.clone())),
                ("id", Some(record.id)),
                ("title", Some(record.title)),
                ("heading", Some(record.heading)),
                ("body", Some(record.content)),
                ("workload", Some("data-engineering".into())),
                ("url", Some("https://example.invalid/remote".into())),
                ("source_url", Some("https://example.invalid/source".into())),
            ];
            if features.metadata {
                fields.push(("metadata_json", record.metadata));
            }
            if features.hierarchy {
                fields.push(("heading_path", record.hierarchy));
            }
            let mut document = TantivyDocument::default();
            for (name, value) in fields {
                if record.omitted == Some(name) {
                    continue;
                }
                if let Some(value) = value {
                    document.add_text(field(name), &value);
                    if record.duplicate == Some(name) {
                        document.add_text(field(name), &value);
                    }
                }
            }
            if features.metadata {
                document.add_text(
                    field("metadata_terms"),
                    format!(
                        "source_path={}",
                        record.posting_source.unwrap_or(record.source)
                    ),
                );
                document.add_text(field("metadata_terms"), "workload=data-engineering");
                document.add_text(field("metadata_terms"), "ms.topic=overview");
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
                source_repository: "https://example.invalid/source".into(),
                source_commit: "a".repeat(40),
                source_markdown_files: 1,
                source_markdown_bytes: 100,
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

    #[test]
    fn exact_article_lookup_returns_full_normalized_content_in_numeric_order() {
        let content = "Full normalized Café lakehouse content.\n".repeat(100);
        let other = "docs/data-engineering/lakehouse-overview-other.md";
        let reader = fixture(
            vec![
                Record::new(10, "Tenth heading."),
                Record::at_source(other, 1, "Must not match a path prefix."),
                Record::new(2, &content),
                Record::new(0, "Preamble."),
            ],
            IndexFeatures::full(),
        );
        let article = reader.get_docs(SOURCE, MAX_BYTES).unwrap();
        assert_eq!(article.source_path, SOURCE);
        assert_eq!(article.address, SOURCE);
        assert_eq!(article.reference, format!("fab-rs get '{SOURCE}'"));
        assert_eq!(article.title, "Lakehouse overview");
        assert_eq!(article.source_commit, "a".repeat(40));
        assert_eq!(
            article
                .sections
                .iter()
                .map(|section| section.id.as_str())
                .collect::<Vec<_>>(),
            [
                format!("{SOURCE}#section-0"),
                format!("{SOURCE}#section-2"),
                format!("{SOURCE}#section-10"),
            ]
        );
        assert_eq!(article.sections[1].content, content);
        assert_eq!(
            article.sections[1].heading_path,
            ["Lakehouse overview", "Heading 2"]
        );
        assert_eq!(article.sections[1].heading, "Heading 2");
        let json = serde_json::to_string(&article).unwrap();
        assert!(!json.contains("https://example.invalid"));
        assert!(!json.contains("\"snippet\""));
    }

    #[test]
    fn section_selection_uses_exact_snapshot_ids_and_does_not_require_contiguous_ordinals() {
        let address = format!("{SOURCE}#section-12");
        let reader = fixture(
            vec![
                Record::new(3, "Earlier section."),
                Record::new(12, "Selected."),
            ],
            IndexFeatures::full(),
        );
        let article = reader.get_docs(&address, MAX_BYTES).unwrap();
        assert_eq!(article.address, address);
        assert_eq!(article.reference, format!("fab-rs get '{address}'"));
        assert_eq!(article.sections.len(), 1);
        assert_eq!(article.sections[0].id, address);
        assert_eq!(article.sections[0].content, "Selected.");
        assert!(matches!(
            reader.get_docs(&format!("{SOURCE}#section-1"), MAX_BYTES),
            Err(SearchError::NotFound)
        ));
        assert!(matches!(
            reader.get_docs("docs/missing.md", MAX_BYTES),
            Err(SearchError::NotFound)
        ));
        assert!(matches!(
            reader.get_docs("docs/Data-engineering/lakehouse-overview.md", MAX_BYTES),
            Err(SearchError::NotFound)
        ));
    }

    #[test]
    fn metadata_only_profile_retrieves_articles_without_guessing_heading_ancestry() {
        let mut record = Record::new(0, "Stored text.");
        record.hierarchy = Some("not JSON; ignored because the feature is absent".into());
        let reader = fixture(
            vec![record],
            IndexFeatures {
                metadata: true,
                ..IndexFeatures::default()
            },
        );
        let article = reader.get_docs(SOURCE, MAX_BYTES).unwrap();
        assert!(article.sections[0].heading_path.is_empty());
    }

    #[test]
    fn baseline_never_scans_stored_paths_or_silently_substitutes_another_query() {
        let reader = fixture(
            vec![Record::new(1, "Stored text.")],
            IndexFeatures::default(),
        );
        assert!(matches!(
            reader.get_docs(SOURCE, MAX_BYTES),
            Err(SearchError::UnsupportedFeature {
                feature: "metadata"
            })
        ));
        for (address, max_bytes) in [
            ("docs/../lakehouse.md", MAX_BYTES),
            (SOURCE, 0),
            (SOURCE, MAX_BYTES + 1),
            ("docs/", MAX_BYTES),
        ] {
            assert!(matches!(
                reader.get_docs(address, max_bytes),
                Err(SearchError::InvalidQuery)
            ));
        }
    }

    #[test]
    fn json_byte_limit_includes_utf8_escaping_identity_and_provenance_without_truncation() {
        let reader = fixture(
            vec![Record::new(1, "Café\n\"quoted\" \\ stored content")],
            IndexFeatures::full(),
        );
        let article = reader.get_docs(SOURCE, MAX_BYTES).unwrap();
        let bytes = serde_json::to_vec(&article).unwrap().len();
        let exact = reader.get_docs(SOURCE, bytes).unwrap();
        assert_eq!(exact.sections[0].content, article.sections[0].content);
        assert!(matches!(
            reader.get_docs(SOURCE, bytes - 1),
            Err(SearchError::ResultLimit)
        ));
        assert!(matches!(
            reader.get_docs(SOURCE, 1),
            Err(SearchError::ResultLimit)
        ));
    }

    #[test]
    fn exact_unicode_yaml_sources_keep_local_references_and_empty_stored_bodies() {
        let source = "docs/工程/owner's guide.yaml";
        let reader = fixture(
            vec![Record::at_source(source, 1, "")],
            IndexFeatures::full(),
        );
        let article = reader.get_docs(source, MAX_BYTES).unwrap();
        assert_eq!(article.source_path, source);
        assert_eq!(
            article.reference,
            "fab-rs get 'docs/工程/owner''s guide.yaml'"
        );
        assert_eq!(article.sections.len(), 1);
        assert_eq!(article.sections[0].content, "");
    }

    #[test]
    fn corrupt_or_duplicate_ids_fail_instead_of_changing_article_order_or_identity() {
        for invalid in [
            "docs/other.md#section-1".into(),
            SOURCE.into(),
            format!("{SOURCE}#section--1"),
            format!("{SOURCE}#section-01"),
            format!("{SOURCE}#section-18446744073709551616"),
            format!("{SOURCE}#section-not-a-number"),
        ] {
            let mut record = Record::new(1, "Stored text.");
            record.id = invalid;
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.get_docs(SOURCE, MAX_BYTES),
                Err(SearchError::IncompatibleIndex)
            ));
        }
        let reader = fixture(
            vec![Record::new(1, "First."), Record::new(1, "Duplicate.")],
            IndexFeatures::full(),
        );
        assert!(matches!(
            reader.get_docs(SOURCE, MAX_BYTES),
            Err(SearchError::IncompatibleIndex)
        ));
    }

    #[test]
    fn mismatched_path_postings_and_inconsistent_titles_are_not_accepted_as_exact_matches() {
        let mut record = Record::at_source("docs/other.md", 1, "Stored text.");
        record.posting_source = Some(SOURCE.into());
        let reader = fixture(vec![record], IndexFeatures::full());
        assert!(matches!(
            reader.get_docs(SOURCE, MAX_BYTES),
            Err(SearchError::IncompatibleIndex)
        ));
        let mut record = Record::new(2, "Second.");
        record.title = "Inconsistent title".into();
        let reader = fixture(
            vec![Record::new(1, "First."), record],
            IndexFeatures::full(),
        );
        assert!(matches!(
            reader.get_docs(SOURCE, MAX_BYTES),
            Err(SearchError::IncompatibleIndex)
        ));
    }

    #[test]
    fn every_required_stored_field_is_singular_and_present_when_features_are_enabled() {
        for field in [
            "source_path",
            "id",
            "title",
            "heading",
            "body",
            "workload",
            "metadata_json",
            "heading_path",
        ] {
            for duplicate in [false, true] {
                let mut record = Record::new(1, "Stored text.");
                if duplicate {
                    record.duplicate = Some(field);
                } else {
                    record.omitted = Some(field);
                }
                let reader = fixture(vec![record], IndexFeatures::full());
                assert!(
                    matches!(
                        reader.get_docs(SOURCE, MAX_BYTES),
                        Err(SearchError::IncompatibleIndex)
                    ),
                    "{field}: duplicate={duplicate}"
                );
            }
        }
    }

    #[test]
    fn metadata_json_is_typed_unique_catalog_checked_and_bound_to_stored_identity() {
        for metadata in [
            "not JSON".into(),
            "[]".into(),
            "{}".into(),
            r#"{"source_path":"not an array"}"#.into(),
            r#"{"source_path":[null]}"#.into(),
            r#"{"source_path":[]}"#.into(),
            format!(r#"{{"source_path":["{SOURCE}"],"source_path":["{SOURCE}"]}}"#),
            format!(
                r#"{{"source_path":["{SOURCE}"],"workload":["data-engineering"],"ms.topic":["overview","overview"]}}"#
            ),
            format!(
                r#"{{"source_path":["{SOURCE}"],"workload":["data-engineering"],"unobserved":["value"]}}"#
            ),
            format!(
                r#"{{"source_path":["{SOURCE}"],"workload":["data-engineering"],"ms.topic":["unknown"]}}"#
            ),
            r#"{"source_path":["docs/other.md"],"workload":["data-engineering"]}"#.into(),
            format!(r#"{{"source_path":["{SOURCE}"],"workload":["unknown"]}}"#),
            format!(r#"{{"source_path":["{SOURCE}"],"workload":["data-engineering"]}} trailing"#),
        ] {
            let mut record = Record::new(1, "Stored text.");
            record.metadata = Some(metadata);
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.get_docs(SOURCE, MAX_BYTES),
                Err(SearchError::IncompatibleIndex)
            ));
        }
    }

    #[test]
    fn hierarchy_json_is_typed_and_not_replaced_with_guessed_ancestry() {
        for hierarchy in ["not JSON", "null", "{}", "[1]", r#"["valid"] trailing"#] {
            let mut record = Record::new(1, "Stored text.");
            record.hierarchy = Some(hierarchy.into());
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.get_docs(SOURCE, MAX_BYTES),
                Err(SearchError::IncompatibleIndex)
            ));
        }
    }

    #[test]
    fn metadata_and_hierarchy_are_byte_bounded_before_json_decoding() {
        for field in ["metadata_json", "heading_path"] {
            let mut record = Record::new(1, "Stored text.");
            let oversized_invalid_json = "[".repeat(MAX_METADATA_BYTES + 1);
            if field == "metadata_json" {
                record.metadata = Some(oversized_invalid_json);
            } else {
                record.hierarchy = Some(oversized_invalid_json);
            }
            let reader = fixture(vec![record], IndexFeatures::full());
            assert!(matches!(
                reader.get_docs(SOURCE, MAX_BYTES),
                Err(SearchError::ResultLimit)
            ));
        }
    }

    #[test]
    fn article_section_cap_is_explicit_even_when_a_single_section_was_requested() {
        let reader = fixture(
            (0..=MAX_SECTIONS)
                .map(|ordinal| Record::new(ordinal as u64, "Text."))
                .collect(),
            IndexFeatures::full(),
        );
        for address in [SOURCE.into(), format!("{SOURCE}#section-0")] {
            assert!(matches!(
                reader.get_docs(&address, MAX_BYTES),
                Err(SearchError::ResultLimit)
            ));
        }
    }

    #[test]
    fn stored_byte_budget_is_explicit_before_returning_a_partial_article() {
        let reader = fixture(
            vec![Record::new(1, &"a".repeat(MAX_BYTES))],
            IndexFeatures::full(),
        );
        assert!(matches!(
            reader.get_docs(SOURCE, MAX_BYTES),
            Err(SearchError::ResultLimit)
        ));
    }
}
