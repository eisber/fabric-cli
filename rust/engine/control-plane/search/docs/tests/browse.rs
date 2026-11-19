// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public local documentation address contracts; RAM retrieval fixtures are
//! beside the implementation and do not need an embedded corpus or Git process.

#![deny(unnameable_test_items)]

use fabric_docs_search::{
    SearchError,
    browse::{DocArticle, DocSection, address_for_source, reference_for_source, validate_address},
};

#[test]
fn exact_source_addresses_preserve_extensions_case_unicode_and_ordinals() {
    for address in [
        "docs/data-engineering/lakehouse-overview.md",
        "docs/data-engineering/lakehouse-overview.md#section-0",
        "docs/data-engineering/lakehouse-overview.md#section-12",
        "docs/Onelake/Case-sensitive.yaml",
        "docs/tutorial.yml#section-1",
        "docs/工程/Café et données.md",
        "docs/owner's guide.md#section-2",
    ] {
        validate_address(address).unwrap();
        assert_eq!(address_for_source(address).unwrap(), address);
    }
}

#[test]
fn root_urls_noncanonical_paths_and_heading_slugs_are_rejected_before_index_open() {
    for address in [
        "",
        "docs",
        "docs/",
        "data-engineering/lakehouse-overview.md",
        "docs data-engineering/lakehouse-overview.md",
        "/docs/overview.md",
        "//server/docs/overview.md",
        r"C:\docs\overview.md",
        r"docs\overview.md",
        "https://learn.microsoft.com/en-us/fabric/overview.md",
        "docs/https://example.invalid/overview.md",
        "docs/../overview.md",
        "docs/./overview.md",
        "docs//overview.md",
        "docs/%2e%2e/overview.md",
        "docs/overview.MD",
        "docs/overview",
        "docs/overview.md?view=fabric",
        "docs/overview.md#lakehouse",
        "docs/overview.md#section-",
        "docs/overview.md#section--1",
        "docs/overview.md#section-+1",
        "docs/overview.md#section-01",
        "docs/overview.md#section-1#section-2",
        "docs/overview.md#section-18446744073709551616",
        "docs/overview.md#section-１",
        "docs/ overview.md",
        "docs/overview.md ",
        "docs/overview\n.md",
        "docs/overview\u{1b}.md",
        "docs/overview\0.md",
    ] {
        assert!(
            matches!(validate_address(address), Err(SearchError::InvalidQuery)),
            "{address:?}"
        );
        assert!(address_for_source(address).is_err());
        assert!(reference_for_source(address).is_err());
    }
    assert!(validate_address(&format!("docs/{}.md", "a".repeat(513))).is_err());
}

#[test]
fn command_references_are_local_single_quoted_and_not_shell_interpolated() {
    assert_eq!(
        reference_for_source("docs/data-engineering/lakehouse-overview.md").unwrap(),
        "fab-rs get 'docs/data-engineering/lakehouse-overview.md'"
    );
    assert_eq!(
        reference_for_source("docs/owner's $guide; (local).md#section-2").unwrap(),
        "fab-rs get 'docs/owner''s $guide; (local).md#section-2'"
    );
    assert_eq!(
        reference_for_source("docs/owner’s ‘guide’.md").unwrap(),
        "fab-rs get 'docs/owner’’s ‘‘guide’’.md'"
    );
    assert_eq!(
        reference_for_source("docs/\u{201a}guide\u{201b}.md").unwrap(),
        "fab-rs get 'docs/\u{201a}\u{201a}guide\u{201b}\u{201b}.md'"
    );
}

#[test]
fn article_serialization_contains_only_local_identity_and_complete_stored_content() {
    let source_path = "docs/工程/overview.md";
    let address = format!("{source_path}#section-3");
    let article = DocArticle {
        source_path: source_path.into(),
        address: address_for_source(&address).unwrap(),
        reference: reference_for_source(&address).unwrap(),
        title: "Complete stored article".into(),
        sections: vec![DocSection {
            id: address.clone(),
            heading: "Heading".into(),
            heading_path: vec!["Article".into(), "Heading".into()],
            content: "Normalized stored text\nnot a search snippet.".repeat(100),
        }],
        source_commit: "a".repeat(40),
    };
    let json = serde_json::to_value(&article).unwrap();
    assert_eq!(json["address"], address);
    assert_eq!(json["source_path"], source_path);
    assert_eq!(json["sections"][0]["content"], article.sections[0].content);
    assert_eq!(json["source_commit"], "a".repeat(40));
    assert!(json.get("url").is_none());
    assert!(json.get("source_url").is_none());
    assert!(json["sections"][0].get("snippet").is_none());
}
