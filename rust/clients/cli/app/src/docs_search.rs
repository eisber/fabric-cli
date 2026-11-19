// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free documentation arguments and output shared by the two Rust heads.

use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command, error::ErrorKind};
use fabric_docs_search::{
    BundleManifest, DocsSearch, LicenseNotice, SearchError, SearchOptions,
    browse::{address_for_source, reference_for_source},
    rich::{
        FilterCatalog, HighlightSpan, MetadataFilter, RichHit, RichSearchOptions, RichSearchResult,
        Suggestion,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{self, Write},
    time::Instant,
};

const ITEM_TYPE_INVENTORY_REPOSITORY: &str = "https://github.com/MicrosoftDocs/fabric-docs";
const ITEM_TYPE_INVENTORY_COMMIT: &str = "3ae7afc73169ac57b874d5aee09217bb35a4f8fc";
const TEXT_WIDTH: usize = 96;
const REFERENCE_GUIDANCE: &str =
    "Read any result offline with its fab-rs get command. References use local docs addresses.";

/// One local documentation invocation; parsing never initializes the index.
#[derive(Clone, Debug)]
pub struct DocsCommand {
    mode: DocsMode,
    text_output: bool,
}

#[derive(Clone, Debug)]
enum DocsMode {
    Search(RichSearchOptions),
    Suggest {
        prefix: String,
        limit: usize,
        filters: Vec<MetadataFilter>,
        workload: Option<String>,
    },
    Filters,
    CorpusInfo,
}

/// Build the explicitly local `search docs` schema, without reading any settings.
pub fn command() -> Command {
    let search_only = [
        "fuzzy",
        "facet",
        "candidate-limit",
        "facet-budget",
        "max-expansions",
        "prefix",
        "no-stemming",
        "no-synonyms",
        "no-rerank",
        "sections",
    ];
    let info_conflicts = [
        "limit",
        "workload",
        "filter",
        "fuzzy",
        "facet",
        "candidate-limit",
        "facet-budget",
        "max-expansions",
        "prefix",
        "no-stemming",
        "no-synonyms",
        "no-rerank",
        "sections",
    ];
    Command::new("search")
        .about("Search explicitly selected sources")
        .args_override_self(false)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("docs")
                .about("Search the immutable embedded Fabric documentation snapshot offline")
                .args_override_self(false)
                .infer_long_args(false)
                .after_help(docs_help())
                .group(ArgGroup::new("mode").args(["text", "suggest", "filters", "corpus-info"]).required(true))
                .arg(Arg::new("text").long("text").num_args(1).help("Lexical query; results are grouped by article by default"))
                .arg(Arg::new("suggest").long("suggest").num_args(1).conflicts_with_all(search_only)
                    .help("Complete actual indexed titles/headings with one alphanumeric prefix (2-20 characters; lowercase form at most 20)"))
                .arg(Arg::new("filters").long("filters").action(ArgAction::SetTrue).conflicts_with_all(info_conflicts)
                    .help("List exact observed metadata values and unique-article counts"))
                .arg(Arg::new("corpus-info").long("corpus-info").action(ArgAction::SetTrue).conflicts_with_all(info_conflicts)
                    .help("Show pinned source provenance, index features and offline notices"))
                .arg(Arg::new("limit").long("limit").default_value("10").value_parser(clap::value_parser!(usize))
                    .help("Maximum results or suggestions (1-50)"))
                .arg(Arg::new("workload").long("workload").num_args(1).help("Exact source-directory workload"))
                .arg(Arg::new("filter").long("filter").num_args(1).action(ArgAction::Append).value_name("FIELD=VALUE")
                    .help("Exact source metadata; repeat for OR within a field, AND across fields"))
                .arg(Arg::new("facet").long("facet").num_args(1).action(ArgAction::Append).value_name("FIELD")
                    .help("Unique matching-article counts; repeat for at most eight fields"))
                .arg(Arg::new("fuzzy").long("fuzzy").default_value("0").value_parser(clap::value_parser!(u8).range(0..=2)))
                .arg(Arg::new("candidate-limit").long("candidate-limit").default_value("200").value_parser(clap::value_parser!(usize))
                    .help("Reranking/grouping candidate pool (1-500; at least --limit)"))
                .arg(Arg::new("facet-budget").long("facet-budget").default_value("5000").value_parser(clap::value_parser!(usize))
                    .help("Maximum matching sections examined for facets (1-10000)"))
                .arg(Arg::new("max-expansions").long("max-expansions").default_value("64").value_parser(clap::value_parser!(usize))
                    .help("Maximum prefix term expansions (1-128)"))
                .arg(Arg::new("prefix").long("prefix").action(ArgAction::SetTrue).help("Expand the final plain-text token as a bounded prefix"))
                .arg(Arg::new("no-stemming").long("no-stemming").action(ArgAction::SetTrue))
                .arg(Arg::new("no-synonyms").long("no-synonyms").action(ArgAction::SetTrue))
                .arg(Arg::new("no-rerank").long("no-rerank").action(ArgAction::SetTrue))
                .arg(Arg::new("sections").long("sections").action(ArgAction::SetTrue).help("Return sections instead of grouping by source article"))
                .arg(Arg::new("format").long("format").value_parser(["json", "text"])),
        )
}

fn docs_help() -> String {
    let behavior = "No credentials, network, extraction, or writable index cache. Boolean AND/OR, quoted phrases and phrase proximity are supported; wildcard and regex syntax are disabled. Rich features require a compatible index profile, never a silent fallback. The corpus is a pinned repository snapshot, not all linked Learn content. Use --filters to discover exact source metadata. Read results with fab-rs get 'docs/<article-path>'. Use --corpus-info for full provenance and license notices.";
    let corpus = match fabric_docs_search::embedded_corpus_info() {
        Ok(Some(manifest)) => format!(
            "Embedded offline corpus:\n  Snapshot: {}\n  Sections: {}; source Markdown inputs: {}\n  Schema: {} (bundle version {})\n  Index: Tantivy {}; features: stemmed={}, metadata={}, hierarchy={}, autocomplete={}",
            terminal_text(&manifest.source_commit),
            manifest.documents,
            manifest.source_markdown_files,
            terminal_text(&manifest.schema_id),
            manifest.schema_version,
            terminal_text(&manifest.tantivy_version),
            manifest.features.stemmed,
            manifest.features.metadata,
            manifest.features.hierarchy,
            manifest.features.autocomplete
        ),
        Ok(None) => "Embedded offline corpus: not configured (engine-only build). Build with FAB_DOCS_INDEX to search local documentation.".into(),
        Err(_) => "Embedded offline corpus metadata is unavailable: the embedded manifest is invalid. No index was opened.".into(),
    };
    format!("{corpus}\n\n{behavior}")
}

