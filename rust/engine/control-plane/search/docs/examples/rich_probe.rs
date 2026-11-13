// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline full-corpus smoke checks for the richer lexical result contracts.
#![cfg_attr(test, deny(unnameable_test_items))]

use fabric_docs_search::{
    DocsSearch, SearchOptions,
    rich::{MetadataFilter, RichSearchOptions, RichSearchResult},
};
use serde_json::json;
use std::collections::BTreeSet;

fn request(text: &str) -> RichSearchOptions {
    RichSearchOptions::new(SearchOptions {
        text: text.into(),
        limit: 10,
        workload: None,
        fuzzy: 0,
    })
}

fn validate(result: &RichSearchResult) -> Result<(), Box<dyn std::error::Error>> {
    let mut articles = BTreeSet::new();
    for hit in &result.data {
        if !articles.insert(&hit.article_id) {
            return Err("Grouped retrieval repeated a source article".into());
        }
        for span in &hit.highlights {
            if span.start >= span.end
                || span.end > hit.hit.snippet.len()
                || !hit.hit.snippet.is_char_boundary(span.start)
                || !hit.hit.snippet.is_char_boundary(span.end)
            {
                return Err("Highlight range is not a valid UTF-8 snippet interval".into());
            }
        }
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let reader = DocsSearch::embedded()?;
    let mut observations = Vec::new();
    for text in [
        "OneLake shortcuts",
        "configuring",
        "RLS",
        "identifiers:spark.sql.parquet.vorder.default",
        "\"workspace identity\" AND (permissions OR firewall OR trusted)",
    ] {
        let result = reader.search_rich(&request(text))?;
        validate(&result)?;
        if result.data.is_empty() {
            return Err(format!("Source-grounded probe returned no hits: {text}").into());
        }
        observations.push(json!({"text":text,"hits":result.data.len(),
            "paths":result.data.iter().map(|hit| &hit.hit.source_path).collect::<Vec<_>>(),
            "expansions":result.expansions,
            "validHighlightSpans":result.data.iter().map(|hit|hit.highlights.len()).sum::<usize>()}));
    }
    let mut filtered = request("Lakehouse");
    filtered.filters.push(MetadataFilter {
        field: "item_type".into(),
        value: "Lakehouse".into(),
    });
    filtered.facets = vec!["ms.topic".into(), "workload".into()];
    let result = reader.search_rich(&filtered)?;
    validate(&result)?;
    if result.data.is_empty()
        || result.data.iter().any(|hit| {
            !hit.metadata
                .get("item_type")
                .is_some_and(|values| values.iter().any(|value| value == "Lakehouse"))
        })
    {
        return Err("Exact source-pinned item-type filter was not enforced".into());
    }
    observations.push(
        json!({"filter":"item_type=Lakehouse","hits":result.data.len(),"facets":result.facets}),
    );
    let mut prefix = request("spark.sql.parquet.vorder.defaul");
    prefix.prefix = true;
    let result = reader.search_rich(&prefix)?;
    validate(&result)?;
    if result.data.is_empty() {
        return Err("Bounded prefix retrieval returned no source hits".into());
    }
    observations
        .push(json!({"prefix":"spark.sql.parquet.vorder.defaul","hits":result.data.len(),"expansions":result.expansions}));
    let suggestions = reader.suggest("lakeh", 10, &[])?;
    if suggestions.is_empty() {
        return Err("Completion returned no source titles/headings".into());
    }
    println!(
        "{}",
        serde_json::to_string(&json!({
            "status":"passed","source_commit":reader.manifest().source_commit,
            "features":reader.manifest().features,"observations":observations,"suggestions":suggestions,
        }))?
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Rich documentation probe failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
