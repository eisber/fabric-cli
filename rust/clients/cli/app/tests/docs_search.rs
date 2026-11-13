// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The actual local documentation command never initializes service credentials.
#![cfg(feature = "docs-search")]
#![deny(unnameable_test_items)]

use fabric_cli::parser::{Invocation, parse_args};
use std::process::{Command, Output};

fn local_docs(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["search", "docs"])
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "private-provider")
        .output()
        .unwrap()
}

#[test]
fn local_parser_accepts_only_explicit_docs_scope_and_finite_criteria() {
    assert!(matches!(
        parse_args(["fab-rs", "search", "docs", "--text", "OneLake shortcuts"]).unwrap(),
        Invocation::DocsSearch(_)
    ));
    for (case, args) in [
        vec![
            "fab-rs", "search", "docs", "--text", "secret", "--limit", "0",
        ],
        vec![
            "fab-rs", "search", "docs", "--text", "secret", "--limit", "51",
        ],
        vec!["fab-rs", "search", "docs", "--text", "*"],
        vec![
            "fab-rs", "search", "docs", "--text", "first", "--text", "secret",
        ],
        vec!["fab-rs", "search", "/", "--text", "secret"],
        vec![
            "fab-rs",
            "search",
            "docs",
            "--text",
            "secret",
            "--corpus-info",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        assert!(parse_args(args).is_err(), "negative argument case {case}");
    }
}

#[test]
fn quoted_and_escaped_parentheses_are_literal_query_content() {
    for text in [
        r#""COUNT(""#,
        "'COUNT)'",
        r"COUNT\(",
        r"COUNT\)",
        r#"'completion: COUNT('"#,
        r#""metadata_terms: COUNT)""#,
        r"completion\: COUNT\(",
        r#"'completion: "COUNT('"#,
    ] {
        assert!(matches!(
            parse_args(["fab-rs", "search", "docs", "--text", text]).unwrap(),
            Invocation::DocsSearch(_)
        ));
        let output = local_docs(&["--text", text]);
        match fabric_docs_search::DocsSearch::embedded() {
            Ok(_) => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["status"], "succeeded");
            }
            Err(fabric_docs_search::SearchError::NotEmbedded) => {
                assert_eq!(output.status.code(), Some(1));
                assert!(output.stdout.is_empty());
            }
            Err(error) => panic!("Broken test corpus: {error}"),
        }
    }
}

#[test]
fn rich_parser_accepts_bounded_search_and_alternative_local_modes() {
    for args in [
        vec![
            "fab-rs",
            "search",
            "docs",
            "--text",
            "lakehouse",
            "--filter",
            "ms.topic=conceptual",
            "--filter",
            "ms.topic=how-to",
            "--facet",
            "ms.topic",
            "--facet",
            "workload",
            "--candidate-limit",
            "500",
            "--facet-budget",
            "10000",
            "--max-expansions",
            "128",
            "--prefix",
            "--no-stemming",
            "--no-synonyms",
            "--no-rerank",
            "--sections",
            "--format",
            "text",
        ],
        vec![
            "fab-rs",
            "search",
            "docs",
            "--text",
            "lakehouse",
            "--candidate-limit",
            "1",
            "--limit",
            "1",
            "--facet-budget",
            "1",
            "--max-expansions",
            "1",
        ],
        vec![
            "fab-rs",
            "search",
            "docs",
            "--suggest",
            "lake",
            "--limit",
            "50",
            "--workload",
            "data-engineering",
            "--filter",
            "ms.topic=conceptual",
            "--format",
            "json",
        ],
        vec!["fab-rs", "search", "docs", "--filters", "--format", "text"],
        vec!["fab-rs", "search", "docs", "--corpus-info"],
    ] {
        assert!(matches!(
            parse_args(args).unwrap(),
            Invocation::DocsSearch(_)
        ));
    }
    let mut args = vec![
        "fab-rs",
        "search",
        "docs",
        "--text",
        "lakehouse",
        "--workload",
        "data-engineering",
    ];
    for _ in 0..32 {
        args.extend(["--filter", "ms.topic=conceptual"]);
    }
    assert!(matches!(
        parse_args(args).unwrap(),
        Invocation::DocsSearch(_)
    ));
    let mut args = vec![
        "fab-rs",
        "search",
        "docs",
        "--suggest",
        "lake",
        "--workload",
        "data-engineering",
    ];
    for _ in 0..32 {
        args.extend(["--filter", "workload=data-engineering"]);
    }
    assert!(matches!(
        parse_args(args).unwrap(),
        Invocation::DocsSearch(_)
    ));
}