/// Convert schema-validated arguments into a bounded local invocation.
///
/// # Errors
/// Rejects invalid bounds and criteria without echoing supplied values.
pub fn from_matches(matches: &ArgMatches) -> Result<DocsCommand, clap::Error> {
    let values = matches.subcommand_matches("docs").ok_or_else(invalid)?;
    let mode = if values.get_flag("corpus-info") {
        DocsMode::CorpusInfo
    } else if values.get_flag("filters") {
        DocsMode::Filters
    } else {
        let filters = values
            .get_many::<String>("filter")
            .into_iter()
            .flatten()
            .map(|value| parse_filter(value))
            .collect::<Result<Vec<_>, _>>()?;
        let suggestion = values.get_one::<String>("suggest");
        let query = SearchOptions {
            text: suggestion
                .or_else(|| values.get_one::<String>("text"))
                .cloned()
                .ok_or_else(invalid)?,
            limit: *values.get_one::<usize>("limit").ok_or_else(invalid)?,
            workload: values.get_one::<String>("workload").cloned(),
            fuzzy: *values.get_one::<u8>("fuzzy").ok_or_else(invalid)?,
        };
        if query.text.chars().any(char::is_control) || filters.len() > 32 {
            return Err(invalid());
        }
        if let Some(workload) = &query.workload
            && filters.iter().any(|filter| filter.field == "workload")
            && !filters
                .iter()
                .any(|filter| filter.field == "workload" && &filter.value == workload)
        {
            return Err(invalid());
        }
        if suggestion.is_some() {
            query.validate().map_err(|_| invalid())?;
            let extra_workload_filter = query.workload.is_some()
                && !filters.iter().any(|filter| filter.field == "workload");
            if !(2..=20).contains(&query.text.chars().count())
                || query.text.len() > 80
                || query.text.to_lowercase().chars().count() > 20
                || !query.text.chars().all(char::is_alphanumeric)
                || filters.len() + usize::from(extra_workload_filter) > 32
            {
                return Err(invalid());
            }
            DocsMode::Suggest {
                prefix: query.text,
                limit: query.limit,
                filters,
                workload: query.workload,
            }
        } else {
            let mut options = RichSearchOptions::new(query);
            options.filters = filters;
            options.facets = values
                .get_many::<String>("facet")
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            options.candidates = *values
                .get_one::<usize>("candidate-limit")
                .ok_or_else(invalid)?;
            options.facet_budget = *values
                .get_one::<usize>("facet-budget")
                .ok_or_else(invalid)?;
            options.max_expansions = *values
                .get_one::<usize>("max-expansions")
                .ok_or_else(invalid)?;
            options.prefix = values.get_flag("prefix");
            options.stemming = !values.get_flag("no-stemming");
            options.synonyms = !values.get_flag("no-synonyms");
            options.rerank = !values.get_flag("no-rerank");
            options.group_articles = !values.get_flag("sections");
            options.validate().map_err(|_| invalid())?;
            if options.prefix
                && (options.query.fuzzy != 0 || !valid_prefix_query(&options.query.text))
            {
                return Err(invalid());
            }
            DocsMode::Search(options)
        }
    };
    let explicit = values.get_one::<String>("format").map(String::as_str);
    let global = matches
        .try_get_one::<String>("output_format")
        .ok()
        .flatten()
        .map(String::as_str);
    if explicit.is_some() && global.is_some() && explicit != global {
        return Err(invalid());
    }
    Ok(DocsCommand {
        mode,
        text_output: explicit.or(global) == Some("text"),
    })
}

