// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-pinned, partial documentation areas, independent of article classification.
//!
//! Membership is an exact reviewed path set, not an `item_type` metadata assignment.
//! Areas may overlap other resource areas; mentions, prefixes, and unreviewed links
//! never extend membership. Search and completion apply membership before retrieval.
//!
//! # Source and coverage
//!
//! The schema-1 registry in `data/areas.json` records MicrosoftDocs/fabric-docs
//! commit `3ae7afc73169ac57b874d5aee09217bb35a4f8fc`, the repository provenance,
//! membership policy, and evidence for each of the 13 exact Lakehouse paths.
//! Eleven are the `Store and manage` / `Lakehouse` subtree in
//! `docs/data-engineering/toc.yml`, lines 52-77. Two are individually reviewed
//! links in `docs/data-engineering/lakehouse-overview.md`, lines 123-127:
//! `load-data-lakehouse.md` and `lakehouse-sql-analytics-endpoint.md`.
//! Other links from the overview, including notebooks, are not followed.
//!
//! # Local use
//!
//! Call [`DocsSearch::item_area`] with `Lakehouse` or its ASCII case-insensitive
//! alias `lakehouse` to enumerate
//! [`DocArea::articles`]. Use each indexed article's [`AreaArticle::reference`]
//! for local GET retrieval; an unindexed member is not a successful GET claim.
//! Source evidence paths are provenance locators, not generated GET references
//! for TOCs. [`DocsSearch::search_item_docs`] and
//! [`DocsSearch::suggest_item_docs`] validate original filters, then intersect
//! source-path alternatives with indexed members before retrieval. Query text
//! and workload remain additional restrictions. At most 32 combined filters are
//! accepted; an empty path intersection is an error, never a broader search.
//!
//! Different corpus provenance or missing metadata support is explicitly
//! unsupported. Unknown areas and areas without indexed members are not found.
//! No fallback, index/schema migration, builder change, network, or runtime is used.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BundleManifest, DocsSearch, SearchError,
    browse::{address_for_source, reference_for_source},
    rich::{
        FilterCatalog, MetadataFilter, RichSearchOptions, RichSearchResult, Suggestion,
        validate_filter,
    },
};

const REGISTRY_JSON: &str = include_str!("../data/areas.json");
const SOURCE_REPOSITORY: &str = "https://github.com/MicrosoftDocs/fabric-docs";
const SOURCE_COMMIT: &str = "3ae7afc73169ac57b874d5aee09217bb35a4f8fc";
const ROOT_ARTICLE: &str = "docs/data-engineering/lakehouse-overview.md";

/// A reviewed documentation area, not exclusive ownership or article classification.
#[derive(Clone, Debug, Serialize)]
pub struct DocArea {
    /// Canonical registry identifier; lookup accepts ASCII case variants.
    pub id: String,
    /// Human-readable area name.
    pub label: String,
    /// Related Fabric item type, not an assignment to the member articles.
    pub item_type: String,
    /// Exact source revision supporting all membership evidence.
    pub source_commit: String,
    /// Local address of the source root article; availability is in `articles`.
    pub root_address: String,
    /// Reviewed scope and explicit non-exclusive membership explanation.
    pub description: String,
    /// Coverage claim: `partial`, never complete workload or product coverage.
    pub coverage: String,
    /// All reviewed articles, including explicitly unindexed members.
    pub articles: Vec<AreaArticle>,
    /// Source-pinned, machine-readable membership evidence.
    pub rules: Vec<AreaRule>,
}

/// One exact reviewed member and its snapshot-local retrieval reference.
#[derive(Clone, Debug, Serialize)]
pub struct AreaArticle {
    /// Exact repository-relative source article path.
    pub source_path: String,
    /// Local documentation address, not a remote URL.
    pub address: String,
    /// Local GET command reference, not a remote URL.
    pub reference: String,
    /// Whether this snapshot's metadata catalog contains the article.
    pub indexed: bool,
}

/// A reviewed rule enumerating exact paths; it is not evaluated recursively.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AreaRule {
    /// Reviewed evidence kind, such as `reviewed_toc_subtree`.
    pub kind: String,
    /// Source location supporting this fixed membership set.
    pub evidence: AreaEvidence,
    /// Explicit source articles admitted by this rule.
    pub source_paths: Vec<String>,
}