#[test]
fn rich_parser_rejects_bounds_malformed_metadata_and_duplicate_scalars() {
    for (flag, value) in [
        ("--candidate-limit", "0"),
        ("--candidate-limit", "501"),
        ("--candidate-limit", "9"),
        ("--facet-budget", "0"),
        ("--facet-budget", "10001"),
        ("--max-expansions", "0"),
        ("--max-expansions", "129"),
        ("--filter", "missing-equals"),
        ("--filter", "=value"),
        ("--filter", "ms.topic="),
        ("--filter", "unsafe field=value"),
        ("--filter", "ms.topic=bad\nvalue"),
        ("--facet", "unsafe field"),
    ] {
        assert!(
            parse_args([
                "fab-rs",
                "search",
                "docs",
                "--text",
                "private-query",
                flag,
                value
            ])
            .is_err(),
            "accepted {flag}"
        );
    }
    for (flag, value) in [
        ("--limit", "10"),
        ("--workload", "security"),
        ("--fuzzy", "0"),
        ("--candidate-limit", "200"),
        ("--facet-budget", "5000"),
        ("--max-expansions", "64"),
        ("--format", "json"),
    ] {
        assert!(
            parse_args([
                "fab-rs",
                "search",
                "docs",
                "--text",
                "private-query",
                flag,
                value,
                flag,
                value,
            ])
            .is_err(),
            "accepted duplicate {flag}"
        );
    }
    for flag in [
        "--prefix",
        "--sections",
        "--no-stemming",
        "--no-synonyms",
        "--no-rerank",
    ] {
        assert!(
            parse_args([
                "fab-rs",
                "search",
                "docs",
                "--text",
                "private-query",
                flag,
                flag
            ])
            .is_err(),
            "accepted duplicate {flag}"
        );
    }
    let mut args = vec!["fab-rs", "search", "docs", "--text", "lakehouse"];
    for _ in 0..9 {
        args.extend(["--facet", "workload"]);
    }
    assert!(parse_args(args).is_err());
    let mut args = vec!["fab-rs", "search", "docs", "--text", "lakehouse"];
    for _ in 0..33 {
        args.extend(["--filter", "ms.topic=conceptual"]);
    }
    assert!(parse_args(args).is_err());
    let mut args = vec![
        "fab-rs",
        "search",
        "docs",
        "--suggest",
        "lake",
        "--workload",
        "data-engineering",
    ];
    for _ in 0..32 {
        args.extend(["--filter", "ms.topic=conceptual"]);
    }
    assert!(parse_args(args).is_err());
    for criteria in [
        vec!["--text", "ab", "--prefix"],
        vec!["--text", "lakehouse AND tabl", "--prefix"],
        vec!["--text", "\"lakehouse tabl\"", "--prefix"],
        vec!["--text", "lakehouse", "--prefix", "--fuzzy", "1"],
        vec!["--text", "private\nquery"],
        vec![
            "--text",
            "lakehouse",
            "--workload",
            "security",
            "--filter",
            "workload=data-engineering",
        ],
    ] {
        let mut args = vec!["fab-rs", "search", "docs"];
        args.extend(criteria);
        assert!(parse_args(args).is_err());
    }
}