fn valid_prefix_query(text: &str) -> bool {
    let tokens: Vec<_> = text.split_whitespace().collect();
    tokens.last().is_some_and(|last| last.chars().count() >= 3)
        && tokens.iter().all(|token| {
            !matches!(token.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT")
                && token.chars().next().is_some_and(char::is_alphanumeric)
                && token
                    .chars()
                    .all(|character| character.is_alphanumeric() || "._/-".contains(character))
        })
}

fn valid_field(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= 80
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn parse_filter(value: &str) -> Result<MetadataFilter, clap::Error> {
    let (field, value) = value.split_once('=').ok_or_else(invalid)?;
    if !valid_field(field)
        || value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    Ok(MetadataFilter {
        field: field.into(),
        value: value.into(),
    })
}

fn invalid() -> clap::Error {
    clap::Error::raw(
        ErrorKind::InvalidValue,
        "Invalid documentation search arguments.",
    )
}

/// Parse and run the local source directly, including help, for the explicit preview head.
///
/// # Errors
/// Reports stream failures without retrying search.
pub fn run_args(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    match command().try_get_matches_from(args) {
        Ok(matches) => match from_matches(&matches) {
            Ok(command) => write(&command, stdout, stderr),
            Err(_) => argument_error(stderr),
        },
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            stdout.write_all(error.to_string().as_bytes())?;
            stdout.flush()?;
            Ok(0)
        }
        Err(_) => argument_error(stderr),
    }
}

fn argument_error(stderr: &mut dyn Write) -> io::Result<u8> {
    writeln!(
        stderr,
        "{}",
        json!({"error":{"code":"invalid_arguments","message":"Invalid docs search arguments. Run search docs --help."}})
    )?;
    stderr.flush()?;
    Ok(2)
}

/// Execute a parsed local invocation before settings, authentication, or update setup.
///
/// # Errors
/// Reports output failures without repeating the operation.
pub fn write(
    command: &DocsCommand,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    write_scoped(command, None, stdout, stderr)
}

/// Execute a local search or suggestion within an exact reviewed documentation area.
///
/// Catalog and corpus-info modes are not area searches and are rejected.
///
/// # Errors
/// Reports output failures without repeating the operation.
pub fn write_in_area(
    command: &DocsCommand,
    area: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    write_scoped(command, Some(area), stdout, stderr)
}

fn write_scoped(
    command: &DocsCommand,
    area: Option<&str>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    if area.is_some() && matches!(&command.mode, DocsMode::Filters | DocsMode::CorpusInfo) {
        return write_error(SearchError::InvalidQuery, stderr);
    }
    let opened = Instant::now();
    let reader = match DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(error) => return write_error(error, stderr),
    };
    let open_us = opened.elapsed().as_micros();
    let manifest = reader.manifest();
    match &command.mode {
        DocsMode::Search(options) => {
            let searched = if let Some(area) = area {
                reader.search_item_docs(area, options)
            } else {
                reader.search_rich(options)
            };
            let result = match searched {
                Ok(result) => result,
                Err(error) => return write_error(error, stderr),
            };
            let references =
                match local_references(result.data.iter().map(|hit| &hit.hit.source_path)) {
                    Ok(references) => references,
                    Err(error) => return write_error(error, stderr),
                };
            if command.text_output {
                let facet_references = match local_references(
                    result
                        .facets
                        .counts
                        .get("source_path")
                        .into_iter()
                        .flat_map(|values| values.keys()),
                ) {
                    Ok(references) => references,
                    Err(error) => return write_error(error, stderr),
                };
                write_search_text(&result, &references, &facet_references, stdout)?;
            } else {
                serde_json::to_writer(
                    &mut *stdout,
                    &json!({
                        "status":"succeeded", "source":"embeddedFabricDocs",
                        "data":search_data(&result.data, &references),
                        "meta":{"sourceCommit":result.source_commit,"indexedSections":result.indexed_sections,
                            "resultLimit":result.result_limit,"openUs":open_us,"timings":result.timings,
                            "candidatesExamined":result.candidates_examined,
                            "candidatesLimited":result.candidates_limited,"expansions":result.expansions,
                            "facets":result.facets,"indexFeatures":manifest.features,
                            "sourceCoverage":source_coverage(manifest),
                            "unresolvedIncludes":manifest.unresolved_includes.len(),
                            "coverage":"pinned Fabric-docs repository; external publishing dependencies not included"}
                    }),
                )?;
                writeln!(stdout)?;
            }
        }
        DocsMode::Suggest {
            prefix,
            limit,
            filters,
            workload,
        } => {
            let mut filters = filters.clone();
            if let Some(workload) = workload {
                if !manifest.features.metadata {
                    return write_error(
                        SearchError::UnsupportedFeature {
                            feature: "metadata",
                        },
                        stderr,
                    );
                }
                if filters.iter().any(|filter| {
                    !manifest
                        .filter_catalog
                        .get(&filter.field)
                        .is_some_and(|values| values.contains_key(&filter.value))
                }) || !manifest
                    .filter_catalog
                    .get("workload")
                    .is_some_and(|values| values.contains_key(workload))
                {
                    return write_error(SearchError::InvalidQuery, stderr);
                }
                filters.retain(|filter| filter.field != "workload");
                filters.push(MetadataFilter {
                    field: "workload".into(),
                    value: workload.clone(),
                });
            }
            let suggested = if let Some(area) = area {
                reader.suggest_item_docs(area, prefix, *limit, &filters)
            } else {
                reader.suggest(prefix, *limit, &filters)
            };
            let suggestions = match suggested {
                Ok(suggestions) => suggestions,
                Err(error) => return write_error(error, stderr),
            };
            let references = match local_references(
                suggestions.iter().map(|suggestion| &suggestion.source_path),
            ) {
                Ok(references) => references,
                Err(error) => return write_error(error, stderr),
            };
            if command.text_output {
                write_wrapped(stdout, REFERENCE_GUIDANCE, "", "")?;
                write_wrapped(
                    stdout,
                    "Title/heading suggestions (bounded source-section pool, not an exhaustive catalog):",
                    "",
                    "",
                )?;
                for (rank, (suggestion, reference)) in
                    suggestions.iter().zip(&references).enumerate()
                {
                    writeln!(stdout)?;
                    write_wrapped(
                        stdout,
                        &format!("{}. {}", rank + 1, display_text(&suggestion.text)),
                        "",
                        "   ",
                    )?;
                    write_wrapped(stdout, &display_text(&suggestion.workload), "   ", "   ")?;
                    writeln!(stdout, "   {}", reference.reference)?;
                }
                if suggestions.is_empty() {
                    writeln!(stdout, "No matching documentation titles or headings.")?;
                }
            } else {
                serde_json::to_writer(
                    &mut *stdout,
                    &json!({
                        "status":"succeeded","source":"embeddedFabricDocs",
                        "data":suggestion_data(&suggestions, &references),
                        "meta":{"sourceCommit":manifest.source_commit,"indexedSections":manifest.documents,
                            "resultLimit":limit,"openUs":open_us,"indexFeatures":manifest.features,
                            "coverage":"bounded title/heading suggestions, not exhaustive enumeration",
                            "sourceCoverage":source_coverage(manifest)}
                    }),
                )?;
                writeln!(stdout)?;
            }
        }
        DocsMode::Filters => {
            if !manifest.features.metadata {
                return write_error(
                    SearchError::UnsupportedFeature {
                        feature: "metadata",
                    },
                    stderr,
                );
            }
            if command.text_output {
                let source_references = match local_references(
                    manifest
                        .filter_catalog
                        .get("source_path")
                        .into_iter()
                        .flat_map(|values| values.keys()),
                ) {
                    Ok(references) => references,
                    Err(error) => return write_error(error, stderr),
                };
                write_wrapped(stdout, REFERENCE_GUIDANCE, "", "")?;
                writeln!(
                    stdout,
                    "Exact observed metadata (unique source-article counts):"
                )?;
                for (field, values) in &manifest.filter_catalog {
                    writeln!(stdout, "\n{}", terminal_text(field))?;
                    if field == "source_path" {
                        for ((_, count), reference) in values.iter().zip(&source_references) {
                            writeln!(stdout, "  {}: {count}", reference.reference)?;
                        }
                    } else {
                        for (value, count) in values {
                            write_wrapped(
                                stdout,
                                &format!("{}: {count}", display_text(value)),
                                "  ",
                                "    ",
                            )?;
                        }
                    }
                }
                let item_type_fields = observed_item_type_fields(&manifest.filter_catalog);
                writeln!(stdout)?;
                write_wrapped(
                    stdout,
                    &format!(
                        "Indexed item-type metadata fields observed (item_type, itemType, ms.item_type): {}. Missing values are not inferred from article mentions.",
                        if item_type_fields.is_empty() {
                            "none".into()
                        } else {
                            item_type_fields.join(", ")
                        }
                    ),
                    "",
                    "",
                )?;
                write_wrapped(
                    stdout,
                    &format!(
                        "Reviewed item-type supplement applied: {}. See --corpus-info TRANSFORMATION-ATTRIBUTION for provenance; supplemented values are not original article frontmatter.",
                        has_item_type_supplement(&manifest.licenses)
                    ),
                    "",
                    "",
                )?;
                write_wrapped(
                    stdout,
                    &format!(
                        "Item-type coverage (unique articles): curated {}; source-authored {}. Only dedicated reviewed articles are classified; this is not full Fabric coverage.",
                        curated_item_type_articles(&manifest.licenses)
                            .map_or_else(|| "unavailable".into(), |count| count.to_string()),
                        source_authored_item_type_articles(
                            &manifest.source_repository,
                            &manifest.source_commit
                        )
                        .map_or_else(|| "unavailable".into(), |count| count.to_string())
                    ),
                    "",
                    "",
                )?;
                write_wrapped(
                    stdout,
                    &format!(
                        "Coverage: pinned repository only; {} unresolved includes. See --corpus-info for provenance and notices.",
                        manifest.unresolved_includes.len()
                    ),
                    "",
                    "",
                )?;
            } else {
                serde_json::to_writer(
                    &mut *stdout,
                    &json!({
                        "status":"succeeded","source":"embeddedFabricDocs","data":manifest.filter_catalog,
                        "meta":{"sourceCommit":manifest.source_commit,"indexFeatures":manifest.features,
                            "countUnit":"uniqueSourceArticles","sourceCoverage":source_coverage(manifest)}
                    }),
                )?;
                writeln!(stdout)?;
            }
        }
        DocsMode::CorpusInfo => {
            if command.text_output {
                writeln!(stdout, "{}", serde_json::to_string_pretty(manifest)?)?;
            } else {
                serde_json::to_writer(
                    &mut *stdout,
                    &json!({
                        "status":"succeeded","source":"embeddedFabricDocs","data":manifest
                    }),
                )?;
                writeln!(stdout)?;
            }
        }
    }
    stdout.flush()?;
    Ok(0)
}