/// A source locator, not a claim that the evidence file is a retrievable article.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AreaEvidence {
    /// Repository-relative source path; TOCs remain machine metadata, not URLs.
    pub source_path: String,
    /// Inclusive one-based source line range at the registry's exact commit.
    pub lines: [usize; 2],
    /// Reviewed source navigation ancestry or related-content heading.
    pub node_names: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaRegistry {
    copyright: String,
    schema_version: u32,
    source_repository: String,
    source_commit: String,
    policy: String,
    areas: Vec<RegisteredArea>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredArea {
    id: String,
    name: String,
    item_type: String,
    root_article: String,
    description: String,
    coverage: String,
    rules: Vec<AreaRule>,
}

impl DocsSearch {
    /// Describe a source-pinned documentation area with explicit partial coverage.
    ///
    /// Article membership never modifies the index's `item_type` classification.
    /// The known `Lakehouse` ID accepts ASCII case variants, including `lakehouse`,
    /// while the returned ID always retains the canonical registry spelling.
    /// Unindexed members retain local addresses but are marked unavailable; a
    /// snapshot with no indexed members does not advertise an available area.
    ///
    /// # Errors
    /// Returns `UnsupportedFeature` for a different source snapshot or missing
    /// metadata catalog, `NotFound` for an unknown area or no indexed members,
    /// and a typed error for invalid registry or local address data.
    pub fn item_area(&self, id: &str) -> Result<DocArea, SearchError> {
        area_from_manifest(&self.manifest, id)
    }

    /// Search only indexed, exact-path members of a reviewed documentation area.
    ///
    /// The caller's options are unchanged. Existing source-path OR alternatives
    /// are intersected with membership, while all other filters, query text,
    /// workload, facets, ranking choices, and budgets retain their normal meaning.
    /// Every original filter is validated before intersection, including values
    /// that would otherwise be discarded.
    ///
    /// # Errors
    /// Returns area/provenance errors, `InvalidQuery` for invalid filters, an
    /// empty source-path intersection or more than 32 combined filters, and all
    /// normal rich-search validation, feature, expansion and retrieval errors.
    pub fn search_item_docs(
        &self,
        id: &str,
        options: &RichSearchOptions,
    ) -> Result<RichSearchResult, SearchError> {
        options.validate()?;
        let area = self.item_area(id)?;
        let mut scoped = options.clone();
        scoped.filters = scoped_filters(&area, &options.filters, self.filter_catalog())?;
        self.search_rich(&scoped)
    }

    /// Suggest indexed source titles/headings only within a reviewed area.
    ///
    /// Membership intersects exact source-path alternatives before completion
    /// retrieval, without rewriting metadata classifications or caller filters.
    ///
    /// # Errors
    /// Returns area/provenance errors, `InvalidQuery` for invalid filters, an
    /// empty path intersection or excessive combined filters, and the normal
    /// prefix, limit, autocomplete-feature and retrieval errors from `suggest`.
    pub fn suggest_item_docs(
        &self,
        id: &str,
        prefix: &str,
        limit: usize,
        filters: &[MetadataFilter],
    ) -> Result<Vec<Suggestion>, SearchError> {
        let area = self.item_area(id)?;
        let scoped = scoped_filters(&area, filters, self.filter_catalog())?;
        self.suggest(prefix, limit, &scoped)
    }
}

fn unsupported() -> SearchError {
    SearchError::UnsupportedFeature {
        feature: "item documentation areas for this snapshot",
    }
}

fn registry() -> Result<AreaRegistry, SearchError> {
    let registry: AreaRegistry =
        serde_json::from_str(REGISTRY_JSON).map_err(SearchError::Manifest)?;
    validate_registry(&registry)?;
    Ok(registry)
}

fn validate_registry(registry: &AreaRegistry) -> Result<(), SearchError> {
    if registry.schema_version != 1
        || registry.source_repository != SOURCE_REPOSITORY
        || registry.source_commit != SOURCE_COMMIT
        || registry.copyright
            != "Copyright (c) Microsoft Corporation. Licensed under the MIT License."
        || registry.policy.is_empty()
        || registry.areas.len() != 1
    {
        return Err(unsupported());
    }
    let area = &registry.areas[0];
    if area.id != "Lakehouse"
        || area.name != "Lakehouse"
        || area.item_type != "Lakehouse"
        || area.root_article != ROOT_ARTICLE
        || area.description.is_empty()
        || area.coverage != "partial"
        || area.rules.len() != 2
    {
        return Err(unsupported());
    }
    let toc = &area.rules[0];
    let related = &area.rules[1];
    if toc.kind != "reviewed_toc_subtree"
        || toc.evidence.source_path != "docs/data-engineering/toc.yml"
        || toc.evidence.lines != [52, 77]
        || toc.evidence.node_names != ["Store and manage", "Lakehouse"]
        || toc.source_paths.len() != 11
        || related.kind != "reviewed_related_links"
        || related.evidence.source_path != ROOT_ARTICLE
        || related.evidence.lines != [123, 127]
        || related.evidence.node_names != ["Related content"]
        || related.source_paths.len() != 2
    {
        return Err(unsupported());
    }
    let paths: BTreeSet<_> = area
        .rules
        .iter()
        .flat_map(|rule| &rule.source_paths)
        .collect();
    if paths.len() != 13
        || !paths.iter().any(|path| path.as_str() == area.root_article)
        || paths.iter().any(|path| address_for_source(path).is_err())
    {
        return Err(unsupported());
    }
    Ok(())
}

fn area_from_manifest(manifest: &BundleManifest, id: &str) -> Result<DocArea, SearchError> {
    let registry = registry()?;
    if manifest.source_repository != registry.source_repository
        || manifest.source_commit != registry.source_commit
        || !manifest.features.metadata
    {
        return Err(unsupported());
    }
    let catalog = manifest
        .filter_catalog
        .get("source_path")
        .ok_or_else(unsupported)?;
    let area = registry
        .areas
        .into_iter()
        .find(|area| area.id.eq_ignore_ascii_case(id))
        .ok_or(SearchError::NotFound)?;
    let mut articles = Vec::with_capacity(13);
    for path in area.rules.iter().flat_map(|rule| &rule.source_paths) {
        articles.push(AreaArticle {
            source_path: path.clone(),
            address: address_for_source(path)?,
            reference: reference_for_source(path)?,
            indexed: catalog.contains_key(path),
        });
    }
    if !articles.iter().any(|article| article.indexed) {
        return Err(SearchError::NotFound);
    }
    Ok(DocArea {
        id: area.id,
        label: area.name,
        item_type: area.item_type,
        source_commit: registry.source_commit,
        root_address: address_for_source(&area.root_article)?,
        description: area.description,
        coverage: area.coverage,
        articles,
        rules: area.rules,
    })
}

fn scoped_filters(
    area: &DocArea,
    filters: &[MetadataFilter],
    catalog: &FilterCatalog,
) -> Result<Vec<MetadataFilter>, SearchError> {
    if filters.len() > 32 {
        return Err(SearchError::InvalidQuery);
    }
    for filter in filters {
        validate_filter(filter)?;
        if !catalog
            .get(&filter.field)
            .is_some_and(|values| values.contains_key(&filter.value))
        {
            return Err(SearchError::InvalidQuery);
        }
    }
    let allowed: BTreeSet<_> = area
        .articles
        .iter()
        .filter(|article| article.indexed)
        .map(|article| article.source_path.as_str())
        .collect();
    let has_paths = filters.iter().any(|filter| filter.field == "source_path");
    let mut scoped: Vec<_> = filters
        .iter()
        .filter(|filter| filter.field != "source_path" || allowed.contains(filter.value.as_str()))
        .cloned()
        .collect();
    if has_paths {
        if !scoped.iter().any(|filter| filter.field == "source_path") {
            return Err(SearchError::InvalidQuery);
        }
    } else {
        // Absent members stay visible in the area, but are not valid catalog filters.
        scoped.extend(
            area.articles
                .iter()
                .filter(|article| article.indexed)
                .map(|article| MetadataFilter {
                    field: "source_path".into(),
                    value: article.source_path.clone(),
                }),
        );
    }
    if scoped.len() > 32 {
        return Err(SearchError::InvalidQuery);
    }
    Ok(scoped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich::IndexFeatures;

    const NOTEBOOK: &str = "docs/data-engineering/how-to-use-notebook.md";
    const EXPECTED_PATHS: [&str; 13] = [
        ROOT_ARTICLE,
        "docs/data-engineering/create-lakehouse.md",
        "docs/data-engineering/navigate-lakehouse-explorer.md",
        "docs/data-engineering/lakehouse-query-explorer.md",
        "docs/data-engineering/lakehouse-table-maintenance.md",
        "docs/data-engineering/high-concurrency-for-lakehouse-operations.md",
        "docs/data-engineering/lakehouse-schemas.md",
        "docs/data-engineering/lakehouse-shortcuts.md",
        "docs/data-engineering/lakehouse-spark-views.md",
        "docs/data-engineering/lakehouse-git-deployment-pipelines.md",
        "docs/data-engineering/lakehouse-api.md",
        "docs/data-engineering/load-data-lakehouse.md",
        "docs/data-engineering/lakehouse-sql-analytics-endpoint.md",
    ];

    fn manifest(features: IndexFeatures) -> BundleManifest {
        BundleManifest {
            features,
            filter_catalog: FilterCatalog::from([
                (
                    "source_path".into(),
                    EXPECTED_PATHS
                        .iter()
                        .copied()
                        .chain([NOTEBOOK])
                        .map(|path| (path.into(), 1))
                        .collect(),
                ),
                (
                    "item_type".into(),
                    [("Lakehouse".into(), 1), ("Notebook".into(), 1)]
                        .into_iter()
                        .collect(),
                ),
                ("workload".into(), [("data-engineering".into(), 14)].into()),
                ("ms.topic".into(), [("overview".into(), 2)].into()),
            ]),
            schema_version: 1,
            tantivy_version: "0.27.0".into(),
            schema_id: crate::SCHEMA_ID.into(),
            source_repository: SOURCE_REPOSITORY.into(),
            source_commit: SOURCE_COMMIT.into(),
            source_markdown_files: 14,
            source_markdown_bytes: 0,
            documents: 14,
            compression: "none".into(),
            segments: 1,
            unresolved_includes: vec![],
            skipped_inputs: vec![],
            licenses: vec![],
            files: vec![],
        }
    }

    fn filter(field: &str, value: &str) -> MetadataFilter {
        MetadataFilter {
            field: field.into(),
            value: value.into(),
        }
    }

    fn assert_unsupported(result: Result<DocArea, SearchError>) {
        assert!(matches!(
            result,
            Err(SearchError::UnsupportedFeature {
                feature: "item documentation areas for this snapshot"
            })
        ));
    }

    #[test]
    fn registry_is_source_pinned_partial_and_exactly_the_reviewed_thirteen_paths() {
        let registry = registry().unwrap();
        let area = &registry.areas[0];
        let paths: Vec<_> = area
            .rules
            .iter()
            .flat_map(|rule| rule.source_paths.iter().map(String::as_str))
            .collect();
        assert_eq!(registry.schema_version, 1);
        assert_eq!(registry.source_repository, SOURCE_REPOSITORY);
        assert_eq!(registry.source_commit, SOURCE_COMMIT);
        assert_eq!(paths, EXPECTED_PATHS);
        assert_eq!(paths.iter().collect::<BTreeSet<_>>().len(), 13);
        assert_eq!(area.rules[0].source_paths.len(), 11);
        assert_eq!(area.rules[1].source_paths.len(), 2);
        assert_eq!(area.coverage, "partial");
        assert!(!paths.contains(&NOTEBOOK));
        assert!(!paths.contains(&"docs/data-engineering/lakehouse-notebook-explore.md"));
        assert!(!paths.contains(&"docs/data-engineering/lakehouse-notebook-load-data.md"));
    }

    #[test]
    fn local_references_and_evidence_do_not_reclassify_articles() {
        let manifest = manifest(IndexFeatures::full());
        let original = manifest.filter_catalog.clone();
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        assert_eq!(area.id, "Lakehouse");
        assert_eq!(area.label, "Lakehouse");
        assert_eq!(area.item_type, "Lakehouse");
        assert_eq!(area.source_commit, SOURCE_COMMIT);
        assert_eq!(area.root_address, address_for_source(ROOT_ARTICLE).unwrap());
        assert!(area.articles.iter().all(|article| article.indexed));
        for article in &area.articles {
            assert_eq!(
                article.address,
                address_for_source(&article.source_path).unwrap()
            );
            assert_eq!(
                article.reference,
                reference_for_source(&article.source_path).unwrap()
            );
            assert!(!article.reference.contains("://"));
        }
        let json = serde_json::to_value(&area).unwrap();
        assert_eq!(json["coverage"], "partial");
        assert_eq!(
            json["rules"][0]["evidence"]["source_path"],
            "docs/data-engineering/toc.yml"
        );
        assert!(!serde_json::to_string(&area).unwrap().contains("https://"));
        assert_eq!(manifest.filter_catalog, original);
        assert_eq!(manifest.filter_catalog["item_type"]["Lakehouse"], 1);
        assert_eq!(manifest.filter_catalog["item_type"]["Notebook"], 1);
    }

    #[test]
    fn only_metadata_profiles_with_the_exact_source_provenance_advertise_areas() {
        for profile in ["metadata", "full"] {
            let manifest = manifest(IndexFeatures::profile(profile).unwrap());
            assert!(area_from_manifest(&manifest, "Lakehouse").is_ok());
        }
        for profile in ["baseline", "stemmed", "hierarchy", "autocomplete"] {
            let manifest = manifest(IndexFeatures::profile(profile).unwrap());
            assert_unsupported(area_from_manifest(&manifest, "Lakehouse"));
        }
        let mut manifest = manifest(IndexFeatures::full());
        manifest.source_commit = "0000000000000000000000000000000000000000".into();
        assert_unsupported(area_from_manifest(&manifest, "Lakehouse"));
        manifest.source_commit = SOURCE_COMMIT.into();
        manifest.source_repository = "https://example.invalid/fabric-docs".into();
        assert_unsupported(area_from_manifest(&manifest, "Lakehouse"));
        manifest.source_repository = SOURCE_REPOSITORY.into();
        manifest.filter_catalog.remove("source_path");
        assert_unsupported(area_from_manifest(&manifest, "Lakehouse"));
    }

    #[test]
    fn absent_members_are_explicit_and_never_used_as_unknown_catalog_filters() {
        let mut manifest = manifest(IndexFeatures::full());
        manifest
            .filter_catalog
            .get_mut("source_path")
            .unwrap()
            .retain(|path, _| path == ROOT_ARTICLE || path == NOTEBOOK);
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        assert_eq!(area.articles.len(), 13);
        assert_eq!(
            area.articles
                .iter()
                .filter(|article| article.indexed)
                .count(),
            1
        );
        assert_eq!(
            scoped_filters(&area, &[], &manifest.filter_catalog).unwrap(),
            [filter("source_path", ROOT_ARTICLE)]
        );
        manifest
            .filter_catalog
            .get_mut("source_path")
            .unwrap()
            .remove(ROOT_ARTICLE);
        assert!(matches!(
            area_from_manifest(&manifest, "Lakehouse"),
            Err(SearchError::NotFound)
        ));
    }

    #[test]
    fn known_area_ascii_case_aliases_preserve_the_canonical_returned_identity() {
        let manifest = manifest(IndexFeatures::full());
        for id in ["Lakehouse", "lakehouse", "LAKEHOUSE", "LaKeHoUsE"] {
            let area = area_from_manifest(&manifest, id).unwrap();
            assert_eq!(area.id, "Lakehouse");
            assert_eq!(area.label, "Lakehouse");
            assert_eq!(area.item_type, "Lakehouse");
            assert_eq!(area.articles.len(), 13);
            assert_eq!(area.root_address, address_for_source(ROOT_ARTICLE).unwrap());
        }
    }

    #[test]
    fn unknown_areas_are_not_guessed_and_an_unindexed_root_is_marked_unavailable() {
        let mut manifest = manifest(IndexFeatures::full());
        for id in [
            "",
            "Notebook",
            "Unknown",
            " lakehouse",
            "lakehouse ",
            "lakehouse/notebook",
            "lakehouse-x",
        ] {
            assert!(matches!(
                area_from_manifest(&manifest, id),
                Err(SearchError::NotFound)
            ));
        }
        manifest
            .filter_catalog
            .get_mut("source_path")
            .unwrap()
            .retain(|path, _| path == EXPECTED_PATHS[1]);
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        assert_eq!(area.root_address, address_for_source(ROOT_ARTICLE).unwrap());
        assert_eq!(area.articles[0].source_path, ROOT_ARTICLE);
        assert!(!area.articles[0].indexed);
        assert!(area.articles[1].indexed);
        assert_eq!(
            scoped_filters(&area, &[], &manifest.filter_catalog).unwrap(),
            [filter("source_path", EXPECTED_PATHS[1])]
        );
    }

    #[test]
    fn registry_schema_evidence_and_unique_membership_are_validated_without_fallback() {
        for invalid_field in [
            "schema",
            "commit",
            "root",
            "coverage",
            "evidence",
            "duplicate",
        ] {
            let mut registry = registry().unwrap();
            match invalid_field {
                "schema" => registry.schema_version = 2,
                "commit" => registry.source_commit = "0".repeat(40),
                "root" => registry.areas[0].root_article = NOTEBOOK.into(),
                "coverage" => registry.areas[0].coverage = "complete".into(),
                "evidence" => registry.areas[0].rules[0].evidence.lines = [1, 200],
                "duplicate" => {
                    registry.areas[0].rules[1].source_paths[0] = ROOT_ARTICLE.into();
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                validate_registry(&registry),
                Err(SearchError::UnsupportedFeature {
                    feature: "item documentation areas for this snapshot"
                })
            ));
        }
    }

    #[test]
    fn filters_intersect_source_path_or_values_and_preserve_other_criteria() {
        let manifest = manifest(IndexFeatures::full());
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        let original = vec![
            filter("source_path", NOTEBOOK),
            filter("source_path", ROOT_ARTICLE),
            filter("item_type", "Lakehouse"),
            filter("ms.topic", "overview"),
        ];
        let scoped = scoped_filters(&area, &original, &manifest.filter_catalog).unwrap();
        assert_eq!(scoped, original[1..]);
        assert_eq!(original[0], filter("source_path", NOTEBOOK));
        let all = scoped_filters(
            &area,
            &[filter("item_type", "Notebook")],
            &manifest.filter_catalog,
        )
        .unwrap();
        assert_eq!(all.len(), 14);
        assert_eq!(all[0], filter("item_type", "Notebook"));
        assert_eq!(
            all[1..]
                .iter()
                .map(|filter| filter.value.as_str())
                .collect::<Vec<_>>(),
            EXPECTED_PATHS
        );
        assert!(matches!(
            scoped_filters(
                &area,
                &[filter("source_path", NOTEBOOK)],
                &manifest.filter_catalog
            ),
            Err(SearchError::InvalidQuery)
        ));
    }

    #[test]
    fn unknown_or_malformed_original_filters_are_never_discarded_by_intersection() {
        let manifest = manifest(IndexFeatures::full());
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        for invalid in [
            filter("source_path", "docs/data-engineering/not-a-real-article.md"),
            filter("unknown_field", "overview"),
            filter("ms.topic", "unknown"),
            filter("bad field", "overview"),
            filter("ms.topic", ""),
            filter("ms.topic", "over\nview"),
        ] {
            let filters = [filter("source_path", ROOT_ARTICLE), invalid];
            assert!(matches!(
                scoped_filters(&area, &filters, &manifest.filter_catalog),
                Err(SearchError::InvalidQuery)
            ));
        }
    }

    #[test]
    fn combined_filter_budget_rejects_instead_of_truncating_or_broadening() {
        let manifest = manifest(IndexFeatures::full());
        let area = area_from_manifest(&manifest, "Lakehouse").unwrap();
        let nineteen = vec![filter("ms.topic", "overview"); 19];
        assert_eq!(
            scoped_filters(&area, &nineteen, &manifest.filter_catalog)
                .unwrap()
                .len(),
            32
        );
        for count in [20, 32, 33] {
            let filters = vec![filter("ms.topic", "overview"); count];
            assert!(matches!(
                scoped_filters(&area, &filters, &manifest.filter_catalog),
                Err(SearchError::InvalidQuery)
            ));
        }
        let mut excessive = vec![filter("source_path", NOTEBOOK); 33];
        excessive.push(filter("source_path", ROOT_ARTICLE));
        assert!(matches!(
            scoped_filters(&area, &excessive, &manifest.filter_catalog),
            Err(SearchError::InvalidQuery)
        ));
    }
}