#[test]
fn modes_conflict_and_reject_even_default_valued_unrelated_query_flags() {
    for modes in [
        vec!["--text", "private-query", "--suggest", "private-prefix"],
        vec!["--text", "private-query", "--filters"],
        vec!["--text", "private-query", "--corpus-info"],
        vec!["--suggest", "private-prefix", "--filters"],
        vec!["--suggest", "private-prefix", "--corpus-info"],
        vec!["--filters", "--corpus-info"],
    ] {
        let mut args = vec!["fab-rs", "search", "docs"];
        args.extend(modes);
        assert!(parse_args(args).is_err());
    }
    for mode in ["--suggest", "--filters", "--corpus-info"] {
        for flag in [
            vec!["--fuzzy", "0"],
            vec!["--candidate-limit", "200"],
            vec!["--facet-budget", "5000"],
            vec!["--max-expansions", "64"],
            vec!["--facet", "workload"],
            vec!["--prefix"],
            vec!["--no-stemming"],
            vec!["--no-synonyms"],
            vec!["--no-rerank"],
            vec!["--sections"],
        ] {
            let mut args = vec!["fab-rs", "search", "docs", mode];
            if mode == "--suggest" {
                args.push("private-prefix");
            }
            args.extend(flag);
            assert!(
                parse_args(args).is_err(),
                "accepted unrelated flag for {mode}"
            );
        }
    }
    for mode in ["--filters", "--corpus-info"] {
        for flag in [
            vec!["--limit", "10"],
            vec!["--workload", "security"],
            vec!["--filter", "ms.topic=conceptual"],
        ] {
            let mut args = vec!["fab-rs", "search", "docs", mode];
            args.extend(flag);
            assert!(parse_args(args).is_err());
        }
    }
    for args in [
        vec!["--suggest", "private", "--suggest", "private"],
        vec!["--suggest", "private", "--limit", "0"],
        vec!["--suggest", "private", "--limit", "51"],
        vec!["--suggest", "private\nprefix"],
        vec!["--suggest", "a"],
        vec!["--suggest", "two words"],
        vec!["--suggest", "prefix-with-hyphen"],
        vec!["--suggest", "abcdefghijklmnopqrstu"],
        vec![
            "--suggest",
            "lake",
            "--workload",
            "security",
            "--filter",
            "workload=data-engineering",
        ],
        vec!["--filters", "--filters"],
        vec!["--corpus-info", "--corpus-info"],
    ] {
        let mut command = vec!["fab-rs", "search", "docs"];
        command.extend(args);
        assert!(parse_args(command).is_err());
    }
}

#[test]
fn suggestion_prefix_limits_account_for_unicode_lowercase_expansion_before_index_open() {
    let within_limit = "İ".repeat(10);
    assert!(matches!(
        parse_args(["fab-rs", "search", "docs", "--suggest", &within_limit]).unwrap(),
        Invocation::DocsSearch(_)
    ));
    let beyond_limit = "İ".repeat(11);
    assert!(parse_args(["fab-rs", "search", "docs", "--suggest", &beyond_limit]).is_err());
}

#[test]
fn actual_binary_help_and_search_do_not_read_invalid_service_or_config_settings() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    let output = command
        .args(["search", "docs", "--help"])
        .env("FAB_RUST_CONFIG_ROOT", root.path().join("must-not-exist"))
        .env("FAB_RUST_AUTH_MODE", "invalid-secret-provider")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--corpus-info"));
    assert!(help.contains("Embedded offline corpus:"));
    assert!(help.contains("'docs/<article-path>'"));
    match fabric_docs_search::embedded_corpus_info().unwrap() {
        Some(manifest) => {
            assert!(help.contains(&manifest.source_commit));
            assert!(help.contains(&manifest.schema_id));
            assert!(help.contains(&manifest.documents.to_string()));
            assert!(help.contains("features:"));
        }
        None => assert!(help.contains("not configured (engine-only build)")),
    }
    assert!(!root.path().join("must-not-exist").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["search", "docs", "--text", "OneLake shortcuts"])
        .env("FAB_RUST_CONFIG_ROOT", root.path().join("must-not-exist"))
        .env("FAB_RUST_AUTH_MODE", "invalid-secret-provider")
        .output()
        .unwrap();
    match fabric_docs_search::DocsSearch::embedded() {
        Ok(reader) => {
            if reader.manifest().features.stemmed {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["source"], "embeddedFabricDocs");
                assert!(!value["data"].as_array().unwrap().is_empty());
            } else {
                assert_eq!(output.status.code(), Some(1));
                assert!(output.stdout.is_empty());
                let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(value["error"]["code"], "docs_search_unsupported_feature");
            }
        }
        Err(fabric_docs_search::SearchError::NotEmbedded) => {
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(value["error"]["code"], "docs_search_not_configured");
        }
        Err(error) => panic!("Broken embedded test index: {error}"),
    }
    assert!(!root.path().join("must-not-exist").exists());
}