fn source_coverage(manifest: &BundleManifest) -> Value {
    let item_type_fields = observed_item_type_fields(&manifest.filter_catalog);
    let curated_articles = manifest
        .features
        .metadata
        .then(|| curated_item_type_articles(&manifest.licenses))
        .flatten();
    let source_authored_articles = manifest
        .features
        .metadata
        .then(|| {
            source_authored_item_type_articles(&manifest.source_repository, &manifest.source_commit)
        })
        .flatten();
    json!({
        "provenance":"Use --corpus-info for the source repository and legal notices.",
        "sourceMarkdownFiles":manifest.source_markdown_files,
        "sourceMarkdownBytes":manifest.source_markdown_bytes,
        "skippedInputs":manifest.skipped_inputs.len(),
        "unresolvedIncludes":manifest.unresolved_includes.len(),
        "scope":"pinned repository snapshot, not all linked Learn content",
        "metadataCatalogAvailable":manifest.features.metadata,
        "itemTypeFieldObserved":manifest.features.metadata.then_some(!item_type_fields.is_empty()),
        "itemTypeFieldsObserved":manifest.features.metadata.then_some(&item_type_fields),
        "missingItemType":manifest.features.metadata.then_some(item_type_fields.is_empty()),
        "reviewedItemTypeSupplementApplied":has_item_type_supplement(&manifest.licenses),
        "itemTypeAttribution":"See --corpus-info TRANSFORMATION-ATTRIBUTION. Reviewed supplements are not original article frontmatter.",
        "itemTypeCoverage":{
            "curatedArticles":curated_articles,"sourceAuthoredArticles":source_authored_articles,
            "countUnit":"uniqueSourceArticles",
            "scope":"dedicated reviewed articles only; unmapped articles remain unknown, not full Fabric coverage"
        },
        "missingMetadata":"Absent source fields are not inferred from article mentions."
    })
}

fn curated_item_type_articles(notices: &[LicenseNotice]) -> Option<usize> {
    let notice = notices.iter().find(|notice| {
        notice.name == "TRANSFORMATION-ATTRIBUTION"
            && notice.text.contains("Reviewed item-type supplement:")
    })?;
    let (_, evidence) = notice.text.split_once("Reviewed item-type supplement:")?;
    let paths: BTreeSet<_> = evidence
        .lines()
        .filter(|line| line.contains(" Source: "))
        .filter_map(|line| line.strip_prefix("- docs/"))
        .filter_map(|line| line.split_once(": ").map(|(path, _)| path))
        .collect();
    Some(paths.len())
}

fn source_authored_item_type_articles(repository: &str, commit: &str) -> Option<usize> {
    (repository == ITEM_TYPE_INVENTORY_REPOSITORY && commit == ITEM_TYPE_INVENTORY_COMMIT)
        .then_some(0)
}

