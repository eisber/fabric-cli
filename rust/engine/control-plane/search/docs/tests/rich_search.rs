// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public rich-query contract checks; the reader's private-schema RAM fixtures
//! live beside its implementation and require no embedded corpus or Git process.

#![deny(unnameable_test_items)]

use fabric_docs_search::{
    SearchOptions,
    rich::{HighlightSpan, IndexFeatures, MetadataFilter, RichSearchOptions},
};

fn query(text: &str) -> SearchOptions {
    SearchOptions {
        text: text.into(),
        limit: 10,
        workload: None,
        fuzzy: 0,
    }
}

#[test]
fn rich_defaults_are_explicit_bounded_and_preserve_the_original_criteria() {
    let mut original = query("RLS lakehouse");
    original.workload = Some("security".into());
    let rich = RichSearchOptions::new(original);
    assert_eq!(rich.query.text, "RLS lakehouse");
    assert_eq!(rich.query.workload.as_deref(), Some("security"));
    assert!(rich.stemming && rich.synonyms && rich.group_articles && rich.rerank);
    assert!(!rich.prefix);
    assert_eq!(rich.candidates, 200);
    assert_eq!(rich.max_expansions, 64);
    assert_eq!(rich.facet_budget, 5000);
    assert!(rich.filters.is_empty() && rich.facets.is_empty());
}

#[test]
fn existing_input_rejections_still_apply_to_wrapped_queries() {
    for text in ["", "  ", "*", "body:/.*", "(unclosed", "\"quoted\"*"] {
        assert!(
            RichSearchOptions::new(query(text)).validate().is_err(),
            "{text}"
        );
    }
    for limit in [0, 51] {
        let mut request = RichSearchOptions::new(query("lakehouse"));
        request.query.limit = limit;
        assert!(request.validate().is_err());
    }
    let mut request = RichSearchOptions::new(query("\"lakehouse storage\""));
    request.query.fuzzy = 1;
    assert!(request.validate().is_err());
}

#[test]
fn rich_scalar_and_filter_shape_validation_needs_no_reader() {
    for candidates in [0, 5, 501] {
        let mut request = RichSearchOptions::new(query("storage"));
        request.candidates = candidates;
        assert!(request.validate().is_err());
    }
    for max_expansions in [0, 129] {
        let mut request = RichSearchOptions::new(query("storage"));
        request.max_expansions = max_expansions;
        assert!(request.validate().is_err());
    }
    for facet_budget in [0, 10_001] {
        let mut request = RichSearchOptions::new(query("storage"));
        request.facet_budget = facet_budget;
        assert!(request.validate().is_err());
    }
    let mut request = RichSearchOptions::new(query("storage"));
    request.filters = vec![
        MetadataFilter {
            field: "ms.topic".into(),
            value: "overview".into()
        };
        33
    ];
    assert!(request.validate().is_err());
    request.filters.truncate(32);
    request.query.workload = Some("security".into());
    assert!(request.validate().is_ok());
    request.filters[0].field = "bad field".into();
    assert!(request.validate().is_err());
    request.filters.clear();
    request.facets = vec!["ms.topic".into(); 9];
    assert!(request.validate().is_err());
}

#[test]
fn feature_profiles_never_guess_unknown_or_partial_names() {
    assert_eq!(
        IndexFeatures::profile("baseline"),
        Some(IndexFeatures::default())
    );
    assert_eq!(IndexFeatures::profile("full"), Some(IndexFeatures::full()));
    for name in ["", "FULL", "stem", "metadata+hierarchy", "full "] {
        assert_eq!(IndexFeatures::profile(name), None);
    }
    let stemmed = IndexFeatures::profile("stemmed").unwrap();
    assert!(stemmed.stemmed);
    assert!(!stemmed.metadata && !stemmed.hierarchy && !stemmed.autocomplete);
}

#[test]
fn exact_metadata_values_and_utf8_byte_evidence_are_not_normalized_by_serialization() {
    let filter = MetadataFilter {
        field: "ms.topic".into(),
        value: "How-To".into(),
    };
    let encoded = serde_json::to_value(&filter).unwrap();
    assert_eq!(encoded["field"], "ms.topic");
    assert_eq!(encoded["value"], "How-To");
    let snippet = "Café lakehouse";
    let span = HighlightSpan {
        start: "Café ".len(),
        end: snippet.len(),
    };
    let encoded = serde_json::to_value(&span).unwrap();
    let decoded: HighlightSpan = serde_json::from_value(encoded).unwrap();
    assert_eq!(&snippet[decoded.start..decoded.end], "lakehouse");
}