#[test]
fn invalid_binary_arguments_are_redacted_before_any_index_or_host_setup() {
    for args in [
        vec!["--text", "private-query", "--limit", "0"],
        vec!["--text", "private-query", "--candidate-limit", "501"],
        vec!["--text", "private-query", "--facet-budget", "10001"],
        vec!["--text", "private-query", "--max-expansions", "129"],
        vec!["--text", "private-query", "--filter", "private-invalid"],
        vec!["--suggest", "private-prefix", "--prefix"],
        vec!["--filters", "--text", "private-query"],
        vec!["--corpus-info", "--limit", "10"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(["search", "docs"])
            .args(args)
            .env("FAB_RUST_AUTH_MODE", "private-provider")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["error"]["code"], "invalid_arguments");
    }
}

#[test]
fn filter_catalog_and_corpus_info_expose_exact_manifest_facts_without_auth() {
    let output = local_docs(&["--filters"]);
    let reader = match fabric_docs_search::DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(fabric_docs_search::SearchError::NotEmbedded) => {
            assert_eq!(output.status.code(), Some(1));
            let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(value["error"]["code"], "docs_search_not_configured");
            return;
        }
        Err(error) => panic!("Broken embedded test index: {error}"),
    };
    if reader.manifest().features.metadata {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["data"],
            serde_json::to_value(&reader.manifest().filter_catalog).unwrap()
        );
        assert_eq!(value["meta"]["countUnit"], "uniqueSourceArticles");
        assert_eq!(
            value["meta"]["sourceCoverage"]["itemTypeFieldObserved"],
            ["item_type", "itemType", "ms.item_type"]
                .iter()
                .any(|field| reader.manifest().filter_catalog.contains_key(*field))
        );
        assert_eq!(
            value["meta"]["sourceCoverage"]["missingItemType"],
            ["item_type", "itemType", "ms.item_type"]
                .iter()
                .all(|field| !reader.manifest().filter_catalog.contains_key(*field))
        );
        assert_eq!(
            value["meta"]["indexFeatures"],
            serde_json::to_value(reader.manifest().features).unwrap()
        );
        if reader.manifest().source_commit == "3ae7afc73169ac57b874d5aee09217bb35a4f8fc"
            && reader.manifest().licenses.iter().any(|notice| {
                notice.name == "TRANSFORMATION-ATTRIBUTION"
                    && notice.text.contains("Reviewed item-type supplement:")
            })
        {
            let coverage = &value["meta"]["sourceCoverage"]["itemTypeCoverage"];
            assert_eq!(coverage["curatedArticles"], 10);
            assert_eq!(coverage["sourceAuthoredArticles"], 0);
            assert_eq!(coverage["countUnit"], "uniqueSourceArticles");
        }
        let text_output = local_docs(&["--filters", "--format", "text"]);
        assert!(text_output.status.success());
        let text_output = String::from_utf8(text_output.stdout).unwrap();
        assert!(!text_output.contains("https://"));
        assert!(!text_output.contains("http://"));
        assert!(!text_output.contains("Fabric docs snapshot"));
        if let Some(source) = reader
            .manifest()
            .filter_catalog
            .get("source_path")
            .and_then(|values| values.keys().next())
        {
            let reference = fabric_docs_search::browse::reference_for_source(source).unwrap();
            assert!(text_output.contains(&reference));
        }
    } else {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "docs_search_unsupported_feature");
    }
    let output = local_docs(&["--corpus-info"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["data"],
        serde_json::to_value(reader.manifest()).unwrap()
    );
}

