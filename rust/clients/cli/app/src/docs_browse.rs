// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free documentation browsing and item-area entry points.

use clap::{Arg, ArgMatches, Command, error::ErrorKind};
use fabric_docs_search::{DocsSearch, SearchError, browse::validate_address};
use serde_json::json;
use std::{
    ffi::OsString,
    io::{self, Write},
};

/// One local article or item-area operation, independent of Fabric resource hosts.
#[derive(Clone, Debug)]
pub struct DocsBrowseCommand {
    mode: Mode,
    text: bool,
}

#[derive(Clone, Debug)]
enum Mode {
    Get {
        address: String,
        max_bytes: usize,
    },
    Area {
        name: String,
        search: Option<crate::docs_search::DocsCommand>,
    },
}

fn invalid() -> clap::Error {
    clap::Error::raw(
        ErrorKind::InvalidValue,
        "Invalid local documentation arguments. Run fab-rs search docs --help.",
    )
}

/// Whether one token selects the local docs namespace, without treating other GETs as local.
pub fn is_docs_address(value: &str) -> bool {
    value == "docs" || value == "docs/" || value.starts_with("docs/")
}

/// Select the first GET positional without mistaking option values for addresses.
pub fn selects_docs_get(args: &[OsString]) -> bool {
    let mut index = usize::from(args.first().is_some_and(|value| value == "get"));
    while let Some(value) = args.get(index).and_then(|value| value.to_str()) {
        index += 1;
        if value == "--" {
            return args
                .get(index)
                .and_then(|value| value.to_str())
                .is_some_and(is_docs_address);
        }
        if !value.starts_with('-') || value == "-" {
            return is_docs_address(value);
        }
        let name = value.split_once('=').map_or(value, |(name, _)| name);
        if value.contains('=') {
            continue;
        }
        match name {
            "--format" | "--output_format" | "--max-bytes" => index += 1,
            "-q" | "--query" | "-o" | "--output" => {
                while args
                    .get(index)
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| !value.starts_with('-'))
                {
                    index += 1;
                }
            }
            "-v" | "--verbose" | "-f" | "--force" | "-h" | "--help" => {}
            _ => return false,
        }
    }
    false
}

/// A separate local GET schema; it never broadens the existing remote GET schema.
pub fn get_command() -> Command {
    Command::new("get")
        .about("Read stored documentation locally; no network or credentials")
        .args_override_self(false)
        .arg(Arg::new("address").required(true).num_args(1))
        .arg(Arg::new("path").num_args(0..=1))
        .arg(Arg::new("format").long("format").value_parser(["text", "json"]).default_value("text"))
        .arg(Arg::new("output_format").long("output_format").value_parser(["text", "json"]))
        .arg(Arg::new("max-bytes").long("max-bytes").default_value("2097152")
            .value_parser(clap::value_parser!(usize)))
        .after_help("Use get docs/ for the local documentation guide. Search results print exact local article references. A #section-N suffix reads one stored section. Stored text is normalized documentation, not a byte-for-byte source Markdown export.")
}

/// Parse local GET arguments, including the `get` token, without opening an index.
///
/// # Errors
/// Rejects unsafe addresses, ambiguous path forms, excessive bounds and unsupported arguments.
pub fn parse_get(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<DocsBrowseCommand, clap::Error> {
    let values = get_command().try_get_matches_from(args)?;
    let first = values.get_one::<String>("address").ok_or_else(invalid)?;
    let address = match (first.as_str(), values.get_one::<String>("path")) {
        ("docs" | "docs/", Some(path)) if !path.starts_with("docs/") => format!("docs/{path}"),
        (_, Some(_)) => return Err(invalid()),
        (_, None) if is_docs_address(first) => first.clone(),
        _ => return Err(invalid()),
    };
    if !matches!(address.as_str(), "docs" | "docs/") {
        validate_address(&address).map_err(|_| invalid())?;
    }
    let max_bytes = *values.get_one::<usize>("max-bytes").ok_or_else(invalid)?;
    if !(1..=8 * 1024 * 1024).contains(&max_bytes) {
        return Err(invalid());
    }
    let explicit = values.get_one::<String>("format").map(String::as_str);
    let global = values
        .get_one::<String>("output_format")
        .map(String::as_str);
    if values.value_source("format") == Some(clap::parser::ValueSource::CommandLine)
        && global.is_some()
        && explicit != global
    {
        return Err(invalid());
    }
    let format = global.or(explicit).ok_or_else(invalid)?;
    Ok(DocsBrowseCommand {
        mode: Mode::Get { address, max_bytes },
        text: format == "text",
    })
}

/// Register source-backed item documentation areas without adding a remote item implementation.
pub fn item_command() -> Command {
    let mut docs = crate::docs_search::command()
        .find_subcommand("docs")
        .expect("registered docs schema")
        .clone();
    docs = docs.mut_group("mode", |group| group.required(false));
    Command::new("item").about("Item-specific documentation areas")
        .args_override_self(false).arg_required_else_help(true)
        .subcommand(Command::new("lakehouse")
            .about("Lakehouse documentation from reviewed source navigation, not a guessed type classifier")
            .args_override_self(false).arg_required_else_help(true)
            .subcommand(docs))
}

/// Convert an item-area schema match to an explicitly local operation.
///
/// # Errors
/// Rejects unknown areas and unsupported query modes before any service setup.
pub fn item_from_matches(values: &ArgMatches) -> Result<DocsBrowseCommand, clap::Error> {
    let lakehouse = values.subcommand_matches("lakehouse").ok_or_else(invalid)?;
    let docs = lakehouse.subcommand_matches("docs").ok_or_else(invalid)?;
    let search_mode = docs.get_one::<String>("text").is_some()
        || docs.get_one::<String>("suggest").is_some()
        || docs.get_flag("filters")
        || docs.get_flag("corpus-info");
    let explicit = docs.get_one::<String>("format").map(String::as_str);
    let global = docs
        .try_get_one::<String>("output_format")
        .ok()
        .flatten()
        .map(String::as_str);
    if explicit.is_some() && global.is_some() && explicit != global {
        return Err(invalid());
    }
    let text = explicit.or(global).is_none_or(|format| format == "text");
    let search = if search_mode {
        Some(crate::docs_search::from_matches(lakehouse)?)
    } else {
        for id in [
            "filter",
            "facet",
            "workload",
            "fuzzy",
            "limit",
            "prefix",
            "candidate-limit",
            "facet-budget",
            "max-expansions",
            "no-stemming",
            "no-synonyms",
            "no-rerank",
            "sections",
        ] {
            if docs.value_source(id) == Some(clap::parser::ValueSource::CommandLine) {
                return Err(invalid());
            }
        }
        None
    };
    Ok(DocsBrowseCommand {
        mode: Mode::Area {
            name: "lakehouse".into(),
            search,
        },
        text,
    })
}

/// Parse an item-area invocation, including its `item` token, without opening an index.
///
/// # Errors
/// Returns Clap display results or sanitized local argument failures.
pub fn parse_item(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<DocsBrowseCommand, clap::Error> {
    let matches = item_command().try_get_matches_from(args)?;
    item_from_matches(&matches)
}

fn terminal(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character == '\n'
                || character == '\t'
                || !character.is_control()
                    && !matches!(character, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                character.to_string().chars().collect::<Vec<_>>()
            } else {
                character.escape_default().collect()
            }
        })
        .collect()
}