fn has_item_type_supplement(notices: &[LicenseNotice]) -> bool {
    notices.iter().any(|notice| {
        notice.name == "TRANSFORMATION-ATTRIBUTION"
            && notice.text.contains("Reviewed item-type supplement:")
    })
}

fn observed_item_type_fields(catalog: &FilterCatalog) -> Vec<&'static str> {
    ["item_type", "itemType", "ms.item_type"]
        .into_iter()
        .filter(|field| catalog.contains_key(*field))
        .collect()
}

struct LocalReference {
    address: String,
    reference: String,
}

fn local_references<'a>(
    sources: impl IntoIterator<Item = &'a String>,
) -> Result<Vec<LocalReference>, SearchError> {
    sources
        .into_iter()
        .map(|source| {
            if terminal_text(source) != source.as_str() {
                return Err(SearchError::InvalidQuery);
            }
            Ok(LocalReference {
                address: address_for_source(source)?,
                reference: reference_for_source(source)?,
            })
        })
        .collect()
}

fn search_data(hits: &[RichHit], references: &[LocalReference]) -> Vec<Value> {
    hits.iter()
        .zip(references)
        .map(|(hit, reference)| {
            json!({
                "id":hit.hit.id,
                "title":hit.hit.title,
                "heading":hit.hit.heading,
                "url":reference.reference,
                "address":reference.address,
                "reference":reference.reference,
                "source_path":hit.hit.source_path,
                "workload":hit.hit.workload,
                "score":hit.hit.score,
                "snippet":hit.hit.snippet,
                "article_id":hit.article_id,
                "heading_path":hit.heading_path,
                "metadata":hit.metadata,
                "highlights":hit.highlights,
                "rerank_bonus":hit.rerank_bonus
            })
        })
        .collect()
}

fn suggestion_data(suggestions: &[Suggestion], references: &[LocalReference]) -> Vec<Value> {
    suggestions
        .iter()
        .zip(references)
        .map(|(suggestion, reference)| {
            json!({
                "text":suggestion.text,
                "source_path":suggestion.source_path,
                "workload":suggestion.workload,
                "address":reference.address,
                "reference":reference.reference
            })
        })
        .collect()
}