#[test]
fn unknown_metadata_fields_values_and_facets_are_redacted_without_remote_auth() {
    let reader = match fabric_docs_search::DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(fabric_docs_search::SearchError::NotEmbedded) => return,
        Err(error) => panic!("Broken embedded test index: {error}"),
    };
    if !reader.manifest().features.metadata || !reader.manifest().features.stemmed {
        return;
    }
    for args in [
        vec![
            "--text",
            "lakehouse",
            "--filter",
            "private_unknown_field=private-value",
        ],
        vec![
            "--text",
            "lakehouse",
            "--filter",
            "workload=private-unknown-value",
        ],
        vec!["--text", "lakehouse", "--facet", "private_unknown_field"],
    ] {
        let output = local_docs(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["error"]["code"], "invalid_arguments");
    }
    if reader.manifest().features.autocomplete {
        for filter in [
            "private_unknown_field=private-value",
            "workload=private-unknown-value",
        ] {
            let output = local_docs(&["--suggest", "lake", "--filter", filter]);
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
        }
        let workload = reader.manifest().filter_catalog["workload"]
            .keys()
            .next()
            .unwrap();
        let matched = format!("workload={workload}");
        let output = local_docs(&[
            "--suggest",
            "lake",
            "--workload",
            workload,
            "--filter",
            &matched,
            "--filter",
            "workload=private-unknown-value",
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["error"]["code"], "invalid_arguments");
    }
}