/// Write a local browsing operation without reading settings, credentials or remote metadata.
///
/// # Errors
/// Propagates output failures without replaying retrieval.
pub fn write(
    command: &DocsBrowseCommand,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    if let Mode::Area {
        name,
        search: Some(search),
    } = &command.mode
    {
        return crate::docs_search::write_in_area(search, name, stdout, stderr);
    }
    let reader = match DocsSearch::embedded() {
        Ok(reader) => reader,
        Err(error) => return write_error(error, stderr),
    };
    match &command.mode {
        Mode::Get { address, .. } if matches!(address.as_str(), "docs" | "docs/") => {
            let areas = reader.item_area("lakehouse");
            if command.text {
                writeln!(
                    stdout,
                    "FABRIC DOCUMENTATION\n\nRead:    fab-rs get 'docs/<source-path>.md'\nSearch:  fab-rs search docs --text <query>\nExplore: fab-rs item lakehouse docs\n\nContent is read from the embedded snapshot without network access.\nUse fab-rs search docs -h for the documentation version and capabilities."
                )?;
            } else {
                let area = match areas {
                    Ok(area) => Some(area),
                    Err(SearchError::UnsupportedFeature { .. }) | Err(SearchError::NotFound) => {
                        None
                    }
                    Err(error) => return write_error(error, stderr),
                };
                writeln!(
                    stdout,
                    "{}",
                    json!({"status":"succeeded","source":"embeddedFabricDocs",
                    "data":{"namespace":"docs/","get":"fab-rs get 'docs/<source-path>.md'",
                        "search":"fab-rs search docs --text <query>","areas":area}})
                )?;
            }
        }
        Mode::Get { address, max_bytes } => {
            let article = match reader.get_docs(address, *max_bytes) {
                Ok(article) => article,
                Err(error) => return write_error(error, stderr),
            };
            if command.text {
                writeln!(
                    stdout,
                    "{}\n{}\n",
                    terminal(&article.title),
                    terminal(&article.reference)
                )?;
                for section in article.sections {
                    if !section.heading.is_empty() {
                        writeln!(stdout, "\n{}\n", terminal(&section.heading))?;
                    }
                    writeln!(stdout, "{}", terminal(&section.content))?;
                }
            } else {
                writeln!(
                    stdout,
                    "{}",
                    json!({"status":"succeeded","source":"embeddedFabricDocs","data":article})
                )?;
            }
        }
        Mode::Area { name, search: None } => {
            let area = match reader.item_area(name) {
                Ok(area) => area,
                Err(error) => return write_error(error, stderr),
            };
            if command.text {
                writeln!(
                    stdout,
                    "LAKEHOUSE DOCUMENTATION\n\nA reviewed source-navigation area; not an inferred item-type classification.\nSearch within this area: fab-rs item lakehouse docs --text <query>\nRead any entry using its local reference.\n"
                )?;
                for article in area.articles {
                    writeln!(
                        stdout,
                        "  {}{}",
                        terminal(&article.reference),
                        if article.indexed {
                            ""
                        } else {
                            "  (not indexed in this snapshot)"
                        }
                    )?;
                }
            } else {
                writeln!(
                    stdout,
                    "{}",
                    json!({"status":"succeeded","source":"embeddedFabricDocs","data":area})
                )?;
            }
        }
        Mode::Area {
            search: Some(_), ..
        } => unreachable!("query dispatched before index setup"),
    }
    stdout.flush()?;
    Ok(0)
}

fn write_error(error: SearchError, stderr: &mut dyn Write) -> io::Result<u8> {
    let code = match error {
        SearchError::NotFound => "docs_not_found",
        SearchError::InvalidQuery => "invalid_arguments",
        SearchError::ResultLimit => "docs_result_limit",
        SearchError::NotEmbedded => "docs_search_not_configured",
        _ => "docs_read_failed",
    };
    writeln!(
        stderr,
        "{}",
        json!({"error":{"code":code,"message":error.to_string()}})
    )?;
    stderr.flush()?;
    Ok(if matches!(error, SearchError::InvalidQuery) {
        2
    } else {
        1
    })
}