fn write_search_text(
    result: &RichSearchResult,
    references: &[LocalReference],
    facet_references: &[LocalReference],
    stdout: &mut dyn Write,
) -> io::Result<()> {
    write_wrapped(stdout, REFERENCE_GUIDANCE, "", "")?;
    for (rank, (hit, reference)) in result.data.iter().zip(references).enumerate() {
        let hierarchy = if hit.heading_path.is_empty() {
            display_text(&hit.hit.heading)
        } else {
            hit.heading_path
                .iter()
                .map(|heading| display_text(heading))
                .collect::<Vec<_>>()
                .join(" > ")
        };
        writeln!(stdout)?;
        write_wrapped(
            stdout,
            &format!("{}. {}", rank + 1, display_text(&hit.hit.title)),
            "",
            "   ",
        )?;
        write_wrapped(
            stdout,
            &format!(
                "{} | score {:.2}",
                display_text(&hit.hit.workload),
                hit.hit.score
            ),
            "   ",
            "   ",
        )?;
        if !hierarchy.is_empty() && hierarchy != display_text(&hit.hit.title) {
            write_wrapped(stdout, &hierarchy, "   Section: ", "            ")?;
        }
        write_wrapped(
            stdout,
            &highlighted_text(&hit.hit.snippet, &hit.highlights),
            "   ",
            "   ",
        )?;
        // Keep the command lossless and copyable, even for unusually long article paths.
        writeln!(stdout, "   {}", reference.reference)?;
    }
    if result.data.is_empty() {
        writeln!(stdout, "No matching documentation articles or sections.")?;
    }
    if result.candidates_limited {
        writeln!(stdout)?;
        write_wrapped(
            stdout,
            &format!(
                "Candidate coverage is limited to {} sections; grouping/reranking may omit other matches.",
                result.candidates_examined
            ),
            "",
            "",
        )?;
    }
    for (field, values) in &result.facets.counts {
        writeln!(
            stdout,
            "\nFacet {} (unique matching articles):",
            display_text(field)
        )?;
        if field == "source_path" {
            for ((_, count), reference) in values.iter().zip(facet_references) {
                writeln!(stdout, "  {}: {count}", reference.reference)?;
            }
        } else {
            for (value, count) in values {
                write_wrapped(
                    stdout,
                    &format!("{}: {count}", display_text(value)),
                    "  ",
                    "    ",
                )?;
            }
        }
    }
    if !result.facets.complete {
        write_wrapped(
            stdout,
            &format!(
                "Facet coverage is partial: {} matching sections examined; counts are not full-corpus totals.",
                result.facets.examined_sections
            ),
            "",
            "",
        )?;
    }
    if !result.expansions.is_empty() {
        write_wrapped(
            stdout,
            &format!(
                "Lexical expansions: {}",
                result
                    .expansions
                    .iter()
                    .map(|value| display_text(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "",
            "",
        )?;
    }
    Ok(())
}

fn write_wrapped(
    stdout: &mut dyn Write,
    text: &str,
    first_indent: &str,
    following_indent: &str,
) -> io::Result<()> {
    let mut line = first_indent.to_owned();
    let mut width = text_width(first_indent);
    let mut indent_width = width;
    for word in text.split_whitespace() {
        let word_width = text_width(word);
        let separator = usize::from(width > indent_width);
        if width + separator + word_width > TEXT_WIDTH && width > indent_width {
            writeln!(stdout, "{line}")?;
            line = following_indent.to_owned();
            width = text_width(following_indent);
            indent_width = width;
        }
        if width > indent_width {
            line.push(' ');
            width += 1;
        }
        for character in word.chars() {
            let character_width = character_width(character);
            if width + character_width > TEXT_WIDTH {
                writeln!(stdout, "{line}")?;
                line = following_indent.to_owned();
                width = text_width(following_indent);
                indent_width = width;
            }
            line.push(character);
            width += character_width;
        }
    }
    if width > indent_width {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn text_width(value: &str) -> usize {
    value.chars().map(character_width).sum()
}

fn character_width(character: char) -> usize {
    // Conservatively budget two cells for Unicode without a terminal-width dependency.
    if character.is_ascii() { 1 } else { 2 }
}

fn web_address_ranges(value: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    let lowercase = value.to_ascii_lowercase();
    while let Some(offset) = lowercase[cursor..].find("http") {
        let start = cursor + offset;
        if lowercase[start..].starts_with("https://") || lowercase[start..].starts_with("http://") {
            let length = value[start..]
                .find(|character: char| character.is_whitespace() || "<>\"".contains(character))
                .unwrap_or(value.len() - start);
            cursor = start + length;
            ranges.push((start, cursor));
        } else {
            cursor = start + "http".len();
        }
    }
    ranges
}

fn display_text(value: &str) -> String {
    let mut rendered = String::new();
    let mut cursor = 0;
    for (start, end) in web_address_ranges(value) {
        rendered.push_str(&value[cursor..start]);
        rendered.push_str("[external link]");
        cursor = end;
    }
    rendered.push_str(&value[cursor..]);
    terminal_text(&rendered)
}

fn terminal_text(value: &str) -> String {
    let mut rendered = String::new();
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
            )
        {
            rendered.extend(character.escape_default());
        } else {
            rendered.push(character);
        }
    }
    rendered
}

fn highlighted_text(snippet: &str, spans: &[HighlightSpan]) -> String {
    let mut rendered = String::new();
    let mut cursor = 0;
    let web_addresses = web_address_ranges(snippet);
    for span in spans {
        if span.start < cursor
            || span.start >= span.end
            || web_addresses
                .iter()
                .any(|&(start, end)| span.start < end && span.end > start)
        {
            continue;
        }
        let (Some(before), Some(highlight)) = (
            snippet.get(cursor..span.start),
            snippet.get(span.start..span.end),
        ) else {
            continue;
        };
        rendered.push_str(&display_text(before));
        rendered.push_str("[[");
        rendered.push_str(&display_text(highlight));
        rendered.push_str("]]");
        cursor = span.end;
    }
    rendered.push_str(&display_text(&snippet[cursor..]));
    rendered
}

fn write_error(error: SearchError, stderr: &mut dyn Write) -> io::Result<u8> {
    let (code, exit, message) = match &error {
        SearchError::InvalidQuery => ("invalid_arguments", 2, "Invalid documentation query, metadata field/value, or bounds. Run search docs --help and use --filters for exact observed metadata.".to_owned()),
        SearchError::ExpansionLimit => ("docs_search_expansion_limit", 2, "Documentation prefix expansion exceeds its finite budget. Use a longer prefix. For --text --prefix, --max-expansions may be raised within its supported bounds.".to_owned()),
        SearchError::UnsupportedFeature { feature } => ("docs_search_unsupported_feature", 1, format!("The embedded documentation index profile does not support {feature}. Rebuild with the required profile; no fallback was used.")),
        SearchError::NotEmbedded => ("docs_search_not_configured", 1, error.to_string()),
        _ => ("docs_search_failed", 1, error.to_string()),
    };
    writeln!(
        stderr,
        "{}",
        json!({"error":{"code":code,"message":message}})
    )?;
    stderr.flush()?;
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::{
        DocsMode, ITEM_TYPE_INVENTORY_COMMIT, ITEM_TYPE_INVENTORY_REPOSITORY, command,
        curated_item_type_articles, display_text, from_matches, has_item_type_supplement,
        highlighted_text, local_references, observed_item_type_fields, search_data,
        source_authored_item_type_articles, terminal_text, text_width, write_error, write_in_area,
        write_search_text, write_wrapped,
    };
    use fabric_docs_search::{
        Hit, LicenseNotice, SearchError, Timings,
        rich::{FacetResult, FilterCatalog, HighlightSpan, RichHit, RichSearchResult},
    };

    fn card_fixture() -> RichSearchResult {
        RichSearchResult {
            data: vec![RichHit {
                hit: Hit {
                    id: "docs/data-engineering/lakehouse.md#tables".into(),
                    title: "Work with Lakehouse tables".into(),
                    heading: "Tables".into(),
                    url: "https://learn.microsoft.com/fabric/data-engineering/lakehouse".into(),
                    source_url: "https://github.com/MicrosoftDocs/fabric-docs/blob/pin/docs/data-engineering/lakehouse.md".into(),
                    source_path: "docs/data-engineering/lakehouse.md".into(),
                    workload: "data-engineering".into(),
                    score: 12.345_678,
                    snippet: "Explore 湖 tables safely.\u{1b}[31m See https://example.invalid/details for more.".into(),
                },
                article_id: "docs/data-engineering/lakehouse.md".into(),
                heading_path: vec!["Lakehouse".into(), "Tables".into()],
                metadata: [("ms.topic".into(), vec!["conceptual".into()])].into(),
                highlights: vec![HighlightSpan { start: 8, end: 11 }],
                rerank_bonus: 0.123_456,
            }],
            source_commit: "snapshot-must-not-appear".into(),
            indexed_sections: 31_343,
            result_limit: 1,
            candidates_examined: 1,
            candidates_limited: false,
            expansions: vec![],
            facets: FacetResult {
                counts: FilterCatalog::new(),
                examined_sections: 0,
                complete: true,
            },
            timings: Timings {
                parse_us: 0,
                search_us: 0,
                fetch_us: 0,
                total_us: 0,
            },
        }
    }

    #[test]
    fn search_cards_are_local_ranked_readable_and_round_only_the_display_score() {
        let result = card_fixture();
        let references =
            local_references(result.data.iter().map(|hit| &hit.hit.source_path)).unwrap();
        let mut stdout = Vec::new();
        write_search_text(&result, &references, &[], &mut stdout).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.starts_with("Read any result offline"));
        assert_eq!(
            text.matches("References use local docs addresses.").count(),
            1
        );
        assert!(text.contains("1. Work with Lakehouse tables"));
        assert!(text.contains("data-engineering | score 12.35"));
        assert!(text.contains("Section: Lakehouse > Tables"));
        assert!(text.contains("[[湖]] tables safely."));
        assert!(text.contains("\\u{1b}[31m"));
        assert!(text.contains("fab-rs get 'docs/data-engineering/lakehouse.md'"));
        for forbidden in [
            "https://",
            "http://",
            "Fabric docs snapshot",
            "snapshot-must-not-appear",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(text.lines().all(|line| text_width(line) <= 96));

        let data = search_data(&result.data, &references);
        assert_eq!(
            data[0]["score"],
            serde_json::json!(result.data[0].hit.score)
        );
        assert_eq!(
            data[0]["rerank_bonus"],
            serde_json::json!(result.data[0].rerank_bonus)
        );
        assert_ne!(data[0]["score"], serde_json::json!(12.35));
        assert_eq!(data[0]["address"], "docs/data-engineering/lakehouse.md");
        assert_eq!(data[0]["reference"], references[0].reference);
        assert_eq!(data[0]["url"], data[0]["reference"]);
        assert!(data[0].get("source_url").is_none());
        assert_eq!(data[0]["snippet"], result.data[0].hit.snippet);
    }

    #[test]
    fn prose_wraps_long_identifiers_without_splitting_utf8_or_exceeding_width() {
        let text = format!("{} {}", "é湖".repeat(80), "identifier".repeat(30));
        let mut stdout = Vec::new();
        write_wrapped(&mut stdout, &text, "   ", "   ").unwrap();
        let rendered = String::from_utf8(stdout).unwrap();
        assert!(rendered.lines().count() > 5);
        assert!(rendered.lines().all(|line| text_width(line) <= 96));
        let unwrapped: String = rendered.split_whitespace().collect();
        assert_eq!(unwrapped, text.split_whitespace().collect::<String>());
    }

    #[test]
    fn local_reference_validation_rejects_bad_sources_before_output() {
        for source in [
            "../secret.md",
            "https://example.invalid/article",
            "docs/unsafe\u{202e}.md",
        ] {
            let source = source.to_owned();
            assert!(local_references([&source]).is_err());
        }
        let source = "docs/data-engineering/lakehouse.md".to_owned();
        let references = local_references([&source]).unwrap();
        assert_eq!(references[0].address, "docs/data-engineering/lakehouse.md");
        assert_eq!(
            references[0].reference,
            "fab-rs get 'docs/data-engineering/lakehouse.md'"
        );
    }

    #[test]
    fn empty_search_and_meaningful_coverage_warnings_remain_visible() {
        let mut result = card_fixture();
        result.data.clear();
        result.candidates_limited = true;
        result.facets.complete = false;
        let mut stdout = Vec::new();
        write_search_text(&result, &[], &[], &mut stdout).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.contains("No matching documentation articles or sections."));
        assert!(text.contains("Candidate coverage is limited"));
        assert!(text.contains("Facet coverage is partial"));
        assert!(!text.contains("snapshot"));
    }

    #[test]
    fn source_path_facets_show_local_get_references_instead_of_bare_paths() {
        let mut result = card_fixture();
        let source = result.data[0].hit.source_path.clone();
        result
            .facets
            .counts
            .insert("source_path".into(), [(source.clone(), 1)].into());
        let references = local_references([&source]).unwrap();
        let mut stdout = Vec::new();
        write_search_text(&result, &references, &references, &mut stdout).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.contains("fab-rs get 'docs/data-engineering/lakehouse.md': 1"));
        assert!(!text.contains("\n  docs/data-engineering/lakehouse.md"));
    }

    #[test]
    fn area_writer_rejects_catalog_and_provenance_modes_before_index_setup() {
        for mode in ["--filters", "--corpus-info"] {
            let matches = command()
                .try_get_matches_from(["search", "docs", mode])
                .unwrap();
            let parsed = from_matches(&matches).unwrap();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                write_in_area(&parsed, "lakehouse", &mut stdout, &mut stderr).unwrap(),
                2
            );
            assert!(stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(error["error"]["code"], "invalid_arguments");
        }
    }

    #[test]
    fn curated_article_counts_come_from_distinct_attribution_evidence_paths() {
        let notices = [LicenseNotice {
            name: "TRANSFORMATION-ATTRIBUTION".into(),
            text: "Reviewed item-type supplement:\n\
                   - docs/a.md: Lakehouse. Source: https://example.invalid/a\n\
                   - docs/a.md: Lakehouse. Source: https://example.invalid/a\n\
                   - docs/b.md: Notebook. Source: https://example.invalid/b\n\
                   - docs/not-evidence.md: no source link\n"
                .into(),
        }];
        assert_eq!(curated_item_type_articles(&notices), Some(2));
        assert_eq!(curated_item_type_articles(&[]), None);
        assert_eq!(
            source_authored_item_type_articles(
                ITEM_TYPE_INVENTORY_REPOSITORY,
                ITEM_TYPE_INVENTORY_COMMIT
            ),
            Some(0)
        );
        assert_eq!(
            source_authored_item_type_articles(ITEM_TYPE_INVENTORY_REPOSITORY, "fixture"),
            None
        );
        assert_eq!(
            source_authored_item_type_articles(
                "https://example.invalid",
                ITEM_TYPE_INVENTORY_COMMIT
            ),
            None
        );
    }

    #[test]
    fn reviewed_item_type_supplement_requires_the_explicit_transformation_notice() {
        let mut notices = vec![LicenseNotice {
            name: "LICENSE".into(),
            text: "Reviewed item-type supplement:".into(),
        }];
        assert!(!has_item_type_supplement(&notices));
        notices[0].name = "TRANSFORMATION-ATTRIBUTION".into();
        notices[0].text = "No item-type supplement was applied.".into();
        assert!(!has_item_type_supplement(&notices));
        notices[0].text = "\nReviewed item-type supplement: exact-pin reviewed paths.".into();
        assert!(has_item_type_supplement(&notices));
    }

    #[test]
    fn item_type_coverage_uses_all_exact_source_spellings_without_renaming() {
        let mut catalog = FilterCatalog::new();
        assert!(observed_item_type_fields(&catalog).is_empty());
        for field in ["item_type", "itemType", "ms.item_type"] {
            catalog.clear();
            catalog.insert(field.into(), [("Lakehouse".into(), 1)].into());
            assert_eq!(observed_item_type_fields(&catalog), [field]);
            assert!(catalog.contains_key(field));
        }
        catalog.insert("item_type".into(), [("Warehouse".into(), 2)].into());
        catalog.insert("itemType".into(), [("Notebook".into(), 3)].into());
        assert_eq!(
            observed_item_type_fields(&catalog),
            ["item_type", "itemType", "ms.item_type"]
        );
    }

    #[test]
    fn parsed_rich_options_preserve_defaults_and_each_explicit_control() {
        let matches = command()
            .try_get_matches_from(["search", "docs", "--text", "lakehouse"])
            .unwrap();
        let parsed = from_matches(&matches).unwrap();
        assert!(!parsed.text_output);
        let DocsMode::Search(defaults) = parsed.mode else {
            panic!("expected rich search");
        };
        assert!(
            defaults.stemming && defaults.synonyms && defaults.group_articles && defaults.rerank
        );
        assert!(!defaults.prefix);
        assert_eq!(defaults.candidates, 200);
        assert_eq!(defaults.max_expansions, 64);
        assert_eq!(defaults.facet_budget, 5000);
        assert_eq!(defaults.query.limit, 10);
        assert!(defaults.filters.is_empty() && defaults.facets.is_empty());

        let matches = command()
            .try_get_matches_from([
                "search",
                "docs",
                "--text",
                "lakehouse tabl",
                "--workload",
                "data-engineering",
                "--filter",
                "ms.topic=conceptual",
                "--filter",
                "ms.topic=how-to",
                "--facet",
                "workload",
                "--limit",
                "2",
                "--candidate-limit",
                "11",
                "--max-expansions",
                "12",
                "--facet-budget",
                "13",
                "--prefix",
                "--no-stemming",
                "--no-synonyms",
                "--no-rerank",
                "--sections",
                "--format",
                "text",
            ])
            .unwrap();
        let parsed = from_matches(&matches).unwrap();
        assert!(parsed.text_output);
        let DocsMode::Search(options) = parsed.mode else {
            panic!("expected rich search");
        };
        assert!(
            !options.stemming && !options.synonyms && !options.group_articles && !options.rerank
        );
        assert!(options.prefix);
        assert_eq!(options.query.text, "lakehouse tabl");
        assert_eq!(options.query.workload.as_deref(), Some("data-engineering"));
        assert_eq!(options.query.limit, 2);
        assert_eq!(options.candidates, 11);
        assert_eq!(options.max_expansions, 12);
        assert_eq!(options.facet_budget, 13);
        assert_eq!(options.facets, ["workload"]);
        assert_eq!(options.filters.len(), 2);
        assert_eq!(options.filters[0].field, "ms.topic");
        assert_eq!(options.filters[0].value, "conceptual");
        assert_eq!(options.filters[1].value, "how-to");
    }

    #[test]
    fn highlights_use_raw_utf8_byte_boundaries_before_terminal_escaping() {
        let raw = "é\t湖house\\path";
        assert_eq!(
            highlighted_text(raw, &[HighlightSpan { start: 3, end: 6 }]),
            "é\\t[[湖]]house\\path"
        );
        assert_eq!(
            terminal_text("docs\\lake\n\u{1b}[31m"),
            "docs\\lake\\n\\u{1b}[31m"
        );
        assert_eq!(
            terminal_text("a\u{202e}b\u{2028}c"),
            "a\\u{202e}b\\u{2028}c"
        );
    }

    #[test]
    fn invalid_overlapping_or_out_of_range_spans_never_slice_invalid_utf8() {
        let spans = [
            HighlightSpan { start: 1, end: 2 },
            HighlightSpan { start: 0, end: 2 },
            HighlightSpan { start: 0, end: 3 },
            HighlightSpan { start: 2, end: 99 },
            HighlightSpan { start: 3, end: 3 },
        ];
        assert_eq!(highlighted_text("é湖", &spans), "[[é]]湖");
    }

    #[test]
    fn terminal_previews_hide_web_addresses_without_corrupting_highlights() {
        let snippet = "湖 https://example.invalid/path more";
        assert_eq!(display_text(snippet), "湖 [external link] more");
        assert_eq!(
            display_text("See HTTPS://example.invalid/path for details."),
            "See [external link] for details."
        );
        assert_eq!(
            highlighted_text(
                snippet,
                &[
                    HighlightSpan { start: 0, end: 3 },
                    HighlightSpan { start: 4, end: 9 },
                ],
            ),
            "[[湖]] [external link] more"
        );
        assert_eq!(
            terminal_text("a\u{200b}\u{2060}\u{feff}b"),
            "a\\u{200b}\\u{2060}\\u{feff}b"
        );
    }

    #[test]
    fn query_profile_and_expansion_failures_have_explicit_safe_diagnostics() {
        for (error, code, exit) in [
            (SearchError::InvalidQuery, "invalid_arguments", 2),
            (SearchError::NotEmbedded, "docs_search_not_configured", 1),
            (
                SearchError::ExpansionLimit,
                "docs_search_expansion_limit",
                2,
            ),
            (
                SearchError::UnsupportedFeature {
                    feature: "autocomplete",
                },
                "docs_search_unsupported_feature",
                1,
            ),
        ] {
            let mut stderr = Vec::new();
            assert_eq!(write_error(error, &mut stderr).unwrap(), exit);
            let value: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(value["error"]["code"], code);
            assert!(value["error"]["message"].as_str().unwrap().len() > 10);
        }
    }
}