#[test]
fn rich_json_preserves_flat_local_hit_contract_and_full_precision_with_facet_coverage() {
    let reader = match fabric_docs_search::DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(fabric_docs_search::SearchError::NotEmbedded) => return,
        Err(error) => panic!("Broken embedded test index: {error}"),
    };
    if !reader.manifest().features.stemmed || !reader.manifest().features.metadata {
        return;
    }
    let output = local_docs(&["--text", "lakehouse", "--facet", "workload", "--limit", "1"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "succeeded");
    assert_eq!(value["source"], "embeddedFabricDocs");
    for field in [
        "sourceCommit",
        "indexedSections",
        "resultLimit",
        "openUs",
        "timings",
        "candidatesExamined",
        "candidatesLimited",
        "expansions",
        "facets",
        "indexFeatures",
        "sourceCoverage",
    ] {
        assert!(!value["meta"][field].is_null(), "missing meta.{field}");
    }
    let hits = value["data"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    for hit in hits {
        for field in [
            "id",
            "title",
            "heading",
            "url",
            "address",
            "reference",
            "source_path",
            "workload",
            "score",
            "snippet",
            "article_id",
            "heading_path",
            "metadata",
            "highlights",
            "rerank_bonus",
        ] {
            assert!(!hit[field].is_null(), "missing data[].{field}");
        }
        assert!(
            hit.get("hit").is_none(),
            "legacy hit fields must remain flat"
        );
        assert!(hit.get("source_url").is_none());
        let source = hit["source_path"].as_str().unwrap();
        let address = fabric_docs_search::browse::address_for_source(source).unwrap();
        let reference = fabric_docs_search::browse::reference_for_source(source).unwrap();
        assert_eq!(hit["address"], address);
        assert_eq!(hit["reference"], reference);
        assert_eq!(hit["url"], reference);
        assert!(!reference.contains("https://"));
        let snippet = hit["snippet"].as_str().unwrap();
        let mut end = 0;
        for span in hit["highlights"].as_array().unwrap() {
            let start = span["start"].as_u64().unwrap() as usize;
            let next_end = span["end"].as_u64().unwrap() as usize;
            assert!(start >= end && start < next_end);
            assert!(snippet.get(start..next_end).is_some());
            end = next_end;
        }
    }
    let mut options =
        fabric_docs_search::rich::RichSearchOptions::new(fabric_docs_search::SearchOptions {
            text: "lakehouse".into(),
            limit: 1,
            workload: None,
            fuzzy: 0,
        });
    options.facets.push("workload".into());
    let expected = reader.search_rich(&options).unwrap();
    assert_eq!(
        hits[0]["score"],
        serde_json::json!(expected.data[0].hit.score)
    );
    assert_eq!(
        hits[0]["rerank_bonus"],
        serde_json::json!(expected.data[0].rerank_bonus)
    );
    let wider = local_docs(&[
        "--text",
        "lakehouse",
        "--facet",
        "workload",
        "--limit",
        "50",
    ]);
    assert!(wider.status.success());
    let wider: serde_json::Value = serde_json::from_slice(&wider.stdout).unwrap();
    assert_eq!(value["meta"]["facets"], wider["meta"]["facets"]);
    let partial = local_docs(&[
        "--text",
        "lakehouse",
        "--facet",
        "workload",
        "--facet-budget",
        "1",
        "--format",
        "text",
    ]);
    assert!(partial.status.success());
    let partial = String::from_utf8(partial.stdout).unwrap();
    assert!(!partial.contains('\u{1b}'));
    assert!(!partial.contains("Fabric docs snapshot"));
    assert!(!partial.contains("https://"));
    assert!(!partial.contains("http://"));
    assert_eq!(partial.matches("Read any result offline").count(), 1);
    for hit in &expected.data {
        let reference =
            fabric_docs_search::browse::reference_for_source(&hit.hit.source_path).unwrap();
        assert!(partial.contains(&reference));
    }
    for line in partial.lines().filter(|line| line.contains("| score ")) {
        let score = line.split_once("| score ").unwrap().1;
        assert_eq!(score.split_once('.').unwrap().1.len(), 2);
    }
    if value["meta"]["facets"]["examined_sections"]
        .as_u64()
        .unwrap()
        > 1
    {
        assert!(partial.contains("Facet coverage is partial"));
    }
}

#[test]
fn suggestions_are_actual_titles_or_headings_and_respect_exact_metadata() {
    let reader = match fabric_docs_search::DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(fabric_docs_search::SearchError::NotEmbedded) => return,
        Err(error) => panic!("Broken embedded test index: {error}"),
    };
    let output = local_docs(&["--suggest", "firewal", "--limit", "1"]);
    if !reader.manifest().features.autocomplete {
        assert_eq!(output.status.code(), Some(1));
        let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "docs_search_unsupported_feature");
        return;
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let suggestion = &value["data"][0];
    let text = suggestion["text"].as_str().unwrap();
    let path = suggestion["source_path"].as_str().unwrap();
    let workload = suggestion["workload"].as_str().unwrap();
    let expected = reader.suggest("firewal", 1, &[]).unwrap();
    assert_eq!(text, expected[0].text);
    assert_eq!(path, expected[0].source_path);
    assert_eq!(
        suggestion["address"],
        fabric_docs_search::browse::address_for_source(path).unwrap()
    );
    assert_eq!(
        suggestion["reference"],
        fabric_docs_search::browse::reference_for_source(path).unwrap()
    );
    let text_output = local_docs(&["--suggest", "firewal", "--limit", "1", "--format", "text"]);
    assert!(text_output.status.success());
    let text_output = String::from_utf8(text_output.stdout).unwrap();
    assert!(text_output.contains(suggestion["reference"].as_str().unwrap()));
    assert!(!text_output.contains("https://"));
    assert!(!text_output.contains("Fabric docs snapshot"));
    let broad = local_docs(&["--suggest", "lakeh", "--limit", "1"]);
    assert!(
        broad.status.success(),
        "{}",
        String::from_utf8_lossy(&broad.stderr)
    );
    let broad: serde_json::Value = serde_json::from_slice(&broad.stdout).unwrap();
    assert!(
        broad["meta"]["coverage"]
            .as_str()
            .unwrap()
            .contains("not exhaustive")
    );
    let completions = broad["data"].as_array().unwrap();
    assert_eq!(completions.len(), 1);
    for completion in completions {
        assert!(
            completion["text"]
                .as_str()
                .unwrap()
                .split(|character: char| !character.is_alphanumeric())
                .any(|token| token.to_lowercase().starts_with("lakeh"))
        );
    }
    if reader.manifest().features.metadata {
        let filter = format!("source_path={path}");
        let matched_workload = format!("workload={workload}");
        let alternative_workload = reader.manifest().filter_catalog["workload"]
            .keys()
            .find(|value| value.as_str() != workload)
            .map(|value| format!("workload={value}"))
            .unwrap_or_else(|| matched_workload.clone());
        let output = local_docs(&[
            "--suggest",
            "firewal",
            "--workload",
            workload,
            "--filter",
            &filter,
            "--filter",
            &matched_workload,
            "--filter",
            &alternative_workload,
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!value["data"].as_array().unwrap().is_empty());
        for item in value["data"].as_array().unwrap() {
            assert_eq!(item["source_path"], path);
            assert_eq!(item["workload"], workload);
        }
    }
}

#[test]
fn docs_only_batch_and_help_do_not_read_unrelated_configuration() {
    let root = tempfile::tempdir().unwrap();
    let invalid_config = root.path().join("not-a-directory");
    std::fs::write(&invalid_config, b"preserve").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args([
            "-c",
            "search docs --corpus-info",
            "-c",
            "search docs --corpus-info",
        ])
        .env("FAB_RUST_CONFIG_ROOT", &invalid_config)
        .env("FAB_RUST_AUTH_MODE", "invalid-provider")
        .output()
        .unwrap();
    let embedded = fabric_docs_search::DocsSearch::embedded();
    assert_eq!(
        output.status.code(),
        Some(if embedded.is_ok() { 0 } else { 1 })
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("2 commands executed."));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("OutputFailed"), "{error}");
    assert!(!error.contains("output_failure"), "{error}");
    let help = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["-c", "search docs --help"])
        .env("FAB_RUST_CONFIG_ROOT", &invalid_config)
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert_eq!(std::fs::read(invalid_config).unwrap(), b"preserve");
}

#[test]
fn existing_non_docs_display_batches_retain_configured_summary_format() {
    let root = tempfile::tempdir().unwrap();
    let configured = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["config", "set", "output_format", "json"])
        .env("FAB_RUST_CONFIG_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["-c", "config", "-c", "job"])
        .env("FAB_RUST_CONFIG_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"command\": ["), "{stdout}");
    assert!(stdout.contains("\"status\": \"Success\""), "{stdout}");
    assert!(stdout.ends_with("}\n"));
}

#[cfg(feature = "headless")]
#[test]
fn preview_head_routes_docs_before_remote_identity_selection() {
    let root = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["--help"])
        .env("FAB_RUST_AUTH_MODE", "private-provider")
        .output()
        .unwrap();
    assert!(root.status.success());
    assert!(root.stderr.is_empty());
    let help = String::from_utf8(root.stdout).unwrap();
    for expected in [
        "Offline exception:",
        "fab-cli-v2 search docs --text QUERY",
        "get docs/<article-path>",
        "item lakehouse docs",
        "require no identity",
    ] {
        assert!(help.contains(expected), "{expected}: {help}");
    }
    #[cfg(feature = "runtime-v2")]
    assert!(help.contains("Opt-in runtime list:"));
    let output = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["search", "docs", "--help"])
        .env("FAB_RUST_AUTH_MODE", "private-provider")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("--corpus-info")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["search", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("search docs")
    );
}
